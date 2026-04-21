//! End-to-end WS-RPC acceptance.
//!
//! Unlike the in-memory integration tests, this one runs over
//! REAL sockets. Both the controller's accept loop and the agent's
//! `WsTransport::connect` ride on an actual TCP/WS stack.
//! That's the stack the colo deployment will use — if this test
//! passes, the PR-2e shape really works outside of tests.
//!
//! Flow:
//! 1. Start an accept loop bound to `127.0.0.1:0` and read back
//!    the ephemeral port it chose.
//! 2. Start the fleet HTTP server on a second ephemeral port.
//! 3. Dial `ws://<controller-ws>` from a `LeaseClient`.
//! 4. Wait for the authority handle to report `Held(_)`.
//! 5. GET `/api/v1/fleet`; expect the agent to appear.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use mm_agent::{AgentConfig, LeaseClient};
use mm_controller::{http_router, spawn_accept_loop, AgentRegistry, FleetState, LeasePolicy};
use mm_control::lease::LeaseState;
use mm_control::messages::AgentId;
use mm_control::ws_transport::WsTransport;

#[tokio::test]
async fn controller_and_agent_talk_over_real_ws() {
    // Bind the controller's HTTP listener first (ephemeral port).
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = http_listener.local_addr().unwrap();

    // Bind an ephemeral TCP listener ourselves + read back the
    // chosen port, then hand the address to the controller's accept
    // loop. Using `127.0.0.1:0` directly in `spawn_accept_loop`
    // would work but wouldn't give us the port back — so we
    // pre-bind and drop the probe listener before the real
    // accept loop starts.
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
    let accept_task = spawn_accept_loop(ws_addr, fleet.clone(), registry.clone(), Arc::clone(&policy));

    // Give the accept loop a beat to bind before dialing.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Agent dials the controller.
    let transport = WsTransport::connect(&ws_addr.to_string())
        .await
        .expect("agent dial succeeds");
    let (client, authority) = LeaseClient::new(
        transport,
        AgentConfig {
            id: AgentId::new("eu-ws-e2e-01"),
            ..Default::default()
        },
    );
    let agent_task = tokio::spawn(async move { client.run().await });

    // Wait until the agent has a Held lease — proves the full
    // register → grant handshake travelled the real wire.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(authority.current(), LeaseState::Held(_)) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("agent never received a lease over WS");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // HTTP /api/v1/fleet reflects the live session.
    let body: serde_json::Value = reqwest::get(format!("http://{}/api/v1/fleet", http_addr))
        .await
        .expect("HTTP GET")
        .json()
        .await
        .expect("JSON parse");
    let agents = body.as_array().expect("array");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["agent_id"], "eu-ws-e2e-01");
    assert!(
        agents[0]["current_lease"].is_object(),
        "lease populated after WS handshake"
    );

    // Tear down.
    agent_task.abort();
    let _ = agent_task.await;
    accept_task.abort();
    let _ = accept_task.await;
    http_task.abort();
    let _ = http_task.await;
}
