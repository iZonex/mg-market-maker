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
                .and_then(|m| serde_json::to_value(m).ok()),
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
                let model: LearnedMicroprice = serde_json::from_value(model_json.clone())
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
mod tests;
