//! Vault CRUD HTTP contract — unified exchange-credential +
//! generic-secret store.

use std::net::SocketAddr;

use mm_controller::{http_router_full, AgentRegistry, FleetState, MasterKey, VaultStore};
use serde_json::json;

async fn bind_server(with_store: bool) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let fleet = FleetState::new();
    let registry = AgentRegistry::new();
    let store = if with_store {
        Some(VaultStore::in_memory_with_key(MasterKey::from_bytes([7u8; 32])))
    } else {
        None
    };
    let app = http_router_full(fleet, registry, store, None, None);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, task)
}

#[tokio::test]
async fn post_exchange_entry_appears_in_list() {
    let (addr, task) = bind_server(true).await;
    let client = reqwest::Client::new();

    let body = json!({
        "name": "binance_spot_main",
        "kind": "exchange",
        "values": { "api_key": "live-key", "api_secret": "live-secret" },
        "metadata": { "exchange": "binance", "product": "spot", "default_symbol": "BTCUSDT" }
    });
    let res = client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let list: serde_json::Value =
        reqwest::get(format!("http://{}/api/v1/vault", addr))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "binance_spot_main");
    assert_eq!(rows[0]["kind"], "exchange");
    assert_eq!(rows[0]["metadata"]["exchange"], "binance");
    // Secrets must not appear anywhere in the listing response.
    let raw = serde_json::to_string(&list).unwrap();
    assert!(!raw.contains("live-key"));
    assert!(!raw.contains("live-secret"));

    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn post_telegram_entry() {
    let (addr, task) = bind_server(true).await;
    let client = reqwest::Client::new();
    let body = json!({
        "name": "telegram_ops_bot",
        "kind": "telegram",
        "description": "ops alerts",
        "values": { "token": "BOT_TOKEN" },
        "metadata": { "chat_id": "-123456" }
    });
    let res = client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let list: serde_json::Value = reqwest::get(format!("http://{}/api/v1/vault", addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let raw = serde_json::to_string(&list).unwrap();
    assert!(!raw.contains("BOT_TOKEN"), "telegram token leaked: {raw}");

    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn post_duplicate_returns_conflict() {
    let (addr, task) = bind_server(true).await;
    let client = reqwest::Client::new();
    let body = json!({
        "name": "c1",
        "kind": "exchange",
        "values": { "api_key": "k", "api_secret": "s" },
        "metadata": { "exchange": "binance", "product": "spot" }
    });
    let _ = client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    let res = client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn put_rotates_existing_entry() {
    let (addr, task) = bind_server(true).await;
    let client = reqwest::Client::new();
    let create = json!({
        "name": "rotated",
        "kind": "exchange",
        "values": { "api_key": "old-k", "api_secret": "old-s" },
        "metadata": { "exchange": "binance", "product": "spot" }
    });
    client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&create)
        .send()
        .await
        .unwrap();
    let put_body = json!({
        "name": "rotated",
        "kind": "exchange",
        "values": { "api_key": "new-k", "api_secret": "new-s" },
        "metadata": { "exchange": "bybit", "product": "linear_perp" }
    });
    let res = client
        .put(format!("http://{}/api/v1/vault/rotated", addr))
        .json(&put_body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let list: serde_json::Value = reqwest::get(format!("http://{}/api/v1/vault", addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = list.as_array().unwrap();
    assert_eq!(rows[0]["metadata"]["exchange"], "bybit");
    assert_eq!(rows[0]["metadata"]["product"], "linear_perp");
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn delete_removes_entry() {
    let (addr, task) = bind_server(true).await;
    let client = reqwest::Client::new();
    let body = json!({
        "name": "gone",
        "kind": "exchange",
        "values": { "api_key": "k", "api_secret": "s" },
        "metadata": { "exchange": "binance", "product": "spot" }
    });
    client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    let res = client
        .delete(format!("http://{}/api/v1/vault/gone", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    let list: serde_json::Value = reqwest::get(format!("http://{}/api/v1/vault", addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn missing_store_returns_503() {
    let (addr, task) = bind_server(false).await;
    let client = reqwest::Client::new();
    let body = json!({ "name": "nope", "kind": "generic", "values": { "v": "x" } });
    let res = client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn exchange_without_required_values_returns_400() {
    let (addr, task) = bind_server(true).await;
    let client = reqwest::Client::new();
    let body = json!({
        "name": "x",
        "kind": "exchange",
        "values": { "api_key": "k" },    // api_secret missing
        "metadata": { "exchange": "binance", "product": "spot" }
    });
    let res = client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn bad_exchange_returns_400() {
    let (addr, task) = bind_server(true).await;
    let client = reqwest::Client::new();
    let body = json!({
        "name": "x",
        "kind": "exchange",
        "values": { "api_key": "k", "api_secret": "s" },
        "metadata": { "exchange": "nonesuch", "product": "spot" }
    });
    let res = client
        .post(format!("http://{}/api/v1/vault", addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    task.abort();
    let _ = task.await;
}
