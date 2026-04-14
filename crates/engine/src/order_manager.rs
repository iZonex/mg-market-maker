use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use mm_common::types::{LiveOrder, OrderId, OrderType, Price, Qty, Quote, QuotePair, Side};
use mm_common::types::{ProductSpec, TimeInForce};
use mm_exchange_core::connector::{ExchangeConnector, NewOrder};
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

/// Manages live orders on the exchange.
/// Performs order diffing: only cancels/places orders that actually changed.
pub struct OrderManager {
    /// Our currently live orders on the exchange, keyed by order ID.
    live_orders: HashMap<OrderId, LiveOrder>,
    /// Map from (side, price) to order ID for quick lookup.
    price_index: HashMap<(Side, Price), OrderId>,
}

impl OrderManager {
    pub fn new() -> Self {
        Self {
            live_orders: HashMap::new(),
            price_index: HashMap::new(),
        }
    }

    /// Number of live orders.
    pub fn live_count(&self) -> usize {
        self.live_orders.len()
    }

    /// Get all live order IDs.
    pub fn live_order_ids(&self) -> Vec<OrderId> {
        self.live_orders.keys().copied().collect()
    }

    /// Total value locked in open orders (quote asset: price * remaining_qty).
    pub fn locked_value_quote(&self) -> Qty {
        self.live_orders
            .values()
            .map(|o| o.price * (o.qty - o.filled_qty))
            .sum()
    }

    /// Reconcile desired quotes with live orders.
    /// Returns (orders_to_cancel, quotes_to_place).
    pub fn diff_orders(
        &self,
        desired: &[QuotePair],
        product: &ProductSpec,
    ) -> (Vec<OrderId>, Vec<Quote>) {
        let mut to_cancel = Vec::new();
        let mut to_place = Vec::new();
        let mut desired_prices: HashMap<(Side, Price), Qty> = HashMap::new();

        // Collect all desired price levels.
        for pair in desired {
            if let Some(bid) = &pair.bid {
                let price = product.round_price(bid.price);
                desired_prices.insert((Side::Buy, price), bid.qty);
            }
            if let Some(ask) = &pair.ask {
                let price = product.round_price(ask.price);
                desired_prices.insert((Side::Sell, price), ask.qty);
            }
        }

        // Cancel orders that are no longer desired.
        for (&(side, price), &order_id) in &self.price_index {
            if !desired_prices.contains_key(&(side, price)) {
                to_cancel.push(order_id);
            }
        }

        // Place orders at new price levels.
        for ((side, price), qty) in &desired_prices {
            if !self.price_index.contains_key(&(*side, *price)) {
                to_place.push(Quote {
                    side: *side,
                    price: *price,
                    qty: *qty,
                });
            }
        }

        (to_cancel, to_place)
    }

    /// Execute the diff: cancel stale orders, place new ones.
    pub async fn execute_diff(
        &mut self,
        symbol: &str,
        desired: &[QuotePair],
        product: &ProductSpec,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> Result<()> {
        let (to_cancel, to_place) = self.diff_orders(desired, product);

        // Cancel stale orders.
        for order_id in &to_cancel {
            match connector.cancel_order(symbol, *order_id).await {
                Ok(_) => {
                    debug!(%order_id, "cancelled stale order");
                    self.remove_order(*order_id);
                }
                Err(e) => {
                    warn!(%order_id, error = %e, "failed to cancel order");
                    self.remove_order(*order_id);
                }
            }
        }

        // Place new orders.
        for quote in &to_place {
            let order = NewOrder {
                symbol: symbol.to_string(),
                side: quote.side,
                order_type: OrderType::Limit,
                price: Some(quote.price),
                qty: quote.qty,
                time_in_force: Some(TimeInForce::PostOnly),
                client_order_id: None,
            };

            match connector.place_order(&order).await {
                Ok(order_id) => {
                    info!(
                        %order_id,
                        side = ?quote.side,
                        price = %quote.price,
                        qty = %quote.qty,
                        "placed order"
                    );
                    self.track_order(LiveOrder {
                        order_id,
                        symbol: symbol.to_string(),
                        side: quote.side,
                        price: quote.price,
                        qty: quote.qty,
                        filled_qty: dec!(0),
                        status: mm_common::types::OrderStatus::Open,
                        created_at: chrono::Utc::now(),
                    });
                }
                Err(e) => {
                    warn!(
                        side = ?quote.side,
                        price = %quote.price,
                        error = %e,
                        "failed to place order"
                    );
                }
            }
        }

        if !to_cancel.is_empty() || !to_place.is_empty() {
            info!(
                cancelled = to_cancel.len(),
                placed = to_place.len(),
                live = self.live_count(),
                "order diff executed"
            );
        }

        Ok(())
    }

    /// Place a single unwind slice on the venue without going
    /// through the diff machinery. Used by kill-switch L4
    /// executors (`TwapExecutor`, `PairedUnwindExecutor`) where
    /// each tick emits a fresh IOC-ish slice that either fills
    /// immediately or gets cleaned up on shutdown.
    ///
    /// The order is placed as a limit with `TimeInForce::Ioc`
    /// so a non-crossing slice evaporates on the venue side
    /// instead of resting and interfering with future slices.
    /// Tracked in `live_orders` so `cancel_all` + fill routing
    /// still work.
    pub async fn execute_unwind_slice(
        &mut self,
        symbol: &str,
        quote: &Quote,
        product: &ProductSpec,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> Result<()> {
        let price = product.round_price(quote.price);
        let qty = product.round_qty(quote.qty);
        if qty.is_zero() {
            return Ok(());
        }
        let order = NewOrder {
            symbol: symbol.to_string(),
            side: quote.side,
            order_type: OrderType::Limit,
            price: Some(price),
            qty,
            time_in_force: Some(TimeInForce::Ioc),
            client_order_id: None,
        };
        match connector.place_order(&order).await {
            Ok(order_id) => {
                info!(
                    %order_id,
                    side = ?quote.side,
                    %price,
                    %qty,
                    "placed unwind slice"
                );
                self.track_order(LiveOrder {
                    order_id,
                    symbol: symbol.to_string(),
                    side: quote.side,
                    price,
                    qty,
                    filled_qty: dec!(0),
                    status: mm_common::types::OrderStatus::Open,
                    created_at: chrono::Utc::now(),
                });
                Ok(())
            }
            Err(e) => {
                warn!(error = %e, "unwind slice placement failed");
                Err(e)
            }
        }
    }

    /// Cancel all live orders (emergency or shutdown).
    pub async fn cancel_all(&mut self, connector: &Arc<dyn ExchangeConnector>, symbol: &str) {
        let ids: Vec<OrderId> = self.live_orders.keys().copied().collect();
        for order_id in ids {
            let _ = connector.cancel_order(symbol, order_id).await;
            self.remove_order(order_id);
        }
        info!("all orders cancelled");
    }

    fn track_order(&mut self, order: LiveOrder) {
        self.price_index
            .insert((order.side, order.price), order.order_id);
        self.live_orders.insert(order.order_id, order);
    }

    fn remove_order(&mut self, order_id: OrderId) {
        if let Some(order) = self.live_orders.remove(&order_id) {
            self.price_index.remove(&(order.side, order.price));
        }
    }

    /// Handle a fill event — update or remove the filled order.
    pub fn on_fill(&mut self, order_id: OrderId, filled_qty: Qty) {
        if let Some(order) = self.live_orders.get_mut(&order_id) {
            order.filled_qty += filled_qty;
            if order.filled_qty >= order.qty {
                let id = order.order_id;
                self.remove_order(id);
            }
        }
    }
}

impl Default for OrderManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_locked_value_quote() {
        let mut mgr = OrderManager::new();

        let o1 = LiveOrder {
            order_id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".to_string(),
            side: Side::Buy,
            price: dec!(50000),
            qty: dec!(0.1),
            filled_qty: dec!(0),
            status: mm_common::types::OrderStatus::Open,
            created_at: chrono::Utc::now(),
        };
        let o2 = LiveOrder {
            order_id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".to_string(),
            side: Side::Sell,
            price: dec!(51000),
            qty: dec!(0.2),
            filled_qty: dec!(0.05),
            status: mm_common::types::OrderStatus::PartiallyFilled,
            created_at: chrono::Utc::now(),
        };

        mgr.track_order(o1);
        mgr.track_order(o2);

        // o1: 50000 * 0.1 = 5000. o2: 51000 * (0.2 - 0.05) = 51000 * 0.15 = 7650.
        assert_eq!(mgr.locked_value_quote(), dec!(12650));
    }
}
