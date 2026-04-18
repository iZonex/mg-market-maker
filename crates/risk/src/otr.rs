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

// ── MM-5: tiered + dual-timeline OTR ─────────────────────────

/// Which depth tier an event lives on. Determines which
/// sub-counter gets incremented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtrTier {
    /// Event sits on the top-of-book (best bid or best ask
    /// level). Tighter tier — venues typically watch TOB OTR
    /// separately because TOB churn is the biggest liquidity
    /// signal.
    Tob,
    /// Event sits within the top-20 levels per side, excluding
    /// TOB. Broader microstructure metric.
    Top20,
}

/// Which aggregation window to read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtrWindow {
    /// Lifetime counter since construction — the venue-facing
    /// number for compliance reports.
    Cumulative,
    /// Rolling 5-minute window — the ops dashboard number;
    /// spots regime changes that the lifetime number smooths
    /// over.
    Rolling5Min,
}

/// One-minute aggregation bucket inside a rolling window. One
/// bucket per minute keeps the VecDeque bounded at 5 entries
/// per (tier) while giving sub-second eviction resolution on
/// bucket boundaries.
#[derive(Debug, Clone, Default)]
struct OtrBucket {
    /// Bucket start time in ms since epoch.
    start_ms: i64,
    adds: u64,
    updates: u64,
    cancels: u64,
    trades: u64,
}

/// Rolling window of `OtrBucket`s. Keeps at most one minute's
/// worth of buckets per 60 s of retention; for the default 5
/// min window that's 5-6 entries in the deque.
#[derive(Debug, Clone)]
struct RollingOtr {
    buckets: std::collections::VecDeque<OtrBucket>,
    bucket_width_ms: i64,
    retention_ms: i64,
}

impl RollingOtr {
    fn new(bucket_width_ms: i64, retention_ms: i64) -> Self {
        Self {
            buckets: std::collections::VecDeque::new(),
            bucket_width_ms,
            retention_ms,
        }
    }

    fn prune(&mut self, now_ms: i64) {
        let cutoff = now_ms - self.retention_ms;
        while let Some(front) = self.buckets.front() {
            if front.start_ms + self.bucket_width_ms <= cutoff {
                self.buckets.pop_front();
            } else {
                break;
            }
        }
    }

    fn current_bucket(&mut self, now_ms: i64) -> &mut OtrBucket {
        self.prune(now_ms);
        let aligned = now_ms - now_ms.rem_euclid(self.bucket_width_ms);
        match self.buckets.back() {
            Some(b) if b.start_ms == aligned => {}
            _ => self.buckets.push_back(OtrBucket {
                start_ms: aligned,
                ..Default::default()
            }),
        }
        self.buckets.back_mut().expect("just pushed")
    }

    fn on_add(&mut self, now_ms: i64) {
        self.current_bucket(now_ms).adds += 1;
    }
    fn on_update(&mut self, now_ms: i64) {
        self.current_bucket(now_ms).updates += 1;
    }
    fn on_cancel(&mut self, now_ms: i64) {
        self.current_bucket(now_ms).cancels += 1;
    }
    fn on_trade(&mut self, now_ms: i64) {
        self.current_bucket(now_ms).trades += 1;
    }

    fn ratio(&mut self, now_ms: i64) -> Decimal {
        self.prune(now_ms);
        let (mut a, mut u, mut c, mut t) = (0u64, 0u64, 0u64, 0u64);
        for b in &self.buckets {
            a += b.adds;
            u += b.updates;
            c += b.cancels;
            t += b.trades;
        }
        let numerator = a + 2 * u + c;
        if numerator == 0 {
            return Decimal::ZERO;
        }
        let denom = t.max(1);
        let raw = (numerator as f64) / (denom as f64);
        Decimal::from_f64(raw - 1.0).unwrap_or(Decimal::ZERO)
    }
}

/// Tiered + dual-timeline OTR tracker. Carries four independent
/// counters internally — `{Tob,Top20} × {Cumulative,Rolling5Min}`
/// — so the dashboard can surface the four-way breakdown that
/// venue compliance teams ask for.
///
/// Callers stream events with a `(tier, now_ms)` pair; the
/// tracker fans out to the right sub-counters. Rolling windows
/// bucket by minute and retain 5 minutes by default; the
/// cumulative counters are the legacy [`OrderToTradeRatio`]
/// reused verbatim.
#[derive(Debug, Clone)]
pub struct TieredOtrTracker {
    tob_cum: OrderToTradeRatio,
    top20_cum: OrderToTradeRatio,
    tob_roll: RollingOtr,
    top20_roll: RollingOtr,
}

impl Default for TieredOtrTracker {
    fn default() -> Self {
        // 60s buckets × 5 retained = 5-minute rolling window.
        // Short enough to reflect intra-session regime changes,
        // long enough to smooth single-tick bursts.
        Self {
            tob_cum: OrderToTradeRatio::new(),
            top20_cum: OrderToTradeRatio::new(),
            tob_roll: RollingOtr::new(60_000, 5 * 60_000),
            top20_roll: RollingOtr::new(60_000, 5 * 60_000),
        }
    }
}

impl TieredOtrTracker {
    pub fn new() -> Self {
        Self::default()
    }

    fn cum_mut(&mut self, tier: OtrTier) -> &mut OrderToTradeRatio {
        match tier {
            OtrTier::Tob => &mut self.tob_cum,
            OtrTier::Top20 => &mut self.top20_cum,
        }
    }
    fn roll_mut(&mut self, tier: OtrTier) -> &mut RollingOtr {
        match tier {
            OtrTier::Tob => &mut self.tob_roll,
            OtrTier::Top20 => &mut self.top20_roll,
        }
    }

    pub fn on_add(&mut self, tier: OtrTier, now_ms: i64) {
        self.cum_mut(tier).on_add();
        self.roll_mut(tier).on_add(now_ms);
    }
    pub fn on_update(&mut self, tier: OtrTier, now_ms: i64) {
        self.cum_mut(tier).on_update();
        self.roll_mut(tier).on_update(now_ms);
    }
    pub fn on_cancel(&mut self, tier: OtrTier, now_ms: i64) {
        self.cum_mut(tier).on_cancel();
        self.roll_mut(tier).on_cancel(now_ms);
    }
    pub fn on_trade(&mut self, tier: OtrTier, now_ms: i64) {
        self.cum_mut(tier).on_trade();
        self.roll_mut(tier).on_trade(now_ms);
    }

    /// Read the ratio for the requested `(tier, window)` pair.
    /// `now_ms` is only used by the rolling window to advance
    /// its eviction cursor; ignored by `Cumulative`.
    pub fn ratio(&mut self, tier: OtrTier, window: OtrWindow, now_ms: i64) -> Decimal {
        match window {
            OtrWindow::Cumulative => self.cum_mut(tier).ratio(),
            OtrWindow::Rolling5Min => self.roll_mut(tier).ratio(now_ms),
        }
    }

    /// Snapshot of all four ratios — useful for a single Prometheus
    /// / audit push that doesn't want four calls.
    pub fn snapshot(&mut self, now_ms: i64) -> TieredOtrSnapshot {
        TieredOtrSnapshot {
            tob_cumulative: self.ratio(OtrTier::Tob, OtrWindow::Cumulative, now_ms),
            tob_rolling_5min: self.ratio(OtrTier::Tob, OtrWindow::Rolling5Min, now_ms),
            top20_cumulative: self.ratio(OtrTier::Top20, OtrWindow::Cumulative, now_ms),
            top20_rolling_5min: self.ratio(OtrTier::Top20, OtrWindow::Rolling5Min, now_ms),
        }
    }
}

/// Four-way OTR snapshot produced by [`TieredOtrTracker::snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TieredOtrSnapshot {
    pub tob_cumulative: Decimal,
    pub tob_rolling_5min: Decimal,
    pub top20_cumulative: Decimal,
    pub top20_rolling_5min: Decimal,
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

    // ── MM-5 tiered + dual-timeline tests ───────────────────

    /// Tiered tracker keeps TOB and Top20 counters independent —
    /// events on one tier don't leak into the other's ratio.
    #[test]
    fn tiered_tob_and_top20_are_independent() {
        let mut t = TieredOtrTracker::new();
        for _ in 0..5 {
            t.on_add(OtrTier::Tob, 0);
        }
        t.on_trade(OtrTier::Tob, 0);
        // Top20 sees nothing.
        assert_eq!(t.ratio(OtrTier::Top20, OtrWindow::Cumulative, 0), dec!(0));
        // TOB: (5 + 0 + 0) / max(1, 1) - 1 = 4.
        assert_eq!(t.ratio(OtrTier::Tob, OtrWindow::Cumulative, 0), dec!(4));
    }

    /// Rolling window evicts events older than 5 minutes while
    /// the cumulative counter keeps them.
    #[test]
    fn rolling_evicts_old_events_cumulative_does_not() {
        let mut t = TieredOtrTracker::new();
        // Inject 3 adds + 1 trade at t=0.
        for _ in 0..3 {
            t.on_add(OtrTier::Tob, 0);
        }
        t.on_trade(OtrTier::Tob, 0);
        // Immediately: both window and cumulative read ratio 2.
        assert_eq!(t.ratio(OtrTier::Tob, OtrWindow::Rolling5Min, 0), dec!(2));
        assert_eq!(t.ratio(OtrTier::Tob, OtrWindow::Cumulative, 0), dec!(2));
        // Advance to 10 minutes later — rolling window prunes.
        let future = 10 * 60_000;
        assert_eq!(
            t.ratio(OtrTier::Tob, OtrWindow::Rolling5Min, future),
            dec!(0),
            "5-min window should have evicted the t=0 bucket"
        );
        // Cumulative still remembers.
        assert_eq!(t.ratio(OtrTier::Tob, OtrWindow::Cumulative, future), dec!(2));
    }

    /// Events landing in different minute buckets aggregate in
    /// the rolling window.
    #[test]
    fn rolling_aggregates_across_recent_buckets() {
        let mut t = TieredOtrTracker::new();
        // 2 adds at t=0 (minute 0).
        t.on_add(OtrTier::Top20, 0);
        t.on_add(OtrTier::Top20, 0);
        // 1 add + 1 trade at t=60_001 (minute 1).
        t.on_add(OtrTier::Top20, 60_001);
        t.on_trade(OtrTier::Top20, 60_001);
        // Read at t=120_000 (still within 5 min).
        // Weighted = 3 adds → 3; trades = 1 → ratio = 3/1 - 1 = 2.
        assert_eq!(
            t.ratio(OtrTier::Top20, OtrWindow::Rolling5Min, 120_000),
            dec!(2)
        );
    }

    /// Snapshot returns all four ratios coherently.
    #[test]
    fn snapshot_returns_all_four_ratios() {
        let mut t = TieredOtrTracker::new();
        t.on_add(OtrTier::Tob, 0);
        t.on_trade(OtrTier::Tob, 0);
        t.on_update(OtrTier::Top20, 0);
        t.on_trade(OtrTier::Top20, 0);
        let snap = t.snapshot(0);
        // TOB: 1 add / 1 trade - 1 = 0.
        assert_eq!(snap.tob_cumulative, dec!(0));
        assert_eq!(snap.tob_rolling_5min, dec!(0));
        // Top20: 2·1 update / 1 trade - 1 = 1.
        assert_eq!(snap.top20_cumulative, dec!(1));
        assert_eq!(snap.top20_rolling_5min, dec!(1));
    }
}
