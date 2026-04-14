//! Bybit V5 WebSocket Trade adapter.
//!
//! Wraps `mm_protocols_ws_rpc::WsRpcClient` with the Bybit-specific
//! request/response envelope and a URL factory that re-signs the auth
//! query parameters on every reconnect. Exposes typed methods for
//! `place_order`, `cancel_order`, `amend_order`, and their batch
//! counterparts.
//!
//! ## Auth
//!
//! Bybit V5 WS Trade authenticates on connection via URL query
//! parameters: the client signs `"GET/realtime" + expires` with the API
//! secret and appends the signature to the connection URL. Because the
//! signature embeds a timestamp, it must be regenerated for every
//! reconnect — that is why we use `spawn_with_url_builder` rather than
//! a static URL.
//!
//! ⚠ **Scaffold status.** The exact auth mechanism for V5 Trade (URL vs
//! `op: auth`) is inconsistent between Bybit documentation revisions.
//! This implementation uses URL-based auth and needs live-testnet
//! verification in Sprint 5 before being routed through
//! `BybitConnector::place_order`. For now the adapter builds, its
//! wire format is unit-tested, but it is not wired into the connector
//! path by default.

use std::time::Duration;

use anyhow::{anyhow, Result};
use mm_protocols_ws_rpc::{Frame, WireFormat, WsRpcClient, WsRpcConfig};
use serde_json::{json, Value};

use crate::auth::sign;

/// Wire format for Bybit V5 WS Trade frames.
pub struct BybitTradeWire;

impl WireFormat for BybitTradeWire {
    fn encode_request(&self, id: u64, method: &str, params: Value) -> String {
        // params is the `args` array for the op.
        json!({
            "reqId": id.to_string(),
            "header": {
                "X-BAPI-TIMESTAMP": chrono::Utc::now().timestamp_millis().to_string(),
                "X-BAPI-RECV-WINDOW": "5000",
            },
            "op": method,
            "args": params,
        })
        .to_string()
    }

    fn decode_frame(&self, frame: &str) -> Result<Frame, String> {
        let v: Value = serde_json::from_str(frame).map_err(|e| e.to_string())?;

        // Pong frames carry `op: "pong"` — no reqId, not a response.
        if v.get("op").and_then(|o| o.as_str()) == Some("pong") {
            return Ok(Frame::Keepalive);
        }

        // Business responses carry `reqId` (echoed from request).
        let Some(req_id_str) = v.get("reqId").and_then(|r| r.as_str()) else {
            // Unsolicited — subscription-like. Route as push for
            // observability; the caller can ignore it.
            return Ok(Frame::Push(v));
        };
        let id: u64 = req_id_str
            .parse()
            .map_err(|_| format!("non-numeric reqId: {req_id_str}"))?;

        let ret_code = v.get("retCode").and_then(|c| c.as_i64()).unwrap_or(-1);
        if ret_code == 0 {
            let data = v.get("data").cloned().unwrap_or(Value::Null);
            Ok(Frame::Response {
                id,
                result: Ok(data),
            })
        } else {
            let err = json!({
                "retCode": ret_code,
                "retMsg": v.get("retMsg").cloned().unwrap_or(Value::Null),
            });
            Ok(Frame::Response {
                id,
                result: Err(err),
            })
        }
    }

    fn encode_ping(&self) -> Option<String> {
        Some(json!({"op": "ping"}).to_string())
    }
}

/// Build a signed URL for the Bybit WS Trade endpoint.
///
/// Signature is `HMAC-SHA256(secret, "GET/realtime" + expires)`. `expires`
/// is the validity window deadline in Unix milliseconds and must be at
/// least a few seconds in the future for the handshake to succeed.
pub fn signed_trade_url(
    base_ws_url: &str,
    api_key: &str,
    api_secret: &str,
    window: Duration,
) -> String {
    let expires = chrono::Utc::now().timestamp_millis() + window.as_millis() as i64;
    let payload = format!("GET/realtime{expires}");
    let signature = sign(api_secret, &payload);
    format!("{base_ws_url}?api_key={api_key}&expires={expires}&signature={signature}")
}

/// Typed wrapper over `WsRpcClient` for Bybit WS Trade.
pub struct BybitWsTrader {
    client: WsRpcClient,
}

impl BybitWsTrader {
    /// Spawn the trader. The URL is re-signed on every (re)connect via
    /// `spawn_with_url_builder`, so expired signatures are automatically
    /// refreshed.
    pub fn connect(base_ws_url: &str, api_key: &str, api_secret: &str) -> Self {
        let base = base_ws_url.to_string();
        let key = api_key.to_string();
        let secret = api_secret.to_string();
        let window = Duration::from_secs(10);

        let config = WsRpcConfig {
            // Seeded value — overridden by the builder on every connect.
            url: signed_trade_url(&base, &key, &secret, window),
            request_timeout: Duration::from_secs(10),
            reconnect_backoff: Duration::from_secs(2),
            app_ping_interval: Some(Duration::from_secs(20)),
        };

        let client = WsRpcClient::spawn_with_url_builder(
            config,
            BybitTradeWire,
            |_| {},
            move || signed_trade_url(&base, &key, &secret, window),
        );

        Self { client }
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    /// `order.create` — single order. `args` is the single order object
    /// exactly as Bybit expects it (`symbol`, `side`, `orderType`, `qty`,
    /// `price`, `timeInForce`, `category`, …).
    pub async fn place_order(&self, args: Value) -> Result<Value> {
        self.client
            .send_request("order.create", json!([args]))
            .await
            .map_err(|e| anyhow!("bybit ws place_order: {e}"))
    }

    pub async fn amend_order(&self, args: Value) -> Result<Value> {
        self.client
            .send_request("order.amend", json!([args]))
            .await
            .map_err(|e| anyhow!("bybit ws amend_order: {e}"))
    }

    pub async fn cancel_order(&self, args: Value) -> Result<Value> {
        self.client
            .send_request("order.cancel", json!([args]))
            .await
            .map_err(|e| anyhow!("bybit ws cancel_order: {e}"))
    }

    pub async fn place_orders_batch(&self, orders: Vec<Value>) -> Result<Value> {
        self.client
            .send_request("order.create-batch", Value::Array(orders))
            .await
            .map_err(|e| anyhow!("bybit ws place_orders_batch: {e}"))
    }

    pub async fn cancel_orders_batch(&self, cancels: Vec<Value>) -> Result<Value> {
        self.client
            .send_request("order.cancel-batch", Value::Array(cancels))
            .await
            .map_err(|e| anyhow!("bybit ws cancel_orders_batch: {e}"))
    }

    pub fn shutdown(&self) {
        self.client.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_business_request_has_header_and_req_id() {
        let args = json!([{"symbol": "BTCUSDT", "side": "Buy"}]);
        let raw = BybitTradeWire.encode_request(7, "order.create", args);
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["reqId"], "7");
        assert_eq!(v["op"], "order.create");
        assert_eq!(v["args"][0]["symbol"], "BTCUSDT");
        assert!(v["header"]["X-BAPI-TIMESTAMP"].is_string());
        assert_eq!(v["header"]["X-BAPI-RECV-WINDOW"], "5000");
    }

    #[test]
    fn decode_success_response_is_routed_by_req_id() {
        let raw = r#"{
            "reqId": "42",
            "retCode": 0,
            "retMsg": "OK",
            "op": "order.create",
            "data": {"orderId": "abc", "orderLinkId": "cloid"}
        }"#;
        let frame = BybitTradeWire.decode_frame(raw).unwrap();
        match frame {
            Frame::Response { id, result: Ok(v) } => {
                assert_eq!(id, 42);
                assert_eq!(v["orderId"], "abc");
            }
            _ => panic!("expected success response"),
        }
    }

    #[test]
    fn decode_error_response_carries_ret_code_and_msg() {
        let raw = r#"{
            "reqId": "9",
            "retCode": 10001,
            "retMsg": "params error: tick size",
            "op": "order.create",
            "data": null
        }"#;
        let frame = BybitTradeWire.decode_frame(raw).unwrap();
        match frame {
            Frame::Response { id, result: Err(v) } => {
                assert_eq!(id, 9);
                assert_eq!(v["retCode"], 10001);
                assert_eq!(v["retMsg"], "params error: tick size");
            }
            _ => panic!("expected error response"),
        }
    }

    #[test]
    fn decode_pong_is_keepalive() {
        let raw = r#"{"op": "pong", "ret_msg": "pong"}"#;
        let frame = BybitTradeWire.decode_frame(raw).unwrap();
        assert!(matches!(frame, Frame::Keepalive));
    }

    #[test]
    fn decode_unsolicited_frame_routes_to_push() {
        // No reqId → treated as push so the caller can observe it.
        let raw = r#"{"topic": "execution", "data": {"orderId": "x"}}"#;
        let frame = BybitTradeWire.decode_frame(raw).unwrap();
        assert!(matches!(frame, Frame::Push(_)));
    }

    #[test]
    fn signed_url_contains_required_params() {
        let url = signed_trade_url(
            "wss://stream.bybit.com/v5/trade",
            "MY_KEY",
            "MY_SECRET",
            Duration::from_secs(10),
        );
        assert!(url.starts_with("wss://stream.bybit.com/v5/trade?"));
        assert!(url.contains("api_key=MY_KEY"));
        assert!(url.contains("expires="));
        assert!(url.contains("signature="));
        // Signature is 64 hex chars (HMAC-SHA256).
        let sig_start = url.find("signature=").unwrap() + "signature=".len();
        let sig = &url[sig_start..];
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
