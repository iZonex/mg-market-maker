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
use tracing::{debug, info, warn};

use crate::auth;

/// Binance Spot + Futures connector implementing ExchangeConnector.
pub struct BinanceConnector {
    client: Client,
    base_url: String,
    ws_url: String,
    api_key: String,
    api_secret: String,
    rate_limiter: RateLimiter,
    capabilities: VenueCapabilities,
}

impl BinanceConnector {
    pub fn new(base_url: &str, ws_url: &str, api_key: &str, api_secret: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            ws_url: ws_url.to_string(),
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            // Binance: 1200 weight / minute, use 80%.
            rate_limiter: RateLimiter::new(1200, Duration::from_secs(60), 0.8),
            capabilities: VenueCapabilities {
                max_batch_size: 5,
                supports_amend: true,
                supports_ws_trading: true,
                supports_fix: true,
                max_order_rate: 300, // per 10s.
            },
        }
    }

    /// Testnet constructor.
    pub fn testnet(api_key: &str, api_secret: &str) -> Self {
        Self::new(
            "https://testnet.binance.vision",
            "wss://testnet.binance.vision/ws",
            api_key,
            api_secret,
        )
    }

    async fn signed_get(&self, path: &str, params: &str) -> anyhow::Result<Value> {
        self.rate_limiter.acquire(1).await;
        let query = auth::signed_query(&self.api_secret, params);
        let url = format!("{}{path}?{query}", self.base_url);
        let resp = self
            .client
            .get(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("Binance API error {status}: {body}");
        }
        Ok(body)
    }

    async fn signed_post(&self, path: &str, params: &str) -> anyhow::Result<Value> {
        self.rate_limiter.acquire(1).await;
        let query = auth::signed_query(&self.api_secret, params);
        let url = format!("{}{path}?{query}", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("Binance API error {status}: {body}");
        }
        Ok(body)
    }

    async fn signed_delete(&self, path: &str, params: &str) -> anyhow::Result<Value> {
        self.rate_limiter.acquire(1).await;
        let query = auth::signed_query(&self.api_secret, params);
        let url = format!("{}{path}?{query}", self.base_url);
        let resp = self
            .client
            .delete(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("Binance API error {status}: {body}");
        }
        Ok(body)
    }
}

#[async_trait]
impl ExchangeConnector for BinanceConnector {
    fn venue_id(&self) -> VenueId {
        VenueId::Binance
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.capabilities
    }

    async fn subscribe(
        &self,
        symbols: &[String],
    ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();

        // Build combined stream URL.
        let streams: Vec<String> = symbols
            .iter()
            .flat_map(|s| {
                let lower = s.to_lowercase();
                vec![format!("{lower}@depth20@100ms"), format!("{lower}@trade")]
            })
            .collect();
        let stream_param = streams.join("/");
        let url = format!("{}/stream?streams={}", self.ws_url, stream_param);

        info!(url = %url, "subscribing to Binance streams");

        tokio::spawn(async move {
            use futures_util::StreamExt;
            use tokio_tungstenite::connect_async;

            loop {
                match connect_async(&url).await {
                    Ok((ws, _)) => {
                        let _ = tx.send(MarketEvent::Connected {
                            venue: VenueId::Binance,
                        });
                        let (_, mut read) = ws.split();
                        while let Some(Ok(msg)) = read.next().await {
                            if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                                // Parse Binance combined stream format.
                                if let Ok(v) = serde_json::from_str::<Value>(&text.to_string()) {
                                    let stream = v.get("stream").and_then(|s| s.as_str());
                                    let data = v.get("data");
                                    if let (Some(stream), Some(data)) = (stream, data) {
                                        if let Some(evt) = parse_binance_event(stream, data) {
                                            if tx.send(evt).is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let _ = tx.send(MarketEvent::Disconnected {
                            venue: VenueId::Binance,
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "Binance WS connect failed");
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
        self.rate_limiter.acquire(5).await; // depth costs 5 weight.
        let url = format!(
            "{}/api/v3/depth?symbol={}&limit={}",
            self.base_url, symbol, depth
        );
        let resp: Value = self.client.get(&url).send().await?.json().await?;

        let bids = parse_levels(resp.get("bids"))?;
        let asks = parse_levels(resp.get("asks"))?;
        let seq = resp
            .get("lastUpdateId")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Ok((bids, asks, seq))
    }

    async fn place_order(&self, order: &NewOrder) -> anyhow::Result<OrderId> {
        let side = match order.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };
        let order_type = match order.order_type {
            OrderType::Limit => "LIMIT",
            OrderType::Market => "MARKET",
        };
        let mut params = format!(
            "symbol={}&side={}&type={}&quantity={}",
            order.symbol, side, order_type, order.qty
        );
        if let Some(price) = &order.price {
            params.push_str(&format!("&price={price}"));
        }
        if let Some(tif) = &order.time_in_force {
            let tif_str = match tif {
                TimeInForce::Gtc => "GTC",
                TimeInForce::Ioc => "IOC",
                TimeInForce::Fok => "FOK",
                TimeInForce::PostOnly => "GTC", // Binance doesn't have PostOnly via this param.
            };
            params.push_str(&format!("&timeInForce={tif_str}"));
        }
        if let Some(coid) = &order.client_order_id {
            params.push_str(&format!("&newClientOrderId={coid}"));
        }

        let resp = self.signed_post("/api/v3/order", &params).await?;
        let order_id_str = resp.get("orderId").and_then(|v| v.as_u64()).unwrap_or(0);
        // Binance uses numeric IDs — wrap in UUID v4 for our tracking.
        let order_id = uuid::Uuid::new_v4();
        debug!(%order_id, binance_id = order_id_str, "placed order on Binance");
        Ok(order_id)
    }

    async fn place_orders_batch(&self, orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>> {
        // Binance spot doesn't have batch — loop individually.
        // For futures, use /fapi/v1/batchOrders.
        let mut ids = Vec::with_capacity(orders.len());
        for order in orders {
            ids.push(self.place_order(order).await?);
        }
        Ok(ids)
    }

    async fn cancel_order(&self, symbol: &str, order_id: OrderId) -> anyhow::Result<()> {
        let params = format!("symbol={symbol}&origClientOrderId={order_id}");
        self.signed_delete("/api/v3/order", &params).await?;
        Ok(())
    }

    async fn cancel_orders_batch(&self, symbol: &str, order_ids: &[OrderId]) -> anyhow::Result<()> {
        for oid in order_ids {
            let _ = self.cancel_order(symbol, *oid).await;
        }
        Ok(())
    }

    async fn cancel_all_orders(&self, symbol: &str) -> anyhow::Result<()> {
        let params = format!("symbol={symbol}");
        self.signed_delete("/api/v3/openOrders", &params).await?;
        Ok(())
    }

    async fn get_open_orders(&self, symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        let _resp = self
            .signed_get("/api/v3/openOrders", &format!("symbol={symbol}"))
            .await?;
        // TODO: parse full response into LiveOrder vec.
        Ok(vec![])
    }

    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        let resp = self.signed_get("/api/v3/account", "").await?;
        let balances = resp
            .get("balances")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(balances
            .iter()
            .filter_map(|b| {
                let asset = b.get("asset")?.as_str()?;
                let free: Decimal = b.get("free")?.as_str()?.parse().ok()?;
                let locked: Decimal = b.get("locked")?.as_str()?.parse().ok()?;
                Some(Balance {
                    asset: asset.to_string(),
                    total: free + locked,
                    locked,
                    available: free,
                })
            })
            .filter(|b| b.total > dec!(0))
            .collect())
    }

    async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec> {
        self.rate_limiter.acquire(10).await;
        let url = format!("{}/api/v3/exchangeInfo?symbol={}", self.base_url, symbol);
        let resp: Value = self.client.get(&url).send().await?.json().await?;

        let sym = resp
            .get("symbols")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| anyhow::anyhow!("symbol not found"))?;

        let base = sym
            .get("baseAsset")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let quote = sym
            .get("quoteAsset")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Extract filters.
        let filters = sym
            .get("filters")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut tick_size = dec!(0.01);
        let mut lot_size = dec!(0.00001);
        let mut min_notional = dec!(10);

        for f in &filters {
            match f.get("filterType").and_then(|v| v.as_str()) {
                Some("PRICE_FILTER") => {
                    if let Some(ts) = f.get("tickSize").and_then(|v| v.as_str()) {
                        tick_size = ts.parse().unwrap_or(tick_size);
                    }
                }
                Some("LOT_SIZE") => {
                    if let Some(ss) = f.get("stepSize").and_then(|v| v.as_str()) {
                        lot_size = ss.parse().unwrap_or(lot_size);
                    }
                }
                Some("NOTIONAL" | "MIN_NOTIONAL") => {
                    if let Some(mn) = f.get("minNotional").and_then(|v| v.as_str()) {
                        min_notional = mn.parse().unwrap_or(min_notional);
                    }
                }
                _ => {}
            }
        }

        Ok(ProductSpec {
            symbol: symbol.to_string(),
            base_asset: base,
            quote_asset: quote,
            tick_size,
            lot_size,
            min_notional,
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.001),
        })
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let url = format!("{}/api/v3/ping", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }
}

fn parse_binance_event(stream: &str, data: &Value) -> Option<MarketEvent> {
    if stream.ends_with("@depth20@100ms") {
        let symbol = stream.split('@').next()?.to_uppercase();
        let bids = parse_levels(data.get("bids")).ok()?;
        let asks = parse_levels(data.get("asks")).ok()?;
        let seq = data
            .get("lastUpdateId")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Some(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol,
            bids,
            asks,
            sequence: seq,
        })
    } else if stream.ends_with("@trade") {
        let symbol = stream.split('@').next()?.to_uppercase();
        let price: Decimal = data.get("p")?.as_str()?.parse().ok()?;
        let qty: Decimal = data.get("q")?.as_str()?.parse().ok()?;
        let is_buyer_maker = data.get("m")?.as_bool()?;
        let taker_side = if is_buyer_maker {
            Side::Sell
        } else {
            Side::Buy
        };
        Some(MarketEvent::Trade {
            venue: VenueId::Binance,
            trade: Trade {
                trade_id: data.get("t").and_then(|v| v.as_u64()).unwrap_or(0),
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
            let pair = entry
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("expected pair"))?;
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
