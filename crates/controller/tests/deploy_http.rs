//! PR-2g acceptance: operator pushes a deployment over HTTP,
//! agent receives it + reconciles.
//!
//! Flow:
//! 1. Start controller WS accept loop + HTTP server on ephemeral ports.
//! 2. Start an agent with a MockEngineFactory; probe the factory's
//!    spawn counter to assert reconcile actually happens.
//! 3. POST a two-strategy slice to `/api/v1/agents/{id}/deployments`.
//!    Verify the probe bumps by two (A + B spawned).
//! 4. POST an empty slice — verify both tasks stop.
//! 5. POST to a non-existent agent id — expect 404.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use mm_agent::{AgentConfig, LeaseClient, MockEngineFactory, StrategyRegistry};
use mm_control::lease::LeaseState;
use mm_control::messages::AgentId;
use mm_control::ws_transport::WsTransport;
use mm_controller::{http_router, spawn_accept_loop, AgentRegistry, FleetState, LeasePolicy};
use serde_json::json;
use tokio::sync::watch;

#[tokio::test]
async fn deploy_http_pushes_set_desired_strategies() {
    // Controller HTTP listener on an ephemeral port.
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = http_listener.local_addr().unwrap();

    // Controller WS port via probe-and-drop.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr: SocketAddr = probe.local_addr().unwrap();
    drop(probe);

    let fleet = FleetState::new();
    let registry = AgentRegistry::new();
    let policy = Arc::new(LeasePolicy::default());

    // HTTP server.
    let http_app = http_router(fleet.clone(), registry.clone());
    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_app).await.unwrap();
    });
    // WS accept loop.
    let accept_task = spawn_accept_loop(
        ws_addr,
        fleet.clone(),
        registry.clone(),
        Arc::clone(&policy),
    );

    // Give the accept loop a moment to bind.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Agent side: mock factory with a probe counter so we can
    // observe spawn events.
    let (probe_tx, mut probe_rx) = watch::channel(0u64);
    let mock_registry = StrategyRegistry::new(Arc::new(MockEngineFactory::new(probe_tx)));

    let transport = WsTransport::connect(&ws_addr.to_string())
        .await
        .expect("agent dials controller");
    let (client, authority) = LeaseClient::new(
        transport,
        AgentConfig {
            id: AgentId::new("eu-deploy-01"),
            ..Default::default()
        },
    );
    let client = client.with_registry(mock_registry);
    let agent_task = tokio::spawn(async move { client.run().await });

    // Wait until agent holds a lease — proves it's registered
    // in AgentRegistry on the controller side too.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(authority.current(), LeaseState::Held(_)) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("agent never got a lease");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let url = format!(
        "http://{}/api/v1/agents/eu-deploy-01/deployments",
        http_addr
    );
    let body = json!({
        "strategies": [
            {
                "deployment_id": "dep-A",
                "template": "mock",
                "symbol": "BTCUSDT",
                "bindings": {},
            },
            {
                "deployment_id": "dep-B",
                "template": "mock",
                "symbol": "ETHUSDT",
                "bindings": {},
            }
        ]
    });
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("POST deploy");
    assert_eq!(resp.status(), 200);
    let resp_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(resp_body["accepted"], 2);

    // Wait for the mock factory to report two spawns.
    let after_first = read_probe_min(&mut probe_rx, 2).await;
    assert!(after_first >= 2, "two strategies spawned");

    // Empty slice — registry stops everything.
    let empty = json!({ "strategies": [] });
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&empty)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // Give the reconcile loop a tick to process the stop.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Non-existent agent → 404.
    let missing_url = format!("http://{}/api/v1/agents/ghost/deployments", http_addr);
    let resp = reqwest::Client::new()
        .post(&missing_url)
        .json(&empty)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // Tear down.
    agent_task.abort();
    let _ = agent_task.await;
    accept_task.abort();
    let _ = accept_task.await;
    http_task.abort();
    let _ = http_task.await;
}

async fn read_probe_min(rx: &mut watch::Receiver<u64>, min: u64) -> u64 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let v = *rx.borrow();
        if v >= min {
            return v;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("probe never reached {} (last saw {})", min, v);
        }
        let _ = tokio::time::timeout(Duration::from_millis(100), rx.changed()).await;
    }
}
