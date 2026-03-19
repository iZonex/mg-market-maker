use mm_common::types::*;
use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

/// Paper trading engine — simulates fills on live market data.
///
/// Unlike backtester (which replays historical data), paper trading
/// runs in real-time but doesn't send actual orders to the exchange.
pub struct PaperTrader {
    /// Simulated orders.
    orders: HashMap<OrderId, PaperOrder>,
    /// Simulated balances.
    balances: HashMap<String, Decimal>,
    /// Simulated fills.
    fills: Vec<Fill>,
    /// Fill counter.
    next_trade_id: u64,
}

#[derive(Debug, Clone)]
struct PaperOrder {
    _order_id: OrderId,
    symbol: String,
    side: Side,
    price: Price,
    _qty: Qty,
    remaining: Qty,
}

impl PaperTrader {
    pub fn new(initial_balances: HashMap<String, Decimal>) -> Self {
        Self {
            orders: HashMap::new(),
            balances: initial_balances,
            fills: Vec::new(),
            next_trade_id: 1,
        }
    }

    /// Place a simulated order.
    pub fn place_order(&mut self, symbol: &str, side: Side, price: Price, qty: Qty) -> OrderId {
        let order_id = Uuid::new_v4();
        self.orders.insert(
            order_id,
            PaperOrder {
                _order_id: order_id,
                symbol: symbol.to_string(),
                side,
                price,
                _qty: qty,
                remaining: qty,
            },
        );
        order_id
    }

    /// Cancel a simulated order.
    pub fn cancel_order(&mut self, order_id: OrderId) -> bool {
        self.orders.remove(&order_id).is_some()
    }

    /// Cancel all orders.
    pub fn cancel_all(&mut self) {
        self.orders.clear();
    }

    /// Check if any orders would be filled by a trade at this price.
    /// Call this for every public trade received.
    pub fn on_trade(&mut self, price: Price, qty: Qty, taker_side: Side) -> Vec<Fill> {
        // Collect fill info first (avoid double borrow).
        let mut pending: Vec<(OrderId, Side, Price, Qty)> = Vec::new();

        for (oid, order) in &mut self.orders {
            let would_fill = match (order.side, taker_side) {
                (Side::Sell, Side::Buy) => price >= order.price,
                (Side::Buy, Side::Sell) => price <= order.price,
                _ => false,
            };

            if would_fill {
                let fill_qty = order.remaining.min(qty);
                order.remaining -= fill_qty;
                pending.push((*oid, order.side, order.price, fill_qty));
            }
        }

        // Now apply fills and build result.
        let mut new_fills = Vec::new();
        let mut filled_ids = Vec::new();

        for (oid, side, fill_price, fill_qty) in pending {
            let fill = Fill {
                trade_id: self.next_trade_id,
                order_id: oid,
                symbol: self
                    .orders
                    .get(&oid)
                    .map(|o| o.symbol.clone())
                    .unwrap_or_default(),
                side,
                price: fill_price,
                qty: fill_qty,
                is_maker: true,
                timestamp: chrono::Utc::now(),
            };
            self.next_trade_id += 1;
            new_fills.push(fill.clone());
            self.fills.push(fill);
            self.apply_fill(side, fill_price, fill_qty);

            if let Some(order) = self.orders.get(&oid) {
                if order.remaining.is_zero() {
                    filled_ids.push(oid);
                }
            }
        }

        for oid in filled_ids {
            self.orders.remove(&oid);
        }

        new_fills
    }

    fn apply_fill(&mut self, side: Side, price: Price, qty: Qty) {
        let quote_amount = price * qty;
        match side {
            Side::Buy => {
                // Bought base, spent quote.
                *self.balances.entry("BASE".into()).or_default() += qty;
                *self.balances.entry("QUOTE".into()).or_default() -= quote_amount;
            }
            Side::Sell => {
                // Sold base, received quote.
                *self.balances.entry("BASE".into()).or_default() -= qty;
                *self.balances.entry("QUOTE".into()).or_default() += quote_amount;
            }
        }
    }

    /// Get current balances.
    pub fn balances(&self) -> &HashMap<String, Decimal> {
        &self.balances
    }

    /// Get all fills.
    pub fn fills(&self) -> &[Fill] {
        &self.fills
    }

    /// Get open order count.
    pub fn open_orders(&self) -> usize {
        self.orders.len()
    }

    /// Get total PnL (mark-to-market).
    pub fn pnl(&self, current_price: Price) -> Decimal {
        let base = self.balances.get("BASE").copied().unwrap_or_default();
        let quote = self.balances.get("QUOTE").copied().unwrap_or_default();
        quote + base * current_price
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_paper_fill() {
        let mut balances = HashMap::new();
        balances.insert("QUOTE".into(), dec!(10000));
        balances.insert("BASE".into(), dec!(0));

        let mut paper = PaperTrader::new(balances);

        // Place a buy order.
        let _oid = paper.place_order("BTCUSDT", Side::Buy, dec!(50000), dec!(0.01));

        // A sell trade at 49999 should fill our buy.
        let fills = paper.on_trade(dec!(49999), dec!(1), Side::Sell);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].side, Side::Buy);
        assert_eq!(fills[0].price, dec!(50000));

        // Balance should reflect the purchase.
        assert_eq!(paper.balances()["BASE"], dec!(0.01));
    }
}
