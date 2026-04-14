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
}

impl InventoryManager {
    pub fn new() -> Self {
        Self {
            inventory: dec!(0),
            avg_entry_price: dec!(0),
            total_bought: dec!(0),
            total_sold: dec!(0),
            realized_pnl: dec!(0),
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

        info!(
            inventory = %self.inventory,
            avg_entry = %self.avg_entry_price,
            realized_pnl = %self.realized_pnl,
            "inventory updated"
        );
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
}
