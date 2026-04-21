//! PR-2d-lite acceptance: the HTTP `/api/v1/fleet` endpoint
//! reflects the in-memory session state.
//!
//! Wiring:
//! 1. Build a [`FleetState`], mount the router on a local
//!    ephemeral listener, kick off the axum server task.
//! 2. Spawn an [`AgentSession`] with `.with_fleet(fleet.clone())`
//!    against an in-memory transport pair.
//! 3. Drive a `LeaseClient` over the other end of the pair so
//!    registration + lease-grant actually happen.
//! 4. `curl`-equivalent GET on `/api/v1/fleet`; assert the
//!    connected agent appears with the expected protocol version
//!    and a populated lease.

use std::net::SocketAddr;
use std::time::Duration;

use mm_agent::{AgentConfig, LeaseClient};
use mm_controller::{http_router, AgentRegistry, AgentSession, FleetState, LeasePolicy};
use mm_control::in_memory_pair;
use mm_control::lease::LeaseState;
use mm_control::messages::AgentId;

#[tokio::test]
async fn fleet_http_returns_connected_agent() {
    let fleet = FleetState::new();

    // Bind a listener on an ephemeral port so parallel test
    // runs do not collide on port numbers.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let app = http_router(fleet.clone(), AgentRegistry::new());
    let http_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Controller-side: one session sharing the fleet map.
    let (controller_side, agent_side) = in_memory_pair();
    let session = AgentSession::new(controller_side, LeasePolicy::default()).with_fleet(fleet.clone());
    let controller_task = tokio::spawn(async move { session.run_until_disconnect().await });

    // Agent-side: drive the handshake through the in-memory pair.
    let (client, authority) = LeaseClient::new(
        agent_side,
        AgentConfig {
            id: AgentId::new("eu-fleet-01"),
            ..Default::default()
        },
    );
    let agent_task = tokio::spawn(async move { client.run().await });

    // Wait for the lease to land on the authority channel so we
    // know the session has registered + written into the fleet.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if matches!(authority.current(), LeaseState::Held(_)) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("agent never received a lease");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Hit the HTTP endpoint and verify the agent shows up.
    let url = format!("http://{}/api/v1/fleet", addr);
    let body: serde_json::Value = reqwest::get(&url)
        .await
        .expect("GET /api/v1/fleet")
        .json()
        .await
        .expect("parse JSON response");
    let agents = body.as_array().expect("fleet endpoint returns an array");
    assert_eq!(agents.len(), 1, "exactly one agent connected");
    let agent = &agents[0];
    assert_eq!(agent["agent_id"], "eu-fleet-01");
    assert_eq!(agent["protocol_version"], mm_control::PROTOCOL_VERSION);
    assert!(
        agent["current_lease"].is_object(),
        "lease should be populated after handshake, got {:?}",
        agent["current_lease"]
    );

    // `/health` is owned by the dashboard router in
    // `mm-server`'s merged app; this controller-only test mounts
    // just the controller half, so the SPA fallback catches
    // unmatched routes. Liveness probe coverage lives in the
    // dashboard's own tests.

    // Tear down cleanly.
    agent_task.abort();
    let _ = agent_task.await;
    let _ = controller_task.await;
    http_task.abort();
    let _ = http_task.await;
}

#[tokio::test]
async fn fleet_http_is_empty_without_sessions() {
    let fleet = FleetState::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let app = http_router(fleet, AgentRegistry::new());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let body: serde_json::Value = reqwest::get(format!("http://{}/api/v1/fleet", addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body.as_array().map(|a| a.len()),
        Some(0),
        "empty fleet returns []"
    );
    task.abort();
    let _ = task.await;
}
