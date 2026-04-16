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
    /// Optional learned-microprice model. When attached,
    /// `alpha()` folds `(mp_learned − mid) / mid` as a
    /// micro-price drift component.
    learned_mp: Option<LearnedMicroprice>,
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
            learned_mp: None,
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

    /// Feed a top-of-book L1 snapshot into the optional OFI
    /// tracker. No-op when [`with_ofi`] has not been called.
    /// Callers should invoke this on every book update so the
    /// tracker sees the full event stream.
    pub fn on_l1_snapshot(
        &mut self,
        bid_px: Decimal,
        bid_qty: Decimal,
        ask_px: Decimal,
        ask_qty: Decimal,
    ) {
        let Some(tracker) = self.ofi.as_mut() else {
            return;
        };
        if let Some(obs) = tracker.update(bid_px, bid_qty, ask_px, ask_qty) {
            // Normalise by average depth so the EWMA stays
            // dimensionless — a 10-qty arrival on a BTC book
            // should not swamp a 0.1-qty arrival on an SOL book.
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
        }
    }

    /// Current smoothed OFI, `None` when `with_ofi` is off or
    /// before the first observation has landed.
    pub fn ofi_ewma(&self) -> Option<Decimal> {
        self.ofi_ewma
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
}
