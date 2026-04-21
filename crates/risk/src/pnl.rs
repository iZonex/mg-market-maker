use chrono::{DateTime, Duration, Utc};
use mm_common::types::{Fill, Price, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::info;

/// PnL Attribution — breaks down profit/loss by source.
///
/// A professional MM needs to know WHERE money is made/lost:
/// - Spread capture: the core MM revenue
/// - Inventory PnL: mark-to-market on held position
/// - Rebate income: exchange fee rebates for maker orders
/// - Adverse selection: cost of being filled by informed traders
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PnlAttribution {
    /// Revenue from capturing bid-ask spread.
    pub spread_pnl: Decimal,
    /// PnL from inventory mark-to-market changes.
    pub inventory_pnl: Decimal,
    /// Income from maker fee rebates.
    pub rebate_income: Decimal,
    /// Fees paid (when we're taker).
    pub fees_paid: Decimal,
    /// Amortized loan cost (Epic 2). Subtracted from total PnL.
    pub loan_cost_amortized: Decimal,
    /// Funding P&L booked at venue settlement instants (Epic
    /// 40.3). Sign convention: positive = we received funding,
    /// negative = we paid. Included in `total_pnl()`.
    #[serde(default)]
    pub funding_pnl_realised: Decimal,
    /// Between-settlement MTM estimate of the current funding
    /// period (Epic 40.3). Recomputed from scratch every tick
    /// so there is nothing to drift; informational only —
    /// **not** part of `total_pnl()` because it will flip into
    /// `funding_pnl_realised` at the next settle and would
    /// double-count otherwise.
    #[serde(default)]
    pub funding_pnl_mtm: Decimal,
    /// Number of round-trips completed.
    pub round_trips: u64,
    /// PNL-COUNTER-1 (2026-04-21) — raw fill count (increments
    /// on every `record_fill`, regardless of round-trip state).
    /// `round_trips` counts buy↔sell cycles where inventory
    /// returns to zero — useful for PnL attribution accuracy
    /// but not what a tenant means by "how many trades have I
    /// done". The tenant portal surfaces `fill_count`.
    #[serde(default)]
    pub fill_count: u64,
    /// Total volume traded (both sides).
    pub total_volume: Decimal,
}

impl PnlAttribution {
    pub fn total_pnl(&self) -> Decimal {
        self.spread_pnl + self.inventory_pnl + self.rebate_income + self.funding_pnl_realised
            - self.fees_paid
            - self.loan_cost_amortized
    }

    /// PnL per unit of volume traded (efficiency metric).
    pub fn pnl_per_volume(&self) -> Decimal {
        if self.total_volume.is_zero() {
            return dec!(0);
        }
        self.total_pnl() / self.total_volume
    }
}

/// Tracks PnL attribution in real-time.
pub struct PnlTracker {
    pub attribution: PnlAttribution,
    /// Last mid price for inventory mark-to-market.
    last_mid: Decimal,
    /// Current net inventory in base asset.
    inventory: Decimal,
    /// Maker fee rate (negative = rebate).
    maker_fee: Decimal,
    /// Taker fee rate.
    taker_fee: Decimal,
    /// Daily loan cost for amortization (Epic 2).
    loan_daily_cost: Decimal,
    /// Most-recent funding rate reported by the venue. `None`
    /// on spot engines and on perp engines before the first
    /// `on_funding_update` call. Sign follows venue convention
    /// (positive → longs pay shorts).
    funding_rate: Option<Decimal>,
    /// Venue's reported next-funding-time UTC instant.
    next_funding_time: Option<DateTime<Utc>>,
    /// Current funding-period start — set to the previous
    /// settlement instant. Needed so a mid-period operator
    /// restart doesn't integrate MTM from the start of the
    /// interval and over-report.
    period_start: Option<DateTime<Utc>>,
    /// Funding cadence from the venue (Binance 8h, HL 1h,
    /// Bybit mostly 8h, some 4h). Honored verbatim — the
    /// engine never hardcodes a schedule; accrual math only
    /// uses wall-clock fractions.
    funding_interval: Option<Duration>,
}

impl PnlTracker {
    pub fn new(maker_fee: Decimal, taker_fee: Decimal) -> Self {
        Self {
            attribution: PnlAttribution::default(),
            last_mid: dec!(0),
            inventory: dec!(0),
            maker_fee,
            taker_fee,
            loan_daily_cost: dec!(0),
            funding_rate: None,
            next_funding_time: None,
            period_start: None,
            funding_interval: None,
        }
    }

    /// Hot-swap the fee schedule. Called by the engine's
    /// fee-tier refresh task whenever a venue reports a new
    /// effective rate (e.g. a month-end VIP tier crossing).
    /// Subsequent `on_fill` calls attribute fees against the new
    /// rates; previously accrued `fees_paid` and `rebate_income`
    /// are not retroactively rewritten — that would conflict
    /// with the audit trail.
    pub fn set_fee_rates(&mut self, maker_fee: Decimal, taker_fee: Decimal) {
        self.maker_fee = maker_fee;
        self.taker_fee = taker_fee;
    }

    /// Read the maker fee currently applied to new fills. Used by
    /// the dashboard / Prometheus exporter to expose the
    /// effective rate as a gauge.
    pub fn maker_fee(&self) -> Decimal {
        self.maker_fee
    }

    /// Read the taker fee currently applied to new fills.
    pub fn taker_fee(&self) -> Decimal {
        self.taker_fee
    }

    /// Record a fill and attribute PnL.
    pub fn on_fill(&mut self, fill: &Fill, current_mid: Price) {
        let fill_value = fill.price * fill.qty;

        // Spread capture: difference between our fill price and mid.
        let spread_capture = match fill.side {
            Side::Buy => (current_mid - fill.price) * fill.qty, // Bought below mid.
            Side::Sell => (fill.price - current_mid) * fill.qty, // Sold above mid.
        };
        self.attribution.spread_pnl += spread_capture;

        // Fee attribution.
        if fill.is_maker {
            let fee = fill_value * self.maker_fee;
            if fee < dec!(0) {
                // Negative fee = rebate.
                self.attribution.rebate_income += fee.abs();
            } else {
                self.attribution.fees_paid += fee;
            }
        } else {
            self.attribution.fees_paid += fill_value * self.taker_fee;
        }

        // Update inventory.
        match fill.side {
            Side::Buy => self.inventory += fill.qty,
            Side::Sell => self.inventory -= fill.qty,
        }

        // Volume tracking.
        self.attribution.total_volume += fill_value;
        self.attribution.fill_count += 1;

        // Round trip detection (simplified: inventory crosses zero).
        if ((self.inventory.is_zero())
            || (self.inventory > dec!(0) && fill.side == Side::Sell)
            || (self.inventory < dec!(0) && fill.side == Side::Buy))
            && self.inventory.is_zero()
        {
            self.attribution.round_trips += 1;
        }
    }

    /// Update inventory mark-to-market with new mid price.
    pub fn mark_to_market(&mut self, mid_price: Price) {
        if !self.last_mid.is_zero() && !self.inventory.is_zero() {
            let price_change = mid_price - self.last_mid;
            let inv_pnl_delta = self.inventory * price_change;
            self.attribution.inventory_pnl += inv_pnl_delta;
        }
        self.last_mid = mid_price;
    }

    /// Set the daily loan cost for amortization (Epic 2).
    /// Called when a loan agreement is loaded or updated.
    pub fn set_loan_daily_cost(&mut self, daily_cost: Decimal) {
        self.loan_daily_cost = daily_cost;
    }

    /// Amortize loan cost over elapsed time. Called periodically
    /// (e.g., every summary tick). `elapsed_days` is typically
    /// a fractional day count (e.g., 30s / 86400s).
    pub fn amortize_loan_cost(&mut self, elapsed_days: Decimal) {
        let cost = self.loan_daily_cost * elapsed_days;
        self.attribution.loan_cost_amortized += cost;
    }

    /// Ingest a fresh funding-rate snapshot from the venue
    /// (Epic 40.3). Called on the engine's periodic poll tick
    /// for every perp symbol. `next` is the venue-reported
    /// next-funding-time UTC instant; `interval` is the
    /// venue-reported cadence. The first call after boot also
    /// seeds `period_start` so the first MTM tick has a
    /// defined elapsed fraction.
    ///
    /// Idempotent on repeat of the same `next`: no re-seeding
    /// of `period_start`, so mid-period rate updates are
    /// consumed without resetting the accrual clock.
    pub fn on_funding_update(
        &mut self,
        rate: Decimal,
        next: DateTime<Utc>,
        interval: Duration,
    ) {
        self.funding_rate = Some(rate);
        self.funding_interval = Some(interval);
        // If we've never seen a funding update or the venue
        // has moved the settlement horizon forward, reseat
        // the period boundaries. `period_start` anchors the
        // elapsed fraction math in `accrue_funding_mtm`.
        match self.next_funding_time {
            None => {
                self.period_start = Some(next - interval);
                self.next_funding_time = Some(next);
            }
            Some(prev_next) if next > prev_next => {
                // Venue advanced next-settle without us having
                // called `settle_funding` (missed settle tick,
                // e.g. clock skew). Re-anchor — we accept the
                // venue's view as canonical.
                self.period_start = Some(next - interval);
                self.next_funding_time = Some(next);
            }
            _ => {}
        }
    }

    /// Continuously recompute the MTM funding P&L for the
    /// current period (Epic 40.3). **Stateless in inventory**
    /// — call every `tick_second` with the live inventory
    /// and mark and the stored MTM is overwritten; never
    /// integrate incrementally, or rate / inventory changes
    /// mid-period will drift.
    ///
    /// No-op when either:
    /// - the venue has not reported a funding rate yet
    ///   (`on_funding_update` was never called);
    /// - the engine has no inventory (`inventory == 0` →
    ///   nothing to accrue).
    pub fn accrue_funding_mtm(&mut self, now: DateTime<Utc>, mark: Price) {
        let (Some(rate), Some(next), Some(_start), Some(interval)) = (
            self.funding_rate,
            self.next_funding_time,
            self.period_start,
            self.funding_interval,
        ) else {
            self.attribution.funding_pnl_mtm = dec!(0);
            return;
        };
        if self.inventory.is_zero() {
            self.attribution.funding_pnl_mtm = dec!(0);
            return;
        }
        // Elapsed fraction ∈ [0, 1]. `Duration::num_seconds`
        // drops sub-second precision which is fine at an 8 h
        // period; also fine at HL's 1 h cadence (worst-case
        // 1/3600 rounding = 0.028 % of a single accrual).
        let period_secs = interval.num_seconds().max(1);
        let remaining_secs = (next - now).num_seconds().clamp(0, period_secs);
        // Prefer `remaining` when we have a next anchor — that
        // way a partial-hour start (operator restarts mid
        // period) uses `1 − remaining/interval` which is what
        // the venue will actually settle.
        let elapsed = period_secs - remaining_secs;
        let elapsed_fraction = Decimal::from(elapsed) / Decimal::from(period_secs);
        // `−inventory × mark × rate × elapsed_frac` — positive
        // rate + long = payment (MTM negative). Recompute
        // fresh every tick, do not accumulate.
        self.attribution.funding_pnl_mtm =
            -self.inventory * mark * rate * elapsed_fraction;
    }

    /// Book the current funding period into `realised` (Epic
    /// 40.3). Called by the engine when `now ≥
    /// next_funding_time`. Resets MTM to zero and advances
    /// the period boundaries by one `interval`. Returns the
    /// booked delta so the caller can route it into the audit
    /// trail.
    pub fn settle_funding(&mut self, now: DateTime<Utc>, mark: Price) -> Option<Decimal> {
        let (Some(rate), Some(next), Some(interval)) = (
            self.funding_rate,
            self.next_funding_time,
            self.funding_interval,
        ) else {
            return None;
        };
        if now < next {
            return None;
        }
        let delta = -self.inventory * mark * rate;
        self.attribution.funding_pnl_realised += delta;
        self.attribution.funding_pnl_mtm = dec!(0);
        self.period_start = Some(next);
        self.next_funding_time = Some(next + interval);
        Some(delta)
    }

    /// Engine-side setter for the tracker's view of live
    /// inventory (Epic 40.3). `on_fill` already updates the
    /// internal `inventory` field on every fill, but the
    /// source of truth for funding accrual is the
    /// [`InventoryManager`] (which reconciles fills against
    /// the venue). This lets the engine push a corrected
    /// figure in once per tick so the guard's reported MTM
    /// matches the real position even after a reconcile
    /// force-correction.
    pub fn set_inventory_for_funding(&mut self, inventory: Decimal) {
        self.inventory = inventory;
    }

    /// Read the last-observed funding rate. Dashboard surface.
    pub fn funding_rate(&self) -> Option<Decimal> {
        self.funding_rate
    }

    /// Read the next settlement instant, if known.
    pub fn next_funding_time(&self) -> Option<DateTime<Utc>> {
        self.next_funding_time
    }

    /// Log a periodic PnL summary.
    pub fn log_summary(&self) {
        let a = &self.attribution;
        info!(
            total = %a.total_pnl(),
            spread = %a.spread_pnl,
            inventory = %a.inventory_pnl,
            rebates = %a.rebate_income,
            fees = %a.fees_paid,
            funding_realised = %a.funding_pnl_realised,
            funding_mtm = %a.funding_pnl_mtm,
            round_trips = a.round_trips,
            volume = %a.total_volume,
            efficiency_bps = %( a.pnl_per_volume() * dec!(10_000)),
            "PnL attribution"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn fill(side: Side, price: &str, qty: &str, is_maker: bool) -> Fill {
        Fill {
            trade_id: 1,
            order_id: Uuid::new_v4(),
            symbol: "BTCUSDT".into(),
            side,
            price: price.parse().unwrap(),
            qty: qty.parse().unwrap(),
            is_maker,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_spread_capture() {
        let mut tracker = PnlTracker::new(dec!(-0.001), dec!(0.002));
        let mid = dec!(50000);

        // Buy below mid.
        tracker.on_fill(&fill(Side::Buy, "49995", "0.01", true), mid);
        // Spread capture = (50000 - 49995) * 0.01 = 0.05.
        assert_eq!(tracker.attribution.spread_pnl, dec!(0.05));

        // Rebate = 49995 * 0.01 * 0.001 = 0.49995.
        assert!(tracker.attribution.rebate_income > dec!(0.49));
    }

    /// `set_fee_rates` must hot-swap so subsequent fills attribute
    /// against the new schedule without rewriting prior accruals.
    /// Regression anchor for the periodic fee-tier refresh task —
    /// without this test a future contributor could refactor the
    /// rate fields into a snapshot taken at construction and not
    /// notice the live engine stops reflecting tier crossings.
    #[test]
    fn set_fee_rates_hot_swaps_for_subsequent_fills() {
        let mut tracker = PnlTracker::new(dec!(-0.0001), dec!(0.0004));
        let mid = dec!(50000);
        // First fill at the original rebate rate.
        tracker.on_fill(&fill(Side::Buy, "50000", "1", true), mid);
        let rebate_after_first = tracker.attribution.rebate_income;
        assert!(rebate_after_first > dec!(4.99) && rebate_after_first < dec!(5.01));

        // Hot-swap to a fatter rebate (VIP 9 territory). Apply a
        // second identical fill.
        tracker.set_fee_rates(dec!(-0.0002), dec!(0.0004));
        assert_eq!(tracker.maker_fee(), dec!(-0.0002));
        tracker.on_fill(&fill(Side::Buy, "50000", "1", true), mid);
        let rebate_delta = tracker.attribution.rebate_income - rebate_after_first;
        assert!(rebate_delta > dec!(9.99) && rebate_delta < dec!(10.01));
    }

    #[test]
    fn test_round_trip() {
        let mut tracker = PnlTracker::new(dec!(-0.001), dec!(0.002));
        let mid = dec!(50000);

        tracker.on_fill(&fill(Side::Buy, "49990", "0.01", true), mid);
        tracker.on_fill(&fill(Side::Sell, "50010", "0.01", true), mid);

        assert_eq!(tracker.attribution.round_trips, 1);
    }

    /// PNL-COUNTER-1 regression — `fill_count` increments on
    /// every fill regardless of whether inventory returns to
    /// zero. `round_trips` only advances on full cycles.
    /// Tenants read `fill_count` for "how many trades have I
    /// done".
    #[test]
    fn fill_count_tracks_raw_fills_independent_of_round_trips() {
        let mut tracker = PnlTracker::new(dec!(-0.001), dec!(0.002));
        let mid = dec!(50000);

        // Two buys in a row — no round trip, but two fills.
        tracker.on_fill(&fill(Side::Buy, "49990", "0.01", true), mid);
        tracker.on_fill(&fill(Side::Buy, "49985", "0.005", true), mid);
        assert_eq!(tracker.attribution.fill_count, 2);
        assert_eq!(tracker.attribution.round_trips, 0);

        // Sell that closes the position — round trip + fill +1.
        tracker.on_fill(&fill(Side::Sell, "50010", "0.015", true), mid);
        assert_eq!(tracker.attribution.fill_count, 3);
        assert_eq!(tracker.attribution.round_trips, 1);
    }

    // ── Property-based tests (Epic 10) ───────────────────────
    //
    // PnL attribution invariants: the accounting identity must
    // hold regardless of fill order, mid path, or fee rates.

    use proptest::prelude::*;
    use proptest::sample::select;

    fn mk_fill(side: Side, price: Decimal, qty: Decimal, is_maker: bool) -> Fill {
        Fill {
            trade_id: 1,
            order_id: Uuid::new_v4(),
            symbol: "TEST".into(),
            side,
            price,
            qty,
            is_maker,
            timestamp: Utc::now(),
        }
    }

    prop_compose! {
        fn price_strat()(cents in 100i64..10_000_000i64) -> Decimal {
            Decimal::new(cents, 2)
        }
    }
    prop_compose! {
        fn qty_strat()(units in 1i64..100_000i64) -> Decimal {
            Decimal::new(units, 4)
        }
    }
    fn side_strat() -> impl Strategy<Value = Side> {
        select(vec![Side::Buy, Side::Sell])
    }
    fn bool_strat() -> impl Strategy<Value = bool> {
        select(vec![true, false])
    }
    prop_compose! {
        fn fill_strat()(
            side in side_strat(),
            price in price_strat(),
            qty in qty_strat(),
            is_maker in bool_strat(),
        ) -> Fill {
            mk_fill(side, price, qty, is_maker)
        }
    }

    proptest! {
        /// total_pnl() identity must hold after any sequence of
        /// fills: total = spread + inventory + rebates − fees
        /// − loan_cost. If this drifts, dashboards and MiCA
        /// reports show a different number than sum-of-parts.
        #[test]
        fn total_pnl_identity_holds(
            fills in proptest::collection::vec(fill_strat(), 0..30),
            mid in price_strat(),
        ) {
            let mut tracker = PnlTracker::new(dec!(-0.0001), dec!(0.001));
            for f in &fills {
                tracker.on_fill(f, mid);
            }
            tracker.mark_to_market(mid);
            let a = &tracker.attribution;
            let expected = a.spread_pnl + a.inventory_pnl + a.rebate_income + a.funding_pnl_realised
                - a.fees_paid - a.loan_cost_amortized;
            prop_assert_eq!(a.total_pnl(), expected);
        }

        /// Fees and rebates are non-negative accumulators — they
        /// only grow. Spread and inventory pnl can be negative.
        #[test]
        fn fees_and_rebates_are_monotonic_non_negative(
            fills in proptest::collection::vec(fill_strat(), 0..30),
            mid in price_strat(),
        ) {
            let mut tracker = PnlTracker::new(dec!(-0.0001), dec!(0.001));
            let mut prev_fees = dec!(0);
            let mut prev_rebates = dec!(0);
            for f in &fills {
                tracker.on_fill(f, mid);
                prop_assert!(tracker.attribution.fees_paid >= prev_fees);
                prop_assert!(tracker.attribution.rebate_income >= prev_rebates);
                prop_assert!(tracker.attribution.fees_paid >= dec!(0));
                prop_assert!(tracker.attribution.rebate_income >= dec!(0));
                prev_fees = tracker.attribution.fees_paid;
                prev_rebates = tracker.attribution.rebate_income;
            }
        }

        /// total_volume strictly sums the per-fill notional — a
        /// rounding or cast error here is how attribution bps
        /// numbers silently drift.
        #[test]
        fn volume_equals_sum_of_notionals(
            fills in proptest::collection::vec(fill_strat(), 0..30),
            mid in price_strat(),
        ) {
            let mut tracker = PnlTracker::new(dec!(-0.0001), dec!(0.001));
            let mut expected = dec!(0);
            for f in &fills {
                tracker.on_fill(f, mid);
                expected += f.price * f.qty;
            }
            prop_assert_eq!(tracker.attribution.total_volume, expected);
        }

        /// A single maker fill with a non-positive maker fee
        /// (rebate) never increases fees_paid — only rebate.
        /// Catches a sign-flip regression in the fee branch.
        #[test]
        fn maker_rebate_goes_to_rebate_income(
            fill in fill_strat(),
            mid in price_strat(),
        ) {
            // Force is_maker = true so we hit the maker branch.
            let f = Fill { is_maker: true, ..fill };
            let mut tracker = PnlTracker::new(dec!(-0.0001), dec!(0.001));
            tracker.on_fill(&f, mid);
            prop_assert!(tracker.attribution.fees_paid.is_zero(),
                "maker rebate leaked into fees_paid");
            prop_assert!(tracker.attribution.rebate_income >= dec!(0));
        }

        /// A single taker fill (positive taker fee) never
        /// increases rebate_income.
        #[test]
        fn taker_fee_goes_to_fees_paid(
            fill in fill_strat(),
            mid in price_strat(),
        ) {
            let f = Fill { is_maker: false, ..fill };
            let mut tracker = PnlTracker::new(dec!(-0.0001), dec!(0.001));
            tracker.on_fill(&f, mid);
            prop_assert!(tracker.attribution.rebate_income.is_zero(),
                "taker fee leaked into rebate_income");
            prop_assert!(tracker.attribution.fees_paid >= dec!(0));
        }
    }

    // ── Epic 40.3 funding accrual tests ─────────────────────

    fn seed_long_one_btc(tracker: &mut PnlTracker, mid: Decimal) {
        let f = fill(Side::Buy, "50000", "1", true);
        tracker.on_fill(&f, mid);
        // Zero out rebates + spread so the later funding total
        // math isn't polluted by the on_fill side-effects.
        tracker.attribution = PnlAttribution::default();
    }

    #[test]
    fn funding_update_seeds_period_boundaries() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        let next = Utc::now() + Duration::seconds(3600);
        t.on_funding_update(dec!(0.0001), next, Duration::seconds(8 * 3600));
        assert_eq!(t.funding_rate(), Some(dec!(0.0001)));
        assert_eq!(t.next_funding_time(), Some(next));
    }

    #[test]
    fn funding_mtm_is_zero_before_rate_known() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        seed_long_one_btc(&mut t, dec!(50000));
        t.accrue_funding_mtm(Utc::now(), dec!(50000));
        assert_eq!(t.attribution.funding_pnl_mtm, dec!(0));
    }

    #[test]
    fn funding_mtm_long_positive_rate_is_negative() {
        // Long 1 BTC, positive rate 0.01% → long pays short →
        // MTM negative at period end.
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        seed_long_one_btc(&mut t, dec!(50000));
        let interval = Duration::seconds(8 * 3600);
        let now = Utc::now();
        t.on_funding_update(dec!(0.0001), now + interval, interval);
        // Jump to just before settle so elapsed ≈ full period.
        let near_settle = now + interval - Duration::seconds(1);
        t.accrue_funding_mtm(near_settle, dec!(50000));
        assert!(
            t.attribution.funding_pnl_mtm < dec!(0),
            "expected negative MTM, got {}",
            t.attribution.funding_pnl_mtm
        );
    }

    #[test]
    fn funding_mtm_short_positive_rate_is_positive() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        // Short via a sell fill against zero inventory.
        let f = fill(Side::Sell, "50000", "1", true);
        t.on_fill(&f, dec!(50000));
        t.attribution = PnlAttribution::default();
        let interval = Duration::seconds(8 * 3600);
        let now = Utc::now();
        t.on_funding_update(dec!(0.0001), now + interval, interval);
        t.accrue_funding_mtm(now + interval - Duration::seconds(1), dec!(50000));
        assert!(t.attribution.funding_pnl_mtm > dec!(0));
    }

    #[test]
    fn funding_mtm_flat_is_always_zero() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        let interval = Duration::seconds(3600);
        let now = Utc::now();
        t.on_funding_update(dec!(0.001), now + interval, interval);
        t.accrue_funding_mtm(now + Duration::seconds(1800), dec!(50000));
        assert_eq!(t.attribution.funding_pnl_mtm, dec!(0));
    }

    #[test]
    fn funding_mtm_is_idempotent_at_same_timestamp() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        seed_long_one_btc(&mut t, dec!(50000));
        let interval = Duration::seconds(8 * 3600);
        let now = Utc::now();
        t.on_funding_update(dec!(0.0001), now + interval, interval);
        t.accrue_funding_mtm(now + Duration::seconds(3600), dec!(50000));
        let a = t.attribution.funding_pnl_mtm;
        t.accrue_funding_mtm(now + Duration::seconds(3600), dec!(50000));
        let b = t.attribution.funding_pnl_mtm;
        assert_eq!(a, b);
    }

    #[test]
    fn funding_settle_books_realised_and_zeros_mtm() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        seed_long_one_btc(&mut t, dec!(50000));
        let interval = Duration::seconds(8 * 3600);
        let now = Utc::now();
        let next = now + interval;
        t.on_funding_update(dec!(0.0001), next, interval);
        // Accrue a bit of MTM then settle.
        t.accrue_funding_mtm(now + Duration::seconds(7200), dec!(50000));
        let delta = t.settle_funding(next + Duration::seconds(5), dec!(50000));
        assert!(delta.is_some(), "settle returned None at/after next");
        assert_eq!(t.attribution.funding_pnl_mtm, dec!(0));
        // Long 1 @ 50k, rate 0.0001 → delta = −1×50000×0.0001 = −5
        assert_eq!(t.attribution.funding_pnl_realised, dec!(-5));
        // Next horizon advanced by one interval.
        assert_eq!(t.next_funding_time(), Some(next + interval));
    }

    #[test]
    fn funding_settle_refuses_before_next_time() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        seed_long_one_btc(&mut t, dec!(50000));
        let interval = Duration::seconds(8 * 3600);
        let now = Utc::now();
        t.on_funding_update(dec!(0.0001), now + interval, interval);
        let r = t.settle_funding(now + Duration::seconds(100), dec!(50000));
        assert!(r.is_none());
        assert_eq!(t.attribution.funding_pnl_realised, dec!(0));
    }

    #[test]
    fn funding_settle_delta_scales_linearly_with_rate() {
        let interval = Duration::seconds(8 * 3600);
        let mk = |rate: Decimal| {
            let mut t = PnlTracker::new(dec!(0), dec!(0));
            seed_long_one_btc(&mut t, dec!(50000));
            let now = Utc::now();
            let next = now + interval;
            t.on_funding_update(rate, next, interval);
            t.settle_funding(next + Duration::seconds(5), dec!(50000))
                .unwrap()
        };
        let a = mk(dec!(0.0001));
        let b = mk(dec!(0.0002));
        // Double the rate → double the delta.
        assert_eq!(a * dec!(2), b);
    }

    #[test]
    fn total_pnl_includes_funding_realised() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        t.attribution.spread_pnl = dec!(10);
        t.attribution.funding_pnl_realised = dec!(5);
        assert_eq!(t.attribution.total_pnl(), dec!(15));
    }

    #[test]
    fn total_pnl_excludes_funding_mtm() {
        let mut t = PnlTracker::new(dec!(0), dec!(0));
        t.attribution.spread_pnl = dec!(10);
        t.attribution.funding_pnl_mtm = dec!(5);
        // MTM is display-only; total_pnl never counts it.
        assert_eq!(t.attribution.total_pnl(), dec!(10));
    }
}
