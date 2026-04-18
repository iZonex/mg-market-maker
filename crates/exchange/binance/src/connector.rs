use std::sync::Arc;
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
use crate::ws_trade::BinanceWsTrader;

/// Binance Spot + Futures connector implementing ExchangeConnector.
pub struct BinanceConnector {
    client: Client,
    base_url: String,
    ws_url: String,
    api_key: String,
    api_secret: String,
    rate_limiter: RateLimiter,
    capabilities: VenueCapabilities,
    /// Optional WS API trader. When set and connected, `place_order` /
    /// `cancel_order` / `cancel_all_orders` go through WS first with
    /// REST as the fallback on disconnect or WS-side error.
    ws_trader: Option<Arc<BinanceWsTrader>>,
    /// Fail-closed withdraw address whitelist (Epic 8). Threaded
    /// into the `withdraw()` impl via `validate_withdraw_address`.
    /// `None` leaves the venue-side whitelist as the only control.
    withdraw_whitelist: Option<Vec<String>>,
}

impl BinanceConnector {
    /// Attach a fail-closed withdraw address whitelist (Epic 8).
    /// See `validate_withdraw_address` for semantics.
    pub fn with_withdraw_whitelist(mut self, list: Option<Vec<String>>) -> Self {
        self.withdraw_whitelist = list;
        self
    }

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
                // Binance Spot has only `order.cancelReplace`, which
                // is a cancel + place under the hood — it loses
                // queue priority. There is no native amend endpoint
                // on `/api/v3`, so we honestly report no support
                // and let the engine's amend planner fall back to
                // cancel+place on this venue. Binance USDⓈ-M
                // futures does have a real `PUT /fapi/v1/order`
                // amend — that capability is reported on the
                // separate `BinanceFuturesConnector`.
                supports_amend: false,
                supports_ws_trading: true,
                // No FIX adapter exists in this crate — the generic
                // session engine lives in `crates/protocols/fix` but no
                // Binance-specific fix_trade.rs wires it up. Until a
                // venue adapter lands (see docs/deployment.md "FIX venue
                // adapters" under operator next steps), this flag MUST
                // report `false` so a capability-driven router never
                // picks Binance for a FIX route.
                supports_fix: false,
                max_order_rate: 300,          // per 10s.
                supports_funding_rate: false, // spot has no funding
                supports_margin_info: false,  // spot — margin is N/A
                supports_margin_mode: false,
            },
            ws_trader: None,
            withdraw_whitelist: None,
        }
    }

    /// Enable the WebSocket API path for order entry. Connects to
    /// `wss://ws-api.binance.com/ws-api/v3` by default (or the matching
    /// testnet endpoint if called after `testnet()`). Must be invoked
    /// before handing the connector to the engine.
    pub fn enable_ws_trading(&mut self, ws_api_url: &str) {
        let trader = BinanceWsTrader::connect(ws_api_url, &self.api_key, &self.api_secret);
        self.ws_trader = Some(Arc::new(trader));
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

    fn classify_error(&self, err: &anyhow::Error) -> mm_exchange_core::VenueError {
        crate::classify(err)
    }

    fn product(&self) -> VenueProduct {
        // This connector targets Binance SPOT (`/api/v3/*` base). A
        // separate `BinanceFuturesConnector` (Sprint D) handles
        // `/fapi/v1/*`.
        VenueProduct::Spot
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
        // Binance's two WS endpoints branch off the same host:
        //   wss://stream.binance.com:9443/ws/<listenKey>      (user-data)
        //   wss://stream.binance.com:9443/stream?streams=…    (combined)
        // Historical `ws_url` values include the trailing `/ws`
        // because the user-data stream needed it. Strip it here so
        // the combined-stream URL does not become `/ws/stream?…`
        // (Binance responds 404 to that shape — observed 2026-04-17).
        let base = self
            .ws_url
            .strip_suffix("/ws")
            .unwrap_or(&self.ws_url)
            .trim_end_matches('/');
        let url = format!("{base}/stream?streams={stream_param}");

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
        let side_buy = matches!(order.side, Side::Buy);
        // PostOnly on Binance Spot is sent as `type=LIMIT_MAKER`,
        // NOT `timeInForce=POST_ONLY`. The venue rejects the order
        // if it would cross (−2010 "Order would immediately match")
        // instead of executing as a taker. Mapping PostOnly to
        // `GTC` here let taker fills slip through — the exact bug
        // the MM-pro review caught (Epic 36.1).
        let is_post_only = matches!(order.time_in_force, Some(TimeInForce::PostOnly));
        let tif_str = match order.time_in_force {
            Some(TimeInForce::Gtc) | Some(TimeInForce::Gtd) | None => "GTC",
            Some(TimeInForce::Ioc) => "IOC",
            Some(TimeInForce::Fok) => "FOK",
            // Binance spot has no `DAY` — fall back to GTC and let
            // the engine cancel at session close if it cares.
            Some(TimeInForce::Day) => "GTC",
            // LIMIT_MAKER is encoded at the `type=` level below;
            // the timeInForce field is omitted for that path.
            Some(TimeInForce::PostOnly) => "GTC", // never serialised
        };
        let order_id = uuid::Uuid::new_v4();
        let cloid = order
            .client_order_id
            .clone()
            .unwrap_or_else(|| order_id.to_string());

        // WS path first when available and connected.
        if order.order_type == OrderType::Limit {
            if let Some(ws) = self.ws_trader.as_ref() {
                if ws.is_connected() {
                    if let Some(price) = &order.price {
                        let price_str = price.to_string();
                        let qty_str = order.qty.to_string();
                        let ws_t0 = Instant::now();
                        let ws_result = ws
                            .place_limit_order_opts(
                                &order.symbol,
                                side_buy,
                                &price_str,
                                &qty_str,
                                tif_str,
                                Some(&cloid),
                                is_post_only,
                            )
                            .await;
                        let ws_elapsed = ws_t0.elapsed().as_secs_f64();
                        ORDER_ENTRY_LATENCY
                            .with_label_values(&["binance", "ws", "place_order"])
                            .observe(ws_elapsed);
                        match ws_result {
                            Ok(resp) => {
                                let binance_id =
                                    resp.get("orderId").and_then(|v| v.as_u64()).unwrap_or(0);
                                debug!(
                                    %order_id,
                                    binance_id,
                                    path = "ws",
                                    "placed order on Binance"
                                );
                                return Ok(order_id);
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    "Binance WS place_order failed; falling back to REST"
                                );
                            }
                        }
                    }
                }
            }
        }

        // REST fallback (original path).
        let side = if side_buy { "BUY" } else { "SELL" };
        let order_type = match (order.order_type, is_post_only) {
            // Post-only limit — Binance Spot expects LIMIT_MAKER
            // (no timeInForce field at all).
            (OrderType::Limit, true) => "LIMIT_MAKER",
            (OrderType::Limit, false) => "LIMIT",
            (OrderType::Market, _) => "MARKET",
        };
        let mut params = format!(
            "symbol={}&side={}&type={}&quantity={}",
            order.symbol, side, order_type, order.qty
        );
        if let Some(price) = &order.price {
            params.push_str(&format!("&price={price}"));
        }
        // LIMIT_MAKER carries no timeInForce (venue would 400).
        if order.order_type == OrderType::Limit && !is_post_only {
            params.push_str(&format!("&timeInForce={tif_str}"));
        }
        params.push_str(&format!("&newClientOrderId={cloid}"));

        let rest_t0 = Instant::now();
        let rest_result = self.signed_post("/api/v3/order", &params).await;
        ORDER_ENTRY_LATENCY
            .with_label_values(&["binance", "rest", "place_order"])
            .observe(rest_t0.elapsed().as_secs_f64());
        let resp = rest_result?;
        let binance_id = resp.get("orderId").and_then(|v| v.as_u64()).unwrap_or(0);
        debug!(%order_id, binance_id, path = "rest", "placed order on Binance");
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
        let cloid = order_id.to_string();

        if let Some(ws) = self.ws_trader.as_ref() {
            if ws.is_connected() {
                match ws.cancel_order(symbol, &cloid).await {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        warn!(error = %e, "Binance WS cancel_order failed; falling back to REST");
                    }
                }
            }
        }

        let params = format!("symbol={symbol}&origClientOrderId={cloid}");
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
        if let Some(ws) = self.ws_trader.as_ref() {
            if ws.is_connected() {
                match ws.cancel_all(symbol).await {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        warn!(error = %e, "Binance WS cancel_all failed; falling back to REST");
                    }
                }
            }
        }

        let params = format!("symbol={symbol}");
        self.signed_delete("/api/v3/openOrders", &params).await?;
        Ok(())
    }

    async fn get_open_orders(&self, symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
        let resp = self
            .signed_get("/api/v3/openOrders", &format!("symbol={symbol}"))
            .await?;
        let orders = resp.as_array().cloned().unwrap_or_default();
        Ok(orders
            .iter()
            .filter_map(|o| {
                let order_id_str = o.get("clientOrderId")?.as_str()?;
                let order_id = order_id_str.parse().ok()?;
                let side_str = o.get("side")?.as_str()?;
                let side = match side_str {
                    "BUY" => Side::Buy,
                    "SELL" => Side::Sell,
                    _ => return None,
                };
                let price: Decimal = o.get("price")?.as_str()?.parse().ok()?;
                let qty: Decimal = o.get("origQty")?.as_str()?.parse().ok()?;
                let filled_qty: Decimal = o.get("executedQty")?.as_str()?.parse().ok()?;
                let status_str = o.get("status")?.as_str()?;
                let status = match status_str {
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
                    wallet: WalletType::Spot,
                    total: free + locked,
                    locked,
                    available: free,
                })
            })
            .filter(|b| b.total > dec!(0))
            .collect())
    }

    async fn server_time_ms(&self) -> anyhow::Result<Option<i64>> {
        self.rate_limiter.acquire(1).await;
        let url = format!("{}/api/v3/time", self.base_url);
        let resp: Value = self.client.get(&url).send().await?.json().await?;
        Ok(resp.get("serverTime").and_then(|v| v.as_i64()))
    }

    async fn get_24h_volume_usd(
        &self,
        symbol: &str,
    ) -> anyhow::Result<Option<rust_decimal::Decimal>> {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        self.rate_limiter.acquire(1).await;
        let url = format!("{}/api/v3/ticker/24hr?symbol={}", self.base_url, symbol);
        let resp: Value = self.client.get(&url).send().await?.json().await?;
        // quoteVolume = sum of price*qty over 24h, already in quote
        // currency (USDT for a *USDT pair). Classifier reads this
        // as a USD-ish proxy for the pair's liquidity tier.
        let vol = resp
            .get("quoteVolume")
            .and_then(|v| v.as_str())
            .and_then(|s| Decimal::from_str(s).ok());
        Ok(vol)
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

        parse_binance_spot_symbol(sym)
            .ok_or_else(|| anyhow::anyhow!("malformed Binance spot symbol entry for {symbol}"))
    }

    /// List every symbol Binance spot currently advertises via
    /// `GET /api/v3/exchangeInfo` (no `symbol=` parameter). Every
    /// row is mapped through the same helper as `get_product_spec`
    /// so the two call sites stay in lockstep on filter parsing and
    /// trading-status mapping. Symbols in non-trading states
    /// (`BREAK`, `HALT`, `PRE_TRADING`, …) are still returned so
    /// the Epic F listing sniper sees new listings during their
    /// auction phase; the consumer filters by `trading_status`.
    async fn list_symbols(&self) -> anyhow::Result<Vec<ProductSpec>> {
        // `/api/v3/exchangeInfo` with no symbol is weight 10 on
        // Binance spot — same cost as the per-symbol path above.
        self.rate_limiter.acquire(10).await;
        let url = format!("{}/api/v3/exchangeInfo", self.base_url);
        let resp: Value = self.client.get(&url).send().await?.json().await?;
        Ok(parse_binance_spot_symbols_array(&resp))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        let url = format!("{}/api/v3/ping", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    async fn rate_limit_remaining(&self) -> u32 {
        self.rate_limiter.remaining().await
    }

    /// Per-account fee tier for `symbol` from
    /// `GET /sapi/v1/asset/tradeFee`. Binance returns either an
    /// array (one row per symbol) or a single object — the
    /// helper handles both shapes.
    async fn fetch_fee_tiers(&self, symbol: &str) -> Result<FeeTierInfo, FeeTierError> {
        let resp = self
            .signed_get("/sapi/v1/asset/tradeFee", &format!("symbol={symbol}"))
            .await
            .map_err(|e| FeeTierError::Other(anyhow::anyhow!("{e}")))?;
        parse_binance_spot_fee_response(&resp, symbol)
            .ok_or_else(|| FeeTierError::Other(anyhow::anyhow!("no fee row for {symbol}")))
    }

    /// Current borrow rate for `asset` from
    /// `GET /sapi/v1/margin/interestRateHistory?asset=&size=1`.
    /// Binance returns the most recent **daily** interest rate
    /// row first; the pure helper converts the daily fraction
    /// into an APR fraction (`× 365`) so the engine's
    /// `BorrowManager` only ever speaks APR.
    ///
    /// P1.3 stage-1: rate fetch only — `borrow_asset` /
    /// `repay_asset` remain `NotSupported` until margin-mode
    /// order routing lands in stage-2.
    async fn get_borrow_rate(&self, asset: &str) -> Result<BorrowRateInfo, BorrowError> {
        let resp = self
            .signed_get(
                "/sapi/v1/margin/interestRateHistory",
                &format!("asset={asset}&size=1"),
            )
            .await
            .map_err(|e| BorrowError::Other(anyhow::anyhow!("{e}")))?;
        parse_binance_borrow_rate_response(&resp, asset)
            .ok_or_else(|| BorrowError::Other(anyhow::anyhow!("no borrow rate for {asset}")))
    }

    /// Withdraw to an external address via
    /// `POST /sapi/v1/capital/withdraw/apply`. Enforces the
    /// configured `withdraw_whitelist` before hitting the
    /// network — a compromised trading key cannot drain funds
    /// to an attacker-controlled address.
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
        let params = format!("coin={asset}&amount={qty}&address={address}&network={network}");
        let resp = self
            .signed_post("/sapi/v1/capital/withdraw/apply", &params)
            .await?;
        resp.get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("missing withdraw id in response"))
    }

    /// Internal transfer between Binance wallets via
    /// `POST /sapi/v1/asset/transfer`.
    async fn internal_transfer(
        &self,
        asset: &str,
        qty: rust_decimal::Decimal,
        from_wallet: &str,
        to_wallet: &str,
    ) -> anyhow::Result<String> {
        let transfer_type = binance_transfer_type(from_wallet, to_wallet)?;
        let params = format!("type={transfer_type}&asset={asset}&amount={qty}");
        let resp = self.signed_post("/sapi/v1/asset/transfer", &params).await?;
        resp.get("tranId")
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow::anyhow!("missing tranId in response"))
    }
}

/// Map generic wallet names to Binance transfer type enum.
fn binance_transfer_type(from: &str, to: &str) -> anyhow::Result<&'static str> {
    match (from, to) {
        ("SPOT", "FUTURES") | ("MAIN", "UMFUTURE") => Ok("MAIN_UMFUTURE"),
        ("FUTURES", "SPOT") | ("UMFUTURE", "MAIN") => Ok("UMFUTURE_MAIN"),
        ("SPOT", "MARGIN") | ("MAIN", "MARGIN") => Ok("MAIN_MARGIN"),
        ("MARGIN", "SPOT") | ("MARGIN", "MAIN") => Ok("MARGIN_MAIN"),
        ("SPOT", "CMFUTURE") | ("MAIN", "CMFUTURE") => Ok("MAIN_CMFUTURE"),
        ("CMFUTURE", "SPOT") | ("CMFUTURE", "MAIN") => Ok("CMFUTURE_MAIN"),
        _ => Err(anyhow::anyhow!(
            "unsupported Binance transfer: {from} → {to}"
        )),
    }
}

/// Parse the response from
/// `GET /sapi/v1/margin/interestRateHistory?asset=&size=1` into a
/// `BorrowRateInfo`. Binance returns the rate as a **daily**
/// fraction (`dailyInterestRate`) — multiply by 365 to get the
/// APR the engine's `BorrowManager` consumes. Pure helper so the
/// wire shape is unit-tested without an HTTP client.
pub(crate) fn parse_binance_borrow_rate_response(
    resp: &Value,
    asset: &str,
) -> Option<BorrowRateInfo> {
    let row = match resp {
        Value::Array(arr) => arr.first()?,
        Value::Object(_) => resp,
        _ => return None,
    };
    let daily_rate: Decimal = row.get("dailyInterestRate")?.as_str()?.parse().ok()?;
    let apr = daily_rate * Decimal::from(365u32);
    Some(BorrowRateInfo::from_apr(asset, apr))
}

/// Parse the response from `GET /sapi/v1/asset/tradeFee?symbol=`
/// into a `FeeTierInfo`. Binance returns a JSON array of
/// `{ symbol, makerCommission, takerCommission }` rows even when
/// querying a single symbol — pure helper so the wire shape is
/// unit-tested without an HTTP client.
pub(crate) fn parse_binance_spot_fee_response(resp: &Value, symbol: &str) -> Option<FeeTierInfo> {
    let row = match resp {
        Value::Array(arr) => arr
            .iter()
            .find(|r| r.get("symbol").and_then(|s| s.as_str()) == Some(symbol))
            .or_else(|| arr.first())?,
        Value::Object(_) => resp,
        _ => return None,
    };
    let maker_fee: Decimal = row.get("makerCommission")?.as_str()?.parse().ok()?;
    let taker_fee: Decimal = row.get("takerCommission")?.as_str()?.parse().ok()?;
    Some(FeeTierInfo {
        maker_fee,
        taker_fee,
        vip_tier: None,
        fetched_at: chrono::Utc::now(),
    })
}

pub(crate) fn parse_binance_event(stream: &str, data: &Value) -> Option<MarketEvent> {
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

/// Parse a single `symbols[]` entry from `/api/v3/exchangeInfo`
/// into a [`ProductSpec`]. Shared between `get_product_spec`
/// (single-symbol path) and `list_symbols` (whole-universe path)
/// so the two call sites stay in lockstep on filter parsing and
/// trading-status mapping. Pure helper so the wire shape is
/// unit-tested without an HTTP client. Returns `None` when a row
/// lacks the `symbol` field (malformed response).
pub(crate) fn parse_binance_spot_symbol(sym: &Value) -> Option<ProductSpec> {
    let symbol = sym.get("symbol").and_then(|v| v.as_str())?.to_string();
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

    // P2.3: parse the venue's per-symbol trading status so
    // the lifecycle manager can detect halts/resumes/delistings.
    // The listing sniper (Epic F) also reads this so it can flag
    // PRE_TRADING / AUCTION_MATCH symbols during their listing
    // auction phase without having to re-query the venue.
    let trading_status = match sym.get("status").and_then(|v| v.as_str()) {
        Some("TRADING") => TradingStatus::Trading,
        Some("HALT") => TradingStatus::Halted,
        Some("BREAK") | Some("END_OF_DAY") | Some("POST_TRADING") => TradingStatus::Break,
        Some("PRE_TRADING") | Some("AUCTION_MATCH") => TradingStatus::PreTrading,
        // Binance Spot does not surface a `DELISTED` status —
        // delisted symbols disappear from `exchangeInfo`
        // entirely and the call returns "symbol not found".
        Some(_) | None => TradingStatus::Trading,
    };

    Some(ProductSpec {
        symbol,
        base_asset: base,
        quote_asset: quote,
        tick_size,
        lot_size,
        min_notional,
        maker_fee: dec!(0.001),
        taker_fee: dec!(0.001),
        trading_status,
    })
}

/// Parse the full `/api/v3/exchangeInfo` response body into the
/// list of [`ProductSpec`] entries used by `list_symbols`. Walks
/// `resp.symbols[]` and maps every row through
/// [`parse_binance_spot_symbol`]. Malformed rows are silently
/// dropped — the consumer sees `Ok(vec)` with the subset the
/// venue returned cleanly, not a hard error.
pub(crate) fn parse_binance_spot_symbols_array(resp: &Value) -> Vec<ProductSpec> {
    resp.get("symbols")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_binance_spot_symbol).collect())
        .unwrap_or_default()
}

pub(crate) fn parse_levels(value: Option<&Value>) -> anyhow::Result<Vec<PriceLevel>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `GET /sapi/v1/asset/tradeFee?symbol=BTCUSDT` returns a JSON
    /// array even for a single-symbol query — the helper must
    /// pick the right row out of the array.
    #[test]
    fn spot_fee_response_array_picks_correct_symbol() {
        let resp = serde_json::json!([
            {"symbol": "ETHUSDT", "makerCommission": "0.001", "takerCommission": "0.001"},
            {"symbol": "BTCUSDT", "makerCommission": "0.0008", "takerCommission": "0.001"}
        ]);
        let info = parse_binance_spot_fee_response(&resp, "BTCUSDT").unwrap();
        assert_eq!(info.maker_fee, dec!(0.0008));
        assert_eq!(info.taker_fee, dec!(0.001));
    }

    /// `dailyInterestRate` × 365 must round-trip through the
    /// `BorrowRateInfo::from_apr` helper into an APR fraction
    /// the BorrowManager understands. Pin the conversion so a
    /// future refactor cannot silently drop the × 365 step.
    #[test]
    fn borrow_rate_response_daily_to_apr() {
        let resp = serde_json::json!([
            {"asset": "BTC", "dailyInterestRate": "0.0001", "timestamp": 1_700_000_000_000_u64}
        ]);
        let info = parse_binance_borrow_rate_response(&resp, "BTC").unwrap();
        // 0.0001 × 365 = 0.0365 → 3.65 % APR
        assert_eq!(info.rate_apr, dec!(0.0365));
        assert_eq!(info.asset, "BTC");
        // 0.0365 × 10_000 / 8_760 ≈ 0.04167 bps/hour
        assert!(
            info.rate_bps_hourly > dec!(0.0416) && info.rate_bps_hourly < dec!(0.0417),
            "got {}",
            info.rate_bps_hourly
        );
    }

    /// Object-shape fallback so the parser is resilient to the
    /// edge case where Binance returns a single record rather
    /// than a wrapping array (mirrors the spot-fee parser).
    #[test]
    fn borrow_rate_response_accepts_object_shape() {
        let resp = serde_json::json!({
            "asset": "BTC",
            "dailyInterestRate": "0.0002"
        });
        let info = parse_binance_borrow_rate_response(&resp, "BTC").unwrap();
        assert_eq!(info.rate_apr, dec!(0.073));
    }

    /// Some Binance edge cases return a bare object instead of an
    /// array — the helper must accept that shape too so the parser
    /// is robust to either response form.
    #[test]
    fn spot_fee_response_object_shape_also_parses() {
        let resp = serde_json::json!({
            "symbol": "BTCUSDT",
            "makerCommission": "0.0009",
            "takerCommission": "0.001"
        });
        let info = parse_binance_spot_fee_response(&resp, "BTCUSDT").unwrap();
        assert_eq!(info.maker_fee, dec!(0.0009));
    }

    /// Listing sniper (Epic F): `parse_binance_spot_symbols_array`
    /// maps every `symbols[]` row through the shared helper. Pin
    /// the whole-universe shape so a schema drift breaks the test
    /// instead of silently dropping a symbol from the sniper's
    /// view of the venue.
    #[test]
    fn list_symbols_parses_full_exchange_info_response() {
        let resp = serde_json::json!({
            "symbols": [
                {
                    "symbol": "BTCUSDT",
                    "baseAsset": "BTC",
                    "quoteAsset": "USDT",
                    "status": "TRADING",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.00001"},
                        {"filterType": "NOTIONAL", "minNotional": "10"}
                    ]
                },
                {
                    "symbol": "ETHUSDT",
                    "baseAsset": "ETH",
                    "quoteAsset": "USDT",
                    "status": "TRADING",
                    "filters": [
                        {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                        {"filterType": "LOT_SIZE", "stepSize": "0.0001"},
                        {"filterType": "NOTIONAL", "minNotional": "10"}
                    ]
                },
                {
                    "symbol": "NEWUSDT",
                    "baseAsset": "NEW",
                    "quoteAsset": "USDT",
                    "status": "PRE_TRADING",
                    "filters": []
                }
            ]
        });
        let specs = parse_binance_spot_symbols_array(&resp);
        assert_eq!(specs.len(), 3);
        let btc = specs.iter().find(|s| s.symbol == "BTCUSDT").unwrap();
        assert_eq!(btc.base_asset, "BTC");
        assert_eq!(btc.quote_asset, "USDT");
        assert_eq!(btc.tick_size, dec!(0.01));
        assert_eq!(btc.trading_status, TradingStatus::Trading);
        let new = specs.iter().find(|s| s.symbol == "NEWUSDT").unwrap();
        // PRE_TRADING symbols are still returned — the sniper
        // consumer filters by trading_status post-hoc.
        assert_eq!(new.trading_status, TradingStatus::PreTrading);
    }

    /// Malformed rows (missing `symbol` field) are silently
    /// dropped so the sniper gets the subset the venue returned
    /// cleanly instead of a hard error.
    #[test]
    fn list_symbols_drops_malformed_rows_silently() {
        let resp = serde_json::json!({
            "symbols": [
                {"symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "status": "TRADING", "filters": []},
                {"baseAsset": "ETH", "quoteAsset": "USDT", "status": "TRADING", "filters": []}
            ]
        });
        let specs = parse_binance_spot_symbols_array(&resp);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].symbol, "BTCUSDT");
    }

    /// Empty response body (venue returned no `symbols` field)
    /// yields an empty vec rather than panicking.
    #[test]
    fn list_symbols_empty_response_is_empty_vec() {
        let resp = serde_json::json!({});
        assert!(parse_binance_spot_symbols_array(&resp).is_empty());
    }

    /// Capability audit: `supports_ws_trading` and `supports_fix` must
    /// reflect the actual presence of adapter types / session engines.
    #[test]
    fn capabilities_match_implementation() {
        let conn = BinanceConnector::testnet("key", "secret");
        let caps = conn.capabilities();
        assert!(
            caps.supports_ws_trading,
            "Binance declares WS trading — BinanceWsTrader must exist"
        );
        // Type-level confirmation:
        let _: fn() = || {
            let _ = std::mem::size_of::<crate::ws_trade::BinanceWsTrader>();
        };
        // FIX must be `false` until a `fix_trade.rs` adapter lands in
        // this crate (see docs/deployment.md "FIX venue adapters"). The
        // generic session engine in `crates/protocols/fix` is not a
        // substitute for a venue adapter.
        assert!(!caps.supports_fix);
        // Binance Spot has no native amend (only `order.cancelReplace`,
        // which loses queue priority). The capability flag must
        // honestly report `false` — the engine's amend planner reads
        // it to decide whether to fall back to cancel+place. Real
        // amend lives on `BinanceFuturesConnector`.
        assert!(!caps.supports_amend);
    }

    #[test]
    fn binance_transfer_type_mapping() {
        assert_eq!(
            super::binance_transfer_type("SPOT", "FUTURES").unwrap(),
            "MAIN_UMFUTURE"
        );
        assert_eq!(
            super::binance_transfer_type("FUTURES", "SPOT").unwrap(),
            "UMFUTURE_MAIN"
        );
        assert_eq!(
            super::binance_transfer_type("SPOT", "MARGIN").unwrap(),
            "MAIN_MARGIN"
        );
        assert!(super::binance_transfer_type("SPOT", "UNKNOWN").is_err());
    }
}
