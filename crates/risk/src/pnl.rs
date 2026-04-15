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
    /// Number of round-trips completed.
    pub round_trips: u64,
    /// Total volume traded (both sides).
    pub total_volume: Decimal,
}

impl PnlAttribution {
    pub fn total_pnl(&self) -> Decimal {
        self.spread_pnl + self.inventory_pnl + self.rebate_income - self.fees_paid
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
}

impl PnlTracker {
    pub fn new(maker_fee: Decimal, taker_fee: Decimal) -> Self {
        Self {
            attribution: PnlAttribution::default(),
            last_mid: dec!(0),
            inventory: dec!(0),
            maker_fee,
            taker_fee,
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

    /// Log a periodic PnL summary.
    pub fn log_summary(&self) {
        let a = &self.attribution;
        info!(
            total = %a.total_pnl(),
            spread = %a.spread_pnl,
            inventory = %a.inventory_pnl,
            rebates = %a.rebate_income,
            fees = %a.fees_paid,
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
}
