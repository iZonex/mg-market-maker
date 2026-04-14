//! Binance WebSocket API adapter (HMAC mode).
//!
//! Wraps `mm_protocols_ws_rpc::WsRpcClient` with Binance's
//! `{id, method, params}` envelope and a per-request HMAC-SHA256 signing
//! helper. Session-level logon (Ed25519 `session.logon`) is not used —
//! every request carries its own `apiKey` / `timestamp` / `signature`,
//! which is the simplest path and reuses the existing HMAC secret from
//! the REST code path.
//!
//! The Binance WS API response shape is:
//!
//! ```json
//! {"id": "...", "status": 200, "result": {...}, "rateLimits": [...]}
//! ```
//!
//! or on error:
//!
//! ```json
//! {"id": "...", "status": 4xx, "error": {"code": -xxx, "msg": "..."}}
//! ```
//!
//! The `id` is echoed as a string even though we feed it to the WsRpc
//! correlation layer as a `u64` — the wire format renders and parses
//! both ends.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use mm_protocols_ws_rpc::{Frame, WireFormat, WsRpcClient, WsRpcConfig};
use serde_json::{json, Value};

use crate::auth::sign;

/// Wire format for Binance WS API frames.
pub struct BinanceWsWire;

impl WireFormat for BinanceWsWire {
    fn encode_request(&self, id: u64, method: &str, params: Value) -> String {
        json!({
            "id": id.to_string(),
            "method": method,
            "params": params,
        })
        .to_string()
    }

    fn decode_frame(&self, frame: &str) -> Result<Frame, String> {
        let v: Value = serde_json::from_str(frame).map_err(|e| e.to_string())?;
        let Some(id_str) = v.get("id").and_then(|i| i.as_str()) else {
            // Binance WS API also emits event messages (e.g. subscription
            // data); we don't currently subscribe from this client, so
            // anything id-less is routed as Push for observability.
            return Ok(Frame::Push(v));
        };
        let id: u64 = id_str
            .parse()
            .map_err(|_| format!("non-numeric id: {id_str}"))?;
        let status = v.get("status").and_then(|s| s.as_u64()).unwrap_or(0);
        if status == 200 {
            let result = v.get("result").cloned().unwrap_or(Value::Null);
            Ok(Frame::Response {
                id,
                result: Ok(result),
            })
        } else {
            let err = v.get("error").cloned().unwrap_or_else(
                || json!({"code": status, "msg": v.get("status").cloned().unwrap_or(Value::Null)}),
            );
            Ok(Frame::Response {
                id,
                result: Err(err),
            })
        }
    }
}

/// Typed wrapper over `WsRpcClient` for Binance WS API.
pub struct BinanceWsTrader {
    client: WsRpcClient,
    api_key: String,
    api_secret: String,
}

impl BinanceWsTrader {
    pub fn connect(base_ws_url: &str, api_key: &str, api_secret: &str) -> Self {
        let config = WsRpcConfig {
            url: base_ws_url.to_string(),
            request_timeout: Duration::from_secs(10),
            reconnect_backoff: Duration::from_secs(2),
            app_ping_interval: None,
        };
        let client = WsRpcClient::spawn(config, BinanceWsWire, |_| {});
        Self {
            client,
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    /// Place a single LIMIT order. The caller supplies pre-formatted
    /// `price` / `quantity` strings already rounded to the symbol's
    /// tick / lot size.
    pub async fn place_limit_order(
        &self,
        symbol: &str,
        side_buy: bool,
        price: &str,
        quantity: &str,
        time_in_force: &str,
        client_order_id: Option<&str>,
    ) -> Result<Value> {
        let mut params: BTreeMap<String, String> = BTreeMap::new();
        params.insert("symbol".into(), symbol.into());
        params.insert("side".into(), if side_buy { "BUY" } else { "SELL" }.into());
        params.insert("type".into(), "LIMIT".into());
        params.insert("timeInForce".into(), time_in_force.into());
        params.insert("price".into(), price.into());
        params.insert("quantity".into(), quantity.into());
        if let Some(cloid) = client_order_id {
            params.insert("newClientOrderId".into(), cloid.into());
        }
        self.send_signed("order.place", params).await
    }

    /// Cancel by client order id (stable across REST and WS paths).
    pub async fn cancel_order(&self, symbol: &str, orig_client_order_id: &str) -> Result<Value> {
        let mut params: BTreeMap<String, String> = BTreeMap::new();
        params.insert("symbol".into(), symbol.into());
        params.insert("origClientOrderId".into(), orig_client_order_id.into());
        self.send_signed("order.cancel", params).await
    }

    /// Cancel all open orders on a symbol.
    pub async fn cancel_all(&self, symbol: &str) -> Result<Value> {
        let mut params: BTreeMap<String, String> = BTreeMap::new();
        params.insert("symbol".into(), symbol.into());
        self.send_signed("openOrders.cancelAll", params).await
    }

    /// Send a signed request. Appends `apiKey`, `timestamp`, and
    /// `signature` to the params after computing the canonical query
    /// string.
    async fn send_signed(
        &self,
        method: &str,
        mut params: BTreeMap<String, String>,
    ) -> Result<Value> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        params.insert("apiKey".into(), self.api_key.clone());
        params.insert("timestamp".into(), timestamp.to_string());

        // Canonical query string: alphabetical, `key=value` joined with &.
        // BTreeMap iteration is already key-sorted.
        let query = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let signature = sign(&self.api_secret, &query);
        params.insert("signature".into(), signature);

        let params_json: Value = Value::Object(
            params
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect(),
        );
        self.client
            .send_request(method, params_json)
            .await
            .map_err(|e| anyhow!("binance ws {method}: {e}"))
    }

    pub fn shutdown(&self) {
        self.client.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_request_wraps_params() {
        let params = json!({"symbol": "BTCUSDT", "side": "BUY"});
        let raw = BinanceWsWire.encode_request(5, "order.place", params);
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["id"], "5");
        assert_eq!(v["method"], "order.place");
        assert_eq!(v["params"]["symbol"], "BTCUSDT");
    }

    #[test]
    fn decode_success_response() {
        let raw = r#"{
            "id": "12",
            "status": 200,
            "result": {"orderId": 98765, "symbol": "BTCUSDT"},
            "rateLimits": []
        }"#;
        let frame = BinanceWsWire.decode_frame(raw).unwrap();
        match frame {
            Frame::Response { id, result: Ok(v) } => {
                assert_eq!(id, 12);
                assert_eq!(v["orderId"], 98765);
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn decode_error_response() {
        let raw = r#"{
            "id": "13",
            "status": 400,
            "error": {"code": -1021, "msg": "Timestamp outside recvWindow"}
        }"#;
        let frame = BinanceWsWire.decode_frame(raw).unwrap();
        match frame {
            Frame::Response { id, result: Err(v) } => {
                assert_eq!(id, 13);
                assert_eq!(v["code"], -1021);
            }
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn decode_id_less_frame_becomes_push() {
        let raw = r#"{"event": "something unsolicited"}"#;
        let frame = BinanceWsWire.decode_frame(raw).unwrap();
        assert!(matches!(frame, Frame::Push(_)));
    }

    /// Signing invariant: two calls with identical inputs (same params,
    /// same secret, same timestamp) produce the same signature.
    ///
    /// We fake this by driving the canonical-string formation manually
    /// with a fixed timestamp, since `send_signed` reads the clock.
    #[test]
    fn canonical_query_string_is_sorted_and_signed_deterministically() {
        let secret = "testsecret";
        let mut params: BTreeMap<String, String> = BTreeMap::new();
        params.insert("symbol".into(), "BTCUSDT".into());
        params.insert("side".into(), "BUY".into());
        params.insert("type".into(), "LIMIT".into());
        params.insert("timestamp".into(), "1700000000000".into());
        params.insert("apiKey".into(), "mykey".into());
        let query = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        // Alphabetical order:
        //   apiKey, side, symbol, timestamp, type
        assert_eq!(
            query,
            "apiKey=mykey&side=BUY&symbol=BTCUSDT&timestamp=1700000000000&type=LIMIT"
        );
        let sig_a = sign(secret, &query);
        let sig_b = sign(secret, &query);
        assert_eq!(sig_a, sig_b);
        assert_eq!(sig_a.len(), 64); // HMAC-SHA256 hex
    }
}
