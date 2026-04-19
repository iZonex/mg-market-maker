use crate::cks_ofi::OfiTracker;
use crate::learned_microprice::LearnedMicroprice;
use mm_common::orderbook::LocalOrderBook;
use mm_common::types::{Price, Side, Trade};
use mm_indicators::Hma;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::debug;

/// Default window for the optional HMA alpha feed. Chosen to
/// match the upstream `mm-toolbox` quickstart and give a HMA
/// lag of ~3 samples on mid-price updates.
pub const DEFAULT_HMA_WINDOW: usize = 9;

/// Default EWMA smoothing applied to CKS OFI observations when
/// they fold into `alpha()`. Half-life of ~10 events.
const OFI_EWMA_ALPHA: Decimal = dec!(0.07);

/// Momentum / alpha signals for quote adjustment.
///
/// Short-term price predictability from:
/// 1. Order book imbalance
/// 2. Trade flow imbalance (signed volume)
/// 3. Micro-price (improved mid estimate)
/// 4. Hull Moving Average slope on mid-price (optional,
///    opt-in via `with_hma`)
/// 5. Cont-Kukanov-Stoikov L1 OFI (optional, opt-in via
///    `with_ofi`). Epic D sub-component #1.
/// 6. Stoikov 2018 learned-microprice drift (optional, opt-in
///    via `with_learned_microprice`). Epic D sub-component #2.
///
/// The output is an `alpha` value (expected price change direction)
/// that shifts the reservation price per Cartea-Jaimungal:
///   reservation = mid + alpha * (T - t) - gamma * sigma^2 * q * (T - t)
pub struct MomentumSignals {
    /// Recent signed trade volumes for flow imbalance.
    signed_volumes: VecDeque<Decimal>,
    window: usize,
    /// Optional Hull Moving Average on mid-price updates. When
    /// attached, `alpha()` folds in a 5-th component that
    /// captures the slope of the HMA — positive when the HMA
    /// is above the current mid, negative when below.
    hma: Option<Hma>,
    /// Most recent HMA value sampled before the current
    /// update. Used to compute the HMA slope as
    /// `(now - prev) / mid`.
    hma_prev: Option<Decimal>,
    /// Optional CKS OFI tracker. When attached, the engine
    /// feeds top-of-book snapshots via
    /// [`MomentumSignals::on_l1_snapshot`] and `alpha()` folds
    /// the EWMA of emitted OFI observations.
    ofi: Option<OfiTracker>,
    /// EWMA state for OFI contributions. Maintained in
    /// lockstep with `ofi` — reset to `None` when `with_ofi`
    /// is re-called.
    ofi_ewma: Option<Decimal>,
    /// Epic G — EWMA of squared OFI observations, so we can
    /// expose `ofi_z = ofi_ewma / sqrt(ofi_ewma_sq)`. Same
    /// α as `ofi_ewma`, so the two decay together.
    ofi_ewma_sq: Option<Decimal>,
    /// Optional learned-microprice model. When attached,
    /// `alpha()` folds `(mp_learned − mid) / mid` as a
    /// micro-price drift component.
    learned_mp: Option<LearnedMicroprice>,
    /// Epic D stage-2 — ring of recent `(imbalance, spread,
    /// mid)` snapshots used to feed the model's online fit.
    /// `Some` iff `with_learned_microprice_online` was called;
    /// on every `on_l1_snapshot` the oldest entry (at
    /// horizon depth) gets paired with the current mid as
    /// a `Δmid` observation and routed through
    /// `LearnedMicroprice::update_online`.
    online_mp_ring: Option<LearnedMpOnlineRing>,
}

/// Bounded snapshot ring that pairs a horizon-k-old
/// `(imbalance, spread)` with the current mid to produce
/// `Δmid` observations for the online lMP fit. `horizon`
/// must match the horizon the offline fit was built against
/// — otherwise the online path would bias against a
/// different lookahead than the g-matrix was trained on.
#[derive(Debug, Clone)]
struct LearnedMpOnlineRing {
    horizon: usize,
    snapshots: VecDeque<(Decimal, Decimal, Decimal)>,
}

impl MomentumSignals {
    pub fn new(window: usize) -> Self {
        Self {
            signed_volumes: VecDeque::with_capacity(window),
            window,
            hma: None,
            hma_prev: None,
            ofi: None,
            ofi_ewma: None,
            ofi_ewma_sq: None,
            learned_mp: None,
            online_mp_ring: None,
        }
    }

    /// Attach a Hull Moving Average on mid-price updates.
    /// Builder-style: returns `self` so callers can chain. Once
    /// attached the engine should call
    /// [`MomentumSignals::on_mid`] on every mid-price refresh
    /// so the HMA sees a steady sample stream.
    pub fn with_hma(mut self, window: usize) -> Self {
        self.hma = Some(Hma::new(window));
        self
    }

    /// Attach a Cont-Kukanov-Stoikov OFI tracker (Epic D
    /// sub-component #1). The engine feeds new L1 snapshots via
    /// [`MomentumSignals::on_l1_snapshot`]; emitted observations
    /// are EWMA-smoothed and the result folds into
    /// `alpha()` as an additional predictive component.
    pub fn with_ofi(mut self) -> Self {
        self.ofi = Some(OfiTracker::new());
        self.ofi_ewma = None;
        self.ofi_ewma_sq = None;
        self
    }

    /// Attach a Stoikov 2018 learned micro-price model (Epic D
    /// sub-component #2). The model must already be `finalize`d;
    /// `alpha()` reads `predict(imbalance, spread)` on every
    /// call and folds the drift-to-mid ratio.
    pub fn with_learned_microprice(mut self, model: LearnedMicroprice) -> Self {
        self.learned_mp = Some(model);
        self
    }

    /// Attach a Stoikov 2018 learned micro-price model with
    /// **online streaming fit** (Epic D stage-2). In addition
    /// to the read-path `predict`, every
    /// [`Self::on_l1_snapshot`] call feeds the oldest
    /// horizon-k-ago `(imbalance, spread)` paired with the
    /// current mid as a `Δmid` observation into the model's
    /// [`LearnedMicroprice::update_online`]. The model's
    /// `refit_every` config controls how often the g-matrix
    /// is rebuilt from the ring; this ring here just tracks
    /// the horizon alignment.
    ///
    /// `horizon` MUST match the horizon the offline fit was
    /// built with — otherwise the online path biases the
    /// g-matrix against a different lookahead than it was
    /// trained on. `1 ≤ horizon ≤ 10 000`.
    pub fn with_learned_microprice_online(
        mut self,
        model: LearnedMicroprice,
        horizon: usize,
    ) -> Self {
        assert!(horizon >= 1, "horizon must be >= 1");
        self.learned_mp = Some(model);
        self.online_mp_ring = Some(LearnedMpOnlineRing {
            horizon,
            snapshots: VecDeque::with_capacity(horizon + 1),
        });
        self
    }

    /// Feed a top-of-book L1 snapshot into the optional OFI
    /// tracker and — if the online lMP fit is attached — into
    /// the g-matrix refresh ring. No-op when neither is
    /// configured. Callers should invoke this on every book
    /// update so the tracker sees the full event stream.
    pub fn on_l1_snapshot(
        &mut self,
        bid_px: Decimal,
        bid_qty: Decimal,
        ask_px: Decimal,
        ask_qty: Decimal,
    ) {
        // OFI update (optional).
        if let Some(tracker) = self.ofi.as_mut() {
            if let Some(obs) = tracker.update(bid_px, bid_qty, ask_px, ask_qty) {
                // Normalise by average depth so the EWMA stays
                // dimensionless — a 10-qty arrival on a BTC book
                // should not swamp a 0.1-qty arrival on an SOL
                // book.
                let depth = bid_qty + ask_qty;
                let normalised = if depth.is_zero() {
                    Decimal::ZERO
                } else {
                    obs / depth
                };
                self.ofi_ewma = Some(match self.ofi_ewma {
                    None => normalised,
                    Some(prev) => OFI_EWMA_ALPHA * normalised + (dec!(1) - OFI_EWMA_ALPHA) * prev,
                });
                // Epic G — track E[x²] in the same α so
                // `ofi_z` is ready on the same timeline as
                // `ofi_ewma`. Using squared observation (not
                // centred around running mean) is the
                // Cont-Kukanov-Stoikov "signal-to-RMS" form:
                // a one-sided-flow regime shows up as
                // `|ewma| / √sq ≈ 1`, while a symmetric
                // quiet regime gives ≈ 0.
                let sq = normalised * normalised;
                self.ofi_ewma_sq = Some(match self.ofi_ewma_sq {
                    None => sq,
                    Some(prev) => OFI_EWMA_ALPHA * sq + (dec!(1) - OFI_EWMA_ALPHA) * prev,
                });
            }
        }

        // Epic D stage-2 — online lMP fit pipeline.
        if let (Some(model), Some(ring)) = (self.learned_mp.as_mut(), self.online_mp_ring.as_mut())
        {
            // Current-tick state. Top-level imbalance + absolute
            // spread — same shape the offline fit consumed.
            let total_qty = bid_qty + ask_qty;
            let imbalance = if total_qty.is_zero() {
                Decimal::ZERO
            } else {
                (bid_qty - ask_qty) / total_qty
            };
            let spread = ask_px - bid_px;
            let mid = (bid_px + ask_px) / dec!(2);

            ring.snapshots.push_back((imbalance, spread, mid));
            // When the ring exceeds `horizon + 1` entries, the
            // oldest snapshot pairs with the current mid as an
            // `(imbalance_{t-h}, spread_{t-h}, Δmid)` observation.
            // We keep horizon entries in the ring after the
            // emission so subsequent ticks keep finding a
            // horizon-aligned partner.
            if ring.snapshots.len() > ring.horizon {
                let (old_imb, old_spread, old_mid) = ring
                    .snapshots
                    .pop_front()
                    .expect("len > horizon implies non-empty");
                let delta_mid = mid - old_mid;
                model.update_online(old_imb, old_spread, delta_mid);
            }
        }
    }

    /// Current smoothed OFI, `None` when `with_ofi` is off or
    /// before the first observation has landed.
    pub fn ofi_ewma(&self) -> Option<Decimal> {
        self.ofi_ewma
    }

    /// Epic G — signed OFI z-score in the signal-to-RMS form:
    /// `ofi_ewma / sqrt(max(ofi_ewma_sq, ε))`. Result stays
    /// in `[-1, +1]` roughly (single-sided pressure
    /// saturates at ±1; balanced tape at ≈ 0). Consumed by
    /// `SocialRiskEngine` for the `mentions × OFI`
    /// cross-validation gate.
    pub fn ofi_z(&self) -> Option<Decimal> {
        let (x, sq) = (self.ofi_ewma?, self.ofi_ewma_sq?);
        if sq <= dec!(0.000000001) {
            return Some(Decimal::ZERO);
        }
        // Newton iteration on Decimal for √sq — we avoid
        // f64 round-trip to keep the workspace's
        // "Decimal-for-signal" discipline. Six iterations
        // from a seed of `sq / 2` converge to ~12 digits
        // for values in `(0, 100]`.
        let mut s = sq / dec!(2);
        for _ in 0..10 {
            if s.is_zero() {
                s = sq;
            }
            s = (s + sq / s) / dec!(2);
        }
        if s.is_zero() {
            Some(Decimal::ZERO)
        } else {
            Some(x / s)
        }
    }

    /// Feed a mid-price update into the optional HMA stream.
    /// No-op when `with_hma` has not been called. Callers
    /// should invoke this once per engine tick before
    /// `alpha()` so the HMA sees every mid sample.
    pub fn on_mid(&mut self, mid: Price) {
        if let Some(h) = self.hma.as_mut() {
            self.hma_prev = h.value();
            h.update(mid);
        }
    }

    /// Current HMA value, if attached and warmed up.
    pub fn hma_value(&self) -> Option<Decimal> {
        self.hma.as_ref().and_then(|h| h.value())
    }

    /// HMA slope as a fraction of `mid` — `(now − prev)/mid`.
    /// Returns `None` until two consecutive HMA readings are
    /// available.
    pub fn hma_slope(&self, mid: Price) -> Option<Decimal> {
        if mid.is_zero() {
            return None;
        }
        let now = self.hma_value()?;
        let prev = self.hma_prev?;
        Some((now - prev) / mid)
    }

    /// Record a public trade.
    pub fn on_trade(&mut self, trade: &Trade) {
        let signed_vol = match trade.taker_side {
            Side::Buy => trade.qty * trade.price,
            Side::Sell => -(trade.qty * trade.price),
        };
        self.signed_volumes.push_back(signed_vol);
        if self.signed_volumes.len() > self.window {
            self.signed_volumes.pop_front();
        }
    }

    /// Order book imbalance at top N levels.
    /// Returns [-1, 1]: positive = more bids (bullish pressure).
    pub fn book_imbalance(book: &LocalOrderBook, levels: usize) -> Decimal {
        book.imbalance(levels).unwrap_or(dec!(0))
    }

    /// Trade flow imbalance over recent window.
    /// Returns a value in approximate [-1, 1] range.
    pub fn trade_flow_imbalance(&self) -> Decimal {
        if self.signed_volumes.is_empty() {
            return dec!(0);
        }
        let total: Decimal = self.signed_volumes.iter().sum();
        let abs_total: Decimal = self.signed_volumes.iter().map(|v| v.abs()).sum();
        if abs_total.is_zero() {
            return dec!(0);
        }
        total / abs_total
    }

    /// 22W-6 — EMA-weighted variant of [`Self::trade_flow_imbalance`]
    /// that emphasises the **most recent** trades via
    /// `mm_indicators::ema_weights`. Same [-1, 1] output range
    /// and zero-safety as the uniform version; the difference
    /// is the oldest trades in the window get small weight, so
    /// a fresh flip in flow surfaces quickly without waiting
    /// for the window to shift.
    ///
    /// `alpha = None` uses the upstream default `3 / (N + 1)`.
    pub fn trade_flow_imbalance_ema_weighted(&self, alpha: Option<Decimal>) -> Decimal {
        let n = self.signed_volumes.len();
        if n < 2 {
            return self.trade_flow_imbalance();
        }
        let weights = mm_indicators::ema_weights(n, alpha);
        // Weights are oldest → newest; signed_volumes is the
        // same order (VecDeque push_back for new trades, front
        // is oldest).
        let mut num = dec!(0);
        let mut den = dec!(0);
        for (w, v) in weights.iter().zip(self.signed_volumes.iter()) {
            num += *w * *v;
            den += *w * v.abs();
        }
        if den.is_zero() {
            dec!(0)
        } else {
            num / den
        }
    }

    /// Micro-price: improved mid-price using order book imbalance.
    ///
    /// micro_price = ask * bid_qty / (bid_qty + ask_qty) + bid * ask_qty / (bid_qty + ask_qty)
    /// This is the weighted mid price — more weight to the side with more quantity.
    pub fn micro_price(book: &LocalOrderBook) -> Option<Price> {
        book.weighted_mid()
    }

    /// Compute combined alpha signal.
    ///
    /// Returns expected price direction * magnitude.
    /// Positive = expected up-move, negative = expected down-move.
    ///
    /// The alpha is in terms of fraction of mid-price.
    ///
    /// Component weights rebalance dynamically based on which
    /// optional signals are attached. Wave-1 components
    /// (imbalance, flow, microprice) always contribute; HMA,
    /// OFI, and learned micro-price shave fixed fractions off
    /// the wave-1 weights when attached. The raw alpha is kept
    /// in `[-1, 1]` and then scaled by `0.0001` so a full-
    /// saturation signal produces at most 1 bps of mid-price
    /// shift.
    pub fn alpha(&self, book: &LocalOrderBook, mid: Price) -> Decimal {
        if mid.is_zero() {
            return dec!(0);
        }

        // Wave-1 components.
        let book_imb = Self::book_imbalance(book, 5);
        let flow_imb = self.trade_flow_imbalance();
        let micro_dev = Self::micro_price(book)
            .map(|mp| (mp - mid) / mid)
            .unwrap_or(dec!(0));

        // Wave-1 optional: HMA slope.
        let hma_slope = self.hma_slope(mid);

        // Wave-2 optional components (Epic D sub-components #1 + #2).
        let ofi_component = self.ofi_ewma;
        let learned_mp_dev = self.learned_microprice_drift(book, mid);

        // Dynamically balance weights. The rule: wave-1
        // baseline is 0.4 / 0.4 / 0.2 (book / flow / micro).
        // Each optional signal that is attached pulls
        // 0.1 of aggregate weight off the wave-1 baseline.
        // The remaining 0.1 allocation rebalances across the
        // wave-1 components proportionally.
        let hma_on = hma_slope.is_some();
        let ofi_on = ofi_component.is_some();
        let lmp_on = learned_mp_dev.is_some();
        let optional_count = u32::from(hma_on) + u32::from(ofi_on) + u32::from(lmp_on);
        let optional_weight = Decimal::from(optional_count) * dec!(0.1);
        let wave1_scale = dec!(1) - optional_weight;

        let mut alpha =
            (book_imb * dec!(0.4) + flow_imb * dec!(0.4) + micro_dev * dec!(0.2)) * wave1_scale;

        if let Some(slope) = hma_slope {
            alpha += slope.max(dec!(-1)).min(dec!(1)) * dec!(0.1);
        }
        if let Some(ofi) = ofi_component {
            alpha += ofi.max(dec!(-1)).min(dec!(1)) * dec!(0.1);
        }
        if let Some(lmp) = learned_mp_dev {
            alpha += lmp.max(dec!(-1)).min(dec!(1)) * dec!(0.1);
        }

        // Scale: raw alpha is in [-1, 1], scale to a small
        // fraction of price. This determines how aggressive
        // the momentum adjustment is.
        let scaled = alpha * dec!(0.0001);

        debug!(
            book_imbalance = %book_imb,
            trade_flow = %flow_imb,
            micro_dev = %micro_dev,
            hma_slope = ?hma_slope,
            ofi = ?ofi_component,
            learned_mp = ?learned_mp_dev,
            alpha = %scaled,
            "momentum signals"
        );

        scaled
    }

    /// Learned-microprice drift relative to the current mid,
    /// as a fraction of mid. Returns `None` when no model is
    /// attached or the model hasn't been finalized yet.
    ///
    /// Promoted to `pub` in Epic D stage-3 so the engine's
    /// dashboard publication path can read the latest drift
    /// without re-deriving the (imbalance, spread) lookup.
    pub fn learned_microprice_drift(&self, book: &LocalOrderBook, mid: Price) -> Option<Decimal> {
        let model = self.learned_mp.as_ref()?;
        if !model.is_finalized() || mid.is_zero() {
            return None;
        }
        // Compute the L1 imbalance + spread on the fly from
        // the local book so callers don't have to maintain a
        // parallel feature-extraction path.
        let bid_px = book.best_bid()?;
        let ask_px = book.best_ask()?;
        let bid_qty = *book.bids.get(&bid_px)?;
        let ask_qty = *book.asks.get(&ask_px)?;
        let total = bid_qty + ask_qty;
        if total.is_zero() {
            return None;
        }
        let imbalance = (bid_qty - ask_qty) / total;
        let spread = ask_px - bid_px;
        let predicted_delta = model.predict(imbalance, spread);
        if predicted_delta.is_zero() {
            return None;
        }
        Some(predicted_delta / mid)
    }

    /// 22B-4 + 22B-6 — snapshot the tracker's persistent state.
    /// Covers:
    ///   * `signed_volumes` — rolling trade-flow window (22B-6)
    ///   * `ofi_ewma` / `ofi_ewma_sq` — OFI EWMA state (22B-6)
    ///   * Stoikov learned-microprice `g_matrix` + spread edges
    ///     (already Serialize via `LearnedMicroprice`) (22B-4)
    ///   * `online_mp_ring` horizon + snapshots (22B-4) — the
    ///     struct is `#[serde(skip)]` on the LearnedMicroprice
    ///     side so we explicitly capture it here.
    ///
    /// Without this, a restart drops all momentum history and
    /// the learned-microprice model reverts to its offline fit,
    /// needing 500+ live observations to catch the ring back up.
    /// Returns `None` iff every optional component is `None` and
    /// the rolling `signed_volumes` window is empty — nothing
    /// worth persisting.
    pub fn snapshot_state(&self) -> Option<serde_json::Value> {
        let nothing = self.signed_volumes.is_empty()
            && self.ofi_ewma.is_none()
            && self.learned_mp.is_none()
            && self.online_mp_ring.is_none();
        if nothing {
            return None;
        }
        let online_ring = self.online_mp_ring.as_ref().map(|r| {
            let snapshots: Vec<(String, String, String)> = r
                .snapshots
                .iter()
                .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
                .collect();
            serde_json::json!({
                "horizon": r.horizon,
                "snapshots": snapshots,
            })
        });
        Some(serde_json::json!({
            "schema_version": 1,
            "window": self.window,
            "signed_volumes": self.signed_volumes.iter()
                .map(|d| d.to_string()).collect::<Vec<_>>(),
            "ofi_ewma": self.ofi_ewma.as_ref().map(|d| d.to_string()),
            "ofi_ewma_sq": self.ofi_ewma_sq.as_ref().map(|d| d.to_string()),
            "learned_mp": self.learned_mp.as_ref()
                .map(|m| serde_json::to_value(m).ok()).flatten(),
            "online_mp_ring": online_ring,
        }))
    }

    /// 22B-4 + 22B-6 — restore a previously captured state. See
    /// [`Self::snapshot_state`] for what's persisted. Schema
    /// version gate keeps future breaking changes behind a
    /// loud failure. `learned_mp`, `online_mp_ring`, and the
    /// EWMA fields are only restored when the destination
    /// tracker has the corresponding component already attached
    /// (via `with_*` builders) — restoring into a bare tracker
    /// without OFI / learned-mp would silently re-enable those
    /// subsystems, which the caller didn't ask for.
    pub fn restore_state(&mut self, state: &serde_json::Value) -> Result<(), String> {
        let schema = state.get("schema_version").and_then(|v| v.as_u64());
        if schema != Some(1) {
            return Err(format!(
                "momentum checkpoint has unsupported schema_version {schema:?}"
            ));
        }
        let signed_volumes: VecDeque<Decimal> = state
            .get("signed_volumes")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "momentum: missing signed_volumes".to_string())?
            .iter()
            .filter_map(|v| v.as_str()?.parse::<Decimal>().ok())
            .collect();

        let ofi_ewma = state
            .get("ofi_ewma")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Decimal>().ok());
        let ofi_ewma_sq = state
            .get("ofi_ewma_sq")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Decimal>().ok());

        // Truncate if the on-disk window is longer than the
        // current cap — supports future window-tuning without
        // rejecting legacy checkpoints.
        let mut trimmed = signed_volumes;
        while trimmed.len() > self.window {
            trimmed.pop_front();
        }
        self.signed_volumes = trimmed;

        // Only restore optional components when the caller has
        // already attached them via the builders — silently
        // skipping otherwise preserves the "config drives
        // subsystem shape" invariant.
        if self.ofi.is_some() {
            self.ofi_ewma = ofi_ewma;
            self.ofi_ewma_sq = ofi_ewma_sq;
        }
        if let Some(model_json) = state.get("learned_mp").filter(|v| !v.is_null()) {
            if self.learned_mp.is_some() {
                let model: LearnedMicroprice =
                    serde_json::from_value(model_json.clone())
                        .map_err(|e| format!("momentum: bad learned_mp: {e}"))?;
                self.learned_mp = Some(model);
            }
        }
        if let Some(ring_json) = state.get("online_mp_ring").filter(|v| !v.is_null()) {
            if let Some(ring) = self.online_mp_ring.as_mut() {
                let snapshots_json = ring_json
                    .get("snapshots")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| "momentum: missing online_mp_ring.snapshots".to_string())?;
                let snaps: VecDeque<(Decimal, Decimal, Decimal)> = snapshots_json
                    .iter()
                    .filter_map(|t| {
                        let arr = t.as_array()?;
                        let a = arr.first()?.as_str()?.parse().ok()?;
                        let b = arr.get(1)?.as_str()?.parse().ok()?;
                        let c = arr.get(2)?.as_str()?.parse().ok()?;
                        Some((a, b, c))
                    })
                    .collect();
                ring.snapshots = snaps;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mm_common::types::PriceLevel;

    #[test]
    fn test_book_imbalance() {
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(10),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let imb = MomentumSignals::book_imbalance(&book, 5);
        // (10 - 5) / (10 + 5) = 0.333
        assert!(imb > dec!(0.3));
    }

    #[test]
    fn test_trade_flow() {
        let mut signals = MomentumSignals::new(100);
        for _ in 0..10 {
            signals.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        let flow = signals.trade_flow_imbalance();
        assert_eq!(flow, dec!(1)); // All buys.
    }

    #[test]
    fn test_alpha_neutral() {
        let signals = MomentumSignals::new(100);
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(5),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();
        let alpha = signals.alpha(&book, mid);
        // Balanced book, no trades → alpha ≈ 0.
        assert!(alpha.abs() < dec!(0.0001));
    }

    // ----- HMA wiring tests -----

    /// Without `with_hma` the HMA accessors return `None` and
    /// `on_mid` is a no-op.
    #[test]
    fn hma_is_none_by_default() {
        let mut s = MomentumSignals::new(10);
        s.on_mid(dec!(100));
        s.on_mid(dec!(101));
        assert!(s.hma_value().is_none());
        assert!(s.hma_slope(dec!(100)).is_none());
    }

    /// After `with_hma` the HMA warms up and produces a value
    /// on enough mid-price samples.
    #[test]
    fn hma_warms_up_after_enough_samples() {
        let mut s = MomentumSignals::new(10).with_hma(DEFAULT_HMA_WINDOW);
        for i in 0..40 {
            s.on_mid(dec!(100) + Decimal::from(i));
        }
        assert!(s.hma_value().is_some());
        // Slope must be positive on a rising mid stream.
        let slope = s.hma_slope(dec!(130)).unwrap();
        assert!(slope > dec!(0));
    }

    /// A warmed-up HMA on a rising stream should drive the
    /// alpha positive — i.e. produce a larger output than the
    /// same `MomentumSignals` without the HMA attached.
    #[test]
    fn hma_attached_tilts_alpha_positive_on_rising_mid() {
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(5),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();

        // Baseline signals — no HMA, same trade stream.
        let mut baseline = MomentumSignals::new(10);
        for _ in 0..20 {
            baseline.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        let base_alpha = baseline.alpha(&book, mid);

        // With HMA on a rising mid stream — slope positive,
        // alpha should be biased up compared to baseline.
        let mut withhma = MomentumSignals::new(10).with_hma(DEFAULT_HMA_WINDOW);
        for _ in 0..20 {
            withhma.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        for i in 0..40 {
            withhma.on_mid(dec!(100) + Decimal::from(i));
        }
        let hma_alpha = withhma.alpha(&book, mid);

        assert!(
            hma_alpha > dec!(0),
            "HMA alpha must stay positive on a rising stream: {hma_alpha}"
        );
        // The two alphas use different weight splits, so the
        // direct comparison is a sanity check: neither should
        // be zero, and neither should be NaN-like.
        assert!(base_alpha > dec!(0));
    }

    // ------ Epic D sub-component #1 + #2 — OFI + learned MP ------

    fn balanced_book() -> LocalOrderBook {
        let mut b = LocalOrderBook::new("BTCUSDT".into());
        b.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(10),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(10),
            }],
            1,
        );
        b
    }

    #[test]
    fn ofi_is_none_by_default() {
        let m = MomentumSignals::new(20);
        assert!(m.ofi_ewma().is_none());
    }

    #[test]
    fn with_ofi_then_l1_snapshots_populate_ewma() {
        let mut m = MomentumSignals::new(20).with_ofi();
        // Seed.
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        assert!(m.ofi_ewma().is_none(), "first snapshot only seeds");
        // Aggressive bid arrival → positive OFI.
        m.on_l1_snapshot(dec!(100), dec!(10), dec!(101), dec!(10));
        let ewma = m.ofi_ewma().expect("EWMA populated");
        assert!(ewma > dec!(0), "positive OFI expected, got {ewma}");
    }

    #[test]
    fn ofi_stream_emits_positive_ewma_on_growing_bid_depth() {
        // Run a stream of monotonically growing bid depth at
        // a fixed touch — every event contributes a strictly
        // positive bid delta, so the EWMA accumulates upward.
        let mut m = MomentumSignals::new(20).with_ofi();
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        for n in 1..=20 {
            let bid_qty = dec!(10) + Decimal::from(n);
            m.on_l1_snapshot(dec!(99), bid_qty, dec!(101), dec!(10));
        }
        let ewma = m.ofi_ewma().expect("EWMA populated");
        assert!(ewma > dec!(0), "expected positive smoothed OFI, got {ewma}");
    }

    #[test]
    fn ofi_z_saturates_near_one_on_one_sided_stream() {
        // Same one-sided bid-growth stream as above — the z
        // score should settle above 0.5 since every
        // observation contributes the same sign.
        let mut m = MomentumSignals::new(20).with_ofi();
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        for n in 1..=30 {
            let bid_qty = dec!(10) + Decimal::from(n);
            m.on_l1_snapshot(dec!(99), bid_qty, dec!(101), dec!(10));
        }
        let z = m.ofi_z().expect("z populated");
        assert!(z > dec!(0.5), "expected strong positive z, got {z}");
        assert!(z <= dec!(1.5), "z should be bounded near signal/RMS, got {z}");
    }

    #[test]
    fn ofi_z_near_zero_on_balanced_tape() {
        // Alternate aggressive bids + aggressive asks —
        // mean near zero, RMS positive → z ≈ 0.
        let mut m = MomentumSignals::new(20).with_ofi();
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        for i in 0..30 {
            let (bq, aq) = if i % 2 == 0 {
                (dec!(15), dec!(10))
            } else {
                (dec!(10), dec!(15))
            };
            m.on_l1_snapshot(dec!(99), bq, dec!(101), aq);
        }
        let z = m.ofi_z().expect("z populated");
        assert!(z.abs() < dec!(0.5), "balanced tape z should be near 0, got {z}");
    }

    #[test]
    fn ofi_z_none_before_any_snapshot() {
        let m = MomentumSignals::new(20).with_ofi();
        assert!(m.ofi_z().is_none());
    }

    #[test]
    fn ofi_alpha_tilts_versus_baseline() {
        // Direct alpha comparison: balanced book → baseline = 0.
        // Attach OFI + feed positive depth growth → alpha tilts up.
        let book = balanced_book();
        let mid = book.mid_price().unwrap();
        let base = MomentumSignals::new(20).alpha(&book, mid);
        assert_eq!(base, dec!(0));

        let mut m = MomentumSignals::new(20).with_ofi();
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        for n in 1..=20 {
            m.on_l1_snapshot(dec!(99), dec!(10) + Decimal::from(n), dec!(101), dec!(10));
        }
        let ofi_alpha = m.alpha(&book, mid);
        assert!(
            ofi_alpha > dec!(0),
            "OFI-attached alpha should be positive, got {ofi_alpha}"
        );
    }

    #[test]
    fn learned_mp_is_none_until_attached_and_finalized() {
        let book = balanced_book();
        let mid = book.mid_price().unwrap();
        let m = MomentumSignals::new(20);
        // No model attached → drift is None.
        assert!(m.learned_microprice_drift(&book, mid).is_none());

        // Attach an unfinalized model → still None.
        let model = crate::learned_microprice::LearnedMicroprice::empty(
            crate::learned_microprice::LearnedMicropriceConfig::default(),
        );
        let m2 = MomentumSignals::new(20).with_learned_microprice(model);
        assert!(m2.learned_microprice_drift(&book, mid).is_none());
    }

    #[test]
    fn learned_mp_finalized_with_zero_buckets_returns_none() {
        let book = balanced_book();
        let mid = book.mid_price().unwrap();
        // A fresh `empty` + `finalize` model has zero in
        // every bucket → predict returns 0 → drift is None.
        let mut model = crate::learned_microprice::LearnedMicroprice::empty(
            crate::learned_microprice::LearnedMicropriceConfig::default(),
        );
        model.finalize();
        let m = MomentumSignals::new(20).with_learned_microprice(model);
        assert!(m.learned_microprice_drift(&book, mid).is_none());
    }

    #[test]
    fn learned_mp_negative_prediction_pulls_alpha_below_baseline() {
        // Train a model so the high-imbalance bucket predicts
        // a *negative* Δmid (mean-reversion). On a bid-heavy
        // book, the wave-1 components want to push alpha up;
        // the LMP component pushes it back down. Net: the
        // LMP-attached alpha should be strictly less than the
        // baseline.
        let cfg = crate::learned_microprice::LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 2,
            ..Default::default()
        };
        let mut model = crate::learned_microprice::LearnedMicroprice::empty(cfg);
        for _ in 0..5 {
            // Big magnitude → enough drift to overcome the
            // wave-1-weight reduction from attaching one
            // optional signal.
            model.accumulate(dec!(0.9), dec!(1), dec!(-50));
        }
        model.finalize();

        let mut tilted = LocalOrderBook::new("BTCUSDT".into());
        tilted.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(50),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(2),
            }],
            1,
        );
        let mid = tilted.mid_price().unwrap();

        let base = MomentumSignals::new(20).alpha(&tilted, mid);
        let withlmp = MomentumSignals::new(20).with_learned_microprice(model);
        let lmp_alpha = withlmp.alpha(&tilted, mid);
        assert!(
            base > dec!(0),
            "baseline should be positive on bid-heavy book"
        );
        assert!(
            lmp_alpha < base,
            "LMP-attached alpha should be pulled below baseline by negative prediction: \
             base={base}, lmp={lmp_alpha}"
        );
    }

    // ---------------------------------------------------------
    // Epic D stage-2 — online lMP fit via on_l1_snapshot
    // ---------------------------------------------------------

    /// Feeding `on_l1_snapshot` with a steady stream of
    /// bid-heavy books AND rising mids should make the online
    /// fit attribute a positive `Δmid` to positive-imbalance
    /// buckets. Within refit_every counts the g-matrix
    /// shouldn't change; past the boundary it should.
    #[test]
    fn online_lmp_refits_g_matrix_after_horizon_and_refit_cadence() {
        use crate::learned_microprice::{LearnedMicroprice, LearnedMicropriceConfig};

        // Build + finalise a model with a neutral seed so the
        // initial g-matrix is non-zero; refit_every=5 so the
        // test runs quickly.
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 2,
            online_ring_capacity: 30,
            refit_every: 5,
        };
        let mut model = LearnedMicroprice::empty(cfg);
        for _ in 0..3 {
            model.accumulate(dec!(-0.75), dec!(0.01), dec!(-0.1));
            model.accumulate(dec!(0.75), dec!(0.01), dec!(0.1));
        }
        model.finalize_iterative(5);
        let initial_g = model.g_matrix().to_vec();

        // Horizon of 2 snapshots — very short so we emit
        // observations quickly.
        let mut m = MomentumSignals::new(10).with_learned_microprice_online(model, 2);

        // Push 20 bid-heavy snapshots with monotonically rising
        // mid. Horizon=2 → first two snapshots buffer, third
        // emits the first `update_online` call with a positive
        // Δmid on the +0.75 imbalance bucket. After 5 emits
        // the refit triggers. We need >= horizon + refit_every
        // + epsilon = 2 + 5 + buffer = ~10 ticks.
        for t in 0..20 {
            let mid = dec!(100) + Decimal::from(t);
            let bid = mid - dec!(0.005);
            let ask = mid + dec!(0.005);
            // Bid-heavy book.
            m.on_l1_snapshot(bid, dec!(10), ask, dec!(1));
        }

        // After 20 ticks the model should have fired at least
        // one refit. The g-matrix at imbalance +0.75 should now
        // skew STRONGER positive than the seed (more recent
        // data has larger Δmid = +1 per tick × horizon 2 = +2,
        // vs. seed +0.1).
        let new_g = m
            .learned_mp
            .as_ref()
            .map(|mp| mp.g_matrix().to_vec())
            .expect("model attached");
        assert_ne!(new_g, initial_g, "online fit should have mutated g-matrix");
    }

    #[test]
    fn online_lmp_builder_panics_on_zero_horizon() {
        use crate::learned_microprice::{LearnedMicroprice, LearnedMicropriceConfig};
        let model = LearnedMicroprice::empty(LearnedMicropriceConfig::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = MomentumSignals::new(10).with_learned_microprice_online(model, 0);
        }));
        assert!(result.is_err(), "horizon=0 must panic");
    }

    /// 22B-6 — basic snapshot_state returns None on empty tracker.
    #[test]
    fn empty_tracker_returns_none() {
        let m = MomentumSignals::new(10);
        assert!(m.snapshot_state().is_none());
    }

    fn mk_trade(qty: Decimal, taker_side: Side, id: u64) -> Trade {
        Trade {
            trade_id: id,
            symbol: "BTCUSDT".into(),
            price: dec!(100),
            qty,
            taker_side,
            timestamp: Utc::now(),
        }
    }

    /// 22B-6 — signed_volumes round-trips through snapshot/restore.
    #[test]
    fn signed_volumes_round_trip() {
        let mut src = MomentumSignals::new(10);
        // Drive a few trades to populate signed_volumes.
        src.on_trade(&mk_trade(dec!(1), Side::Buy, 1));
        src.on_trade(&mk_trade(dec!(2), Side::Sell, 2));
        let snap = src.snapshot_state().expect("has data");

        let mut dst = MomentumSignals::new(10);
        dst.restore_state(&snap).unwrap();
        assert_eq!(dst.signed_volumes.len(), 2);
        // on_trade signs notional (price * qty): +100 buy, -200 sell.
        assert_eq!(dst.signed_volumes[0], dec!(100));
        assert_eq!(dst.signed_volumes[1], dec!(-200));
    }

    /// 22B-6 — window cap truncates oversize signed_volumes
    /// buffer during restore.
    #[test]
    fn restore_truncates_oversize_window() {
        let mut src = MomentumSignals::new(100);
        for i in 0..80 {
            src.on_trade(&mk_trade(Decimal::from(i + 1), Side::Buy, i as u64));
        }
        let snap = src.snapshot_state().expect("has data");

        let mut dst = MomentumSignals::new(10); // smaller cap
        dst.restore_state(&snap).unwrap();
        assert_eq!(dst.signed_volumes.len(), 10);
    }

    /// 22W-6 — ema-weighted trade-flow imbalance uses
    /// `mm_indicators::ema_weights` to emphasise recent trades
    /// over the oldest ones. With 5 buy trades followed by 1
    /// sell, uniform-weighted is positive; EMA-weighted should
    /// tilt less positive because the most recent trade (sell)
    /// gets the biggest weight.
    #[test]
    fn ema_weighted_flow_emphasises_recent_trades() {
        let mut m = MomentumSignals::new(10);
        for i in 0..5 {
            m.on_trade(&mk_trade(dec!(1), Side::Buy, i));
        }
        m.on_trade(&mk_trade(dec!(1), Side::Sell, 5));

        let uniform = m.trade_flow_imbalance();
        let weighted = m.trade_flow_imbalance_ema_weighted(None);
        assert!(uniform > dec!(0), "uniform = {uniform}");
        assert!(
            weighted < uniform,
            "weighted ({weighted}) must be < uniform ({uniform}) when the most recent trade flipped"
        );
    }

    /// 22W-6 — fewer than 2 samples falls through to the uniform
    /// path so the ema_weights call never panics on `window < 2`.
    #[test]
    fn ema_weighted_flow_short_window_matches_uniform() {
        let mut m = MomentumSignals::new(10);
        m.on_trade(&mk_trade(dec!(1), Side::Buy, 1));
        let u = m.trade_flow_imbalance();
        let w = m.trade_flow_imbalance_ema_weighted(None);
        assert_eq!(u, w);
    }

    /// 22B-4 — learned_mp round-trips through snapshot/restore
    /// when both sides have the subsystem attached.
    #[test]
    fn learned_mp_round_trip() {
        use crate::learned_microprice::{LearnedMicroprice, LearnedMicropriceConfig};
        let cfg = LearnedMicropriceConfig::default();
        let src_model = LearnedMicroprice::empty(cfg.clone());
        let dst_model = LearnedMicroprice::empty(cfg);
        let src = MomentumSignals::new(10)
            .with_learned_microprice(src_model);
        let mut dst = MomentumSignals::new(10)
            .with_learned_microprice(dst_model);
        let snap = src.snapshot_state().expect("has data");
        dst.restore_state(&snap).unwrap();
        assert!(dst.learned_mp.is_some());
    }

    #[test]
    fn restore_rejects_wrong_schema() {
        let mut m = MomentumSignals::new(10);
        let bogus = serde_json::json!({
            "schema_version": 999,
            "window": 10,
            "signed_volumes": [],
            "ofi_ewma": null,
            "ofi_ewma_sq": null,
            "learned_mp": null,
            "online_mp_ring": null,
        });
        assert!(m.restore_state(&bogus).is_err());
    }
}
