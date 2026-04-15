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
            },
            ws_trader: None,
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
        let side_buy = matches!(order.side, Side::Buy);
        let tif_str = match order.time_in_force {
            Some(TimeInForce::Gtc)
            | Some(TimeInForce::PostOnly)
            | Some(TimeInForce::Gtd)
            | None => "GTC",
            Some(TimeInForce::Ioc) => "IOC",
            Some(TimeInForce::Fok) => "FOK",
            // Binance spot has no `DAY` — fall back to GTC and let
            // the engine cancel at session close if it cares.
            Some(TimeInForce::Day) => "GTC",
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
                            .place_limit_order(
                                &order.symbol,
                                side_buy,
                                &price_str,
                                &qty_str,
                                tif_str,
                                Some(&cloid),
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

        // P2.3: parse the venue's per-symbol trading status so
        // the lifecycle manager can detect halts/resumes/delistings
        // on the periodic refresh cadence.
        let trading_status = match sym.get("status").and_then(|v| v.as_str()) {
            Some("TRADING") => TradingStatus::Trading,
            Some("HALT") => TradingStatus::Halted,
            Some("BREAK") | Some("END_OF_DAY") | Some("POST_TRADING") => TradingStatus::Break,
            Some("PRE_TRADING") | Some("AUCTION_MATCH") => TradingStatus::PreTrading,
            // Binance Spot does not surface a `DELISTED` status —
            // delisted symbols disappear from `exchangeInfo`
            // entirely and the call returns "symbol not found",
            // which the lifecycle manager treats as `Delisted`.
            Some(_) | None => TradingStatus::Trading,
        };

        Ok(ProductSpec {
            symbol: symbol.to_string(),
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

    async fn health_check(&self) -> anyhow::Result<bool> {
        let url = format!("{}/api/v3/ping", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
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
}
