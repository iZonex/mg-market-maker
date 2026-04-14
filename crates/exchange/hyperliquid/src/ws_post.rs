//! HyperLiquid WebSocket `post` adapter — signed order entry over a
//! dedicated WS connection, bypassing REST.
//!
//! Layout is the same as REST `/exchange` on the wire: the outer
//! envelope carries `{"type":"action","payload":<signed>}` and the
//! `payload` is byte-for-byte identical to what REST expects. We reuse
//! the existing `sign_l1_action` unchanged.
//!
//! Uses its own WS connection — NOT the market-data one — to keep the
//! correlation layer clean. HL allows multiple WS connections per
//! address; the extra socket cost is negligible versus the architectural
//! simplification of not multiplexing sub/post frames on a single stream.

use std::time::Duration;

use anyhow::{anyhow, Result};
use mm_protocols_ws_rpc::{Frame, WireFormat, WsRpcClient, WsRpcConfig};
use serde_json::{json, Value};

use crate::auth::{sign_l1_action, PrivateKey};
use crate::types::{HlCancelByCloid, HlCancelByCloidAction, HlOrder, HlOrderAction};

/// Wire format for HL WS `method=post` envelopes.
pub struct HlPostWire;

impl WireFormat for HlPostWire {
    fn encode_request(&self, id: u64, _method: &str, params: Value) -> String {
        // params is the inner `request` object (`{type, payload}`).
        // `method` is fixed to `"post"` on this transport, and HL does
        // not care about the caller-supplied method string — it only
        // reads the outer envelope.
        json!({
            "method": "post",
            "id": id,
            "request": params,
        })
        .to_string()
    }

    fn decode_frame(&self, frame: &str) -> Result<Frame, String> {
        let v: Value = serde_json::from_str(frame).map_err(|e| e.to_string())?;
        let Some(channel) = v.get("channel").and_then(|c| c.as_str()) else {
            return Ok(Frame::Keepalive);
        };

        if channel != "post" {
            // Any non-post frame on this dedicated connection is
            // unexpected but harmless — treat it as keepalive so we do
            // not tear down.
            return Ok(Frame::Keepalive);
        }

        let data = v.get("data").ok_or("post response missing data")?;
        let id = data
            .get("id")
            .and_then(|i| i.as_u64())
            .ok_or("post response missing data.id")?;
        let response = data.get("response").cloned().unwrap_or(Value::Null);
        let rtype = response.get("type").and_then(|t| t.as_str());

        if rtype == Some("error") {
            let payload = response.get("payload").cloned().unwrap_or(Value::Null);
            return Ok(Frame::Response {
                id,
                result: Err(payload),
            });
        }

        // For action/info responses, inner payload shape is
        // `{status: "ok", response: ...}` or `{status: "err", response: "..."}`.
        let payload = response.get("payload").cloned().unwrap_or(Value::Null);
        match payload.get("status").and_then(|s| s.as_str()) {
            Some("ok") => {
                let inner = payload.get("response").cloned().unwrap_or(Value::Null);
                Ok(Frame::Response {
                    id,
                    result: Ok(inner),
                })
            }
            _ => Ok(Frame::Response {
                id,
                result: Err(payload),
            }),
        }
    }
}

/// Typed wrapper over a [`WsRpcClient`] for HL order entry.
pub struct HlWsTrader {
    client: WsRpcClient,
    key: PrivateKey,
    is_mainnet: bool,
    vault: Option<[u8; 20]>,
}

impl HlWsTrader {
    /// Spawn a new trader. Opens its own WS connection (separate from
    /// the market-data one). The connection runs with 10s request
    /// timeout and 2s reconnect backoff by default.
    pub fn connect(url: impl Into<String>, key: PrivateKey, is_mainnet: bool) -> Self {
        let config = WsRpcConfig {
            url: url.into(),
            request_timeout: Duration::from_secs(10),
            reconnect_backoff: Duration::from_secs(2),
            app_ping_interval: None,
        };
        let client = WsRpcClient::spawn(config, HlPostWire, |_| {});
        Self {
            client,
            key,
            is_mainnet,
            vault: None,
        }
    }

    /// Whether the background task currently holds an authenticated
    /// connection. (Authentication is per-request via EIP-712 so the
    /// "authenticated" state is equivalent to being connected at all.)
    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    /// Place a single order. Returns the raw HL response value on
    /// success so the caller can extract `statuses[0].resting.oid` or
    /// any filled data as appropriate.
    pub async fn place_order(&self, order: HlOrder) -> Result<Value> {
        let action = HlOrderAction::new(vec![order]);
        self.send_signed(&action).await
    }

    /// Place several orders in a single signed action. Returns the HL
    /// response with a per-sub-order statuses array.
    pub async fn place_orders(&self, orders: Vec<HlOrder>) -> Result<Value> {
        let action = HlOrderAction::new(orders);
        self.send_signed(&action).await
    }

    /// Cancel one order by cloid.
    pub async fn cancel_by_cloid(&self, asset: u32, cloid: String) -> Result<Value> {
        let action = HlCancelByCloidAction::new(vec![HlCancelByCloid { asset, cloid }]);
        self.send_signed(&action).await
    }

    /// Cancel a batch of orders by cloid on the same asset.
    pub async fn cancel_batch_by_cloid(&self, asset: u32, cloids: Vec<String>) -> Result<Value> {
        let cancels = cloids
            .into_iter()
            .map(|cloid| HlCancelByCloid { asset, cloid })
            .collect();
        let action = HlCancelByCloidAction::new(cancels);
        self.send_signed(&action).await
    }

    /// Low-level: sign and send an arbitrary action.
    async fn send_signed<A: serde::Serialize>(&self, action: &A) -> Result<Value> {
        let nonce = chrono::Utc::now().timestamp_millis() as u64;
        let sig = sign_l1_action(
            &self.key,
            action,
            nonce,
            self.vault.as_ref(),
            self.is_mainnet,
        )?;
        let payload = json!({
            "action": action,
            "nonce": nonce,
            "signature": sig.to_json(),
            "vaultAddress": self.vault.map(|a| format!("0x{}", hex::encode(a))),
        });
        let request = json!({
            "type": "action",
            "payload": payload,
        });
        self.client
            .send_request("post", request)
            .await
            .map_err(|e| anyhow!("HL WS post failed: {e}"))
    }

    pub fn shutdown(&self) {
        self.client.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_post_action_ok_response() {
        let frame = r#"{
            "channel": "post",
            "data": {
                "id": 42,
                "response": {
                    "type": "action",
                    "payload": {
                        "status": "ok",
                        "response": {
                            "type": "order",
                            "data": {
                                "statuses": [{"resting": {"oid": 12345}}]
                            }
                        }
                    }
                }
            }
        }"#;
        let frame = HlPostWire.decode_frame(frame).unwrap();
        match frame {
            Frame::Response { id, result: Ok(v) } => {
                assert_eq!(id, 42);
                let oid = v
                    .pointer("/data/statuses/0/resting/oid")
                    .and_then(|v| v.as_u64());
                assert_eq!(oid, Some(12345));
            }
            _ => panic!("expected Response::Ok"),
        }
    }

    #[test]
    fn decode_post_error_response() {
        let frame = r#"{
            "channel": "post",
            "data": {
                "id": 7,
                "response": {
                    "type": "error",
                    "payload": "insufficient margin"
                }
            }
        }"#;
        let frame = HlPostWire.decode_frame(frame).unwrap();
        match frame {
            Frame::Response { id, result: Err(v) } => {
                assert_eq!(id, 7);
                assert_eq!(v.as_str(), Some("insufficient margin"));
            }
            _ => panic!("expected Response::Err"),
        }
    }

    #[test]
    fn decode_non_post_channel_is_keepalive() {
        let frame = r#"{"channel":"l2Book","data":{}}"#;
        let frame = HlPostWire.decode_frame(frame).unwrap();
        assert!(matches!(frame, Frame::Keepalive));
    }

    #[test]
    fn encode_wraps_params_in_post_envelope() {
        let params = json!({"type": "action", "payload": {"hello": "world"}});
        let encoded = HlPostWire.encode_request(99, "post", params);
        let v: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(v["method"], "post");
        assert_eq!(v["id"], 99);
        assert_eq!(v["request"]["type"], "action");
        assert_eq!(v["request"]["payload"]["hello"], "world");
    }

    #[test]
    fn decode_status_err_routes_to_error() {
        let frame = r#"{
            "channel": "post",
            "data": {
                "id": 1,
                "response": {
                    "type": "action",
                    "payload": {
                        "status": "err",
                        "response": "price too far from oracle"
                    }
                }
            }
        }"#;
        let frame = HlPostWire.decode_frame(frame).unwrap();
        assert!(matches!(
            frame,
            Frame::Response {
                id: 1,
                result: Err(_)
            }
        ));
    }
}
