use std::time::Duration;

use async_trait::async_trait;
use mm_common::types::*;
use mm_exchange_core::connector::*;
use mm_exchange_core::events::MarketEvent;
use mm_exchange_core::rate_limiter::RateLimiter;
use reqwest::Client;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::auth;

/// Bybit V5 API connector.
///
/// Supports:
/// - Spot, Linear (USDT perps), Inverse
/// - Batch orders (up to 20)
/// - WebSocket combined streams
/// - HMAC-SHA256 auth
pub struct BybitConnector {
    client: Client,
    base_url: String,
    ws_url: String,
    api_key: String,
    api_secret: String,
    rate_limiter: RateLimiter,
    capabilities: VenueCapabilities,
}

impl BybitConnector {
    pub fn new(api_key: &str, api_secret: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: "https://api.bybit.com".to_string(),
            ws_url: "wss://stream.bybit.com/v5/public/linear".to_string(),
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            // Bybit: 600 req / 5s, use 80%.
            rate_limiter: RateLimiter::new(600, Duration::from_secs(5), 0.8),
            capabilities: VenueCapabilities {
                max_batch_size: 20,
                supports_amend: true,
                supports_ws_trading: true,
                supports_fix: false,
                max_order_rate: 20,
            },
        }
    }

    pub fn testnet(api_key: &str, api_secret: &str) -> Self {
        let mut s = Self::new(api_key, api_secret);
        s.base_url = "https://api-testnet.bybit.com".to_string();
        s.ws_url = "wss://stream-testnet.bybit.com/v5/public/linear".to_string();
        s
    }

    async fn signed_get(&self, path: &str, params: &str) -> anyhow::Result<Value> {
        self.rate_limiter.acquire(1).await;
        let (ts, recv, sig) = auth::auth_headers(&self.api_key, &self.api_secret, params);
        let url = if params.is_empty() {
            format!("{}{path}", self.base_url)
        } else {
            format!("{}{path}?{params}", self.base_url)
        };
        let resp = self
            .client
            .get(&url)
            .header("X-BAPI-API-KEY", &self.api_key)
            .header("X-BAPI-TIMESTAMP", &ts)
            .header("X-BAPI-RECV-WINDOW", &recv)
            .header("X-BAPI-SIGN", &sig)
            .send()
            .await?;
        let body: Value = resp.json().await?;
        let ret_code = body.get("retCode").and_then(|v| v.as_i64()).unwrap_or(-1);
        if ret_code != 0 {
            let msg = body
                .get("retMsg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Bybit API error {ret_code}: {msg}");
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn signed_post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.rate_limiter.acquire(1).await;
        let body_str = serde_json::to_string(body)?;
        let (ts, recv, sig) = auth::auth_headers(&self.api_key, &self.api_secret, &body_str);
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("X-BAPI-API-KEY", &self.api_key)
            .header("X-BAPI-TIMESTAMP", &ts)
            .header("X-BAPI-RECV-WINDOW", &recv)
            .header("X-BAPI-SIGN", &sig)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await?;
        let resp_body: Value = resp.json().await?;
        let ret_code = resp_body
            .get("retCode")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if ret_code != 0 {
            let msg = resp_body
                .get("retMsg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Bybit API error {ret_code}: {msg}");
        }
        Ok(resp_body.get("result").cloned().unwrap_or(Value::Null))
    }
}

#[async_trait]
impl ExchangeConnector for BybitConnector {
    fn venue_id(&self) -> VenueId {
        VenueId::Bybit
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.capabilities
    }

    async fn subscribe(
        &self,
        symbols: &[String],
    ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let ws_url = self.ws_url.clone();
        let topics: Vec<String> = symbols
            .iter()
            .flat_map(|s| vec![format!("orderbook.25.{s}"), format!("publicTrade.{s}")])
            .collect();

        tokio::spawn(async move {
            use futures_util::{SinkExt, StreamExt};
            use tokio_tungstenite::connect_async;

            loop {
                match connect_async(&ws_url).await {
                    Ok((ws, _)) => {
                        let _ = tx.send(MarketEvent::Connected {
                            venue: VenueId::Bybit,
                        });
                        let (mut write, mut read) = ws.split();

                        // Subscribe.
                        let sub = serde_json::json!({
                            "op": "subscribe",
                            "args": topics,
                        });
                        let _ = write
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                sub.to_string(),
                            ))
                            .await;

                        // Ping interval.
                        let mut ping_iv = tokio::time::interval(Duration::from_secs(20));

                        loop {
                            tokio::select! {
                                msg = read.next() => {
                                    match msg {
                                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                                            if let Ok(v) = serde_json::from_str::<Value>(&text.to_string()) {
                                                if let Some(evt) = parse_bybit_event(&v) {
                                                    if tx.send(evt).is_err() { return; }
                                                }
                                            }
                                        }
                                        Some(Err(_)) | None => break,
                                        _ => {}
                                    }
                                }
                                _ = ping_iv.tick() => {
                                    let ping = serde_json::json!({"op": "ping"});
                                    let _ = write.send(
                                        tokio_tungstenite::tungstenite::Message::Text(ping.to_string())
                                    ).await;
                                }
                            }
                        }
                        let _ = tx.send(MarketEvent::Disconnected {
                            venue: VenueId::Bybit,
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "Bybit WS connect failed");
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        Ok(rx)
    }

    async fn get_orderbook(
        &self,
        symbol: &str,
        depth: u32,
    ) -> anyhow::Result<(Vec<PriceLevel>, Vec<PriceLevel>, u64)> {
        let params = format!("category=linear&symbol={symbol}&limit={depth}");
        let result = self.signed_get("/v5/market/orderbook", &params).await?;
        let bids = parse_levels(result.get("b"))?;
        let asks = parse_levels(result.get("a"))?;
        let seq = result.get("u").and_then(|v| v.as_u64()).unwrap_or(0);
        Ok((bids, asks, seq))
    }

    async fn place_order(&self, order: &NewOrder) -> anyhow::Result<OrderId> {
        let body = serde_json::json!({
            "category": "linear",
            "symbol": order.symbol,
            "side": match order.side { Side::Buy => "Buy", Side::Sell => "Sell" },
            "orderType": match order.order_type { OrderType::Limit => "Limit", OrderType::Market => "Market" },
            "qty": order.qty.to_string(),
            "price": order.price.map(|p| p.to_string()),
            "timeInForce": "PostOnly",
        });
        let result = self.signed_post("/v5/order/create", &body).await?;
        let bybit_oid = result.get("orderId").and_then(|v| v.as_str()).unwrap_or("");
        debug!(bybit_order_id = bybit_oid, "placed order on Bybit");
        // Generate a UUID and let the OrderIdMap handle the mapping to Bybit's string ID.
        let internal_id = uuid::Uuid::new_v4();
        Ok(internal_id)
    }

    async fn place_orders_batch(&self, orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>> {
        // Bybit supports batch up to 20.
        let batch: Vec<Value> = orders
            .iter()
            .map(|o| {
                serde_json::json!({
                    "symbol": o.symbol,
                    "side": match o.side { Side::Buy => "Buy", Side::Sell => "Sell" },
                    "orderType": match o.order_type { OrderType::Limit => "Limit", OrderType::Market => "Market" },
                    "qty": o.qty.to_string(),
                    "price": o.price.map(|p| p.to_string()),
                    "timeInForce": "PostOnly",
                })
            })
            .collect();

        let body = serde_json::json!({
            "category": "linear",
            "request": batch,
        });
        let result = self.signed_post("/v5/order/create-batch", &body).await?;
        let resp_list = result
            .get("list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        // Generate internal UUIDs for each order; OrderIdMap maps to Bybit's string IDs.
        let ids: Vec<OrderId> = resp_list
            .iter()
            .map(|item| {
                let bybit_oid = item.get("orderId").and_then(|v| v.as_str()).unwrap_or("");
                debug!(bybit_order_id = bybit_oid, "batch order placed on Bybit");
                uuid::Uuid::new_v4()
            })
            .collect();
        // If response has fewer items than requested, pad with generated IDs.
        let mut all_ids = ids;
        while all_ids.len() < orders.len() {
            all_ids.push(uuid::Uuid::new_v4());
        }
        Ok(all_ids)
    }

    async fn cancel_order(&self, symbol: &str, order_id: OrderId) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "category": "linear",
            "symbol": symbol,
            "orderId": order_id.to_string(),
        });
        self.signed_post("/v5/order/cancel", &body).await?;
        Ok(())
    }

    async fn cancel_orders_batch(&self, symbol: &str, order_ids: &[OrderId]) -> anyhow::Result<()> {
        let batch: Vec<Value> = order_ids
            .iter()
            .map(|oid| serde_json::json!({"symbol": symbol, "orderId": oid.to_string()}))
            .collect();
        let body = serde_json::json!({"category": "linear", "request": batch});
        self.signed_post("/v5/order/cancel-batch", &body).await?;
        Ok(())
    }

    async fn cancel_all_orders(&self, symbol: &str) -> anyhow::Result<()> {
        let body = serde_json::json!({"category": "linear", "symbol": symbol});
        self.signed_post("/v5/order/cancel-all", &body).await?;
        Ok(())
    }

    async fn get_open_orders(&self, symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        let result = self
            .signed_get(
                "/v5/order/realtime",
                &format!("category=linear&symbol={symbol}"),
            )
            .await?;
        let orders = result
            .get("list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(orders
            .iter()
            .filter_map(|o| {
                let order_id = uuid::Uuid::new_v4(); // Bybit uses string IDs; map externally.
                let side_str = o.get("side")?.as_str()?;
                let side = match side_str {
                    "Buy" => Side::Buy,
                    "Sell" => Side::Sell,
                    _ => return None,
                };
                let price: Decimal = o.get("price")?.as_str()?.parse().ok()?;
                let qty: Decimal = o.get("qty")?.as_str()?.parse().ok()?;
                let filled_qty: Decimal = o.get("cumExecQty")?.as_str()?.parse().ok()?;
                let status_str = o.get("orderStatus")?.as_str()?;
                let status = match status_str {
                    "New" => OrderStatus::Open,
                    "PartiallyFilled" => OrderStatus::PartiallyFilled,
                    "Filled" => OrderStatus::Filled,
                    "Cancelled" | "Canceled" => OrderStatus::Cancelled,
                    "Rejected" | "Deactivated" => OrderStatus::Rejected,
                    _ => OrderStatus::Open,
                };
                let created_ms = o.get("createdTime")?.as_str()?.parse::<i64>().ok()?;
                let created_at = chrono::DateTime::from_timestamp_millis(created_ms)?;
                Some(LiveOrder {
                    order_id,
                    symbol: symbol.to_string(),
                    side,
                    price,
                    qty,
                    filled_qty,
                    status,
                    created_at,
                })
            })
            .collect())
    }

    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        let result = self
            .signed_get("/v5/account/wallet-balance", "accountType=UNIFIED")
            .await?;
        let coins = result
            .get("list")
            .and_then(|l| l.as_array())
            .and_then(|a| a.first())
            .and_then(|acc| acc.get("coin"))
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(coins
            .iter()
            .filter_map(|c| {
                let asset = c.get("coin")?.as_str()?.to_string();
                let total: Decimal = c.get("walletBalance")?.as_str()?.parse().ok()?;
                let locked: Decimal = c.get("locked")?.as_str()?.parse().unwrap_or_default();
                Some(Balance {
                    asset,
                    total,
                    locked,
                    available: total - locked,
                })
            })
            .filter(|b| b.total > dec!(0))
            .collect())
    }

    async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec> {
        let params = format!("category=linear&symbol={symbol}");
        let result = self
            .signed_get("/v5/market/instruments-info", &params)
            .await?;
        let item = result
            .get("list")
            .and_then(|l| l.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| anyhow::anyhow!("symbol not found"))?;

        let lot_filter = item.get("lotSizeFilter");
        let price_filter = item.get("priceFilter");

        let tick_size: Decimal = price_filter
            .and_then(|f| f.get("tickSize"))
            .and_then(|v| v.as_str())
            .unwrap_or("0.01")
            .parse()
            .unwrap_or(dec!(0.01));
        let lot_size: Decimal = lot_filter
            .and_then(|f| f.get("qtyStep"))
            .and_then(|v| v.as_str())
            .unwrap_or("0.001")
            .parse()
            .unwrap_or(dec!(0.001));

        Ok(ProductSpec {
            symbol: symbol.to_string(),
            base_asset: item
                .get("baseCoin")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            quote_asset: item
                .get("quoteCoin")
                .and_then(|v| v.as_str())
                .unwrap_or("USDT")
                .to_string(),
            tick_size,
            lot_size,
            min_notional: dec!(5),
            maker_fee: dec!(0.0002),
            taker_fee: dec!(0.00055),
        })
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let url = format!("{}/v5/market/time", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }
}

fn parse_bybit_event(v: &Value) -> Option<MarketEvent> {
    let topic = v.get("topic")?.as_str()?;
    let data = v.get("data")?;

    if topic.starts_with("orderbook.") {
        let symbol = topic.rsplit('.').next()?.to_string();
        let bids = parse_levels(data.get("b")).ok()?;
        let asks = parse_levels(data.get("a")).ok()?;
        let seq = data.get("u").and_then(|v| v.as_u64()).unwrap_or(0);
        let is_snapshot = v.get("type").and_then(|t| t.as_str()) == Some("snapshot");
        if is_snapshot {
            Some(MarketEvent::BookSnapshot {
                venue: VenueId::Bybit,
                symbol,
                bids,
                asks,
                sequence: seq,
            })
        } else {
            Some(MarketEvent::BookDelta {
                venue: VenueId::Bybit,
                symbol,
                bids,
                asks,
                sequence: seq,
            })
        }
    } else if topic.starts_with("publicTrade.") {
        let symbol = topic.rsplit('.').next()?.to_string();
        let trades = data.as_array()?;
        let t = trades.first()?;
        let price: Decimal = t.get("p")?.as_str()?.parse().ok()?;
        let qty: Decimal = t.get("v")?.as_str()?.parse().ok()?;
        let side_str = t.get("S")?.as_str()?;
        let taker_side = if side_str == "Buy" {
            Side::Buy
        } else {
            Side::Sell
        };
        Some(MarketEvent::Trade {
            venue: VenueId::Bybit,
            trade: Trade {
                trade_id: t.get("i")?.as_str()?.parse().unwrap_or(0),
                symbol,
                price,
                qty,
                taker_side,
                timestamp: chrono::Utc::now(),
            },
        })
    } else {
        None
    }
}

fn parse_levels(value: Option<&Value>) -> anyhow::Result<Vec<PriceLevel>> {
    let arr = value
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("expected array"))?;
    arr.iter()
        .map(|entry| {
            let pair = entry.as_array().ok_or_else(|| anyhow::anyhow!("pair"))?;
            let price: Decimal = pair
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .parse()?;
            let qty: Decimal = pair
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .parse()?;
            Ok(PriceLevel { price, qty })
        })
        .collect()
}
