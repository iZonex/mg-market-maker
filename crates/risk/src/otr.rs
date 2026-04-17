//! Order-to-Trade Ratio (OTR).
//!
//! Regulatory surveillance metric tracked per symbol: the
//! number of order-book events (new / update / cancel) relative
//! to the number of actually executed public trades. A high OTR
//! can indicate layering, spoofing, or other
//! quote-stuffing-style strategies — regulators (MiCA, ESMA,
//! SEBI, MAS) monitor this as a market-quality proxy and market
//! makers are expected to keep their own OTR within venue
//! obligations.
//!
//! Ported from VisualHFT's `OrderToTradeRatioStudy.cs`
//! (Apache-2.0). We keep the formula verbatim:
//!
//! ```text
//! OTR = (adds + 2 × updates + cancels) / max(trades, 1) - 1
//! ```
//!
//! Updates are weighted 2× because an update is conceptually a
//! cancel + add. The `-1` at the end normalises so that a
//! perfectly "one order per trade" venue reads as `0`. The
//! `max(trades, 1)` denominator prevents divide-by-zero when no
//! trades have occurred yet.
//!
//! This counter is event-driven: the caller is expected to
//! increment on every order-book event the venue connector
//! produces and on every public trade. The counter does not
//! look at the book directly.

use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;

/// Simple OTR event counter. Not thread-safe — wrap in a
/// `Mutex` or run in a single owner task if shared.
#[derive(Debug, Clone, Default)]
pub struct OrderToTradeRatio {
    adds: u64,
    updates: u64,
    cancels: u64,
    trades: u64,
}

impl OrderToTradeRatio {
    /// Create a fresh counter with all fields zeroed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new-order event (price-level first appearance
    /// or L3 order add).
    pub fn on_add(&mut self) {
        self.adds += 1;
    }

    /// Record an order-update event (size change on an existing
    /// L3 order, or a price-level size delta on L2).
    pub fn on_update(&mut self) {
        self.updates += 1;
    }

    /// Record a cancel event (L3 order cancel or L2 price-level
    /// removal).
    pub fn on_cancel(&mut self) {
        self.cancels += 1;
    }

    /// Record a public trade.
    pub fn on_trade(&mut self) {
        self.trades += 1;
    }

    /// Current adds count.
    pub fn adds(&self) -> u64 {
        self.adds
    }
    /// Current updates count.
    pub fn updates(&self) -> u64 {
        self.updates
    }
    /// Current cancels count.
    pub fn cancels(&self) -> u64 {
        self.cancels
    }
    /// Current trades count.
    pub fn trades(&self) -> u64 {
        self.trades
    }

    /// Total weighted event count used in the OTR numerator:
    /// `adds + 2 × updates + cancels`.
    pub fn weighted_events(&self) -> u64 {
        self.adds + 2 * self.updates + self.cancels
    }

    /// Current OTR as a `Decimal`. Returns `0` if no events
    /// have been observed yet; the denominator is clamped at 1
    /// so the expression is always defined.
    pub fn ratio(&self) -> Decimal {
        let numerator = self.weighted_events();
        if numerator == 0 {
            return Decimal::ZERO;
        }
        let denom = self.trades.max(1);
        let raw = (numerator as f64) / (denom as f64);
        let normalised = raw - 1.0;
        Decimal::from_f64(normalised).unwrap_or(Decimal::ZERO)
    }

    /// Reset all counters to zero — called at the start of a
    /// new aggregation window (e.g. once per reporting tick for
    /// the audit log).
    pub fn reset(&mut self) {
        self.adds = 0;
        self.updates = 0;
        self.cancels = 0;
        self.trades = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// Fresh counter has zeroed fields and zero ratio.
    #[test]
    fn new_counter_is_zero() {
        let c = OrderToTradeRatio::new();
        assert_eq!(c.adds(), 0);
        assert_eq!(c.updates(), 0);
        assert_eq!(c.cancels(), 0);
        assert_eq!(c.trades(), 0);
        assert_eq!(c.ratio(), Decimal::ZERO);
    }

    /// Hand-computed canonical case: 5 adds + 3 updates + 2
    /// cancels = 13 weighted events; 4 trades → raw 13/4 = 3.25
    /// → normalised 2.25.
    #[test]
    fn canonical_hand_computed_case() {
        let mut c = OrderToTradeRatio::new();
        for _ in 0..5 {
            c.on_add();
        }
        for _ in 0..3 {
            c.on_update();
        }
        for _ in 0..2 {
            c.on_cancel();
        }
        for _ in 0..4 {
            c.on_trade();
        }
        assert_eq!(c.weighted_events(), 13);
        assert_eq!(c.ratio(), dec!(2.25));
    }

    /// Zero trades: the denominator is clamped at 1, so OTR
    /// equals `weighted - 1`.
    #[test]
    fn zero_trades_uses_denominator_floor_of_one() {
        let mut c = OrderToTradeRatio::new();
        c.on_add();
        c.on_add();
        c.on_add();
        // 3 adds / max(0, 1) - 1 = 2.
        assert_eq!(c.ratio(), dec!(2));
    }

    /// A venue where every trade is backed by exactly one add
    /// produces OTR = 0.
    #[test]
    fn perfect_one_to_one_matching_is_zero_otr() {
        let mut c = OrderToTradeRatio::new();
        for _ in 0..10 {
            c.on_add();
            c.on_trade();
        }
        assert_eq!(c.ratio(), Decimal::ZERO);
    }

    /// Updates count double in the numerator — 1 update + 1
    /// trade should produce ratio 1 (2 weighted - 1 trade - 1).
    #[test]
    fn updates_are_weighted_double() {
        let mut c = OrderToTradeRatio::new();
        c.on_update();
        c.on_trade();
        // (0 + 2·1 + 0) / max(1, 1) - 1 = 1
        assert_eq!(c.ratio(), dec!(1));
    }

    /// Reset clears all counters.
    #[test]
    fn reset_clears_all_counters() {
        let mut c = OrderToTradeRatio::new();
        c.on_add();
        c.on_update();
        c.on_cancel();
        c.on_trade();
        c.reset();
        assert_eq!(c.adds(), 0);
        assert_eq!(c.updates(), 0);
        assert_eq!(c.cancels(), 0);
        assert_eq!(c.trades(), 0);
        assert_eq!(c.ratio(), Decimal::ZERO);
    }

    // ── Property-based tests (Epic 11) ───────────────────────

    use proptest::prelude::*;

    proptest! {
        /// Counters always reflect exactly the number of `on_*`
        /// calls — no off-by-one in the increment path.
        #[test]
        fn counters_equal_call_counts(
            adds in 0u64..1000u64,
            updates in 0u64..1000u64,
            cancels in 0u64..1000u64,
            trades in 0u64..1000u64,
        ) {
            let mut c = OrderToTradeRatio::new();
            for _ in 0..adds { c.on_add(); }
            for _ in 0..updates { c.on_update(); }
            for _ in 0..cancels { c.on_cancel(); }
            for _ in 0..trades { c.on_trade(); }
            prop_assert_eq!(c.adds(), adds);
            prop_assert_eq!(c.updates(), updates);
            prop_assert_eq!(c.cancels(), cancels);
            prop_assert_eq!(c.trades(), trades);
        }

        /// weighted_events() = adds + 2·updates + cancels for
        /// any counter state.
        #[test]
        fn weighted_events_formula_holds(
            adds in 0u64..1000u64,
            updates in 0u64..1000u64,
            cancels in 0u64..1000u64,
        ) {
            let mut c = OrderToTradeRatio::new();
            for _ in 0..adds { c.on_add(); }
            for _ in 0..updates { c.on_update(); }
            for _ in 0..cancels { c.on_cancel(); }
            prop_assert_eq!(c.weighted_events(), adds + 2 * updates + cancels);
        }

        /// reset() zeroes the counter regardless of prior state.
        #[test]
        fn reset_is_idempotent_zeroing(
            adds in 0u64..1000u64,
            updates in 0u64..1000u64,
            cancels in 0u64..1000u64,
            trades in 0u64..1000u64,
        ) {
            let mut c = OrderToTradeRatio::new();
            for _ in 0..adds { c.on_add(); }
            for _ in 0..updates { c.on_update(); }
            for _ in 0..cancels { c.on_cancel(); }
            for _ in 0..trades { c.on_trade(); }
            c.reset();
            prop_assert_eq!(c.weighted_events(), 0);
            prop_assert_eq!(c.trades(), 0);
            prop_assert_eq!(c.ratio(), Decimal::ZERO);
        }

        /// When trades ≥ weighted_events, ratio ≤ 0 (MM is
        /// keeping the quote-stuffing in check — venue should
        /// like us). When trades = 0 and weighted > 0, ratio =
        /// weighted - 1 (denominator clamped at 1).
        #[test]
        fn zero_trades_edge_case_matches_formula(
            adds in 0u64..1000u64,
            updates in 0u64..1000u64,
            cancels in 0u64..1000u64,
        ) {
            let mut c = OrderToTradeRatio::new();
            for _ in 0..adds { c.on_add(); }
            for _ in 0..updates { c.on_update(); }
            for _ in 0..cancels { c.on_cancel(); }
            let weighted = c.weighted_events();
            if weighted == 0 {
                prop_assert_eq!(c.ratio(), Decimal::ZERO);
            } else {
                let expected = Decimal::from(weighted) - dec!(1);
                prop_assert_eq!(c.ratio(), expected);
            }
        }
    }
}
