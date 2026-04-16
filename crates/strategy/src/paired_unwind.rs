//! Paired unwind executor — flattens both legs of a basis /
//! funding-arb position in matched slices.
//!
//! Sprint J of the spot-and-cross-product epic. AD-11: when the
//! kill switch escalates to L4 `FlattenAll` on an instrument
//! pair, the single-leg `TwapExecutor` is the wrong tool — it
//! only knows about one symbol, so flattening the primary leg
//! leaves the hedge leg open and breaks delta-neutrality exactly
//! when the operator most wants it preserved. `PairedUnwindExecutor`
//! is the paired counterpart: one instance tracks both legs and
//! emits one quote per leg per slice, sized so the slice-by-slice
//! net delta stays bounded.
//!
//! # Slice model
//!
//! ```text
//! target_pair = (primary_qty, hedge_qty)
//! slice_pair  = (primary_qty / N, hedge_qty / N)    for N slices
//! ```
//!
//! where `hedge_qty = primary_qty * multiplier` (taken from
//! `InstrumentPair.multiplier`). Each slice reverses both sides
//! of the open position — if the operator was long spot + short
//! perp, the unwind executor emits a sell-spot quote + a
//! buy-perp quote per tick.
//!
//! # Delta preservation
//!
//! After slice `k` (0-indexed, `k = 0..N`), the open net delta
//! is `(N - k) / N * initial_delta`. Because the two legs are
//! sliced in the same fraction, whatever delta imbalance the
//! executor started with (ideally zero for a clean basis trade,
//! but a pair-break may have left a tilt) stays proportional
//! across the unwind — the executor does not try to
//! "opportunistically rebalance" in the middle. If a slice fills
//! on one leg and fails on the other, the next slice's `on_fill`
//! calls simply keep the two `executed_qty` counters honest and
//! the next slice resumes.
//!
//! # Scope of Sprint J
//!
//! - The executor is a pure data structure that emits two
//!   `Quote` values per tick. It does **not** own connectors or
//!   dispatch itself — the engine's tick loop reads the pair and
//!   places them via `OrderManager` just like any other quote.
//! - No compensating logic on unilateral slice failures: the
//!   executor accepts asymmetric progress on the two legs and
//!   lets the next tick catch up. A hard L5 `Disconnect` is the
//!   operator's escalation path if the situation worsens.
//! - No price ladder: every slice quotes aggressively near the
//!   leg's mid (configurable bps aggressiveness), one level per
//!   side. Mid-heavy urgency pricing is a follow-up.

use mm_common::types::{InstrumentPair, Price, PriceLevel, Qty, Quote, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info};

use crate::features::market_impact;

/// Result of one `next_slice` tick — zero or one quote per leg.
/// Both fields are `None` when the executor is idle, complete, or
/// between scheduled slice times.
#[derive(Debug, Clone)]
pub struct SlicePair {
    pub primary: Option<Quote>,
    pub hedge: Option<Quote>,
}

impl SlicePair {
    pub const EMPTY: Self = Self {
        primary: None,
        hedge: None,
    };
    pub fn is_empty(&self) -> bool {
        self.primary.is_none() && self.hedge.is_none()
    }
}

/// Paired unwind executor. One instance per open pair position.
pub struct PairedUnwindExecutor {
    pair: InstrumentPair,
    /// Direction of the current position on each leg. The
    /// executor reverses these — if `primary_side = Buy`
    /// (long spot), the unwind emits `Sell` slices on the
    /// primary leg.
    primary_side: Side,
    hedge_side: Side,
    target_primary: Qty,
    target_hedge: Qty,
    slice_primary: Qty,
    slice_hedge: Qty,
    executed_primary: Qty,
    executed_hedge: Qty,
    total_slices: u32,
    current_slice: u32,
    started_at: chrono::DateTime<chrono::Utc>,
    duration_secs: u64,
    aggressiveness_bps: Decimal,
    active: bool,
}

impl PairedUnwindExecutor {
    /// Construct an executor that reverses a `(primary_side,
    /// hedge_side)` position of size `primary_qty` on the
    /// primary leg. The hedge leg qty is derived from
    /// `pair.multiplier`.
    ///
    /// `duration_secs` is the total unwind window;
    /// `num_slices` = how many equal chunks; `aggressiveness_bps`
    /// = how far from each leg's mid to place (0 = at mid,
    /// higher = more passive).
    pub fn new(
        pair: InstrumentPair,
        primary_side: Side,
        hedge_side: Side,
        primary_qty: Qty,
        duration_secs: u64,
        num_slices: u32,
        aggressiveness_bps: Decimal,
    ) -> Self {
        let hedge_qty = primary_qty * pair.multiplier;
        let num_dec = Decimal::from(num_slices.max(1));
        let slice_primary = primary_qty / num_dec;
        let slice_hedge = hedge_qty / num_dec;

        info!(
            primary_symbol = %pair.primary_symbol,
            hedge_symbol = %pair.hedge_symbol,
            ?primary_side,
            ?hedge_side,
            %primary_qty,
            %hedge_qty,
            num_slices,
            "paired unwind executor created"
        );

        Self {
            pair,
            primary_side,
            hedge_side,
            target_primary: primary_qty,
            target_hedge: hedge_qty,
            slice_primary,
            slice_hedge,
            executed_primary: dec!(0),
            executed_hedge: dec!(0),
            total_slices: num_slices,
            current_slice: 0,
            started_at: chrono::Utc::now(),
            duration_secs,
            aggressiveness_bps,
            active: true,
        }
    }

    pub fn pair(&self) -> &InstrumentPair {
        &self.pair
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// Total progress as a fraction `[0, 1]`. Equal-weight
    /// average of the two legs — both legs must finish for
    /// `progress() == 1`.
    pub fn progress(&self) -> Decimal {
        let p = if self.target_primary.is_zero() {
            dec!(1)
        } else {
            (self.executed_primary / self.target_primary).min(dec!(1))
        };
        let h = if self.target_hedge.is_zero() {
            dec!(1)
        } else {
            (self.executed_hedge / self.target_hedge).min(dec!(1))
        };
        (p + h) / dec!(2)
    }

    /// `true` when **both** legs have hit their target.
    pub fn is_complete(&self) -> bool {
        self.executed_primary >= self.target_primary && self.executed_hedge >= self.target_hedge
    }

    /// Residual delta still open, expressed in primary base
    /// units. Positive = unwind short the primary leg, negative
    /// = unwind long. A clean-start delta-neutral pair unwinds
    /// to zero; a tilted-start pair unwinds toward whatever
    /// tilt the caller started with.
    pub fn residual_delta(&self) -> Decimal {
        let primary_remaining = self.target_primary - self.executed_primary;
        let hedge_remaining = self.target_hedge - self.executed_hedge;
        // Convert the hedge remainder back into primary units
        // via the multiplier, then take the signed difference.
        // A long-spot-short-perp pair starts net flat in primary
        // units, so a fully-remaining pair returns residual 0.
        let hedge_in_primary = if self.pair.multiplier.is_zero() {
            hedge_remaining
        } else {
            hedge_remaining / self.pair.multiplier
        };
        primary_remaining - hedge_in_primary
    }

    /// Taker cost (in bps of `reference_price`) of
    /// liquidating the **remaining** primary-leg qty right now
    /// against the supplied book levels. Positive = the
    /// taker walk is unfavourable (pays above / receives below
    /// the reference).
    ///
    /// Used by higher-level callers to decide whether the
    /// executor should escalate from its current post-only
    /// slice pattern to an aggressive taker sweep — if the
    /// residual taker cost is small enough the extra aggression
    /// is cheap; if it is large the operator may prefer to keep
    /// slicing passively and hope the book deepens.
    ///
    /// Returns `None` when the executor has no remaining
    /// primary qty to unwind, when the book side is empty, or
    /// when the book cannot fully absorb the remainder (partial
    /// fill — the caller should treat that as "book too thin to
    /// taker-out").
    pub fn primary_residual_impact_bps(
        &self,
        primary_bids: &[PriceLevel],
        primary_asks: &[PriceLevel],
        reference_price: Price,
    ) -> Option<Decimal> {
        let remaining = self.target_primary - self.executed_primary;
        if remaining <= Decimal::ZERO {
            return None;
        }
        // The UNWIND side is opposite the ORIGINAL position
        // direction: a long-spot position unwinds by SELLING
        // the primary leg (walking the bids), etc.
        let unwind_side = self.primary_side.opposite();
        let levels = match unwind_side {
            Side::Sell => primary_bids,
            Side::Buy => primary_asks,
        };
        let impact = market_impact(levels, unwind_side, remaining, reference_price)?;
        if impact.partial {
            return None;
        }
        Some(impact.impact_bps)
    }

    /// Same as [`primary_residual_impact_bps`] for the hedge
    /// leg. `reference_price` is typically the hedge-leg mid at
    /// the moment of the check.
    pub fn hedge_residual_impact_bps(
        &self,
        hedge_bids: &[PriceLevel],
        hedge_asks: &[PriceLevel],
        reference_price: Price,
    ) -> Option<Decimal> {
        let remaining = self.target_hedge - self.executed_hedge;
        if remaining <= Decimal::ZERO {
            return None;
        }
        let unwind_side = self.hedge_side.opposite();
        let levels = match unwind_side {
            Side::Sell => hedge_bids,
            Side::Buy => hedge_asks,
        };
        let impact = market_impact(levels, unwind_side, remaining, reference_price)?;
        if impact.partial {
            return None;
        }
        Some(impact.impact_bps)
    }

    /// Emit the next slice pair. Returns `SlicePair::EMPTY` when
    /// the executor is idle (not yet scheduled), inactive,
    /// complete, or the caller did not supply mids for both
    /// legs. `primary_mid` and `hedge_mid` are the current best
    /// estimates of each leg's price — the executor uses them
    /// only to compute aggressive offsets, not to derive qty.
    pub fn next_slice(&mut self, primary_mid: Price, hedge_mid: Price) -> SlicePair {
        if !self.active || self.is_complete() {
            return SlicePair::EMPTY;
        }

        // Schedule: slice index is elapsed / (duration / N).
        let elapsed = (chrono::Utc::now() - self.started_at).num_seconds() as u64;
        let expected_slice = (elapsed * self.total_slices as u64)
            .checked_div(self.duration_secs)
            .unwrap_or(self.total_slices as u64) as u32;

        if self.current_slice >= expected_slice {
            return SlicePair::EMPTY;
        }
        self.current_slice = expected_slice;

        let unwind_primary = self.primary_side.opposite();
        let unwind_hedge = self.hedge_side.opposite();

        let primary_remaining = self.target_primary - self.executed_primary;
        let hedge_remaining = self.target_hedge - self.executed_hedge;

        let primary_qty = self.slice_primary.min(primary_remaining);
        let hedge_qty = self.slice_hedge.min(hedge_remaining);

        if primary_qty <= dec!(0) && hedge_qty <= dec!(0) {
            self.active = false;
            return SlicePair::EMPTY;
        }

        let primary_quote = if primary_qty > dec!(0) {
            Some(Quote {
                side: unwind_primary,
                price: Self::aggressive_price(unwind_primary, primary_mid, self.aggressiveness_bps),
                qty: primary_qty,
            })
        } else {
            None
        };

        let hedge_quote = if hedge_qty > dec!(0) {
            Some(Quote {
                side: unwind_hedge,
                price: Self::aggressive_price(unwind_hedge, hedge_mid, self.aggressiveness_bps),
                qty: hedge_qty,
            })
        } else {
            None
        };

        debug!(
            slice = self.current_slice,
            of = self.total_slices,
            primary_qty = %primary_qty,
            hedge_qty = %hedge_qty,
            "paired unwind slice"
        );

        SlicePair {
            primary: primary_quote,
            hedge: hedge_quote,
        }
    }

    /// Record a fill on the primary leg.
    pub fn on_primary_fill(&mut self, qty: Qty) {
        self.executed_primary += qty;
        if self.is_complete() {
            self.active = false;
            info!(
                primary = %self.executed_primary,
                hedge = %self.executed_hedge,
                "paired unwind complete"
            );
        }
    }

    /// Record a fill on the hedge leg.
    pub fn on_hedge_fill(&mut self, qty: Qty) {
        self.executed_hedge += qty;
        if self.is_complete() {
            self.active = false;
            info!(
                primary = %self.executed_primary,
                hedge = %self.executed_hedge,
                "paired unwind complete"
            );
        }
    }

    pub fn cancel(&mut self) {
        info!(
            primary_executed = %self.executed_primary,
            hedge_executed = %self.executed_hedge,
            primary_target = %self.target_primary,
            hedge_target = %self.target_hedge,
            "paired unwind cancelled"
        );
        self.active = false;
    }

    fn aggressive_price(side: Side, mid: Price, bps: Decimal) -> Price {
        let offset = mid * bps / dec!(10_000);
        match side {
            Side::Buy => mid + offset,  // Pay slightly above mid.
            Side::Sell => mid - offset, // Sell slightly below mid.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair_1to1() -> InstrumentPair {
        InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTCUSDT-PERP".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(50),
        }
    }

    fn pair_with_multiplier(m: Decimal) -> InstrumentPair {
        InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTC-CONTRACT".to_string(),
            multiplier: m,
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(50),
        }
    }

    fn mk(
        pair: InstrumentPair,
        primary_side: Side,
        hedge_side: Side,
        qty: Decimal,
        num_slices: u32,
    ) -> PairedUnwindExecutor {
        PairedUnwindExecutor::new(pair, primary_side, hedge_side, qty, 10, num_slices, dec!(5))
    }

    fn advance_started_by(exec: &mut PairedUnwindExecutor, secs: i64) {
        exec.started_at = chrono::Utc::now() - chrono::Duration::seconds(secs);
    }

    #[test]
    fn emits_opposite_sides_on_each_leg() {
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        advance_started_by(&mut exec, 3);

        let slice = exec.next_slice(dec!(50_000), dec!(50_010));
        let p = slice.primary.expect("primary slice");
        let h = slice.hedge.expect("hedge slice");
        assert_eq!(p.side, Side::Sell, "unwind a long primary → sell");
        assert_eq!(h.side, Side::Buy, "unwind a short hedge → buy");
    }

    #[test]
    fn slice_qty_matches_target_divided_by_slices() {
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        advance_started_by(&mut exec, 3);
        let slice = exec.next_slice(dec!(50_000), dec!(50_010));
        assert_eq!(slice.primary.unwrap().qty, dec!(0.02));
        assert_eq!(slice.hedge.unwrap().qty, dec!(0.02));
    }

    #[test]
    fn multiplier_scales_hedge_slice_qty() {
        // 1 spot unit maps to 10 contract-sized hedge units.
        let mut exec = mk(
            pair_with_multiplier(dec!(10)),
            Side::Buy,
            Side::Sell,
            dec!(0.1),
            5,
        );
        advance_started_by(&mut exec, 3);
        let slice = exec.next_slice(dec!(50_000), dec!(50_010));
        assert_eq!(slice.primary.unwrap().qty, dec!(0.02), "primary unchanged");
        assert_eq!(slice.hedge.unwrap().qty, dec!(0.2), "hedge × multiplier");
    }

    #[test]
    fn both_legs_fully_executed_completes_and_flattens_residual() {
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        for _ in 0..5 {
            exec.on_primary_fill(dec!(0.02));
            exec.on_hedge_fill(dec!(0.02));
        }
        assert!(exec.is_complete());
        assert!(!exec.active);
        // Residual delta is 0 after both legs flatten.
        assert_eq!(exec.residual_delta(), dec!(0));
    }

    #[test]
    fn partial_progress_on_one_leg_leaves_residual_tilt() {
        // Caller filled primary more aggressively than hedge.
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        exec.on_primary_fill(dec!(0.06));
        exec.on_hedge_fill(dec!(0.02));
        // Remaining: primary 0.04, hedge 0.08 → in primary units
        // delta = 0.04 - 0.08 = -0.04.
        assert_eq!(exec.residual_delta(), dec!(-0.04));
        assert!(!exec.is_complete());
    }

    #[test]
    fn next_slice_emits_nothing_before_schedule() {
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        // Fresh executor — started_at is "now", elapsed = 0,
        // expected_slice = 0, current_slice = 0 → no slice.
        let slice = exec.next_slice(dec!(50_000), dec!(50_010));
        assert!(slice.is_empty());
    }

    #[test]
    fn aggressive_price_respects_side() {
        // Sell slice below mid, buy slice above mid.
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        advance_started_by(&mut exec, 3);
        let slice = exec.next_slice(dec!(50_000), dec!(50_010));
        let sell = slice.primary.unwrap();
        let buy = slice.hedge.unwrap();
        // Aggressiveness = 5 bps.
        // Sell at 50_000 - 5 bps → 50_000 - 25 = 49_975.
        assert_eq!(sell.price, dec!(49_975));
        // Buy at 50_010 + 5 bps → 50_010 + 25.005 = 50_035.005.
        assert_eq!(buy.price, dec!(50_035.005));
    }

    #[test]
    fn progress_averages_both_legs() {
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        exec.on_primary_fill(dec!(0.05)); // 50 %
        exec.on_hedge_fill(dec!(0.025)); // 25 %
        assert_eq!(exec.progress(), dec!(0.375));
    }

    #[test]
    fn unwinding_short_primary_long_hedge_flips_sides() {
        // Short-spot long-perp starting position.
        let mut exec = mk(pair_1to1(), Side::Sell, Side::Buy, dec!(0.1), 5);
        advance_started_by(&mut exec, 3);
        let slice = exec.next_slice(dec!(50_000), dec!(50_010));
        assert_eq!(slice.primary.unwrap().side, Side::Buy);
        assert_eq!(slice.hedge.unwrap().side, Side::Sell);
    }

    #[test]
    fn cancel_stops_emission() {
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        advance_started_by(&mut exec, 3);
        exec.cancel();
        let slice = exec.next_slice(dec!(50_000), dec!(50_010));
        assert!(slice.is_empty());
    }

    // ---- residual impact helpers ----

    fn lvl(price: Decimal, qty: Decimal) -> PriceLevel {
        PriceLevel { price, qty }
    }

    #[test]
    fn primary_residual_impact_reports_none_when_fully_filled() {
        let mut exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        exec.on_primary_fill(dec!(0.1));
        let bids = vec![lvl(dec!(100), dec!(10))];
        let asks = vec![lvl(dec!(101), dec!(10))];
        assert!(exec
            .primary_residual_impact_bps(&bids, &asks, dec!(100.5))
            .is_none());
    }

    #[test]
    fn primary_residual_impact_returns_positive_bps_for_long_unwind_at_lower_price() {
        // Long-primary position unwinding — we SELL the
        // remaining 0.1 BTC walking the bid side. Best bid is
        // below reference → impact is positive.
        let exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        let bids = vec![lvl(dec!(99), dec!(1))];
        let asks = vec![lvl(dec!(101), dec!(1))];
        let bps = exec
            .primary_residual_impact_bps(&bids, &asks, dec!(100))
            .unwrap();
        // (100 - 99) / 100 * 10000 = 100 bps.
        assert!(bps > dec!(99) && bps < dec!(101));
    }

    #[test]
    fn primary_residual_impact_none_when_book_is_thinner_than_remaining() {
        let exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        // Only 0.05 BTC on the bid side vs 0.1 BTC remaining.
        let bids = vec![lvl(dec!(99), dec!(0.05))];
        let asks = vec![lvl(dec!(101), dec!(10))];
        assert!(exec
            .primary_residual_impact_bps(&bids, &asks, dec!(100))
            .is_none());
    }

    #[test]
    fn hedge_residual_impact_walks_the_right_side_for_a_short_hedge_unwind() {
        // Short-hedge position unwinds by BUYING the hedge leg
        // → walks the hedge asks.
        let exec = mk(pair_1to1(), Side::Buy, Side::Sell, dec!(0.1), 5);
        let hedge_bids = vec![lvl(dec!(99), dec!(10))];
        let hedge_asks = vec![lvl(dec!(101), dec!(10))];
        let bps = exec
            .hedge_residual_impact_bps(&hedge_bids, &hedge_asks, dec!(100))
            .unwrap();
        // Walking asks at 101 vs reference 100 → 100 bps taker cost.
        assert!(bps > dec!(99) && bps < dec!(101));
    }

    #[test]
    fn hedge_residual_impact_scales_with_multiplier() {
        // Pair multiplier 5 → hedge qty is 5x primary qty.
        // Book must be 5x deeper for the full remainder to fit.
        let exec = mk(
            pair_with_multiplier(dec!(5)),
            Side::Buy,
            Side::Sell,
            dec!(0.1),
            5,
        );
        let hedge_bids = vec![lvl(dec!(99), dec!(10))];
        let hedge_asks = vec![lvl(dec!(101), dec!(0.3))]; // only 0.3, need 0.5
        assert!(exec
            .hedge_residual_impact_bps(&hedge_bids, &hedge_asks, dec!(100))
            .is_none());

        let hedge_asks = vec![lvl(dec!(101), dec!(1))]; // 1 > 0.5, fits
        let bps = exec
            .hedge_residual_impact_bps(&hedge_bids, &hedge_asks, dec!(100))
            .unwrap();
        assert!(bps > dec!(99) && bps < dec!(101));
    }
}
