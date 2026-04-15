//! Bybit V5 private WebSocket (user-data) stream.
//!
//! The connector at [`crate::connector::BybitConnector`] subscribes to
//! public `orderbook` and `publicTrade` streams only — fills and
//! balance updates that arrive out-of-band (REST fallback, manual UI
//! orders, RFQ trades, the BybitWsTrader path once it is wired) never
//! reach the engine through the public feed.
//!
//! This module closes that gap by opening Bybit V5's private WS at
//! `wss://stream.bybit.com/v5/private`, signing in with the V5 auth
//! op, subscribing to `execution` + `wallet`, and translating frames
//! into `MarketEvent::Fill` and `MarketEvent::BalanceUpdate` events
//! on the same channel the public subscribe task writes to.
//!
//! ## Auth
//!
//! Bybit V5 private WS does not use a listen key. After the WebSocket
//! handshake we send:
//!
//! ```json
//! {"op": "auth", "args": ["<api_key>", <expires_ms>, "<signature>"]}
//! ```
//!
//! where `signature = HMAC_SHA256(api_secret, "GET/realtime" + expires_ms)`.
//! `expires_ms` is a Unix-millisecond deadline a few seconds in the
//! future. After the auth ack arrives we send:
//!
//! ```json
//! {"op": "subscribe", "args": ["execution", "wallet", "order"]}
//! ```
//!
//! The connection is kept alive by sending `{"op": "ping"}` every
//! 20 s; Bybit replies with `{"op": "pong"}`.

use std::time::Duration;

use mm_common::types::{Fill, Side, WalletType};
use mm_exchange_core::connector::VenueId;
use mm_exchange_core::events::MarketEvent;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::auth::sign;

const DEFAULT_MAINNET_PRIVATE_WS: &str = "wss://stream.bybit.com/v5/private";
const DEFAULT_TESTNET_PRIVATE_WS: &str = "wss://stream-testnet.bybit.com/v5/private";

/// Configuration for the Bybit V5 private WS task.
#[derive(Debug, Clone)]
pub struct UserStreamConfig {
    pub api_key: String,
    pub api_secret: String,
    pub ws_url: String,
    /// Wallet bucket to tag emitted `BalanceUpdate` events with.
    /// Defaults to `Unified` (V5 UTA collateral pool); operators on
    /// classic sub-accounts should override via [`Self::with_wallet`].
    pub wallet: WalletType,
}

impl UserStreamConfig {
    pub fn mainnet(api_key: &str, api_secret: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            ws_url: DEFAULT_MAINNET_PRIVATE_WS.to_string(),
            wallet: WalletType::Unified,
        }
    }

    pub fn testnet(api_key: &str, api_secret: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            ws_url: DEFAULT_TESTNET_PRIVATE_WS.to_string(),
            wallet: WalletType::Unified,
        }
    }

    pub fn with_wallet(mut self, wallet: WalletType) -> Self {
        self.wallet = wallet;
        self
    }
}

/// Handle returned by [`start`]. Dropping it does not stop the
/// background task — it shuts down only when the event channel's
/// receiver is dropped.
pub struct UserDataStream;

/// Spawn a background task that signs in to the Bybit V5 private WS,
/// subscribes to `execution` + `wallet` + `order`, and writes parsed
/// fills and balance updates into `tx`.
pub fn start(config: UserStreamConfig, tx: mpsc::UnboundedSender<MarketEvent>) -> UserDataStream {
    tokio::spawn(async move {
        let _ = run(config, tx).await;
    });
    UserDataStream
}

async fn run(
    config: UserStreamConfig,
    tx: mpsc::UnboundedSender<MarketEvent>,
) -> anyhow::Result<()> {
    loop {
        if let Err(e) = stream_loop(&config, &tx).await {
            warn!(error = %e, "Bybit private WS loop exited");
        }
        if tx.is_closed() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn stream_loop(
    config: &UserStreamConfig,
    tx: &mpsc::UnboundedSender<MarketEvent>,
) -> anyhow::Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let (ws, _) = connect_async(&config.ws_url).await?;
    info!(url = %config.ws_url, "connected to Bybit V5 private WS");
    let _ = tx.send(MarketEvent::Connected {
        venue: VenueId::Bybit,
    });
    let (mut write, mut read) = ws.split();

    // V5 auth handshake.
    let auth_frame = build_auth_frame(&config.api_key, &config.api_secret);
    write.send(Message::Text(auth_frame)).await?;

    // Subscribe to private topics. Bybit V5 private streams subscribe
    // by topic name with no symbol qualifier — `execution` is account-
    // wide and `wallet` is per-account.
    let sub = json!({
        "op": "subscribe",
        "args": ["execution", "wallet", "order"],
    });
    write.send(Message::Text(sub.to_string())).await?;

    let mut ping_iv = tokio::time::interval(Duration::from_secs(20));
    ping_iv.tick().await; // Skip the immediate fire.

    loop {
        tokio::select! {
            msg = read.next() => {
                let Some(msg) = msg else { break; };
                let Ok(Message::Text(text)) = msg else { continue; };
                let v: Value = match serde_json::from_str(text.as_ref()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(op) = v.get("op").and_then(|o| o.as_str()) {
                    match op {
                        "auth" => {
                            let success = v.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
                            if !success {
                                anyhow::bail!("Bybit private WS auth rejected: {v}");
                            }
                            debug!("Bybit private WS auth ok");
                            continue;
                        }
                        "subscribe" => {
                            let success = v.get("success").and_then(|s| s.as_bool()).unwrap_or(true);
                            if !success {
                                anyhow::bail!("Bybit private WS subscribe rejected: {v}");
                            }
                            continue;
                        }
                        "pong" => continue,
                        _ => {}
                    }
                }
                for evt in parse_user_event(&v, config.wallet) {
                    if tx.send(evt).is_err() {
                        return Ok(());
                    }
                }
            }
            _ = ping_iv.tick() => {
                let ping = json!({"op": "ping"});
                if write.send(Message::Text(ping.to_string())).await.is_err() {
                    break;
                }
            }
        }
    }

    let _ = tx.send(MarketEvent::Disconnected {
        venue: VenueId::Bybit,
    });
    anyhow::bail!("Bybit private WS closed")
}

fn build_auth_frame(api_key: &str, api_secret: &str) -> String {
    let expires = chrono::Utc::now().timestamp_millis() + 10_000;
    let payload = format!("GET/realtime{expires}");
    let signature = sign(api_secret, &payload);
    json!({
        "op": "auth",
        "args": [api_key, expires, signature],
    })
    .to_string()
}

/// Parse one Bybit V5 private WS frame into `MarketEvent`s — public
/// entry point for integration tests in downstream crates that need to
/// assert the frame → cache path end-to-end without spinning up a live
/// WS session.
pub fn parse_user_event_for_test(v: &Value, wallet: WalletType) -> Vec<MarketEvent> {
    parse_user_event(v, wallet)
}

pub(crate) fn parse_user_event(v: &Value, wallet: WalletType) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    let topic = v.get("topic").and_then(|t| t.as_str()).unwrap_or("");
    match topic {
        "execution" => {
            let Some(items) = v.get("data").and_then(|d| d.as_array()) else {
                return out;
            };
            for item in items {
                if item.get("execType").and_then(|t| t.as_str()) != Some("Trade") {
                    continue;
                }
                if let Some(fill) = parse_execution_item(item) {
                    out.push(MarketEvent::Fill {
                        venue: VenueId::Bybit,
                        fill,
                    });
                }
            }
        }
        "wallet" => {
            let Some(items) = v.get("data").and_then(|d| d.as_array()) else {
                return out;
            };
            for account in items {
                let Some(coins) = account.get("coin").and_then(|c| c.as_array()) else {
                    continue;
                };
                for coin in coins {
                    if let Some(evt) = parse_wallet_coin(coin, wallet) {
                        out.push(evt);
                    }
                }
            }
        }
        // `order` topic carries OrderUpdate-shaped events (status,
        // cumExecQty). Engine consumption of those is handled by the
        // existing reconciliation path; surfacing them here would
        // duplicate signalling. Left as a future extension.
        _ => {}
    }
    out
}

fn parse_execution_item(v: &Value) -> Option<Fill> {
    let symbol = v.get("symbol")?.as_str()?.to_string();
    let side = match v.get("side")?.as_str()? {
        "Buy" => Side::Buy,
        "Sell" => Side::Sell,
        _ => return None,
    };
    let price: Decimal = v.get("execPrice")?.as_str()?.parse().ok()?;
    let qty: Decimal = v.get("execQty")?.as_str()?.parse().ok()?;
    // execId is an opaque string on Bybit V5; truncate to a u64 hash so
    // the Fill struct's u64 trade_id stays populated. Collisions across
    // a 64-bit space are not a concern at MM volumes.
    let trade_id = v
        .get("execId")
        .and_then(|t| t.as_str())
        .map(hash_str_to_u64)
        .unwrap_or(0);
    let order_link_id = v.get("orderLinkId").and_then(|c| c.as_str()).unwrap_or("");
    let order_id = Uuid::parse_str(order_link_id).unwrap_or_else(|_| Uuid::new_v4());
    let is_maker = v.get("isMaker").and_then(|m| m.as_bool()).unwrap_or(false);
    let exec_time_ms: i64 = v
        .get("execTime")
        .and_then(|t| t.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let timestamp =
        chrono::DateTime::from_timestamp_millis(exec_time_ms).unwrap_or_else(chrono::Utc::now);
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

fn parse_wallet_coin(v: &Value, wallet: WalletType) -> Option<MarketEvent> {
    let asset = v.get("coin")?.as_str()?.to_string();
    let total: Decimal = decimal_field(v, "walletBalance").unwrap_or(Decimal::ZERO);
    // Bybit V5 UTA exposes `locked` only on certain account types; for
    // others the held-collateral is `totalOrderIM`. Try both before
    // falling back to zero.
    let locked: Decimal = decimal_field(v, "locked")
        .or_else(|| decimal_field(v, "totalOrderIM"))
        .unwrap_or(Decimal::ZERO);
    // `free` is deprecated under UTA in favour of `availableToWithdraw`.
    // Fall back to `total - locked` to keep the invariant
    // `total = available + locked` even when neither field is sent.
    let available: Decimal = decimal_field(v, "free")
        .or_else(|| decimal_field(v, "availableToWithdraw"))
        .unwrap_or_else(|| (total - locked).max(Decimal::ZERO));
    Some(MarketEvent::BalanceUpdate {
        venue: VenueId::Bybit,
        asset,
        wallet,
        total,
        locked,
        available,
    })
}

fn decimal_field(v: &Value, name: &str) -> Option<Decimal> {
    let raw = v.get(name)?.as_str()?;
    if raw.is_empty() {
        return None;
    }
    raw.parse().ok()
}

fn hash_str_to_u64(s: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::json;

    #[test]
    fn auth_frame_carries_key_expires_and_signature() {
        let raw = build_auth_frame("MY_KEY", "MY_SECRET");
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["op"], "auth");
        let args = v["args"].as_array().expect("args is array");
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "MY_KEY");
        assert!(args[1].is_i64() && args[1].as_i64().unwrap() > 0);
        let sig = args[2].as_str().expect("signature is string");
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn execution_trade_becomes_fill_event() {
        let v = json!({
            "topic": "execution",
            "data": [{
                "category": "spot",
                "symbol": "BTCUSDT",
                "execId": "abc-123",
                "execPrice": "50001.25",
                "execQty": "0.0005",
                "execType": "Trade",
                "side": "Buy",
                "isMaker": true,
                "execTime": "1700000000001",
                "orderId": "order-x",
                "orderLinkId": "550e8400-e29b-41d4-a716-446655440000"
            }]
        });
        let events = parse_user_event(&v, WalletType::Unified);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MarketEvent::Fill { fill, .. } => {
                assert_eq!(fill.symbol, "BTCUSDT");
                assert_eq!(fill.side, Side::Buy);
                assert_eq!(fill.price, dec!(50001.25));
                assert_eq!(fill.qty, dec!(0.0005));
                assert!(fill.is_maker);
                assert_ne!(fill.trade_id, 0);
                assert_eq!(
                    fill.order_id,
                    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
                );
            }
            _ => panic!("expected Fill"),
        }
    }

    #[test]
    fn execution_non_trade_exec_type_is_ignored() {
        // Bybit emits Funding / AdlTrade / BustTrade etc. on the same
        // execution topic — they are not fills against our maker quotes.
        let v = json!({
            "topic": "execution",
            "data": [{
                "symbol": "BTCUSDT",
                "execType": "Funding",
                "execPrice": "0",
                "execQty": "0",
                "side": "Buy",
                "isMaker": false,
                "execTime": "0",
                "orderLinkId": ""
            }]
        });
        assert!(parse_user_event(&v, WalletType::Unified).is_empty());
    }

    #[test]
    fn wallet_coin_unified_uses_wallet_balance_and_available_to_withdraw() {
        let v = json!({
            "topic": "wallet",
            "data": [{
                "accountType": "UNIFIED",
                "coin": [{
                    "coin": "USDT",
                    "walletBalance": "1000",
                    "availableToWithdraw": "950",
                    "totalOrderIM": "50"
                }]
            }]
        });
        let events = parse_user_event(&v, WalletType::Unified);
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
                assert_eq!(*wallet, WalletType::Unified);
                assert_eq!(*total, dec!(1000));
                assert_eq!(*locked, dec!(50));
                assert_eq!(*available, dec!(950));
            }
            _ => panic!("expected BalanceUpdate"),
        }
    }

    #[test]
    fn wallet_coin_classic_spot_uses_free_and_locked() {
        let v = json!({
            "topic": "wallet",
            "data": [{
                "accountType": "SPOT",
                "coin": [{
                    "coin": "BTC",
                    "walletBalance": "0.6",
                    "free": "0.5",
                    "locked": "0.1"
                }]
            }]
        });
        let events = parse_user_event(&v, WalletType::Spot);
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
    fn wallet_coin_falls_back_to_total_minus_locked_when_available_missing() {
        let v = json!({
            "topic": "wallet",
            "data": [{
                "coin": [{
                    "coin": "ETH",
                    "walletBalance": "5",
                    "locked": "1"
                }]
            }]
        });
        let events = parse_user_event(&v, WalletType::Unified);
        assert_eq!(events.len(), 1);
        if let MarketEvent::BalanceUpdate {
            total,
            locked,
            available,
            ..
        } = &events[0]
        {
            assert_eq!(*total, dec!(5));
            assert_eq!(*locked, dec!(1));
            assert_eq!(*available, dec!(4));
        } else {
            panic!("expected BalanceUpdate");
        }
    }

    #[test]
    fn wallet_frame_with_multiple_accounts_emits_all_coins() {
        let v = json!({
            "topic": "wallet",
            "data": [
                {"coin": [{"coin": "USDT", "walletBalance": "100"}]},
                {"coin": [{"coin": "BTC", "walletBalance": "0.01"}]}
            ]
        });
        let events = parse_user_event(&v, WalletType::Unified);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn unknown_topic_is_ignored() {
        let v = json!({"topic": "publicTrade.BTCUSDT", "data": []});
        assert!(parse_user_event(&v, WalletType::Unified).is_empty());
    }

    #[test]
    fn user_stream_config_helpers_pick_right_endpoints() {
        let m = UserStreamConfig::mainnet("k", "s");
        assert_eq!(m.ws_url, "wss://stream.bybit.com/v5/private");
        assert_eq!(m.wallet, WalletType::Unified);
        let t = UserStreamConfig::testnet("k", "s");
        assert_eq!(t.ws_url, "wss://stream-testnet.bybit.com/v5/private");
        let s = UserStreamConfig::mainnet("k", "s").with_wallet(WalletType::Spot);
        assert_eq!(s.wallet, WalletType::Spot);
    }
}
