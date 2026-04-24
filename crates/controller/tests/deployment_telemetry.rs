//! PR-2i acceptance: agent pushes DeploymentState after
//! reconcile; GET /api/v1/agents/{id}/deployments reflects it.

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
async fn deployment_telemetry_surfaces_on_http() {
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = http_listener.local_addr().unwrap();

    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr: SocketAddr = probe.local_addr().unwrap();
    drop(probe);

    let fleet = FleetState::new();
    let registry = AgentRegistry::new();
    let policy = Arc::new(LeasePolicy::default());

    let http_app = http_router(fleet.clone(), registry.clone());
    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_app).await.unwrap();
    });
    let accept_task = spawn_accept_loop(
        ws_addr,
        fleet.clone(),
        registry.clone(),
        Arc::clone(&policy),
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let (probe_tx, _probe_rx) = watch::channel(0u64);
    let mock_registry = StrategyRegistry::new(Arc::new(MockEngineFactory::new(probe_tx)));

    let transport = WsTransport::connect(&ws_addr.to_string()).await.unwrap();
    let (client, authority) = LeaseClient::new(
        transport,
        AgentConfig {
            id: AgentId::new("eu-tel-01"),
            ..Default::default()
        },
    );
    let client = client.with_registry(mock_registry);
    let agent_task = tokio::spawn(async move { client.run().await });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(authority.current(), LeaseState::Held(_)) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("no lease");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Before any deploy, GET returns an empty array (agent is
    // connected but hasn't pushed deployment state yet).
    let empty_body: serde_json::Value = reqwest::get(format!(
        "http://{}/api/v1/agents/eu-tel-01/deployments",
        http_addr
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(empty_body.as_array().map(|a| a.len()), Some(0));

    // Push a deployment + wait for the telemetry to land.
    let deploy_url = format!("http://{}/api/v1/agents/eu-tel-01/deployments", http_addr);
    let body = json!({
        "strategies": [
            { "deployment_id": "dep-A", "template": "mock", "symbol": "BTCUSDT", "bindings": {} },
            { "deployment_id": "dep-B", "template": "mock", "symbol": "ETHUSDT", "bindings": {} }
        ]
    });
    let resp = reqwest::Client::new()
        .post(&deploy_url)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Poll until the GET reflects the pushed state — the agent
    // sends telemetry asynchronously, so give it a short window.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let body: serde_json::Value = reqwest::get(&deploy_url)
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body.as_array().map(|a| a.len()).unwrap_or(0) == 2 {
            let mut ids: Vec<String> = body
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["deployment_id"].as_str().unwrap().to_string())
                .collect();
            ids.sort();
            assert_eq!(ids, vec!["dep-A".to_string(), "dep-B".to_string()]);
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("deployment telemetry never reached the controller HTTP");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Push an empty slice — the agent should emit a fresh frame
    // with the empty deployment set.
    let empty = json!({ "strategies": [] });
    let resp = reqwest::Client::new()
        .post(&deploy_url)
        .json(&empty)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let body: serde_json::Value = reqwest::get(&deploy_url)
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body.as_array().map(|a| a.len()) == Some(0) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("controller never saw the empty deployment set");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    agent_task.abort();
    let _ = agent_task.await;
    accept_task.abort();
    let _ = accept_task.await;
    http_task.abort();
    let _ = http_task.await;
}

#[tokio::test]
async fn deployment_endpoint_returns_404_for_unknown_agent() {
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = http_listener.local_addr().unwrap();
    let fleet = FleetState::new();
    let registry = AgentRegistry::new();
    let app = http_router(fleet, registry);
    let task = tokio::spawn(async move {
        axum::serve(http_listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!(
        "http://{}/api/v1/agents/ghost/deployments",
        http_addr
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 404);

    task.abort();
    let _ = task.await;
}
