use async_trait::async_trait;
use mm_common::types::*;
use mm_exchange_core::connector::*;
use mm_exchange_core::events::MarketEvent;
use tokio::sync::mpsc;
use tracing::debug;

use crate::rest::ExchangeRestClient;
use crate::types::PlaceOrderRequest;
use crate::ws::ExchangeWsClient;

/// Adapter: wraps the custom exchange REST+WS clients into the unified
/// `ExchangeConnector` trait so the engine can use any exchange interchangeably.
pub struct CustomConnector {
    rest: ExchangeRestClient,
    ws_url: String,
    capabilities: VenueCapabilities,
}

impl CustomConnector {
    pub fn new(rest_url: &str, ws_url: &str) -> Self {
        Self {
            rest: ExchangeRestClient::new(rest_url),
            ws_url: ws_url.to_string(),
            capabilities: VenueCapabilities {
                max_batch_size: 1,
                supports_amend: false,
                supports_ws_trading: false,
                supports_fix: false,
                max_order_rate: 100,
                supports_funding_rate: false,
                supports_margin_info: false,
                supports_margin_mode: false,
            supports_liquidation_feed: false,
            supports_set_leverage: false,
                        },
        }
    }
}

#[async_trait]
impl ExchangeConnector for CustomConnector {
    fn venue_id(&self) -> VenueId {
        VenueId::Custom
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.capabilities
    }

    fn product(&self) -> VenueProduct {
        // Our custom exchange is a spot order book.
        VenueProduct::Spot
    }

    async fn subscribe(
        &self,
        symbols: &[String],
    ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>> {
        let ws = ExchangeWsClient::new(&self.ws_url);
        let subscriptions: Vec<String> = symbols
            .iter()
            .flat_map(|s| {
                vec![
                    format!("orderbook.{s}"),
                    format!("trade.{s}"),
                    "orders".to_string(),
                    "fills".to_string(),
                ]
            })
            .collect();
        let ws_rx = ws.connect(subscriptions).await?;

        // Convert WsEvent → MarketEvent in a bridge task.
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut ws_rx = ws_rx;
            while let Some(evt) = ws_rx.recv().await {
                let market_evt = match evt {
                    crate::ws::WsEvent::BookSnapshot {
                        symbol,
                        bids,
                        asks,
                        sequence,
                    } => MarketEvent::BookSnapshot {
                        venue: VenueId::Custom,
                        symbol,
                        bids,
                        asks,
                        sequence,
                    },
                    crate::ws::WsEvent::BookDelta {
                        symbol,
                        bids,
                        asks,
                        sequence,
                    } => MarketEvent::BookDelta {
                        venue: VenueId::Custom,
                        symbol,
                        bids,
                        asks,
                        sequence,
                    },
                    crate::ws::WsEvent::Trade(trade) => MarketEvent::Trade {
                        venue: VenueId::Custom,
                        trade,
                    },
                    crate::ws::WsEvent::FillUpdate(fill) => MarketEvent::Fill {
                        venue: VenueId::Custom,
                        fill,
                    },
                    crate::ws::WsEvent::Connected => MarketEvent::Connected {
                        venue: VenueId::Custom,
                    },
                    crate::ws::WsEvent::Disconnected => MarketEvent::Disconnected {
                        venue: VenueId::Custom,
                    },
                    crate::ws::WsEvent::OrderUpdate { .. } => continue,
                };
                if tx.send(market_evt).is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }

    async fn get_orderbook(
        &self,
        symbol: &str,
        depth: u32,
    ) -> anyhow::Result<(Vec<PriceLevel>, Vec<PriceLevel>, u64)> {
        self.rest.get_orderbook(symbol, depth).await
    }

    async fn place_order(&self, order: &NewOrder) -> anyhow::Result<OrderId> {
        let req = PlaceOrderRequest {
            symbol: order.symbol.clone(),
            side: order.side,
            order_type: order.order_type,
            price: order.price,
            qty: order.qty,
            time_in_force: order.time_in_force,
        };
        let resp = self.rest.place_order(&req).await?;
        debug!(order_id = %resp.order_id, "custom exchange order placed");
        Ok(resp.order_id)
    }

    async fn place_orders_batch(&self, orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>> {
        // Custom exchange has no batch endpoint — place one by one.
        let mut ids = Vec::with_capacity(orders.len());
        for order in orders {
            ids.push(self.place_order(order).await?);
        }
        Ok(ids)
    }

    async fn cancel_order(&self, _symbol: &str, order_id: OrderId) -> anyhow::Result<()> {
        self.rest.cancel_order(order_id).await?;
        Ok(())
    }

    async fn cancel_orders_batch(&self, symbol: &str, order_ids: &[OrderId]) -> anyhow::Result<()> {
        for &oid in order_ids {
            let _ = self.cancel_order(symbol, oid).await;
        }
        Ok(())
    }

    async fn cancel_all_orders(&self, symbol: &str) -> anyhow::Result<()> {
        // Custom exchange: cancel each live order individually.
        let orders = self.get_open_orders(symbol).await?;
        for o in orders {
            let _ = self.cancel_order(symbol, o.order_id).await;
        }
        Ok(())
    }

    async fn get_open_orders(&self, _symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        // Custom exchange doesn't have a get_open_orders REST endpoint;
        // the engine tracks live orders internally via OrderManager.
        Ok(vec![])
    }

    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        self.rest.get_balances().await
    }

    async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec> {
        // Custom exchange: product specs come from config, not API.
        anyhow::bail!(
            "custom exchange does not expose product specs via API — use config for {symbol}"
        )
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        self.rest.health().await
    }
}
