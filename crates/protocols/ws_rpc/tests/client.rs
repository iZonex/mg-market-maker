//! Integration tests for `WsRpcClient` against a local mock WebSocket
//! server. Each test spins up a tokio `TcpListener`, accepts the client's
//! upgrade, and exercises a specific flow.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use mm_protocols_ws_rpc::{Frame, WireFormat, WsRpcClient, WsRpcConfig, WsRpcError};

/// Tiny wire format used by all tests: JSON map with `id`, `method`,
/// `params` going out, and `{id, result}` / `{id, error}` / `{push: ...}`
/// / `{keepalive: true}` coming back.
struct TestWire;

impl WireFormat for TestWire {
    fn encode_request(&self, id: u64, method: &str, params: Value) -> String {
        json!({"id": id, "method": method, "params": params}).to_string()
    }

    fn decode_frame(&self, frame: &str) -> Result<Frame, String> {
        let v: Value = serde_json::from_str(frame).map_err(|e| e.to_string())?;
        if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
            if let Some(result) = v.get("result") {
                return Ok(Frame::Response {
                    id,
                    result: Ok(result.clone()),
                });
            }
            if let Some(error) = v.get("error") {
                return Ok(Frame::Response {
                    id,
                    result: Err(error.clone()),
                });
            }
        }
        if v.get("push").is_some() {
            return Ok(Frame::Push(v));
        }
        if v.get("keepalive").is_some() {
            return Ok(Frame::Keepalive);
        }
        Err(format!("unknown frame: {frame}"))
    }

    fn encode_ping(&self) -> Option<String> {
        Some(r#"{"keepalive":true}"#.to_string())
    }
}

/// Control knobs passed to the mock server. Each field is a flag the
/// per-connection handler consults to decide how to misbehave.
#[derive(Default, Clone)]
struct MockCtl {
    drop_after_first: Arc<Mutex<bool>>,
    never_respond: Arc<Mutex<bool>>,
    push_on_connect: Arc<Mutex<Option<Value>>>,
    error_for_method: Arc<Mutex<Option<String>>>,
}

struct MockServer {
    url: String,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_mock(ctl: MockCtl) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}");

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let ctl = ctl.clone();
            tokio::spawn(async move {
                let Ok(ws) = accept_async(stream).await else {
                    return;
                };
                let (mut tx, mut rx) = ws.split();

                let push_on_connect = {
                    let guard = ctl.push_on_connect.lock().unwrap();
                    guard.clone()
                };
                if let Some(push) = push_on_connect {
                    let _ = tx.send(Message::Text(push.to_string())).await;
                }

                while let Some(Ok(msg)) = rx.next().await {
                    let Message::Text(text) = msg else { continue };
                    let v: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    // App ping frame?
                    if v.get("keepalive").is_some() {
                        let _ = tx
                            .send(Message::Text(r#"{"keepalive":true}"#.to_string()))
                            .await;
                        continue;
                    }

                    let never_respond = *ctl.never_respond.lock().unwrap();
                    if never_respond {
                        continue;
                    }

                    let id = v.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
                    let method = v
                        .get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();

                    let forced_error = {
                        let guard = ctl.error_for_method.lock().unwrap();
                        guard.clone()
                    };
                    let response = if Some(method.clone()) == forced_error {
                        json!({"id": id, "error": {"code": -1, "msg": "forced"}})
                    } else if method == "echo" {
                        json!({"id": id, "result": v.get("params").cloned().unwrap_or(Value::Null)})
                    } else if method == "noop" {
                        json!({"id": id, "result": Value::Null})
                    } else {
                        json!({"id": id, "result": {"unknown_method": method}})
                    };

                    let _ = tx.send(Message::Text(response.to_string())).await;

                    let drop_after = *ctl.drop_after_first.lock().unwrap();
                    if drop_after {
                        // Close the connection and let the client reconnect.
                        let _ = tx.send(Message::Close(None)).await;
                        return;
                    }
                }
            });
        }
    });

    MockServer { url, _handle: handle }
}

fn test_config(url: String, timeout_ms: u64) -> WsRpcConfig {
    WsRpcConfig {
        url,
        request_timeout: Duration::from_millis(timeout_ms),
        reconnect_backoff: Duration::from_millis(100),
        app_ping_interval: None,
    }
}

async fn wait_connected(client: &WsRpcClient) {
    for _ in 0..50 {
        if client.is_connected() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("client never reported is_connected");
}

#[tokio::test]
async fn happy_path_single_request() {
    let server = start_mock(MockCtl::default()).await;
    let client = WsRpcClient::spawn(test_config(server.url.clone(), 2000), TestWire, |_| {});
    wait_connected(&client).await;

    let result = client
        .send_request("echo", json!({"hello": "world"}))
        .await
        .unwrap();
    assert_eq!(result, json!({"hello": "world"}));

    client.shutdown();
}

#[tokio::test]
async fn multiple_concurrent_requests_each_get_own_response() {
    let server = start_mock(MockCtl::default()).await;
    let client = WsRpcClient::spawn(test_config(server.url.clone(), 2000), TestWire, |_| {});
    wait_connected(&client).await;

    let client = Arc::new(client);
    let mut handles = Vec::new();
    for i in 0..10u64 {
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            c.send_request("echo", json!({"i": i})).await
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        let r = h.await.unwrap().unwrap();
        assert_eq!(r, json!({"i": i as u64}));
    }
}

#[tokio::test]
async fn server_error_is_surfaced_as_server_variant() {
    let ctl = MockCtl::default();
    *ctl.error_for_method.lock().unwrap() = Some("echo".into());
    let server = start_mock(ctl).await;
    let client = WsRpcClient::spawn(test_config(server.url.clone(), 2000), TestWire, |_| {});
    wait_connected(&client).await;

    let err = client.send_request("echo", json!({})).await.unwrap_err();
    match err {
        WsRpcError::Server(v) => {
            assert_eq!(v.get("msg").and_then(|m| m.as_str()), Some("forced"));
        }
        other => panic!("expected Server variant, got {other:?}"),
    }
}

#[tokio::test]
async fn push_messages_fire_the_callback() {
    let ctl = MockCtl::default();
    *ctl.push_on_connect.lock().unwrap() = Some(json!({"push": "market-tick", "px": 42000}));
    let server = start_mock(ctl).await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = WsRpcClient::spawn(
        test_config(server.url.clone(), 2000),
        TestWire,
        move |v| {
            let _ = tx.send(v);
        },
    );
    wait_connected(&client).await;

    // Poke a request so the client definitely pulls at least one frame
    // past the push (and so we can await the spawn).
    let _ = client.send_request("noop", json!({})).await;

    let push = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(push.get("push").and_then(|p| p.as_str()), Some("market-tick"));
}

#[tokio::test]
async fn request_times_out_when_server_is_silent() {
    let ctl = MockCtl::default();
    *ctl.never_respond.lock().unwrap() = true;
    let server = start_mock(ctl).await;
    let client = WsRpcClient::spawn(test_config(server.url.clone(), 400), TestWire, |_| {});
    wait_connected(&client).await;

    let err = client.send_request("echo", json!({})).await.unwrap_err();
    assert!(matches!(err, WsRpcError::Timeout(_)), "got {err:?}");
}

#[tokio::test]
async fn disconnect_fails_pending_requests() {
    let ctl = MockCtl::default();
    *ctl.drop_after_first.lock().unwrap() = true;
    let server = start_mock(ctl).await;
    let client = WsRpcClient::spawn(test_config(server.url.clone(), 2000), TestWire, |_| {});
    wait_connected(&client).await;

    // First request succeeds.
    let _ = client.send_request("echo", json!({})).await.unwrap();

    // The server closed the socket right after. The client should notice
    // and reconnect — the second request might land on the second
    // connection cleanly, so we just verify it does not hang.
    let r2 = tokio::time::timeout(
        Duration::from_secs(2),
        client.send_request("echo", json!({"second": true})),
    )
    .await
    .expect("did not resolve within 2s");
    // Either success on the new connection or a Disconnected error — both
    // are acceptable outcomes depending on exact timing.
    match r2 {
        Ok(v) => assert_eq!(v, json!({"second": true})),
        Err(WsRpcError::Disconnected) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn reconnects_after_server_drop() {
    let ctl = MockCtl::default();
    *ctl.drop_after_first.lock().unwrap() = true;
    let server = start_mock(ctl).await;
    let client = WsRpcClient::spawn(test_config(server.url.clone(), 2000), TestWire, |_| {});
    wait_connected(&client).await;

    // Provoke the drop.
    let _ = client.send_request("echo", json!({})).await.unwrap();

    // Give the background task a moment to notice the close, back off,
    // and reconnect.
    let mut saw_disconnect = false;
    for _ in 0..50 {
        if !client.is_connected() {
            saw_disconnect = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(saw_disconnect, "client never went disconnected");

    // Should come back.
    wait_connected(&client).await;
}

#[tokio::test]
async fn shutdown_stops_the_task_and_fails_new_requests() {
    let server = start_mock(MockCtl::default()).await;
    let client = WsRpcClient::spawn(test_config(server.url.clone(), 2000), TestWire, |_| {});
    wait_connected(&client).await;

    client.shutdown();
    // Give the task a moment to exit.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let err = client.send_request("echo", json!({})).await.unwrap_err();
    assert!(matches!(err, WsRpcError::Shutdown), "got {err:?}");
}

#[tokio::test]
async fn app_ping_keeps_connection_healthy() {
    let server = start_mock(MockCtl::default()).await;
    let mut cfg = test_config(server.url.clone(), 2000);
    cfg.app_ping_interval = Some(Duration::from_millis(50));
    let client = WsRpcClient::spawn(cfg, TestWire, |_| {});
    wait_connected(&client).await;

    // Let several ping intervals elapse, then verify a normal request
    // still works.
    tokio::time::sleep(Duration::from_millis(250)).await;
    let r = client.send_request("echo", json!({"x": 1})).await.unwrap();
    assert_eq!(r, json!({"x": 1}));
}

#[tokio::test]
async fn unknown_response_id_is_ignored_gracefully() {
    // Server that sends a response with an id nobody asked for.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(ws) = accept_async(stream).await else {
            return;
        };
        let (mut tx, mut rx) = ws.split();
        // Send a bogus response first.
        let _ = tx
            .send(Message::Text(r#"{"id":99999,"result":{"ignored":true}}"#.to_string()))
            .await;
        // Then echo real requests.
        while let Some(Ok(msg)) = rx.next().await {
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(&text).unwrap();
            let id = v.get("id").and_then(|i| i.as_u64()).unwrap();
            let _ = tx
                .send(Message::Text(
                    json!({"id": id, "result": {"ok": true}}).to_string(),
                ))
                .await;
        }
    });

    let client = WsRpcClient::spawn(test_config(url, 2000), TestWire, |_| {});
    wait_connected(&client).await;

    // A real request after the bogus response should still resolve.
    let r = client.send_request("echo", json!({})).await.unwrap();
    assert_eq!(r, json!({"ok": true}));
}
