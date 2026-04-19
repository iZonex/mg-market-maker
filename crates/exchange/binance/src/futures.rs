//! Binance USDⓈ-M futures connector.
//!
//! Parallel struct to [`BinanceConnector`](crate::connector::BinanceConnector)
//! that targets `/fapi/v1/*` instead of `/api/v3/*`. Reuses `auth.rs`
//! because the HMAC-SHA256 signing scheme is identical between spot
//! and futures; only the URLs and the endpoint shapes differ.
//!
//! Scope:
//!
//! - USDⓈ-margined perps (`BTCUSDT`, `ETHUSDT`, …)
//! - REST order entry, open-orders, balances, exchange info
//! - Native batch orders via `/fapi/v1/batchOrders`
//! - Funding rate read via `/fapi/v1/premiumIndex`
//! - Public depth + trade WebSocket streams on `fstream.binance.com`
//! - One-way position mode (hedge mode deferred until needed)
//!
//! Out of scope for Sprint D:
//!
//! - WS API order entry (separate `BinanceFuturesWsTrader` adapter)
//! - Listen-key user data stream (Sprint F closes this gap for
//!   both spot and futures)
//! - COIN-M futures (`/dapi/v1/*` — different base URL, separate
//!   epic when needed)

use std::time::{Duration, Instant};

use async_trait::async_trait;
use mm_common::types::*;
use mm_exchange_core::connector::*;
use mm_exchange_core::events::MarketEvent;
use mm_exchange_core::metrics::ORDER_ENTRY_LATENCY;
use mm_exchange_core::rate_limiter::RateLimiter;
use reqwest::Client;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::auth;

/// Binance USDⓈ-M futures connector.
pub struct BinanceFuturesConnector {
    client: Client,
    base_url: String,
    ws_url: String,
    api_key: String,
    api_secret: String,
    rate_limiter: RateLimiter,
    capabilities: VenueCapabilities,
}

impl BinanceFuturesConnector {
    pub fn new(api_key: &str, api_secret: &str) -> Self {
        Self::with_urls(
            "https://fapi.binance.com",
            "wss://fstream.binance.com/ws",
            api_key,
            api_secret,
        )
    }

    pub fn testnet(api_key: &str, api_secret: &str) -> Self {
        Self::with_urls(
            "https://testnet.binancefuture.com",
            "wss://stream.binancefuture.com/ws",
            api_key,
            api_secret,
        )
    }

    fn with_urls(base_url: &str, ws_url: &str, api_key: &str, api_secret: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            ws_url: ws_url.to_string(),
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            // Binance USDⓈ-M: 2400 weight / min, 80% budget.
            rate_limiter: RateLimiter::new(2400, Duration::from_secs(60), 0.8),
            capabilities: VenueCapabilities {
                max_batch_size: 5,
                supports_amend: true,
                supports_ws_trading: false, // adapter + listen-key in later sprints
                supports_fix: false,
                max_order_rate: 300,
                supports_funding_rate: true,
                supports_margin_info: true,
                supports_margin_mode: true,
                // R5.5 — Binance USDⓈ-M `!forceOrder@arr`
                // all-market liquidation stream; single WS
                // topic, no per-symbol fan-out.
                supports_liquidation_feed: true,
                // `/fapi/v1/leverage` accepts per-symbol leverage
                // up to the venue's risk-limit cap.
                supports_set_leverage: true,
            },
        }
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
            anyhow::bail!("Binance futures API error {status}: {body}");
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
            anyhow::bail!("Binance futures API error {status}: {body}");
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
            anyhow::bail!("Binance futures API error {status}: {body}");
        }
        Ok(body)
    }

    async fn signed_put(&self, path: &str, params: &str) -> anyhow::Result<Value> {
        self.rate_limiter.acquire(1).await;
        let query = auth::signed_query(&self.api_secret, params);
        let url = format!("{}{path}?{query}", self.base_url);
        let resp = self
            .client
            .put(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("Binance futures API error {status}: {body}");
        }
        Ok(body)
    }
}

#[async_trait]
impl ExchangeConnector for BinanceFuturesConnector {
    fn venue_id(&self) -> VenueId {
        VenueId::Binance
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.capabilities
    }

    fn classify_error(&self, err: &anyhow::Error) -> mm_exchange_core::VenueError {
        crate::classify(err)
    }

    fn product(&self) -> VenueProduct {
        // USDⓈ-M perpetual futures.
        VenueProduct::LinearPerp
    }

    async fn get_open_interest(
        &self,
        symbol: &str,
    ) -> anyhow::Result<Option<mm_exchange_core::connector::OpenInterestInfo>> {
        // R6.3 — `/fapi/v1/openInterest?symbol=BTCUSDT` returns
        // `{"openInterest":"123.456","symbol":"BTCUSDT","time":1700000000000}`.
        self.rate_limiter.acquire(1).await;
        let url = format!("{}/fapi/v1/openInterest?symbol={symbol}", self.base_url);
        let body: Value = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;
        let contracts_s = body.get("openInterest").and_then(|v| v.as_str());
        let contracts = contracts_s.and_then(|s| s.parse::<Decimal>().ok());
        let ts_ms = body.get("time").and_then(|v| v.as_i64()).unwrap_or_else(|| {
            chrono::Utc::now().timestamp_millis()
        });
        let timestamp = chrono::DateTime::from_timestamp_millis(ts_ms)
            .unwrap_or_else(chrono::Utc::now);
        Ok(contracts.map(|c| mm_exchange_core::connector::OpenInterestInfo {
            symbol: symbol.to_string(),
            oi_contracts: Some(c),
            oi_usd: None,
            timestamp,
        }))
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate, FundingRateError> {
        // `/fapi/v1/premiumIndex` returns current mark price,
        // funding rate, and next funding time for the symbol.
        self.rate_limiter.acquire(1).await;
        let url = format!("{}/fapi/v1/premiumIndex?symbol={symbol}", self.base_url);
        let body: Value = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| FundingRateError::Other(anyhow::anyhow!(e)))?
            .json()
            .await
            .map_err(|e| FundingRateError::Other(anyhow::anyhow!(e)))?;

        let rate_str = body
            .get("lastFundingRate")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FundingRateError::Other(anyhow::anyhow!("missing lastFundingRate")))?;
        let rate: Decimal = rate_str
            .parse()
            .map_err(|e| FundingRateError::Other(anyhow::anyhow!("bad rate: {e}")))?;
        let next_ms = body
            .get("nextFundingTime")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| FundingRateError::Other(anyhow::anyhow!("missing nextFundingTime")))?;
        let next_funding_time =
            chrono::DateTime::from_timestamp_millis(next_ms).ok_or_else(|| {
                FundingRateError::Other(anyhow::anyhow!("bad nextFundingTime: {next_ms}"))
            })?;

        Ok(FundingRate {
            rate,
            next_funding_time,
            // Binance USDⓈ-M funding cadence is 8h on most symbols.
            // Exceptions exist (1h or 4h on a handful of contracts);
            // the authoritative value is in the `/fapi/v1/fundingInfo`
            // endpoint, which we read lazily only if the downstream
            // strategy asks for it. For now, report the common case.
            interval: Duration::from_secs(8 * 3600),
        })
    }

    async fn subscribe(
        &self,
        symbols: &[String],
    ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();

        // Depth20 + trade per symbol, combined stream.
        // R5.1 — also subscribe to `!forceOrder@arr` once
        // globally so the engine's LiquidationHeatmap starts
        // receiving real Binance USDⓈ-M liquidation events.
        // Single topic (all-market); the parser tags each event
        // with its own symbol so multi-symbol engines all see
        // the same stream.
        let mut streams: Vec<String> = symbols
            .iter()
            .flat_map(|s| {
                let lower = s.to_lowercase();
                vec![format!("{lower}@depth20@100ms"), format!("{lower}@trade")]
            })
            .collect();
        streams.push("!forceOrder@arr".to_string());
        let stream_param = streams.join("/");
        // Combined-stream endpoint pattern:
        //   wss://fstream.binance.com/stream?streams=<s1>/<s2>/…
        let host = self
            .ws_url
            .trim_end_matches("/ws")
            .trim_end_matches("/stream");
        let url = format!("{host}/stream?streams={stream_param}");

        info!(url = %url, "subscribing to Binance futures streams");

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
                                if let Ok(v) = serde_json::from_str::<Value>(&text.to_string()) {
                                    let stream = v.get("stream").and_then(|s| s.as_str());
                                    let data = v.get("data");
                                    if let (Some(stream), Some(data)) = (stream, data) {
                                        if let Some(evt) =
                                            super::connector::parse_binance_event(stream, data)
                                        {
                                            // Parser tags events with
                                            // VenueId::Binance regardless of
                                            // product — that's fine because
                                            // the engine routes on symbol.
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
                        warn!(error = %e, "Binance futures WS connect failed");
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
        self.rate_limiter.acquire(5).await;
        // Futures accepts 5, 10, 20, 50, 100, 500, 1000 — clamp.
        let clamped = if depth <= 5 {
            5
        } else if depth <= 10 {
            10
        } else if depth <= 20 {
            20
        } else if depth <= 50 {
            50
        } else if depth <= 100 {
            100
        } else if depth <= 500 {
            500
        } else {
            1000
        };
        let url = format!(
            "{}/fapi/v1/depth?symbol={}&limit={}",
            self.base_url, symbol, clamped
        );
        let resp: Value = self.client.get(&url).send().await?.json().await?;
        let bids = super::connector::parse_levels(resp.get("bids"))?;
        let asks = super::connector::parse_levels(resp.get("asks"))?;
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
        if order.order_type == OrderType::Limit {
            let tif = match order.time_in_force {
                Some(TimeInForce::Gtc) | Some(TimeInForce::Gtd) | None => "GTC",
                Some(TimeInForce::PostOnly) => "GTX", // Binance futures post-only
                Some(TimeInForce::Ioc) => "IOC",
                Some(TimeInForce::Fok) => "FOK",
                Some(TimeInForce::Day) => "GTC",
            };
            params.push_str(&format!("&timeInForce={tif}"));
        }
        let cloid = order
            .client_order_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        params.push_str(&format!("&newClientOrderId={cloid}"));

        let t0 = Instant::now();
        let rest_result = self.signed_post("/fapi/v1/order", &params).await;
        ORDER_ENTRY_LATENCY
            .with_label_values(&["binance_futures", "rest", "place_order"])
            .observe(t0.elapsed().as_secs_f64());
        let resp = rest_result?;

        let binance_id = resp.get("orderId").and_then(|v| v.as_u64()).unwrap_or(0);
        let internal = uuid::Uuid::new_v4();
        debug!(%internal, binance_id, "placed order on Binance futures");
        Ok(internal)
    }

    async fn place_orders_batch(&self, orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>> {
        // Native `/fapi/v1/batchOrders` supports up to 5 orders per
        // request. For larger batches the caller must chunk. We ship
        // up to 5 here and let the engine's diff layer handle
        // anything bigger.
        if orders.is_empty() {
            return Ok(Vec::new());
        }
        if orders.len() > 5 {
            // Fall back to per-order placement. Still returns the
            // full list of UUIDs in order.
            let mut ids = Vec::with_capacity(orders.len());
            for o in orders {
                ids.push(self.place_order(o).await?);
            }
            return Ok(ids);
        }

        let mut batch: Vec<Value> = Vec::with_capacity(orders.len());
        let mut uuids: Vec<OrderId> = Vec::with_capacity(orders.len());
        for o in orders {
            let side = if matches!(o.side, Side::Buy) {
                "BUY"
            } else {
                "SELL"
            };
            let ord_type = match o.order_type {
                OrderType::Limit => "LIMIT",
                OrderType::Market => "MARKET",
            };
            let tif = match o.time_in_force {
                Some(TimeInForce::Gtc) | Some(TimeInForce::Gtd) | None => "GTC",
                Some(TimeInForce::PostOnly) => "GTX",
                Some(TimeInForce::Ioc) => "IOC",
                Some(TimeInForce::Fok) => "FOK",
                Some(TimeInForce::Day) => "GTC",
            };
            let uuid = uuid::Uuid::new_v4();
            uuids.push(uuid);
            let mut entry = serde_json::json!({
                "symbol": o.symbol,
                "side": side,
                "type": ord_type,
                "quantity": o.qty.to_string(),
                "newClientOrderId": uuid.to_string(),
            });
            if o.order_type == OrderType::Limit {
                entry["timeInForce"] = Value::String(tif.into());
                if let Some(p) = o.price {
                    entry["price"] = Value::String(p.to_string());
                }
            }
            batch.push(entry);
        }
        let orders_json = serde_json::to_string(&batch)?;
        // Binance wants the batch as a URL-encoded JSON string in the
        // `batchOrders` query param.
        let params = format!("batchOrders={}", urlencoding::encode(&orders_json));
        let t0 = Instant::now();
        let _resp = self.signed_post("/fapi/v1/batchOrders", &params).await?;
        ORDER_ENTRY_LATENCY
            .with_label_values(&["binance_futures", "rest", "place_orders_batch"])
            .observe(t0.elapsed().as_secs_f64());
        Ok(uuids)
    }

    async fn cancel_order(&self, symbol: &str, order_id: OrderId) -> anyhow::Result<()> {
        let params = format!("symbol={symbol}&origClientOrderId={order_id}");
        self.signed_delete("/fapi/v1/order", &params).await?;
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
        self.signed_delete("/fapi/v1/allOpenOrders", &params)
            .await?;
        Ok(())
    }

    /// Native USDⓈ-M futures amend — preserves queue priority via
    /// `PUT /fapi/v1/order`. Binance requires `quantity` and
    /// `price` on every modify; the order is identified by
    /// `origClientOrderId` to stay consistent with how
    /// `cancel_order` and `place_order` route the engine's UUID
    /// through the client-order-id field.
    ///
    /// Binance Spot does **not** offer a true amend (only
    /// `order.cancelReplace`, which loses queue priority), so the
    /// override lives only on the futures connector — see the
    /// honest `supports_amend = false` flag on `BinanceConnector`.
    async fn amend_order(&self, amend: &AmendOrder) -> anyhow::Result<()> {
        let params = build_amend_query(amend)?;
        let t0 = Instant::now();
        let result = self.signed_put("/fapi/v1/order", &params).await;
        ORDER_ENTRY_LATENCY
            .with_label_values(&["binance_futures", "rest", "amend_order"])
            .observe(t0.elapsed().as_secs_f64());
        result?;
        debug!(order_id = %amend.order_id, "amended Binance futures order in place");
        Ok(())
    }

    async fn get_open_orders(&self, symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        let resp = self
            .signed_get("/fapi/v1/openOrders", &format!("symbol={symbol}"))
            .await?;
        let arr = resp.as_array().cloned().unwrap_or_default();
        Ok(arr
            .iter()
            .filter_map(|o| {
                let order_id_str = o.get("clientOrderId")?.as_str()?;
                let order_id = order_id_str.parse().ok()?;
                let side = match o.get("side")?.as_str()? {
                    "BUY" => Side::Buy,
                    "SELL" => Side::Sell,
                    _ => return None,
                };
                let price: Decimal = o.get("price")?.as_str()?.parse().ok()?;
                let qty: Decimal = o.get("origQty")?.as_str()?.parse().ok()?;
                let filled_qty: Decimal = o.get("executedQty")?.as_str()?.parse().ok()?;
                let status = match o.get("status")?.as_str()? {
                    "NEW" => OrderStatus::Open,
                    "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled,
                    "FILLED" => OrderStatus::Filled,
                    "CANCELED" | "CANCELLED" => OrderStatus::Cancelled,
                    "REJECTED" | "EXPIRED" => OrderStatus::Rejected,
                    _ => OrderStatus::Open,
                };
                let time_ms = o.get("time")?.as_i64()?;
                let created_at = chrono::DateTime::from_timestamp_millis(time_ms)?;
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
        // `/fapi/v2/balance` — newer balance endpoint with asset-level
        // rollup. Returns `[{accountAlias, asset, balance, …}]`.
        let resp = self.signed_get("/fapi/v2/balance", "").await?;
        let arr = resp.as_array().cloned().unwrap_or_default();
        Ok(arr
            .iter()
            .filter_map(|b| {
                let asset = b.get("asset")?.as_str()?.to_string();
                let total: Decimal = b.get("balance")?.as_str()?.parse().ok()?;
                // `crossUnPnl` is the margin tied up; approximate
                // `locked` with `max(0, total - available)`.
                let available: Decimal = b
                    .get("availableBalance")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(total);
                let locked = (total - available).max(Decimal::ZERO);
                Some(Balance {
                    asset,
                    wallet: WalletType::UsdMarginedFutures,
                    total,
                    locked,
                    available,
                })
            })
            .filter(|b| b.total > dec!(0))
            .collect())
    }

    async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec> {
        self.rate_limiter.acquire(1).await;
        let url = format!("{}/fapi/v1/exchangeInfo", self.base_url);
        let resp: Value = self.client.get(&url).send().await?.json().await?;
        let symbols = resp
            .get("symbols")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let sym = symbols
            .iter()
            .find(|s| s.get("symbol").and_then(|v| v.as_str()) == Some(symbol))
            .ok_or_else(|| anyhow::anyhow!("symbol not found: {symbol}"))?;
        parse_binance_futures_symbol(sym)
            .ok_or_else(|| anyhow::anyhow!("malformed Binance futures symbol entry for {symbol}"))
    }

    /// List every symbol Binance USDⓈ-M futures currently advertises
    /// via `GET /fapi/v1/exchangeInfo`. Shares the row parser with
    /// `get_product_spec` so a schema drift on one call site drifts
    /// the other the same way. `contractStatus` is mapped to
    /// [`TradingStatus`] so the Epic F listing sniper can filter
    /// out `SETTLING` / `CLOSE` / `PENDING_TRADING` rows post-hoc
    /// without re-querying the venue.
    async fn list_symbols(&self) -> anyhow::Result<Vec<ProductSpec>> {
        self.rate_limiter.acquire(1).await;
        let url = format!("{}/fapi/v1/exchangeInfo", self.base_url);
        let resp: Value = self.client.get(&url).send().await?.json().await?;
        Ok(parse_binance_futures_symbols_array(&resp))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let url = format!("{}/fapi/v1/ping", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    async fn rate_limit_remaining(&self) -> u32 {
        self.rate_limiter.remaining().await
    }

    /// USDⓈ-M futures fee schedule via
    /// `GET /fapi/v1/commissionRate?symbol=`. Returns a single
    /// `{ makerCommissionRate, takerCommissionRate }` object that
    /// the pure helper parses into a `FeeTierInfo`.
    async fn fetch_fee_tiers(&self, symbol: &str) -> Result<FeeTierInfo, FeeTierError> {
        let resp = self
            .signed_get("/fapi/v1/commissionRate", &format!("symbol={symbol}"))
            .await
            .map_err(|e| FeeTierError::Other(anyhow::anyhow!("{e}")))?;
        parse_binance_futures_fee_response(&resp)
            .ok_or_else(|| FeeTierError::Other(anyhow::anyhow!("malformed commissionRate body")))
    }

    /// Account margin snapshot via `/fapi/v2/account` (Epic 40.4).
    /// Returns `totalMarginBalance`, `totalMaintMargin`, plus
    /// `positions[]` with `isolatedMargin` and `liquidationPrice`
    /// for each open symbol. Margin ratio is computed client-side
    /// as `totalMaintMargin / totalMarginBalance` since Binance's
    /// account endpoint does not surface a single ratio field.
    async fn account_margin_info(&self) -> Result<AccountMarginInfo, MarginError> {
        let ts_param = format!("timestamp={}", chrono::Utc::now().timestamp_millis());
        let resp = self
            .signed_get("/fapi/v2/account", &ts_param)
            .await
            .map_err(MarginError::Other)?;
        parse_binance_futures_account(&resp)
            .ok_or_else(|| MarginError::Other(anyhow::anyhow!(
                "malformed /fapi/v2/account response"
            )))
    }

    /// Set per-symbol margin mode via
    /// `POST /fapi/v1/marginType` (Epic 40.7). Binance treats
    /// "already in this mode" as error `-4046` which we
    /// normalise to `Ok(())` so a re-run of the startup hook
    /// does not fail on a healthy account.
    async fn set_margin_mode(
        &self,
        symbol: &str,
        mode: MarginMode,
    ) -> Result<(), MarginError> {
        let kind = match mode {
            MarginMode::Isolated => "ISOLATED",
            MarginMode::Cross => "CROSSED",
        };
        let ts = chrono::Utc::now().timestamp_millis();
        let params = format!("symbol={symbol}&marginType={kind}&timestamp={ts}");
        match self.signed_post("/fapi/v1/marginType", &params).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                // -4046 = "No need to change margin type"
                if msg.contains("-4046") || msg.contains("No need to change") {
                    info!(symbol, mode = kind, "margin mode already set, skipping");
                    Ok(())
                } else {
                    Err(MarginError::Other(e))
                }
            }
        }
    }

    /// Set per-symbol leverage via
    /// `POST /fapi/v1/leverage` (Epic 40.7). Binance silently
    /// clamps to the symbol's bracket limit if we request more
    /// than the tier allows — an under-quota value always
    /// succeeds.
    async fn set_leverage(
        &self,
        symbol: &str,
        leverage: u32,
    ) -> Result<(), MarginError> {
        let ts = chrono::Utc::now().timestamp_millis();
        let params = format!("symbol={symbol}&leverage={leverage}&timestamp={ts}");
        self.signed_post("/fapi/v1/leverage", &params)
            .await
            .map(|_| ())
            .map_err(MarginError::Other)
    }
}

/// Parse a single `symbols[]` entry from `/fapi/v1/exchangeInfo`
/// into a [`ProductSpec`]. Shared by `get_product_spec` (single-
/// symbol path) and `list_symbols` (whole-universe path). Pure
/// helper so the wire shape is unit-tested without an HTTP client.
///
/// Binance USDⓈ-M surfaces a `contractStatus` field per contract
/// (`TRADING`, `PENDING_TRADING`, `SETTLING`, `PRE_DELIVERING`,
/// `DELIVERING`, `DELIVERED`, `PRE_SETTLE`, `CLOSE`). Only
/// `TRADING` is mapped to `TradingStatus::Trading`; all other
/// values are mapped conservatively so the listing sniper does
/// not treat a settling contract as a new listing.
pub(crate) fn parse_binance_futures_symbol(sym: &Value) -> Option<ProductSpec> {
    let symbol = sym.get("symbol").and_then(|v| v.as_str())?.to_string();
    let base = sym
        .get("baseAsset")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let quote = sym
        .get("quoteAsset")
        .and_then(|v| v.as_str())
        .unwrap_or("USDT")
        .to_string();

    let filters = sym
        .get("filters")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut tick_size = dec!(0.01);
    let mut lot_size = dec!(0.001);
    let mut min_notional = dec!(5);
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
            Some("MIN_NOTIONAL") => {
                if let Some(mn) = f.get("notional").and_then(|v| v.as_str()) {
                    min_notional = mn.parse().unwrap_or(min_notional);
                }
            }
            _ => {}
        }
    }

    let trading_status = match sym.get("contractStatus").and_then(|v| v.as_str()) {
        Some("TRADING") => TradingStatus::Trading,
        Some("PENDING_TRADING") => TradingStatus::PreTrading,
        Some("SETTLING") | Some("PRE_SETTLE") | Some("CLOSE") => TradingStatus::Break,
        Some("PRE_DELIVERING") | Some("DELIVERING") | Some("DELIVERED") => TradingStatus::Delisted,
        // Absent / unknown — spot-style fallback.
        Some(_) | None => TradingStatus::Trading,
    };

    Some(ProductSpec {
        symbol,
        base_asset: base,
        quote_asset: quote,
        tick_size,
        lot_size,
        min_notional,
        // Binance futures default tier: 0.02% maker / 0.05% taker.
        maker_fee: dec!(0.0002),
        taker_fee: dec!(0.0005),
        trading_status,
    })
}

/// Parse the full `/fapi/v1/exchangeInfo` response body into the
/// list of [`ProductSpec`] entries used by `list_symbols`. Malformed
/// rows (missing `symbol`) are dropped silently.
pub(crate) fn parse_binance_futures_symbols_array(resp: &Value) -> Vec<ProductSpec> {
    resp.get("symbols")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(parse_binance_futures_symbol)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the response from `GET /fapi/v1/commissionRate` into a
/// `FeeTierInfo`. Binance futures returns a single object with
/// the `makerCommissionRate` / `takerCommissionRate` fields as
/// strings — pure helper so the wire shape is unit-tested
/// without an HTTP client.
pub(crate) fn parse_binance_futures_fee_response(resp: &Value) -> Option<FeeTierInfo> {
    let maker_fee: Decimal = resp.get("makerCommissionRate")?.as_str()?.parse().ok()?;
    let taker_fee: Decimal = resp.get("takerCommissionRate")?.as_str()?.parse().ok()?;
    Some(FeeTierInfo {
        maker_fee,
        taker_fee,
        vip_tier: None,
        fetched_at: chrono::Utc::now(),
    })
}

/// Parse `/fapi/v2/account` JSON into [`AccountMarginInfo`]
/// (Epic 40.4). Binance publishes string-encoded decimals —
/// every `.parse()` here defaults to `Decimal::ZERO` on a
/// missing or malformed field so a single screwed-up position
/// entry does not wipe out the whole snapshot. The guard
/// will still escalate on the aggregate ratio even if one
/// position parse failed.
pub(crate) fn parse_binance_futures_account(resp: &Value) -> Option<AccountMarginInfo> {
    let parse_dec = |v: Option<&Value>| -> Decimal {
        v.and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(Decimal::ZERO)
    };
    let total_equity = parse_dec(resp.get("totalMarginBalance"));
    let total_initial_margin = parse_dec(resp.get("totalInitialMargin"));
    let total_maintenance_margin = parse_dec(resp.get("totalMaintMargin"));
    let available_balance = parse_dec(resp.get("availableBalance"));
    let margin_ratio = if total_equity > Decimal::ZERO {
        total_maintenance_margin / total_equity
    } else {
        Decimal::ONE
    };
    let positions = resp
        .get("positions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(parse_binance_futures_position)
                .collect()
        })
        .unwrap_or_default();
    Some(AccountMarginInfo {
        total_equity,
        total_initial_margin,
        total_maintenance_margin,
        available_balance,
        margin_ratio,
        positions,
        reported_at_ms: chrono::Utc::now().timestamp_millis(),
    })
}

/// Parse one `positions[]` entry from `/fapi/v2/account`.
/// Zero-size entries are filtered — Binance returns a row
/// for every listed symbol regardless of whether we hold a
/// position, and including the zero rows would bloat the
/// per-position map without carrying any margin info.
fn parse_binance_futures_position(pos: &Value) -> Option<PositionMargin> {
    let symbol = pos.get("symbol")?.as_str()?.to_string();
    let parse_dec = |v: Option<&Value>| -> Decimal {
        v.and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(Decimal::ZERO)
    };
    let position_amt = parse_dec(pos.get("positionAmt"));
    if position_amt == Decimal::ZERO {
        return None;
    }
    let side = if position_amt > Decimal::ZERO {
        Side::Buy
    } else {
        Side::Sell
    };
    let entry_price = parse_dec(pos.get("entryPrice"));
    let mark_price = parse_dec(pos.get("markPrice"));
    let isolated_margin = pos
        .get("isolatedMargin")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Decimal>().ok())
        .filter(|d| *d > Decimal::ZERO);
    let liq_price = pos
        .get("liquidationPrice")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Decimal>().ok())
        .filter(|d| *d > Decimal::ZERO);
    // PERP-4 — Binance futures exposes `adlQuantile` on
    // positionRisk (0–4). Absent on isolated positions that
    // have not yet been through an ADL recalculation — parse
    // as Option.
    let adl_quantile = pos
        .get("adlQuantile")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .and_then(|n| u8::try_from(n).ok());
    Some(PositionMargin {
        symbol,
        side,
        size: position_amt.abs(),
        entry_price,
        mark_price,
        isolated_margin,
        liq_price,
        adl_quantile,
    })
}

/// Build the URL-encoded query string for `PUT /fapi/v1/order`.
/// Pure helper so the wire shape can be unit-tested without a live
/// HTTP client. Bails when either `new_price` or `new_qty` is None
/// because Binance requires both on every modify and a missing
/// field would otherwise produce a malformed request that the
/// venue rejects with a generic 400.
pub(crate) fn build_amend_query(amend: &AmendOrder) -> anyhow::Result<String> {
    let qty = amend
        .new_qty
        .ok_or_else(|| anyhow::anyhow!("Binance futures amend requires new_qty"))?;
    let price = amend
        .new_price
        .ok_or_else(|| anyhow::anyhow!("Binance futures amend requires new_price"))?;
    Ok(format!(
        "symbol={}&origClientOrderId={}&quantity={}&price={}",
        amend.symbol, amend.order_id, qty, price
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_is_linear_perp() {
        let c = BinanceFuturesConnector::testnet("k", "s");
        assert_eq!(c.product(), VenueProduct::LinearPerp);
    }

    #[test]
    fn capabilities_claim_funding_rate_support() {
        let c = BinanceFuturesConnector::testnet("k", "s");
        assert!(c.capabilities().supports_funding_rate);
        assert!(c.capabilities().supports_amend);
        assert!(!c.capabilities().supports_ws_trading);
        assert!(!c.capabilities().supports_fix);
    }

    /// `PUT /fapi/v1/order` requires `symbol`, `origClientOrderId`,
    /// `quantity` and `price` — pin the wire shape so any future
    /// refactor of `build_amend_query` cannot silently drop a field.
    #[test]
    fn amend_query_contains_required_fields() {
        let oid = uuid::Uuid::nil();
        let amend = AmendOrder {
            order_id: oid,
            symbol: "BTCUSDT".into(),
            new_price: Some(dec!(50000.5)),
            new_qty: Some(dec!(0.01)),
        };
        let q = build_amend_query(&amend).unwrap();
        assert!(q.contains("symbol=BTCUSDT"));
        assert!(q.contains(&format!("origClientOrderId={oid}")));
        assert!(q.contains("quantity=0.01"));
        assert!(q.contains("price=50000.5"));
    }

    /// `GET /fapi/v1/commissionRate` returns a single object with
    /// the maker / taker rates as strings. Pin the wire shape so a
    /// schema drift breaks the test instead of silently dropping a
    /// new tier into the default.
    #[test]
    fn futures_fee_response_parses_maker_taker_rates() {
        let resp = serde_json::json!({
            "symbol": "BTCUSDT",
            "makerCommissionRate": "0.0002",
            "takerCommissionRate": "0.0004"
        });
        let info = parse_binance_futures_fee_response(&resp).unwrap();
        assert_eq!(info.maker_fee, dec!(0.0002));
        assert_eq!(info.taker_fee, dec!(0.0004));
        assert!(info.vip_tier.is_none());
    }

    /// Missing price or qty is a programmer error — bail loudly so
    /// the engine's amend-fallback path triggers instead of sending
    /// a malformed request that the venue rejects with a generic
    /// 400.
    #[test]
    fn amend_query_bails_when_price_or_qty_missing() {
        let amend_no_price = AmendOrder {
            order_id: uuid::Uuid::nil(),
            symbol: "BTCUSDT".into(),
            new_price: None,
            new_qty: Some(dec!(0.01)),
        };
        assert!(build_amend_query(&amend_no_price).is_err());
        let amend_no_qty = AmendOrder {
            order_id: uuid::Uuid::nil(),
            symbol: "BTCUSDT".into(),
            new_price: Some(dec!(50000)),
            new_qty: None,
        };
        assert!(build_amend_query(&amend_no_qty).is_err());
    }

    /// Listing sniper (Epic F): the futures parser walks the full
    /// `/fapi/v1/exchangeInfo` response and maps every row,
    /// including contracts in non-trading states that the sniper
    /// consumer filters post-hoc.
    #[test]
    fn list_symbols_parses_full_futures_exchange_info() {
        let resp = serde_json::json!({
            "symbols": [
                {
                    "symbol": "BTCUSDT",
                    "baseAsset": "BTC",
                    "quoteAsset": "USDT",
                    "contractStatus": "TRADING",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.10"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.001"},
                        {"filterType": "MIN_NOTIONAL", "notional": "5"}
                    ]
                },
                {
                    "symbol": "DEADUSDT",
                    "baseAsset": "DEAD",
                    "quoteAsset": "USDT",
                    "contractStatus": "SETTLING",
                    "filters": []
                },
                {
                    "symbol": "NEWUSDT",
                    "baseAsset": "NEW",
                    "quoteAsset": "USDT",
                    "contractStatus": "PENDING_TRADING",
                    "filters": []
                }
            ]
        });
        let specs = parse_binance_futures_symbols_array(&resp);
        assert_eq!(specs.len(), 3);
        let btc = specs.iter().find(|s| s.symbol == "BTCUSDT").unwrap();
        assert_eq!(btc.tick_size, dec!(0.10));
        assert_eq!(btc.trading_status, TradingStatus::Trading);
        let dead = specs.iter().find(|s| s.symbol == "DEADUSDT").unwrap();
        assert_eq!(dead.trading_status, TradingStatus::Break);
        let new = specs.iter().find(|s| s.symbol == "NEWUSDT").unwrap();
        assert_eq!(new.trading_status, TradingStatus::PreTrading);
    }

    #[test]
    fn testnet_uses_testnet_urls() {
        let c = BinanceFuturesConnector::testnet("k", "s");
        assert!(c.base_url.contains("binancefuture.com"));
        assert!(c.ws_url.contains("binancefuture.com"));
    }

    #[test]
    fn mainnet_uses_fapi_urls() {
        let c = BinanceFuturesConnector::new("k", "s");
        assert!(c.base_url.contains("fapi.binance.com"));
        assert!(c.ws_url.contains("fstream.binance.com"));
    }

    /// Default impl of `get_funding_rate` lives in the trait; we
    /// override it. The override returns `Err(Other)` when the venue
    /// is unreachable (tested on a non-routable URL) rather than
    /// falling through to `NotSupported`.
    #[tokio::test]
    async fn get_funding_rate_returns_other_not_notsupported_on_network_fail() {
        let c = BinanceFuturesConnector::with_urls(
            "http://127.0.0.1:1", // unreachable
            "ws://127.0.0.1:1",
            "k",
            "s",
        );
        let err = c.get_funding_rate("BTCUSDT").await.unwrap_err();
        match err {
            FundingRateError::NotSupported => {
                panic!("futures connector must NOT report NotSupported")
            }
            FundingRateError::Other(_) => {}
        }
    }

    /// Epic 40.4 — pin the `/fapi/v2/account` wire shape so a
    /// venue schema drift fails the test instead of silently
    /// zeroing the guard's ratio (which would hide the account
    /// from the kill switch until someone noticed).
    #[test]
    fn account_margin_parser_extracts_ratio_and_positions() {
        let resp = serde_json::json!({
            "totalMarginBalance": "10000.50",
            "totalInitialMargin": "2000.00",
            "totalMaintMargin": "500.00",
            "availableBalance": "8000.00",
            "positions": [
                {
                    "symbol": "BTCUSDT",
                    "positionAmt": "0.050",
                    "entryPrice": "50000.0",
                    "markPrice": "50500.0",
                    "isolatedMargin": "250.0",
                    "liquidationPrice": "45000.0"
                },
                {
                    "symbol": "DUMMY",
                    "positionAmt": "0",
                    "entryPrice": "0",
                    "markPrice": "0",
                    "isolatedMargin": "0",
                    "liquidationPrice": "0"
                },
                {
                    "symbol": "ETHUSDT",
                    "positionAmt": "-1.5",
                    "entryPrice": "3000",
                    "markPrice": "2900",
                    "isolatedMargin": "0",
                    "liquidationPrice": "4500"
                }
            ]
        });
        let info = parse_binance_futures_account(&resp).unwrap();
        assert_eq!(info.total_equity, dec!(10000.50));
        assert_eq!(info.total_maintenance_margin, dec!(500.00));
        // 500 / 10000.50 ≈ 0.0499975…
        assert!(info.margin_ratio > dec!(0.049));
        assert!(info.margin_ratio < dec!(0.051));
        // DUMMY zero-size row filtered; BTCUSDT + ETHUSDT kept.
        assert_eq!(info.positions.len(), 2);
        let btc = info.positions.iter().find(|p| p.symbol == "BTCUSDT").unwrap();
        assert_eq!(btc.side, Side::Buy);
        assert_eq!(btc.size, dec!(0.050));
        assert_eq!(btc.isolated_margin, Some(dec!(250.0)));
        assert_eq!(btc.liq_price, Some(dec!(45000.0)));
        let eth = info.positions.iter().find(|p| p.symbol == "ETHUSDT").unwrap();
        assert_eq!(eth.side, Side::Sell);
        assert_eq!(eth.size, dec!(1.5));
        // Cross-margin position: no isolated allocation.
        assert!(eth.isolated_margin.is_none());
    }

    /// Malformed `totalMarginBalance` → zero equity → the
    /// parser saturates ratio at 1.0 so the guard's
    /// `CancelAll` threshold is guaranteed to trip regardless
    /// of the MM field. Better to over-escalate than to
    /// silently pass through a near-zero ratio from a drifted
    /// schema.
    #[test]
    fn account_margin_parser_zero_equity_forces_ratio_one() {
        let resp = serde_json::json!({
            "totalMarginBalance": "0",
            "totalInitialMargin": "0",
            "totalMaintMargin": "0",
            "availableBalance": "0",
            "positions": []
        });
        let info = parse_binance_futures_account(&resp).unwrap();
        assert_eq!(info.margin_ratio, Decimal::ONE);
    }
}
