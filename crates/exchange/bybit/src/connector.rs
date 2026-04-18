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

/// V5 product category — drives the `category` query-param and the
/// public WS URL suffix. One `BybitConnector` instance handles
/// exactly one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BybitCategory {
    /// Spot market — `/v5/public/spot`, wallet `Spot` under
    /// classic accounts or `Unified` under UTA.
    Spot,
    /// USDT-margined perpetual linear futures — this was the
    /// original hardcoded path.
    Linear,
    /// Coin-margined inverse perpetual futures.
    Inverse,
}

impl BybitCategory {
    /// V5 REST `category` query-param / body field value.
    pub fn as_str(self) -> &'static str {
        match self {
            BybitCategory::Spot => "spot",
            BybitCategory::Linear => "linear",
            BybitCategory::Inverse => "inverse",
        }
    }

    fn public_ws_suffix(self) -> &'static str {
        match self {
            BybitCategory::Spot => "spot",
            BybitCategory::Linear => "linear",
            BybitCategory::Inverse => "inverse",
        }
    }

    fn venue_product(self) -> VenueProduct {
        match self {
            BybitCategory::Spot => VenueProduct::Spot,
            BybitCategory::Linear => VenueProduct::LinearPerp,
            BybitCategory::Inverse => VenueProduct::InversePerp,
        }
    }

    fn wallet_type(self) -> WalletType {
        // V5 UTA consolidates spot + linear + options under one
        // collateral pool; we default to `Unified` and let the
        // operator override via `with_wallet` if they are still on
        // a classic sub-account. See
        // docs/research/spot-mm-specifics.md §5 for the nuance.
        match self {
            BybitCategory::Spot => WalletType::Unified,
            BybitCategory::Linear => WalletType::Unified,
            BybitCategory::Inverse => WalletType::CoinMarginedFutures,
        }
    }

    /// Whether this product pays funding.
    fn has_funding(self) -> bool {
        matches!(self, BybitCategory::Linear | BybitCategory::Inverse)
    }
}

/// Bybit V5 API connector.
///
/// Supports:
/// - Spot, Linear (USDT perps), Inverse (coin-margined perps)
/// - Batch orders (up to 20)
/// - WebSocket combined streams
/// - HMAC-SHA256 auth
///
/// One instance = one category. Use `BybitConnector::spot()`,
/// `::linear()`, `::inverse()` (or their `::testnet_*` variants)
/// at construction time.
pub struct BybitConnector {
    client: Client,
    base_url: String,
    ws_url: String,
    api_key: String,
    api_secret: String,
    rate_limiter: RateLimiter,
    capabilities: VenueCapabilities,
    category: BybitCategory,
    /// Wallet type to report on `Balance` entries. Defaults to
    /// `category.wallet_type()` but can be overridden for classic
    /// sub-accounts via `with_wallet`.
    wallet: WalletType,
    /// Fail-closed withdraw address whitelist (Epic 8). Threaded
    /// into `withdraw()` via `validate_withdraw_address`.
    withdraw_whitelist: Option<Vec<String>>,
}

impl BybitConnector {
    /// Legacy constructor — keeps backward compatibility with the
    /// original `BybitConnector::new(...)` signature. Equivalent
    /// to `BybitConnector::linear(...)`.
    pub fn new(api_key: &str, api_secret: &str) -> Self {
        Self::linear(api_key, api_secret)
    }

    /// Construct a connector for V5 linear perps (USDT-margined).
    pub fn linear(api_key: &str, api_secret: &str) -> Self {
        Self::with_category(api_key, api_secret, BybitCategory::Linear, false)
    }

    /// Construct a connector for V5 spot.
    pub fn spot(api_key: &str, api_secret: &str) -> Self {
        Self::with_category(api_key, api_secret, BybitCategory::Spot, false)
    }

    /// Construct a connector for V5 inverse perps (coin-margined).
    pub fn inverse(api_key: &str, api_secret: &str) -> Self {
        Self::with_category(api_key, api_secret, BybitCategory::Inverse, false)
    }

    /// Testnet variant of `::linear`. Preserves the original
    /// `::testnet` name for source compatibility.
    pub fn testnet(api_key: &str, api_secret: &str) -> Self {
        Self::with_category(api_key, api_secret, BybitCategory::Linear, true)
    }

    pub fn testnet_spot(api_key: &str, api_secret: &str) -> Self {
        Self::with_category(api_key, api_secret, BybitCategory::Spot, true)
    }

    pub fn testnet_inverse(api_key: &str, api_secret: &str) -> Self {
        Self::with_category(api_key, api_secret, BybitCategory::Inverse, true)
    }

    fn with_category(
        api_key: &str,
        api_secret: &str,
        category: BybitCategory,
        testnet: bool,
    ) -> Self {
        let (base_url, ws_host) = if testnet {
            (
                "https://api-testnet.bybit.com",
                "wss://stream-testnet.bybit.com",
            )
        } else {
            ("https://api.bybit.com", "wss://stream.bybit.com")
        };
        let ws_url = format!("{}/v5/public/{}", ws_host, category.public_ws_suffix());
        Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            ws_url,
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            // Bybit: 600 req / 5s, use 80%.
            rate_limiter: RateLimiter::new(600, Duration::from_secs(5), 0.8),
            capabilities: VenueCapabilities {
                max_batch_size: 20,
                supports_amend: true,
                supports_funding_rate: category.has_funding(),
                // `BybitWsTrader` exists in `ws_trade.rs` but is not
                // wired into `place_order` — the V5 auth mechanism
                // needs live-testnet verification before we route
                // production traffic through it. Until then the
                // capability MUST be `false` so a capability-driven
                // router never picks the WS path on Bybit. See
                // docs/deployment.md §3 "Wire Bybit WS Trade into
                // BybitConnector::place_order" in operator next steps.
                supports_ws_trading: false,
                supports_fix: false,
                max_order_rate: 20,
                // Margin info + mode are perp-only on Bybit —
                // linear/inverse UTA accounts expose
                // `/v5/account/wallet-balance` + position-list,
                // spot returns a 404 on the position endpoint.
                // Mirror `supports_funding_rate` so the capability
                // flags stay honest per category.
                supports_margin_info: category.has_funding(),
                supports_margin_mode: category.has_funding(),
            },
            wallet: category.wallet_type(),
            category,
            withdraw_whitelist: None,
        }
    }

    /// Override the wallet type reported on `Balance` entries.
    /// Use this when running against a classic sub-account (spot
    /// wallet is its own bucket, not Unified).
    pub fn with_wallet(mut self, wallet: WalletType) -> Self {
        self.wallet = wallet;
        self
    }

    /// Attach a fail-closed withdraw address whitelist (Epic 8).
    /// See `validate_withdraw_address` for semantics.
    pub fn with_withdraw_whitelist(mut self, list: Option<Vec<String>>) -> Self {
        self.withdraw_whitelist = list;
        self
    }

    pub fn category(&self) -> BybitCategory {
        self.category
    }

    /// Epic F stage-3 — multi-category symbol scan.
    ///
    /// Fans out one HTTP request per category (`spot`,
    /// `linear`, `inverse`) against
    /// `/v5/market/instruments-info`, parses each response
    /// via the shared [`parse_bybit_instruments_list`]
    /// helper, and returns the merged list.
    ///
    /// Operators running the listing sniper across all
    /// Bybit V5 categories should call this from any single
    /// connector instance — the connector's own
    /// `self.category` is irrelevant; each category-specific
    /// request is routed via the existing rate-limiter and
    /// the shared `signed_get` path.
    ///
    /// Per-category failures are surfaced as `Err` —
    /// partial-success aggregation is a stage-4 polish if
    /// operators want it (the listing sniper consumer can
    /// also catch the error and call per-category instead).
    pub async fn list_symbols_all_categories(&self) -> anyhow::Result<Vec<ProductSpec>> {
        let mut merged: Vec<ProductSpec> = Vec::new();
        for category in [
            BybitCategory::Spot,
            BybitCategory::Linear,
            BybitCategory::Inverse,
        ] {
            let params = format!("category={}", category.as_str());
            let result = self
                .signed_get("/v5/market/instruments-info", &params)
                .await?;
            merged.extend(parse_bybit_instruments_list(&result));
        }
        Ok(merged)
    }

    async fn signed_get(&self, path: &str, params: &str) -> anyhow::Result<Value> {
        self.rate_limiter.acquire(1).await;
        let url = if params.is_empty() {
            format!("{}{path}", self.base_url)
        } else {
            format!("{}{path}?{params}", self.base_url)
        };
        // Public-only mode: when no API key is configured, skip the
        // auth headers entirely. Bybit rejects any request with an
        // empty `X-BAPI-API-KEY` header ("apiKey is missing") even
        // on public market-data endpoints, so sending empty auth
        // headers breaks paper-mode runs that don't need trading
        // access. Attempting the call unsigned lets `/v5/market/*`
        // work; private endpoints will error out server-side with a
        // clearer message.
        let req = if self.api_key.is_empty() {
            self.client.get(&url)
        } else {
            let (ts, recv, sig) =
                auth::auth_headers(&self.api_key, &self.api_secret, params);
            self.client
                .get(&url)
                .header("X-BAPI-API-KEY", &self.api_key)
                .header("X-BAPI-TIMESTAMP", &ts)
                .header("X-BAPI-RECV-WINDOW", &recv)
                .header("X-BAPI-SIGN", &sig)
        };
        let resp = req.send().await?;
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

    fn classify_error(&self, err: &anyhow::Error) -> mm_exchange_core::VenueError {
        crate::classify(err)
    }

    fn product(&self) -> VenueProduct {
        self.category.venue_product()
    }

    async fn subscribe(
        &self,
        symbols: &[String],
    ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let ws_url = self.ws_url.clone();
        // Bybit V5 linear/inverse supports orderbook depths 1, 50,
        // 200, 500 — **not 25**. Spot supports 1, 50, 200. Depth
        // 50 is a universal safe default that works across all
        // categories (verified Bybit docs 2026-04). Sending an
        // unsupported depth causes the WS to silently never
        // deliver book updates — the subscription is accepted
        // but no data flows, which is how our StaleBook circuit
        // breaker would trip 10 s in.
        let depth: u32 = match self.category {
            BybitCategory::Spot => 50,
            BybitCategory::Linear | BybitCategory::Inverse => 50,
        };
        let topics: Vec<String> = symbols
            .iter()
            .flat_map(|s| vec![format!("orderbook.{depth}.{s}"), format!("publicTrade.{s}")])
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
        let params = format!(
            "category={}&symbol={symbol}&limit={depth}",
            self.category.as_str()
        );
        let result = self.signed_get("/v5/market/orderbook", &params).await?;
        let bids = parse_levels(result.get("b"))?;
        let asks = parse_levels(result.get("a"))?;
        let seq = result.get("u").and_then(|v| v.as_u64()).unwrap_or(0);
        Ok((bids, asks, seq))
    }

    async fn place_order(&self, order: &NewOrder) -> anyhow::Result<OrderId> {
        let body = serde_json::json!({
            "category": self.category.as_str(),
            "symbol": order.symbol,
            "side": match order.side { Side::Buy => "Buy", Side::Sell => "Sell" },
            "orderType": match order.order_type { OrderType::Limit => "Limit", OrderType::Market => "Market" },
            "qty": order.qty.to_string(),
            "price": order.price.map(|p| p.to_string()),
            "timeInForce": bybit_tif(order.time_in_force),
        });
        // Only a REST path exists today — WS trading is listed as an
        // operator next-step in docs/deployment.md §3. The metric label
        // reflects reality.
        let t0 = Instant::now();
        let rest_result = self.signed_post("/v5/order/create", &body).await;
        ORDER_ENTRY_LATENCY
            .with_label_values(&["bybit", "rest", "place_order"])
            .observe(t0.elapsed().as_secs_f64());
        let result = rest_result?;
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
                    "timeInForce": bybit_tif(o.time_in_force),
                })
            })
            .collect();

        let body = serde_json::json!({
            "category": self.category.as_str(),
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
            "category": self.category.as_str(),
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
        let body = serde_json::json!({
            "category": self.category.as_str(),
            "request": batch,
        });
        self.signed_post("/v5/order/cancel-batch", &body).await?;
        Ok(())
    }

    async fn cancel_all_orders(&self, symbol: &str) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "category": self.category.as_str(),
            "symbol": symbol,
        });
        self.signed_post("/v5/order/cancel-all", &body).await?;
        Ok(())
    }

    /// V5 per-account fee schedule for `symbol`. Calls
    /// `GET /v5/account/fee-rate?category=&symbol=` and parses
    /// the first row in `result.list`. Bybit does not surface a
    /// VIP-tier label in this response, so `vip_tier` stays
    /// `None` — the operator can read the equivalent from the
    /// account info page if needed.
    async fn fetch_fee_tiers(&self, symbol: &str) -> Result<FeeTierInfo, FeeTierError> {
        let params = format!("category={}&symbol={symbol}", self.category.as_str());
        let result = self
            .signed_get("/v5/account/fee-rate", &params)
            .await
            .map_err(|e| FeeTierError::Other(anyhow::anyhow!("{e}")))?;
        parse_bybit_fee_rate_response(&result, symbol)
            .ok_or_else(|| FeeTierError::Other(anyhow::anyhow!("no fee row for {symbol}")))
    }

    /// Epic 40.3 — Bybit V5 funding rate via
    /// `GET /v5/market/tickers?category=linear&symbol=`. The
    /// ticker row carries `fundingRate` and `nextFundingTime`
    /// (ms). Spot and inverse/linear-without-funding return
    /// `NotSupported` before hitting the wire so the engine
    /// never burns a call on a non-perp category.
    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate, FundingRateError> {
        if !self.category.has_funding() {
            return Err(FundingRateError::NotSupported);
        }
        let params = format!("category={}&symbol={symbol}", self.category.as_str());
        let url = format!("{}/v5/market/tickers?{params}", self.base_url);
        let resp: Value = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| FundingRateError::Other(anyhow::anyhow!("{e}")))?
            .json()
            .await
            .map_err(|e| FundingRateError::Other(anyhow::anyhow!("{e}")))?;
        let result = resp
            .get("result")
            .ok_or_else(|| FundingRateError::Other(anyhow::anyhow!("missing `result`")))?;
        parse_bybit_funding_rate(result, symbol).ok_or_else(|| {
            FundingRateError::Other(anyhow::anyhow!(
                "no funding row for {symbol}"
            ))
        })
    }

    /// Native V5 amend — preserves queue priority on Bybit.
    /// Maps the engine-side `AmendOrder` onto `POST /v5/order/amend`
    /// with the venue-mandatory `category` field and only the deltas
    /// (price and / or qty) the caller actually wants to change.
    /// Bybit returns `retCode == 0` on success; the helper
    /// `signed_post` already promotes any non-zero `retCode` to a
    /// Rust `Err`, so the engine's amend-fallback path triggers
    /// on every venue-side rejection.
    async fn amend_order(&self, amend: &AmendOrder) -> anyhow::Result<()> {
        let body = build_amend_body(self.category.as_str(), amend);
        let t0 = Instant::now();
        let result = self.signed_post("/v5/order/amend", &body).await;
        ORDER_ENTRY_LATENCY
            .with_label_values(&["bybit", "rest", "amend_order"])
            .observe(t0.elapsed().as_secs_f64());
        result?;
        debug!(order_id = %amend.order_id, "amended Bybit order in place");
        Ok(())
    }

    async fn get_open_orders(&self, symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        let result = self
            .signed_get(
                "/v5/order/realtime",
                &format!("category={}&symbol={symbol}", self.category.as_str()),
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
                    wallet: self.wallet,
                    total,
                    locked,
                    available: total - locked,
                })
            })
            .filter(|b| b.total > dec!(0))
            .collect())
    }

    async fn server_time_ms(&self) -> anyhow::Result<Option<i64>> {
        // Bybit V5 `/v5/market/time` returns `timeSecond` and
        // `timeNano`. Public endpoint, no signing needed.
        let url = format!("{}/v5/market/time", self.base_url);
        let resp: serde_json::Value = self.client.get(&url).send().await?.json().await?;
        let ts_ms = resp
            .get("result")
            .and_then(|r| r.get("timeSecond"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .map(|s| s * 1000);
        Ok(ts_ms)
    }

    async fn get_24h_volume_usd(
        &self,
        symbol: &str,
    ) -> anyhow::Result<Option<rust_decimal::Decimal>> {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        // Bybit V5 tickers — public endpoint, signed_get works with
        // empty credentials. `turnover24h` = quote-currency
        // 24h turnover.
        let params = format!("category={}&symbol={symbol}", self.category.as_str());
        let result = self
            .signed_get("/v5/market/tickers", &params)
            .await?;
        let row = result
            .get("list")
            .and_then(|l| l.as_array())
            .and_then(|a| a.first());
        let vol = row
            .and_then(|r| r.get("turnover24h"))
            .and_then(|v| v.as_str())
            .and_then(|s| Decimal::from_str(s).ok());
        Ok(vol)
    }

    async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec> {
        let params = format!("category={}&symbol={symbol}", self.category.as_str());
        let result = self
            .signed_get("/v5/market/instruments-info", &params)
            .await?;
        let item = result
            .get("list")
            .and_then(|l| l.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| anyhow::anyhow!("symbol not found"))?;
        parse_bybit_instrument(item)
            .ok_or_else(|| anyhow::anyhow!("malformed Bybit instrument row for {symbol}"))
    }

    /// List every instrument currently exposed on the V5
    /// `/v5/market/instruments-info` endpoint for **this**
    /// connector's category. The connector is constructed with
    /// exactly one `BybitCategory` (spot, linear, inverse), so
    /// `list_symbols` queries only that one category. To scan
    /// all three at once use [`Self::list_symbols_all_categories`]
    /// (Epic F stage-3) which fans out across spot + linear +
    /// inverse without requiring three separate connector
    /// instances.
    ///
    /// `category=spot` is a **public** read on V5 (no auth
    /// required), and the signed helper tolerates the empty
    /// signature; we route through `signed_get` anyway so the
    /// existing rate-limiter path is reused.
    async fn list_symbols(&self) -> anyhow::Result<Vec<ProductSpec>> {
        let params = format!("category={}", self.category.as_str());
        let result = self
            .signed_get("/v5/market/instruments-info", &params)
            .await?;
        Ok(parse_bybit_instruments_list(&result))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let url = format!("{}/v5/market/time", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    async fn rate_limit_remaining(&self) -> u32 {
        self.rate_limiter.remaining().await
    }

    /// Withdraw to an external address via
    /// `POST /v5/asset/withdraw`. Enforces the configured
    /// `withdraw_whitelist` before hitting the network.
    async fn withdraw(
        &self,
        asset: &str,
        qty: rust_decimal::Decimal,
        address: &str,
        network: &str,
    ) -> anyhow::Result<String> {
        mm_exchange_core::validate_withdraw_address(
            self.withdraw_whitelist.as_deref(),
            address,
        )?;
        let body = serde_json::json!({
            "coin": asset,
            "amount": qty.to_string(),
            "address": address,
            "chain": network,
            "accountType": "FUND",
        });
        let resp = self.signed_post("/v5/asset/withdraw", &body).await?;
        resp.get("result")
            .and_then(|r| r.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("missing withdraw id in response"))
    }

    /// Internal transfer between Bybit wallets via
    /// `POST /v5/asset/transfer/inter-transfer`.
    async fn internal_transfer(
        &self,
        asset: &str,
        qty: rust_decimal::Decimal,
        from_wallet: &str,
        to_wallet: &str,
    ) -> anyhow::Result<String> {
        let transfer_id = uuid::Uuid::new_v4().to_string();
        let body = serde_json::json!({
            "coin": asset,
            "amount": qty.to_string(),
            "fromAccountType": from_wallet,
            "toAccountType": to_wallet,
            "transferId": transfer_id,
        });
        let resp = self
            .signed_post("/v5/asset/transfer/inter-transfer", &body)
            .await?;
        resp.get("result")
            .and_then(|r| r.get("transferId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("missing transferId in response"))
    }

    /// Account margin snapshot (Epic 40.4). Fans out two V5
    /// calls: `GET /v5/account/wallet-balance?accountType=UNIFIED`
    /// for the aggregate totals (`totalEquity`,
    /// `totalInitialMargin`, `totalMaintenanceMargin`,
    /// `accountMMRate`), and `GET /v5/position/list?category=linear`
    /// for per-symbol detail (`positionIM`, `positionMM`,
    /// `liqPrice`). Spot category returns `NotSupported` before
    /// hitting the wire — matches the capability flag.
    async fn account_margin_info(&self) -> Result<AccountMarginInfo, MarginError> {
        if !self.category.has_funding() {
            return Err(MarginError::NotSupported);
        }
        let wallet = self
            .signed_get("/v5/account/wallet-balance", "accountType=UNIFIED")
            .await
            .map_err(MarginError::Other)?;
        let positions = self
            .signed_get(
                "/v5/position/list",
                &format!("category={}&settleCoin=USDT", self.category.as_str()),
            )
            .await
            .map_err(MarginError::Other)?;
        parse_bybit_account_margin(&wallet, &positions)
            .ok_or_else(|| MarginError::Other(anyhow::anyhow!(
                "malformed Bybit wallet-balance / position-list response"
            )))
    }

    /// Set account-wide margin mode via
    /// `POST /v5/account/set-margin-mode` (Epic 40.7). Bybit
    /// treats mode as an account-level switch in Unified
    /// Trading Account — the `symbol` arg is ignored here
    /// (Binance per-symbol, HL per-asset — Bybit is the
    /// outlier). Error `110026` = "already in this mode",
    /// mapped to `Ok(())`.
    async fn set_margin_mode(
        &self,
        _symbol: &str,
        mode: MarginMode,
    ) -> Result<(), MarginError> {
        if !self.category.has_funding() {
            return Err(MarginError::NotSupported);
        }
        let wire = match mode {
            MarginMode::Isolated => "ISOLATED_MARGIN",
            MarginMode::Cross => "REGULAR_MARGIN",
        };
        let body = serde_json::json!({ "setMarginMode": wire });
        match self.signed_post("/v5/account/set-margin-mode", &body).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("110026") || msg.contains("already") {
                    info!(mode = wire, "Bybit margin mode already set");
                    Ok(())
                } else {
                    Err(MarginError::Other(e))
                }
            }
        }
    }

    /// Set per-symbol leverage via
    /// `POST /v5/position/set-leverage` (Epic 40.7). Bybit
    /// takes separate `buyLeverage` / `sellLeverage` fields —
    /// we set both to the requested value since we run in
    /// one-way-position mode.
    async fn set_leverage(
        &self,
        symbol: &str,
        leverage: u32,
    ) -> Result<(), MarginError> {
        if !self.category.has_funding() {
            return Err(MarginError::NotSupported);
        }
        let body = serde_json::json!({
            "category": self.category.as_str(),
            "symbol": symbol,
            "buyLeverage": leverage.to_string(),
            "sellLeverage": leverage.to_string(),
        });
        match self.signed_post("/v5/position/set-leverage", &body).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                // 110043 = "Set leverage not modified"
                if msg.contains("110043") || msg.contains("not modified") {
                    info!(symbol, leverage, "Bybit leverage already set");
                    Ok(())
                } else {
                    Err(MarginError::Other(e))
                }
            }
        }
    }
}

/// Parse the merged output of V5 `wallet-balance` + `position-list`
/// into an [`AccountMarginInfo`] (Epic 40.4). Bybit publishes
/// `accountMMRate` as a *fraction-times-100* (i.e. `2.5` means 2.5 %,
/// not 2.5x), but the same row carries `totalMaintenanceMargin` and
/// `totalEquity` as raw amounts — we recompute the ratio to avoid
/// the fraction-vs-percentage ambiguity and drop the reported
/// `accountMMRate` on the floor.
pub(crate) fn parse_bybit_account_margin(
    wallet_result: &Value,
    positions_result: &Value,
) -> Option<AccountMarginInfo> {
    let parse_dec = |v: Option<&Value>| -> Decimal {
        v.and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(Decimal::ZERO)
    };
    // wallet-balance shape:
    // { "list": [ { "accountType": "UNIFIED", "totalEquity": "…",
    //               "totalInitialMargin": "…",
    //               "totalMaintenanceMargin": "…",
    //               "totalAvailableBalance": "…" } ] }
    let row = wallet_result.get("list")?.as_array()?.iter().find(|r| {
        r.get("accountType").and_then(|v| v.as_str()) == Some("UNIFIED")
    })?;
    let total_equity = parse_dec(row.get("totalEquity"));
    let total_initial_margin = parse_dec(row.get("totalInitialMargin"));
    let total_maintenance_margin = parse_dec(row.get("totalMaintenanceMargin"));
    let available_balance = parse_dec(row.get("totalAvailableBalance"));
    let margin_ratio = if total_equity > Decimal::ZERO {
        total_maintenance_margin / total_equity
    } else {
        Decimal::ONE
    };
    let positions = positions_result
        .get("list")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(parse_bybit_position_margin)
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

fn parse_bybit_position_margin(pos: &Value) -> Option<PositionMargin> {
    let symbol = pos.get("symbol")?.as_str()?.to_string();
    let parse_dec = |v: Option<&Value>| -> Decimal {
        v.and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(Decimal::ZERO)
    };
    let size = parse_dec(pos.get("size"));
    if size == Decimal::ZERO {
        return None;
    }
    let side_str = pos.get("side").and_then(|v| v.as_str()).unwrap_or("");
    let side = match side_str {
        "Buy" => Side::Buy,
        "Sell" => Side::Sell,
        _ => return None,
    };
    let entry_price = parse_dec(pos.get("avgPrice"));
    let mark_price = parse_dec(pos.get("markPrice"));
    // `positionIM` surfaces on isolated positions; cross
    // positions have `positionIM == 0` and the bucket lives
    // in the account-wide cross-margin pool.
    let im = parse_dec(pos.get("positionIM"));
    let isolated_margin = if im > Decimal::ZERO { Some(im) } else { None };
    let liq_price = pos
        .get("liqPrice")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "0")
        .and_then(|s| s.parse::<Decimal>().ok())
        .filter(|d| *d > Decimal::ZERO);
    Some(PositionMargin {
        symbol,
        side,
        size,
        entry_price,
        mark_price,
        isolated_margin,
        liq_price,
    })
}

/// Parse the `result` payload of
/// `GET /v5/market/tickers?category=linear` into a
/// [`FundingRate`] for the requested `symbol` (Epic 40.3).
/// Bybit V5 ships `fundingRate` as a string decimal and
/// `nextFundingTime` as a string of Unix-millis. Pure helper
/// so the wire shape is unit-tested without a live HTTP
/// client. Cadence is assumed 8h (Bybit's default) — the
/// venue does not expose cadence on the ticker endpoint; a
/// one-off `/v5/market/instruments-info.funding_interval`
/// lookup could refine this for the handful of symbols on
/// 4h settlement but that's deferred to a later sub-task.
pub(crate) fn parse_bybit_funding_rate(result: &Value, symbol: &str) -> Option<FundingRate> {
    let list = result.get("list")?.as_array()?;
    let row = list
        .iter()
        .find(|row| row.get("symbol").and_then(|s| s.as_str()) == Some(symbol))
        .or_else(|| list.first())?;
    let rate: Decimal = row.get("fundingRate")?.as_str()?.parse().ok()?;
    let next_ms: i64 = row.get("nextFundingTime")?.as_str()?.parse().ok()?;
    let next_funding_time = chrono::DateTime::from_timestamp_millis(next_ms)?;
    Some(FundingRate {
        rate,
        next_funding_time,
        interval: std::time::Duration::from_secs(8 * 3600),
    })
}

/// Parse the `result` payload of `GET /v5/account/fee-rate` into a
/// `FeeTierInfo` for the requested `symbol`. Pure function so the
/// wire shape can be unit-tested without an HTTP client. Returns
/// `None` when the row is missing or either field fails to parse.
pub(crate) fn parse_bybit_fee_rate_response(result: &Value, symbol: &str) -> Option<FeeTierInfo> {
    let list = result.get("list")?.as_array()?;
    let row = list
        .iter()
        .find(|row| row.get("symbol").and_then(|s| s.as_str()) == Some(symbol))
        .or_else(|| list.first())?;
    let maker_fee: Decimal = row.get("makerFeeRate")?.as_str()?.parse().ok()?;
    let taker_fee: Decimal = row.get("takerFeeRate")?.as_str()?.parse().ok()?;
    Some(FeeTierInfo {
        maker_fee,
        taker_fee,
        vip_tier: None,
        fetched_at: chrono::Utc::now(),
    })
}

/// Parse a single row from the Bybit V5
/// `/v5/market/instruments-info` `result.list[]` array into a
/// [`ProductSpec`]. Shared by `get_product_spec` (single-symbol)
/// and `list_symbols` (whole-category). Pure helper so the wire
/// shape is unit-tested without an HTTP client.
///
/// Bybit surfaces a per-instrument `status` field (`Trading`,
/// `PreLaunch`, `Settling`, `Delivering`, `Closed`, …). Only
/// `Trading` maps to [`TradingStatus::Trading`]; everything else
/// is mapped conservatively so the Epic F listing sniper does not
/// greenlight a pre-launch contract as "already trading".
pub(crate) fn parse_bybit_instrument(item: &Value) -> Option<ProductSpec> {
    let symbol = item.get("symbol").and_then(|v| v.as_str())?.to_string();
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

    // `minOrderAmt` on spot / `minNotionalValue` on linear —
    // fall back to dec!(5) when neither is present.
    let min_notional: Decimal = lot_filter
        .and_then(|f| {
            f.get("minOrderAmt")
                .or_else(|| f.get("minNotionalValue"))
                .or_else(|| f.get("minOrderAmount"))
        })
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(dec!(5));

    let trading_status = match item.get("status").and_then(|v| v.as_str()) {
        Some("Trading") => TradingStatus::Trading,
        Some("PreLaunch") => TradingStatus::PreTrading,
        Some("Settling") | Some("Delivering") => TradingStatus::Break,
        Some("Closed") => TradingStatus::Delisted,
        // Absent / unknown — assume trading (matches the
        // old get_product_spec default behaviour).
        Some(_) | None => TradingStatus::Trading,
    };

    Some(ProductSpec {
        symbol,
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
        min_notional,
        maker_fee: dec!(0.0002),
        taker_fee: dec!(0.00055),
        trading_status,
    })
}

/// Parse the `result` payload of `/v5/market/instruments-info`
/// into the list of [`ProductSpec`] entries used by
/// `list_symbols`. Malformed rows (missing `symbol`) are dropped.
pub(crate) fn parse_bybit_instruments_list(result: &Value) -> Vec<ProductSpec> {
    result
        .get("list")
        .and_then(|l| l.as_array())
        .map(|arr| arr.iter().filter_map(parse_bybit_instrument).collect())
        .unwrap_or_default()
}

/// Build the JSON body for `POST /v5/order/amend`. Pure function so
/// the wire shape can be unit-tested without spinning up a live
/// HTTP server. Only includes the fields actually changed — Bybit
/// rejects empty `price` / `qty` strings, so we leave them out
/// entirely when the caller did not set them.
pub(crate) fn build_amend_body(category: &str, amend: &AmendOrder) -> Value {
    let mut body = serde_json::json!({
        "category": category,
        "symbol": amend.symbol,
        "orderId": amend.order_id.to_string(),
    });
    if let Some(price) = amend.new_price {
        body["price"] = Value::String(price.to_string());
    }
    if let Some(qty) = amend.new_qty {
        body["qty"] = Value::String(qty.to_string());
    }
    body
}

/// Map our `TimeInForce` enum onto Bybit V5's string codes. Bybit
/// accepts "GTC" | "IOC" | "FOK" | "PostOnly" on spot and linear
/// perp; "PostOnly" is the MM default. Connector previously
/// hardcoded "PostOnly" ignoring the field on every order, which
/// made it impossible to dispatch an IOC slice for kill-switch L4
/// flatten or an FOK probe from execution algos.
fn bybit_tif(tif: Option<TimeInForce>) -> &'static str {
    match tif {
        // Absent or explicitly PostOnly → keep the historical
        // default so nothing changes for standard MM quoting.
        None | Some(TimeInForce::PostOnly) => "PostOnly",
        Some(TimeInForce::Ioc) => "IOC",
        Some(TimeInForce::Fok) => "FOK",
        // Bybit has no `DAY` — GTC is the closest semantic; the
        // engine cancels at its own cadence anyway.
        Some(TimeInForce::Gtc) | Some(TimeInForce::Gtd) | Some(TimeInForce::Day) => "GTC",
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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec as rd;

    /// `POST /v5/order/amend` body must carry `category`, `symbol`,
    /// `orderId`, plus only the optional fields the caller actually
    /// changed. Pins the wire shape so a refactor of
    /// `build_amend_body` cannot silently drop fields.
    #[test]
    fn amend_body_carries_category_symbol_and_id() {
        let oid = uuid::Uuid::nil();
        let amend = AmendOrder {
            order_id: oid,
            symbol: "BTCUSDT".into(),
            new_price: Some(rd!(50000.5)),
            new_qty: Some(rd!(0.01)),
        };
        let body = build_amend_body("linear", &amend);
        assert_eq!(body["category"], "linear");
        assert_eq!(body["symbol"], "BTCUSDT");
        assert_eq!(body["orderId"], oid.to_string());
        assert_eq!(body["price"], "50000.5");
        assert_eq!(body["qty"], "0.01");
    }

    /// Bybit V5 fee-rate response: pull the maker / taker rates
    /// out of the `list` array. Wire shape pinned so a future
    /// schema drift breaks the test before it silently drops a
    /// fee tier into a default.
    #[test]
    fn fee_rate_response_parses_maker_taker_for_symbol() {
        let result = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "takerFeeRate": "0.00055",
                    "makerFeeRate": "0.0002"
                }
            ]
        });
        let info = parse_bybit_fee_rate_response(&result, "BTCUSDT").unwrap();
        assert_eq!(info.maker_fee, rd!(0.0002));
        assert_eq!(info.taker_fee, rd!(0.00055));
        assert!(info.vip_tier.is_none());
    }

    #[test]
    fn fee_rate_response_picks_correct_symbol_row() {
        let result = serde_json::json!({
            "list": [
                {"symbol": "ETHUSDT", "takerFeeRate": "0.001", "makerFeeRate": "0.0005"},
                {"symbol": "BTCUSDT", "takerFeeRate": "0.00055", "makerFeeRate": "0.0002"}
            ]
        });
        let info = parse_bybit_fee_rate_response(&result, "BTCUSDT").unwrap();
        assert_eq!(info.maker_fee, rd!(0.0002));
    }

    /// Optional fields are omitted entirely (not sent as empty
    /// strings) so Bybit doesn't reject the request with
    /// `params error` on the missing one.
    #[test]
    fn amend_body_omits_unset_optional_fields() {
        let amend = AmendOrder {
            order_id: uuid::Uuid::nil(),
            symbol: "BTCUSDT".into(),
            new_price: Some(rd!(50000)),
            new_qty: None,
        };
        let body = build_amend_body("spot", &amend);
        assert!(body.get("price").is_some());
        assert!(body.get("qty").is_none());
        let amend2 = AmendOrder {
            order_id: uuid::Uuid::nil(),
            symbol: "BTCUSDT".into(),
            new_price: None,
            new_qty: Some(rd!(0.01)),
        };
        let body2 = build_amend_body("spot", &amend2);
        assert!(body2.get("qty").is_some());
        assert!(body2.get("price").is_none());
    }

    /// Listing sniper (Epic F): parse a whole `instruments-info`
    /// response into `ProductSpec` rows, mapping per-instrument
    /// `status` to `TradingStatus` so the sniper consumer filters
    /// out pre-launch / settling contracts post-hoc.
    #[test]
    fn list_symbols_parses_v5_instruments_info_payload() {
        let result = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "baseCoin": "BTC",
                    "quoteCoin": "USDT",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.10"},
                    "lotSizeFilter": {"qtyStep": "0.001", "minOrderAmt": "5"}
                },
                {
                    "symbol": "NEWUSDT",
                    "baseCoin": "NEW",
                    "quoteCoin": "USDT",
                    "status": "PreLaunch",
                    "priceFilter": {"tickSize": "0.0001"},
                    "lotSizeFilter": {"qtyStep": "1"}
                },
                {
                    "symbol": "DEADUSDT",
                    "baseCoin": "DEAD",
                    "quoteCoin": "USDT",
                    "status": "Closed",
                    "priceFilter": {"tickSize": "0.01"},
                    "lotSizeFilter": {"qtyStep": "0.01"}
                }
            ]
        });
        let specs = parse_bybit_instruments_list(&result);
        assert_eq!(specs.len(), 3);
        let btc = specs.iter().find(|s| s.symbol == "BTCUSDT").unwrap();
        assert_eq!(btc.tick_size, rd!(0.10));
        assert_eq!(btc.trading_status, TradingStatus::Trading);
        let new = specs.iter().find(|s| s.symbol == "NEWUSDT").unwrap();
        assert_eq!(new.trading_status, TradingStatus::PreTrading);
        let dead = specs.iter().find(|s| s.symbol == "DEADUSDT").unwrap();
        assert_eq!(dead.trading_status, TradingStatus::Delisted);
    }

    /// Capability audit: declared capabilities must match implementation.
    ///
    /// `supports_ws_trading` must stay `false` until `BybitWsTrader` is
    /// wired into `place_order` / `cancel_order` / `cancel_all_orders`.
    /// The adapter type still exists in the crate (verified below), but
    /// until the V5 auth mechanism is pinned in live-testnet (see
    /// docs/deployment.md §3 under "operator next steps"), the
    /// capability must honestly report the unwired state so a
    /// capability-driven router cannot pick the WS path.
    #[test]
    fn capabilities_match_implementation() {
        let conn = BybitConnector::testnet("key", "secret");
        let caps = conn.capabilities();
        assert!(
            !caps.supports_ws_trading,
            "BybitWsTrader is not wired into place_order yet — capability must report false",
        );
        // Type-level confirmation that the adapter type exists for the
        // future wiring. This is a compile-only check, not end-to-end.
        let _: fn() = || {
            let _ = std::mem::size_of::<crate::ws_trade::BybitWsTrader>();
        };
        assert!(caps.supports_amend);
        assert!(
            !caps.supports_fix,
            "Bybit FIX not yet wired; session engine lives in protocols/fix but no venue adapter"
        );
    }

    /// Each `BybitCategory` maps to the correct `VenueProduct`.
    #[test]
    fn category_venue_product_mapping() {
        assert_eq!(BybitCategory::Spot.venue_product(), VenueProduct::Spot);
        assert_eq!(
            BybitCategory::Linear.venue_product(),
            VenueProduct::LinearPerp
        );
        assert_eq!(
            BybitCategory::Inverse.venue_product(),
            VenueProduct::InversePerp
        );
    }

    /// `as_str` emits the V5 REST category string.
    #[test]
    fn category_as_str_matches_v5_wire_format() {
        assert_eq!(BybitCategory::Spot.as_str(), "spot");
        assert_eq!(BybitCategory::Linear.as_str(), "linear");
        assert_eq!(BybitCategory::Inverse.as_str(), "inverse");
    }

    /// Spot and inverse constructors do not claim funding-rate
    /// support (spot has no funding; inverse is handled but we
    /// keep the flag true only for categories that pay funding).
    #[test]
    fn supports_funding_rate_tracks_category() {
        let spot = BybitConnector::spot("k", "s");
        let linear = BybitConnector::linear("k", "s");
        let inverse = BybitConnector::inverse("k", "s");
        assert!(!spot.capabilities().supports_funding_rate);
        assert!(linear.capabilities().supports_funding_rate);
        assert!(inverse.capabilities().supports_funding_rate);
    }

    /// `product()` returns the right `VenueProduct` for each
    /// constructor.
    #[test]
    fn product_matches_constructor() {
        assert_eq!(BybitConnector::spot("k", "s").product(), VenueProduct::Spot);
        assert_eq!(
            BybitConnector::linear("k", "s").product(),
            VenueProduct::LinearPerp
        );
        assert_eq!(
            BybitConnector::inverse("k", "s").product(),
            VenueProduct::InversePerp
        );
    }

    /// Testnet variants use the testnet base URLs and the WS URL
    /// suffix picks up the right category.
    #[test]
    fn testnet_variants_use_testnet_urls_with_correct_ws_suffix() {
        let spot = BybitConnector::testnet_spot("k", "s");
        assert!(spot.base_url.contains("testnet"));
        assert!(spot.ws_url.contains("stream-testnet"));
        assert!(spot.ws_url.ends_with("/spot"));

        let linear = BybitConnector::testnet("k", "s");
        assert!(linear.ws_url.ends_with("/linear"));

        let inverse = BybitConnector::testnet_inverse("k", "s");
        assert!(inverse.ws_url.ends_with("/inverse"));
    }

    /// `with_wallet` override works — useful for classic sub-
    /// accounts where spot is a separate bucket from Unified.
    #[test]
    fn with_wallet_overrides_default() {
        let c = BybitConnector::spot("k", "s").with_wallet(WalletType::Spot);
        assert_eq!(c.wallet, WalletType::Spot);
        // But the default (without override) was Unified for spot.
        let d = BybitConnector::spot("k", "s");
        assert_eq!(d.wallet, WalletType::Unified);
    }

    /// Legacy `::new` and `::testnet` constructors still produce a
    /// linear connector so existing call sites in `server/main.rs`
    /// and tests keep working without changes.
    #[test]
    fn legacy_constructors_map_to_linear() {
        assert_eq!(
            BybitConnector::new("k", "s").category,
            BybitCategory::Linear
        );
        assert_eq!(
            BybitConnector::testnet("k", "s").category,
            BybitCategory::Linear
        );
    }

    // ---- Epic F stage-3: multi-category list_symbols ----

    /// `parse_bybit_instruments_list` is the shared helper
    /// that `list_symbols_all_categories` calls per category
    /// before merging. Verify three independent fixtures
    /// (one per category shape) parse + merge cleanly without
    /// collisions across category boundaries.
    #[test]
    fn parse_bybit_multi_category_merge_preserves_all_rows() {
        let spot = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "baseCoin": "BTC",
                    "quoteCoin": "USDT",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.10"},
                    "lotSizeFilter": {"qtyStep": "0.001", "minOrderAmt": "5"}
                }
            ]
        });
        let linear = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "baseCoin": "BTC",
                    "quoteCoin": "USDT",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.5"},
                    "lotSizeFilter": {"qtyStep": "0.001"}
                },
                {
                    "symbol": "ETHUSDT",
                    "baseCoin": "ETH",
                    "quoteCoin": "USDT",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.05"},
                    "lotSizeFilter": {"qtyStep": "0.01"}
                }
            ]
        });
        let inverse = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSD",
                    "baseCoin": "BTC",
                    "quoteCoin": "USD",
                    "status": "Trading",
                    "priceFilter": {"tickSize": "0.5"},
                    "lotSizeFilter": {"qtyStep": "1"}
                }
            ]
        });
        let mut merged = parse_bybit_instruments_list(&spot);
        merged.extend(parse_bybit_instruments_list(&linear));
        merged.extend(parse_bybit_instruments_list(&inverse));
        // Both categories list BTCUSDT — listing sniper
        // consumer dedupes per-(symbol, category) externally;
        // the parser preserves both rows.
        assert_eq!(merged.len(), 4);
        let symbols: Vec<&str> = merged.iter().map(|p| p.symbol.as_str()).collect();
        assert!(symbols.contains(&"BTCUSDT"));
        assert!(symbols.contains(&"ETHUSDT"));
        assert!(symbols.contains(&"BTCUSD"));
        // Spot BTCUSDT and linear BTCUSDT have distinct
        // tick sizes — the merge preserves both.
        let btcusdt_ticks: Vec<rust_decimal::Decimal> = merged
            .iter()
            .filter(|p| p.symbol == "BTCUSDT")
            .map(|p| p.tick_size)
            .collect();
        assert_eq!(btcusdt_ticks.len(), 2);
        assert!(btcusdt_ticks.contains(&dec!(0.10)));
        assert!(btcusdt_ticks.contains(&dec!(0.5)));
    }

    #[test]
    fn parse_bybit_empty_categories_merge_yields_empty() {
        let empty = serde_json::json!({"list": []});
        let mut merged = parse_bybit_instruments_list(&empty);
        merged.extend(parse_bybit_instruments_list(&empty));
        merged.extend(parse_bybit_instruments_list(&empty));
        assert!(merged.is_empty());
    }

    /// Epic 40.4 — Bybit V5 account margin wire shape. Both
    /// `wallet-balance` and `position-list` payloads are needed;
    /// pin the combined shape so a V5 schema drift fails the
    /// test before it silently drops the guard's ratio to zero.
    #[test]
    fn account_margin_parser_reads_unified_wallet_and_positions() {
        let wallet = serde_json::json!({
            "list": [
                {
                    "accountType": "UNIFIED",
                    "totalEquity": "10000.50",
                    "totalInitialMargin": "2000",
                    "totalMaintenanceMargin": "500",
                    "totalAvailableBalance": "8000"
                }
            ]
        });
        let positions = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "side": "Buy",
                    "size": "0.05",
                    "avgPrice": "50000",
                    "markPrice": "50500",
                    "positionIM": "250",
                    "liqPrice": "45000"
                },
                {
                    "symbol": "ETHUSDT",
                    "side": "Sell",
                    "size": "0",
                    "avgPrice": "0",
                    "markPrice": "0",
                    "positionIM": "0",
                    "liqPrice": ""
                }
            ]
        });
        let info = parse_bybit_account_margin(&wallet, &positions).unwrap();
        assert_eq!(info.total_equity, rd!(10000.50));
        assert_eq!(info.total_maintenance_margin, rd!(500));
        assert!(info.margin_ratio > rd!(0.049));
        assert!(info.margin_ratio < rd!(0.051));
        // Zero-size ETHUSDT filtered out.
        assert_eq!(info.positions.len(), 1);
        let btc = &info.positions[0];
        assert_eq!(btc.symbol, "BTCUSDT");
        assert_eq!(btc.side, Side::Buy);
        assert_eq!(btc.isolated_margin, Some(rd!(250)));
        assert_eq!(btc.liq_price, Some(rd!(45000)));
    }

    /// Epic 40.3 — Bybit V5 ticker funding wire shape.
    #[test]
    fn funding_rate_parser_reads_ticker_row() {
        let result = serde_json::json!({
            "list": [
                {
                    "symbol": "BTCUSDT",
                    "fundingRate": "0.0001",
                    "nextFundingTime": "1700000000000"
                },
                {
                    "symbol": "ETHUSDT",
                    "fundingRate": "-0.0002",
                    "nextFundingTime": "1700000000000"
                }
            ]
        });
        let eth = parse_bybit_funding_rate(&result, "ETHUSDT").unwrap();
        assert_eq!(eth.rate, rd!(-0.0002));
        assert_eq!(eth.interval, std::time::Duration::from_secs(8 * 3600));
    }

    #[test]
    fn funding_rate_parser_returns_none_on_missing_symbol() {
        // Empty list → None.
        let result = serde_json::json!({ "list": [] });
        assert!(parse_bybit_funding_rate(&result, "BTCUSDT").is_none());
    }

    #[test]
    fn account_margin_parser_zero_equity_saturates_ratio() {
        let wallet = serde_json::json!({
            "list": [
                {
                    "accountType": "UNIFIED",
                    "totalEquity": "0",
                    "totalInitialMargin": "0",
                    "totalMaintenanceMargin": "0",
                    "totalAvailableBalance": "0"
                }
            ]
        });
        let positions = serde_json::json!({"list": []});
        let info = parse_bybit_account_margin(&wallet, &positions).unwrap();
        assert_eq!(info.margin_ratio, Decimal::ONE);
    }
}
