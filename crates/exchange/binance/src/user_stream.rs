//! Binance user data stream (listen-key based).
//!
//! The connector at [`crate::connector::BinanceConnector`] subscribes
//! to public `depth` and `trade` streams only. That means **fills
//! from orders we did not place via WS-API** never reach the engine:
//!
//! - Orders placed via REST fallback
//! - Partial fills that arrive after the WS-API response envelope
//! - Orders placed manually from the UI
//! - RFQ or OTC trades touching the same account
//!
//! This module closes that gap by opening Binance's listen-key
//! user-data WebSocket and translating `executionReport` and
//! `outboundAccountPosition` messages into `MarketEvent::Fill`
//! and `MarketEvent::BalanceUpdate` events on the same channel
//! the main `subscribe` task uses. The futures variant follows the
//! same pattern on `/fapi/v1/listenKey` and
//! `wss://fstream.binance.com`.
//!
//! ## Listen-key lifecycle
//!
//! 1. `POST /api/v3/userDataStream` (or `/fapi/v1/listenKey` for
//!    futures) returns `{"listenKey": "..."}`. Listen key TTL is
//!    60 minutes on both endpoints.
//! 2. Open WS at `wss://stream.binance.com:9443/ws/<key>` (spot)
//!    or `wss://fstream.binance.com/ws/<key>` (futures).
//! 3. Every 30 minutes call `PUT /api/v3/userDataStream?listenKey=<key>`
//!    to extend the key's TTL. If the PUT fails, the key may have
//!    expired — obtain a fresh one and reconnect.
//! 4. On graceful shutdown, `DELETE /api/v3/userDataStream?listenKey=<key>`
//!    to tell Binance we're done. Optional — the key expires on its
//!    own.
//!
//! This module handles (1)–(3). (4) is a nice-to-have reserved for
//! when the engine gets a proper graceful-shutdown hook for
//! background tasks.

use std::time::Duration;

use mm_common::types::{Fill, Side, WalletType};
use mm_exchange_core::connector::{VenueId, VenueProduct};
use mm_exchange_core::events::MarketEvent;
use reqwest::Client;
use rust_decimal::Decimal;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Which listen-key endpoint to hit — differs between spot
/// (`/api/v3/userDataStream`) and futures
/// (`/fapi/v1/listenKey`). Also determines the WS host used
/// for the user-data channel and the wallet type tagged on
/// emitted `BalanceUpdate` events.
#[derive(Debug, Clone, Copy)]
pub enum UserStreamProduct {
    Spot,
    UsdMarginedFutures,
}

impl UserStreamProduct {
    fn listen_key_path(self) -> &'static str {
        match self {
            UserStreamProduct::Spot => "/api/v3/userDataStream",
            UserStreamProduct::UsdMarginedFutures => "/fapi/v1/listenKey",
        }
    }

    fn default_rest_base(self) -> &'static str {
        match self {
            UserStreamProduct::Spot => "https://api.binance.com",
            UserStreamProduct::UsdMarginedFutures => "https://fapi.binance.com",
        }
    }

    fn default_ws_host(self) -> &'static str {
        match self {
            UserStreamProduct::Spot => "wss://stream.binance.com:9443",
            UserStreamProduct::UsdMarginedFutures => "wss://fstream.binance.com",
        }
    }

    fn wallet(self) -> WalletType {
        match self {
            UserStreamProduct::Spot => WalletType::Spot,
            UserStreamProduct::UsdMarginedFutures => WalletType::UsdMarginedFutures,
        }
    }

    #[allow(dead_code)]
    fn venue_product(self) -> VenueProduct {
        match self {
            UserStreamProduct::Spot => VenueProduct::Spot,
            UserStreamProduct::UsdMarginedFutures => VenueProduct::LinearPerp,
        }
    }
}

/// Configuration for the user-data stream task.
#[derive(Debug, Clone)]
pub struct UserStreamConfig {
    pub api_key: String,
    pub product: UserStreamProduct,
    pub rest_base: String,
    pub ws_host: String,
}

impl UserStreamConfig {
    pub fn spot(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            product: UserStreamProduct::Spot,
            rest_base: UserStreamProduct::Spot.default_rest_base().to_string(),
            ws_host: UserStreamProduct::Spot.default_ws_host().to_string(),
        }
    }

    pub fn futures(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            product: UserStreamProduct::UsdMarginedFutures,
            rest_base: UserStreamProduct::UsdMarginedFutures
                .default_rest_base()
                .to_string(),
            ws_host: UserStreamProduct::UsdMarginedFutures
                .default_ws_host()
                .to_string(),
        }
    }
}

/// Handle returned by [`start`]. Dropping it **does not** stop the
/// background task — it shuts down only when the event channel's
/// receiver is dropped (the engine is gone).
pub struct UserDataStream;

/// Spawn a user-data stream task that writes events into `tx`.
///
/// The caller keeps the matching receiver and mixes these events
/// with the public stream from
/// [`crate::connector::BinanceConnector::subscribe`].
pub fn start(config: UserStreamConfig, tx: mpsc::UnboundedSender<MarketEvent>) -> UserDataStream {
    tokio::spawn(async move {
        let _ = run(config, tx).await;
    });
    UserDataStream
}

async fn run(config: UserStreamConfig, tx: mpsc::UnboundedSender<MarketEvent>) -> anyhow::Result<()> {
    let client = Client::new();
    loop {
        let listen_key = match obtain_listen_key(&client, &config).await {
            Ok(k) => k,
            Err(e) => {
                warn!(error = %e, "Binance listen key request failed");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        info!(product = ?config.product, "obtained Binance listen key");
        let keep = spawn_keepalive(client.clone(), config.clone(), listen_key.clone());
        let url = format!("{}/ws/{}", config.ws_host.trim_end_matches('/'), listen_key);

        if let Err(e) = stream_loop(&url, &tx, config.product).await {
            warn!(error = %e, "Binance user-data stream loop exited");
        }

        keep.abort();
        // Backoff before obtaining a fresh key.
        tokio::time::sleep(Duration::from_secs(2)).await;
        if tx.is_closed() {
            return Ok(());
        }
    }
}

async fn obtain_listen_key(client: &Client, config: &UserStreamConfig) -> anyhow::Result<String> {
    let url = format!(
        "{}{}",
        config.rest_base.trim_end_matches('/'),
        config.product.listen_key_path()
    );
    let resp = client
        .post(&url)
        .header("X-MBX-APIKEY", &config.api_key)
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        anyhow::bail!("listen key request {status}: {body}");
    }
    let key = body
        .get("listenKey")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("no listenKey in response"))?;
    Ok(key.to_string())
}

fn spawn_keepalive(client: Client, config: UserStreamConfig, key: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30 * 60));
        interval.tick().await; // fire once immediately after 30min, not at t=0
        loop {
            interval.tick().await;
            let url = format!(
                "{}{}?listenKey={}",
                config.rest_base.trim_end_matches('/'),
                config.product.listen_key_path(),
                key
            );
            match client
                .put(&url)
                .header("X-MBX-APIKEY", &config.api_key)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    debug!("Binance listen key keepalive ok");
                }
                Ok(r) => {
                    warn!(status = %r.status(), "Binance listen key keepalive rejected");
                    return;
                }
                Err(e) => {
                    warn!(error = %e, "Binance listen key keepalive failed");
                    return;
                }
            }
        }
    })
}

async fn stream_loop(
    url: &str,
    tx: &mpsc::UnboundedSender<MarketEvent>,
    product: UserStreamProduct,
) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    use tokio_tungstenite::connect_async;

    let (ws, _) = connect_async(url).await?;
    let _ = tx.send(MarketEvent::Connected {
        venue: VenueId::Binance,
    });
    let (_, mut read) = ws.split();
    while let Some(msg) = read.next().await {
        let msg = msg?;
        let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
            continue;
        };
        let v: Value = match serde_json::from_str(&text.to_string()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for evt in parse_user_event(&v, product) {
            if tx.send(evt).is_err() {
                return Ok(());
            }
        }
    }
    let _ = tx.send(MarketEvent::Disconnected {
        venue: VenueId::Binance,
    });
    anyhow::bail!("user data stream closed")
}

/// Parse one user-data WS frame into `MarketEvent`s. Exposed for
/// unit testing without a live server.
pub(crate) fn parse_user_event(v: &Value, product: UserStreamProduct) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    let event_type = v.get("e").and_then(|s| s.as_str()).unwrap_or("");
    match event_type {
        "executionReport" => {
            // Spot fill event. Fields:
            //   s: symbol, S: side, L: last fill price,
            //   l: last fill qty, i: orderId, c: clientOrderId,
            //   X: execType, t: tradeId, m: isMaker
            if v.get("X").and_then(|s| s.as_str()) != Some("TRADE") {
                return out;
            }
            let Some(fill) = parse_execution_report(v) else {
                return out;
            };
            out.push(MarketEvent::Fill {
                venue: VenueId::Binance,
                fill,
            });
        }
        "ORDER_TRADE_UPDATE" => {
            // Futures fill event. Shape:
            //   { o: { s, S, L, l, i, c, X, t, m, ap, ... } }
            let Some(order) = v.get("o") else {
                return out;
            };
            if order.get("X").and_then(|s| s.as_str()) != Some("TRADE") {
                return out;
            }
            let Some(fill) = parse_execution_report(order) else {
                return out;
            };
            out.push(MarketEvent::Fill {
                venue: VenueId::Binance,
                fill,
            });
        }
        "outboundAccountPosition" => {
            // Spot balance snapshot — `B: [{a, f, l}]`.
            let Some(bals) = v.get("B").and_then(|b| b.as_array()) else {
                return out;
            };
            for b in bals {
                if let Some(evt) = parse_spot_balance(b, product) {
                    out.push(evt);
                }
            }
        }
        "ACCOUNT_UPDATE" => {
            // Futures balance snapshot — `a: { B: [{a, wb, cw}] }`.
            let Some(bals) = v
                .get("a")
                .and_then(|a| a.get("B"))
                .and_then(|b| b.as_array())
            else {
                return out;
            };
            for b in bals {
                if let Some(evt) = parse_futures_balance(b, product) {
                    out.push(evt);
                }
            }
        }
        _ => {}
    }
    out
}

fn parse_execution_report(v: &Value) -> Option<Fill> {
    let symbol = v.get("s")?.as_str()?.to_string();
    let side = match v.get("S")?.as_str()? {
        "BUY" => Side::Buy,
        "SELL" => Side::Sell,
        _ => return None,
    };
    let price: Decimal = v.get("L")?.as_str()?.parse().ok()?;
    let qty: Decimal = v.get("l")?.as_str()?.parse().ok()?;
    let trade_id = v.get("t").and_then(|t| t.as_u64()).unwrap_or(0);
    // Client order id is our canonical identity. Parse back to UUID
    // if we generated it; otherwise synthesise one so tracking still
    // works for manually-placed orders.
    let cloid = v.get("c").and_then(|c| c.as_str()).unwrap_or("");
    let order_id = Uuid::parse_str(cloid).unwrap_or_else(|_| Uuid::new_v4());
    let is_maker = v.get("m").and_then(|m| m.as_bool()).unwrap_or(false);
    let time_ms = v.get("T").and_then(|t| t.as_i64()).unwrap_or(0);
    let timestamp =
        chrono::DateTime::from_timestamp_millis(time_ms).unwrap_or_else(chrono::Utc::now);
    Some(Fill {
        trade_id,
        order_id,
        symbol,
        side,
        price,
        qty,
        is_maker,
        timestamp,
    })
}

fn parse_spot_balance(v: &Value, product: UserStreamProduct) -> Option<MarketEvent> {
    let asset = v.get("a")?.as_str()?.to_string();
    let free: Decimal = v.get("f")?.as_str()?.parse().ok()?;
    let locked: Decimal = v.get("l")?.as_str()?.parse().ok()?;
    Some(MarketEvent::BalanceUpdate {
        venue: VenueId::Binance,
        asset,
        wallet: product.wallet(),
        total: free + locked,
        locked,
        available: free,
    })
}

fn parse_futures_balance(v: &Value, product: UserStreamProduct) -> Option<MarketEvent> {
    let asset = v.get("a")?.as_str()?.to_string();
    let wallet_balance: Decimal = v.get("wb")?.as_str()?.parse().ok()?;
    let cross_wallet: Decimal = v
        .get("cw")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(wallet_balance);
    let locked = (wallet_balance - cross_wallet).max(Decimal::ZERO);
    Some(MarketEvent::BalanceUpdate {
        venue: VenueId::Binance,
        asset,
        wallet: product.wallet(),
        total: wallet_balance,
        locked,
        available: cross_wallet,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::json;

    #[test]
    fn spot_execution_report_becomes_fill_event() {
        let v = json!({
            "e": "executionReport",
            "E": 1_700_000_000_000u64,
            "s": "BTCUSDT",
            "c": "550e8400-e29b-41d4-a716-446655440000",
            "S": "BUY",
            "o": "LIMIT",
            "q": "0.001",
            "p": "50000.0",
            "X": "TRADE",
            "L": "50001.25",
            "l": "0.0005",
            "t": 123,
            "m": true,
            "T": 1_700_000_000_001i64
        });
        let events = parse_user_event(&v, UserStreamProduct::Spot);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MarketEvent::Fill { fill, .. } => {
                assert_eq!(fill.symbol, "BTCUSDT");
                assert_eq!(fill.side, Side::Buy);
                assert_eq!(fill.price, dec!(50001.25));
                assert_eq!(fill.qty, dec!(0.0005));
                assert_eq!(fill.trade_id, 123);
                assert!(fill.is_maker);
            }
            _ => panic!("expected Fill"),
        }
    }

    #[test]
    fn spot_execution_report_without_trade_is_ignored() {
        // Order placement ack — X=NEW, not TRADE. Don't emit a fill.
        let v = json!({
            "e": "executionReport",
            "s": "BTCUSDT",
            "S": "BUY",
            "X": "NEW",
            "L": "0", "l": "0", "c": "x", "t": 0, "m": false, "T": 0
        });
        let events = parse_user_event(&v, UserStreamProduct::Spot);
        assert!(events.is_empty());
    }

    #[test]
    fn futures_order_trade_update_becomes_fill_event() {
        let v = json!({
            "e": "ORDER_TRADE_UPDATE",
            "T": 1_700_000_000_001i64,
            "o": {
                "s": "BTCUSDT",
                "c": "550e8400-e29b-41d4-a716-446655440000",
                "S": "SELL",
                "X": "TRADE",
                "L": "50200",
                "l": "0.002",
                "t": 999,
                "m": false,
                "T": 1_700_000_000_001i64
            }
        });
        let events = parse_user_event(&v, UserStreamProduct::UsdMarginedFutures);
        assert_eq!(events.len(), 1);
        if let MarketEvent::Fill { fill, .. } = &events[0] {
            assert_eq!(fill.side, Side::Sell);
            assert_eq!(fill.price, dec!(50200));
            assert!(!fill.is_maker);
        } else {
            panic!("expected Fill");
        }
    }

    #[test]
    fn spot_outbound_account_position_emits_balance_updates() {
        let v = json!({
            "e": "outboundAccountPosition",
            "E": 1_700_000_000_000u64,
            "u": 1_700_000_000_000u64,
            "B": [
                {"a": "BTC", "f": "0.5", "l": "0.1"},
                {"a": "USDT", "f": "900", "l": "100"}
            ]
        });
        let events = parse_user_event(&v, UserStreamProduct::Spot);
        assert_eq!(events.len(), 2);
        match &events[0] {
            MarketEvent::BalanceUpdate {
                asset,
                wallet,
                total,
                locked,
                available,
                ..
            } => {
                assert_eq!(asset, "BTC");
                assert_eq!(*wallet, WalletType::Spot);
                assert_eq!(*total, dec!(0.6));
                assert_eq!(*locked, dec!(0.1));
                assert_eq!(*available, dec!(0.5));
            }
            _ => panic!("expected BalanceUpdate"),
        }
    }

    #[test]
    fn futures_account_update_emits_balance_updates() {
        let v = json!({
            "e": "ACCOUNT_UPDATE",
            "T": 1_700_000_000_001i64,
            "E": 1_700_000_000_002u64,
            "a": {
                "m": "ORDER",
                "B": [
                    {"a": "USDT", "wb": "1000", "cw": "950", "bc": "0"}
                ],
                "P": []
            }
        });
        let events = parse_user_event(&v, UserStreamProduct::UsdMarginedFutures);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MarketEvent::BalanceUpdate {
                asset,
                wallet,
                total,
                locked,
                available,
                ..
            } => {
                assert_eq!(asset, "USDT");
                assert_eq!(*wallet, WalletType::UsdMarginedFutures);
                assert_eq!(*total, dec!(1000));
                assert_eq!(*available, dec!(950));
                assert_eq!(*locked, dec!(50));
            }
            _ => panic!("expected BalanceUpdate"),
        }
    }

    #[test]
    fn unknown_event_type_is_ignored() {
        let v = json!({"e": "unknownEvent"});
        assert!(parse_user_event(&v, UserStreamProduct::Spot).is_empty());
    }

    #[test]
    fn user_stream_config_helpers_pick_right_endpoints() {
        let spot = UserStreamConfig::spot("key");
        assert_eq!(spot.rest_base, "https://api.binance.com");
        assert_eq!(spot.ws_host, "wss://stream.binance.com:9443");
        let fut = UserStreamConfig::futures("key");
        assert_eq!(fut.rest_base, "https://fapi.binance.com");
        assert_eq!(fut.ws_host, "wss://fstream.binance.com");
    }

    #[test]
    fn product_wallet_mapping_is_consistent() {
        assert_eq!(UserStreamProduct::Spot.wallet(), WalletType::Spot);
        assert_eq!(
            UserStreamProduct::UsdMarginedFutures.wallet(),
            WalletType::UsdMarginedFutures
        );
        assert_eq!(
            UserStreamProduct::Spot.venue_product(),
            VenueProduct::Spot
        );
        assert_eq!(
            UserStreamProduct::UsdMarginedFutures.venue_product(),
            VenueProduct::LinearPerp
        );
    }
}
