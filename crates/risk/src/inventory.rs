use chrono::{DateTime, Utc};
use mm_common::config::RiskConfig;
use mm_common::types::{Fill, Price, QuotePair, Side};
use rust_decimal::prelude::Signed;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{info, warn};

/// Tracks inventory (net position) and applies limits/skew.
pub struct InventoryManager {
    /// Net inventory in base asset. Positive = long, negative = short.
    inventory: Decimal,
    /// Average entry price (for PnL tracking).
    avg_entry_price: Decimal,
    /// Total base asset bought.
    total_bought: Decimal,
    /// Total base asset sold.
    total_sold: Decimal,
    /// Realized PnL in quote asset.
    realized_pnl: Decimal,
    /// INV-2 — when the current "trade" opened. A trade = one
    /// flat → non-flat → flat round-trip. `None` while flat.
    /// Sign flips (long → short in a single fill) close the
    /// previous trade and open a fresh one at the flip time.
    trade_opened_at: Option<DateTime<Utc>>,
    /// INV-2 — peak absolute inventory reached since the
    /// current trade opened. Stays at the high-water mark even
    /// after partial reduces so `trade_drawdown` returns the
    /// true peak-to-trough delta.
    trade_peak_abs: Decimal,
}

impl InventoryManager {
    pub fn new() -> Self {
        Self {
            inventory: dec!(0),
            avg_entry_price: dec!(0),
            total_bought: dec!(0),
            total_sold: dec!(0),
            realized_pnl: dec!(0),
            trade_opened_at: None,
            trade_peak_abs: dec!(0),
        }
    }

    /// Current inventory.
    pub fn inventory(&self) -> Decimal {
        self.inventory
    }

    /// Realized PnL.
    pub fn realized_pnl(&self) -> Decimal {
        self.realized_pnl
    }

    /// RS-3 — read-only accessor for the running average entry
    /// price. `0` when the position is flat (no open exposure).
    pub fn avg_entry_price(&self) -> Decimal {
        self.avg_entry_price
    }

    /// Unrealized PnL at a given mark price.
    pub fn unrealized_pnl(&self, mark_price: Price) -> Decimal {
        if self.inventory.is_zero() || self.avg_entry_price.is_zero() {
            return dec!(0);
        }
        self.inventory * (mark_price - self.avg_entry_price)
    }

    /// Total PnL.
    pub fn total_pnl(&self, mark_price: Price) -> Decimal {
        self.realized_pnl + self.unrealized_pnl(mark_price)
    }

    /// Record a fill and update inventory + PnL.
    pub fn on_fill(&mut self, fill: &Fill) {
        let signed_qty = match fill.side {
            Side::Buy => fill.qty,
            Side::Sell => -fill.qty,
        };

        let old_inventory = self.inventory;

        // If reducing position, realize PnL.
        if (old_inventory > dec!(0) && signed_qty < dec!(0))
            || (old_inventory < dec!(0) && signed_qty > dec!(0))
        {
            let reducing = signed_qty.abs().min(old_inventory.abs());
            let pnl = if old_inventory > dec!(0) {
                // Was long, selling → PnL = (sell_price - avg_entry) * qty.
                reducing * (fill.price - self.avg_entry_price)
            } else {
                // Was short, buying → PnL = (avg_entry - buy_price) * qty.
                reducing * (self.avg_entry_price - fill.price)
            };
            self.realized_pnl += pnl;
        }

        self.inventory += signed_qty;

        // Update average entry price.
        if self.inventory.is_zero() {
            self.avg_entry_price = dec!(0);
        } else if self.inventory.signum() == signed_qty.signum() {
            // Adding to position — weighted average.
            let old_cost = old_inventory.abs() * self.avg_entry_price;
            let new_cost = signed_qty.abs() * fill.price;
            self.avg_entry_price = (old_cost + new_cost) / self.inventory.abs();
        }
        // If flip (going from long to short or vice versa), new entry = fill price.
        if old_inventory.signum() != self.inventory.signum() && !self.inventory.is_zero() {
            self.avg_entry_price = fill.price;
        }

        match fill.side {
            Side::Buy => self.total_bought += fill.qty,
            Side::Sell => self.total_sold += fill.qty,
        }

        // INV-2 — maintain per-trade timestamp + peak absolute
        // inventory. Transitions:
        // - flat → non-flat           → open a new trade
        // - non-flat → flat           → close the current trade
        // - sign flip in one fill     → close the old trade AND
        //                                open a fresh one at the
        //                                flip timestamp
        // - growing the same side     → bump the peak
        // - reducing without flipping → peak unchanged (so
        //                                `trade_drawdown` stays
        //                                peak-to-trough)
        let old_sign = old_inventory.signum();
        let new_sign = self.inventory.signum();
        let now = fill.timestamp;
        if old_sign != new_sign {
            if self.inventory.is_zero() {
                // Closed out.
                self.trade_opened_at = None;
                self.trade_peak_abs = dec!(0);
            } else if old_sign.is_zero() {
                // Opened a fresh trade from flat.
                self.trade_opened_at = Some(now);
                self.trade_peak_abs = self.inventory.abs();
            } else {
                // Sign-flipped in a single fill — close the old
                // leg, open a new one at the same timestamp.
                self.trade_opened_at = Some(now);
                self.trade_peak_abs = self.inventory.abs();
            }
        } else {
            // Same-sign update — bump the peak if we grew.
            let curr_abs = self.inventory.abs();
            if curr_abs > self.trade_peak_abs {
                self.trade_peak_abs = curr_abs;
            }
        }

        info!(
            inventory = %self.inventory,
            avg_entry = %self.avg_entry_price,
            realized_pnl = %self.realized_pnl,
            trade_peak_abs = %self.trade_peak_abs,
            "inventory updated"
        );
    }

    /// INV-2 — seconds since the current trade opened. `None`
    /// while flat or before the first fill.
    pub fn trade_holding_seconds(&self, now: DateTime<Utc>) -> Option<i64> {
        self.trade_opened_at
            .map(|opened| (now - opened).num_seconds().max(0))
    }

    /// INV-2 — peak absolute inventory reached since the
    /// current trade opened. `0` while flat.
    pub fn trade_peak_abs(&self) -> Decimal {
        self.trade_peak_abs
    }

    /// INV-2 — peak-to-trough drawdown of absolute inventory
    /// within the current trade. Non-negative:
    /// `trade_peak_abs − |inventory|`. A long trade that hit
    /// peak 0.8 BTC and is now 0.3 BTC reports `0.5`; flat and
    /// newly-opened trades both report `0`.
    pub fn trade_drawdown(&self) -> Decimal {
        if self.trade_opened_at.is_none() {
            return dec!(0);
        }
        (self.trade_peak_abs - self.inventory.abs()).max(dec!(0))
    }

    /// INV-2 — wall-clock timestamp the current trade opened.
    /// `None` while flat.
    pub fn trade_opened_at(&self) -> Option<DateTime<Utc>> {
        self.trade_opened_at
    }

    /// Check if we're within inventory limits. Returns scaling factor [0, 1].
    /// 0 = at limit, don't add to this side. 1 = plenty of room.
    pub fn inventory_scale(&self, side: Side, config: &RiskConfig) -> Decimal {
        let max = config.max_inventory;
        if max.is_zero() {
            return dec!(1);
        }

        match side {
            Side::Buy => {
                // If long, reduce buy aggressiveness.
                if self.inventory >= max {
                    dec!(0)
                } else if self.inventory > dec!(0) {
                    dec!(1) - self.inventory / max
                } else {
                    dec!(1)
                }
            }
            Side::Sell => {
                // If short, reduce sell aggressiveness.
                if self.inventory <= -max {
                    dec!(0)
                } else if self.inventory < dec!(0) {
                    dec!(1) - self.inventory.abs() / max
                } else {
                    dec!(1)
                }
            }
        }
    }

    /// Force-reset the tracked inventory to the given value
    /// without touching the average entry price or realized
    /// PnL fields. Used by the inventory drift reconciler
    /// when `auto_correct = true` detects a mismatch against
    /// the wallet balance — the correction is a last-resort
    /// self-heal, not a normal code path.
    ///
    /// **Warning.** Because `avg_entry_price` / `realized_pnl`
    /// are not adjusted, subsequent PnL attribution after a
    /// forced reset should be treated as approximate until the
    /// position is flat again. Operators should prefer
    /// alert-only mode and manually intervene on drift.
    pub fn force_reset_inventory_to(&mut self, new_inventory: Decimal) {
        warn!(
            old = %self.inventory,
            new = %new_inventory,
            "force-resetting tracked inventory (drift auto-correct)"
        );
        self.inventory = new_inventory;
    }

    /// Apply inventory limits to quotes — scale down or remove quotes
    /// on the side that would increase inventory beyond limits.
    pub fn apply_limits(&self, quotes: &mut [QuotePair], config: &RiskConfig) {
        let buy_scale = self.inventory_scale(Side::Buy, config);
        let sell_scale = self.inventory_scale(Side::Sell, config);

        if buy_scale.is_zero() {
            warn!(inventory = %self.inventory, "at max long inventory, removing bids");
        }
        if sell_scale.is_zero() {
            warn!(inventory = %self.inventory, "at max short inventory, removing asks");
        }

        for q in quotes.iter_mut() {
            if buy_scale.is_zero() {
                q.bid = None;
            }
            if sell_scale.is_zero() {
                q.ask = None;
            }
        }
    }
}

impl Default for InventoryManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn make_fill(side: Side, price: &str, qty: &str) -> Fill {
        Fill {
            trade_id: 1,
            order_id: Uuid::new_v4(),
            symbol: "BTCUSDT".into(),
            side,
            price: price.parse().unwrap(),
            qty: qty.parse().unwrap(),
            is_maker: true,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_buy_then_sell_pnl() {
        let mut mgr = InventoryManager::new();
        mgr.on_fill(&make_fill(Side::Buy, "50000", "0.01"));
        assert_eq!(mgr.inventory(), dec!(0.01));
        assert_eq!(mgr.avg_entry_price, dec!(50000));

        mgr.on_fill(&make_fill(Side::Sell, "51000", "0.01"));
        assert_eq!(mgr.inventory(), dec!(0));
        // PnL = 0.01 * (51000 - 50000) = 10.
        assert_eq!(mgr.realized_pnl(), dec!(10));
    }

    // ── INV-2 — per-trade drawdown + holding time ─────────────

    fn make_fill_at(side: Side, price: &str, qty: &str, ts: DateTime<Utc>) -> Fill {
        Fill {
            trade_id: 1,
            order_id: Uuid::new_v4(),
            symbol: "BTCUSDT".into(),
            side,
            price: price.parse().unwrap(),
            qty: qty.parse().unwrap(),
            is_maker: true,
            timestamp: ts,
        }
    }

    #[test]
    fn trade_timer_opens_on_first_fill_and_closes_on_flat() {
        let mut mgr = InventoryManager::new();
        assert!(mgr.trade_opened_at().is_none());
        let t0 = Utc::now();
        mgr.on_fill(&make_fill_at(Side::Buy, "50000", "0.01", t0));
        assert_eq!(mgr.trade_opened_at(), Some(t0));
        assert_eq!(mgr.trade_peak_abs(), dec!(0.01));

        // Close the trade.
        mgr.on_fill(&make_fill_at(
            Side::Sell,
            "51000",
            "0.01",
            t0 + chrono::Duration::seconds(30),
        ));
        assert!(mgr.trade_opened_at().is_none());
        assert_eq!(mgr.trade_peak_abs(), dec!(0));
    }

    #[test]
    fn trade_peak_tracks_highest_abs_inventory_seen() {
        let mut mgr = InventoryManager::new();
        let t0 = Utc::now();
        mgr.on_fill(&make_fill_at(Side::Buy, "50000", "0.2", t0));
        mgr.on_fill(&make_fill_at(Side::Buy, "50100", "0.3", t0));
        assert_eq!(mgr.trade_peak_abs(), dec!(0.5));
        // Reduce but don't flatten — peak stays at 0.5.
        mgr.on_fill(&make_fill_at(Side::Sell, "51000", "0.2", t0));
        assert_eq!(mgr.trade_peak_abs(), dec!(0.5));
        assert_eq!(mgr.inventory(), dec!(0.3));
    }

    #[test]
    fn trade_drawdown_is_peak_minus_current_abs() {
        let mut mgr = InventoryManager::new();
        let t0 = Utc::now();
        mgr.on_fill(&make_fill_at(Side::Buy, "50000", "0.5", t0));
        assert_eq!(mgr.trade_drawdown(), dec!(0));
        mgr.on_fill(&make_fill_at(Side::Sell, "51000", "0.2", t0));
        // Peak was 0.5, current abs is 0.3 → drawdown 0.2.
        assert_eq!(mgr.trade_drawdown(), dec!(0.2));
    }

    #[test]
    fn sign_flip_resets_trade_state() {
        let mut mgr = InventoryManager::new();
        let t0 = Utc::now();
        let t1 = t0 + chrono::Duration::seconds(10);
        // Open long.
        mgr.on_fill(&make_fill_at(Side::Buy, "50000", "0.3", t0));
        assert_eq!(mgr.trade_opened_at(), Some(t0));
        assert_eq!(mgr.trade_peak_abs(), dec!(0.3));
        // Flip short in one fill.
        mgr.on_fill(&make_fill_at(Side::Sell, "51000", "0.5", t1));
        // Fresh short trade opens at t1 with 0.2 abs inventory.
        assert_eq!(mgr.inventory(), dec!(-0.2));
        assert_eq!(mgr.trade_opened_at(), Some(t1));
        assert_eq!(mgr.trade_peak_abs(), dec!(0.2));
        assert_eq!(mgr.trade_drawdown(), dec!(0));
    }

    #[test]
    fn holding_seconds_reads_wall_clock_delta() {
        let mut mgr = InventoryManager::new();
        let t0 = Utc::now();
        mgr.on_fill(&make_fill_at(Side::Buy, "50000", "0.1", t0));
        let now = t0 + chrono::Duration::seconds(42);
        assert_eq!(mgr.trade_holding_seconds(now), Some(42));
        // After closing, no timer.
        mgr.on_fill(&make_fill_at(Side::Sell, "51000", "0.1", now));
        assert_eq!(mgr.trade_holding_seconds(now), None);
    }

    #[test]
    fn test_inventory_scale() {
        let mut mgr = InventoryManager::new();
        mgr.inventory = dec!(0.05);
        let config = RiskConfig {
            max_inventory: dec!(0.1),
            max_exposure_quote: dec!(10000),
            max_drawdown_quote: dec!(500),
            inventory_skew_factor: dec!(1),
            max_spread_bps: dec!(500),
            max_spread_to_quote_bps: None,
            stale_book_timeout_secs: 10,
            max_order_size: dec!(0),
            max_daily_volume_quote: dec!(0),
            max_hourly_volume_quote: dec!(0),
        };
        let buy_scale = mgr.inventory_scale(Side::Buy, &config);
        assert_eq!(buy_scale, dec!(0.5)); // 1 - 0.05/0.1

        let sell_scale = mgr.inventory_scale(Side::Sell, &config);
        assert_eq!(sell_scale, dec!(1)); // Not short, full room.
    }

    // ── Property-based tests (Epic 9) ────────────────────────
    //
    // Exercise the `InventoryManager` arithmetic against random
    // fill sequences. Catches off-by-one in the avg-entry-price
    // weighted average + PnL accounting that hand-written tests
    // miss by only exercising a handful of patterns.

    use proptest::prelude::*;
    use proptest::sample::select;

    /// Hand-roll a Fill so proptest-generated inputs go through
    /// the same path live fills do. The only fields `on_fill`
    /// reads are `side`, `price`, and `qty`.
    fn mk_fill(side: Side, price: Decimal, qty: Decimal) -> Fill {
        Fill {
            trade_id: 0,
            order_id: uuid::Uuid::nil(),
            symbol: "TEST".into(),
            side,
            price,
            qty,
            is_maker: true,
            timestamp: chrono::Utc::now(),
        }
    }

    // Concrete Decimal strategy: positive decimals with 4 dp,
    // bounded well clear of overflow.
    prop_compose! {
        fn price_strat()(cents in 1i64..10_000_000i64) -> Decimal {
            Decimal::new(cents, 2)  // 0.01 .. 100_000.00
        }
    }
    prop_compose! {
        fn qty_strat()(units in 1i64..1_000_000i64) -> Decimal {
            Decimal::new(units, 4)  // 0.0001 .. 100.0000
        }
    }
    fn side_strat() -> impl Strategy<Value = Side> {
        select(vec![Side::Buy, Side::Sell])
    }
    prop_compose! {
        fn fill_strat()(
            side in side_strat(),
            price in price_strat(),
            qty in qty_strat(),
        ) -> Fill {
            mk_fill(side, price, qty)
        }
    }

    proptest! {
        /// Inventory after a sequence equals the net signed qty
        /// across every fill. Invariant regardless of price path
        /// or intermediate flip direction.
        #[test]
        fn inventory_equals_net_signed_qty(
            fills in proptest::collection::vec(fill_strat(), 0..50),
        ) {
            let mut mgr = InventoryManager::new();
            let mut expected = dec!(0);
            for f in &fills {
                mgr.on_fill(f);
                expected += match f.side {
                    Side::Buy => f.qty,
                    Side::Sell => -f.qty,
                };
            }
            prop_assert_eq!(mgr.inventory(), expected);
        }

        /// total_bought − total_sold equals inventory. Mirrors
        /// the invariant above but from the accumulator angle —
        /// catches a regression where one counter drifts out of
        /// sync with the signed-qty sum.
        #[test]
        fn bought_minus_sold_equals_inventory(
            fills in proptest::collection::vec(fill_strat(), 0..50),
        ) {
            let mut mgr = InventoryManager::new();
            for f in &fills {
                mgr.on_fill(f);
            }
            let bought: Decimal = fills.iter().filter(|f| f.side == Side::Buy).map(|f| f.qty).sum();
            let sold: Decimal   = fills.iter().filter(|f| f.side == Side::Sell).map(|f| f.qty).sum();
            prop_assert_eq!(mgr.inventory(), bought - sold);
        }

        /// A flat sequence that closes out to zero inventory
        /// must produce zero unrealized PnL at ANY mark price.
        /// The realized PnL captured everything.
        #[test]
        fn closed_position_has_zero_unrealized(
            fill in fill_strat(),
            mark in price_strat(),
        ) {
            let mut mgr = InventoryManager::new();
            mgr.on_fill(&fill);
            // Close by mirroring.
            let opposite = mk_fill(
                match fill.side { Side::Buy => Side::Sell, Side::Sell => Side::Buy },
                fill.price,
                fill.qty,
            );
            mgr.on_fill(&opposite);
            prop_assert!(mgr.inventory().is_zero());
            prop_assert_eq!(mgr.unrealized_pnl(mark), dec!(0));
        }

        /// Open → close at the SAME price nets zero realized PnL.
        /// Flushes out any sign error in the flip branch.
        #[test]
        fn round_trip_at_same_price_is_zero_realized(
            fill in fill_strat(),
        ) {
            let mut mgr = InventoryManager::new();
            mgr.on_fill(&fill);
            let opposite = mk_fill(
                match fill.side { Side::Buy => Side::Sell, Side::Sell => Side::Buy },
                fill.price,
                fill.qty,
            );
            mgr.on_fill(&opposite);
            prop_assert_eq!(mgr.realized_pnl(), dec!(0));
        }

        /// Long-only sequence: realized PnL stays non-negative
        /// if every sell is at a price ≥ avg_entry at sale time.
        /// We ensure that by selling at a price STRICTLY LARGER
        /// than every prior buy — the weighted average is
        /// bounded above by that maximum so the realized
        /// delta per reducing slice is non-negative.
        #[test]
        fn long_sequence_with_higher_exits_never_realizes_loss(
            buys in proptest::collection::vec(
                (price_strat(), qty_strat()),
                1..10
            ),
            exit_premium in 1i64..100i64,
            exit_qty in qty_strat(),
        ) {
            let mut mgr = InventoryManager::new();
            let mut max_entry = dec!(0);
            for (p, q) in &buys {
                mgr.on_fill(&mk_fill(Side::Buy, *p, *q));
                if *p > max_entry { max_entry = *p; }
            }
            // Exit above the highest entry so weighted avg ≤ exit price.
            let exit_price = max_entry + Decimal::new(exit_premium, 2);
            // Exit qty capped at inventory so we don't flip.
            let exit = exit_qty.min(mgr.inventory());
            if exit > dec!(0) {
                mgr.on_fill(&mk_fill(Side::Sell, exit_price, exit));
            }
            prop_assert!(mgr.realized_pnl() >= dec!(0),
                "realized_pnl went negative: {} (max_entry={} exit_price={})",
                mgr.realized_pnl(), max_entry, exit_price);
        }
    }
}
