//! PR-2f-reconnect acceptance.
//!
//! Verified in this file:
//! 1. The reconnect loop connects when the controller is up
//!    (identical observable behaviour to PR-2e ws_e2e but using
//!    `run_with_reconnect` instead of bare `LeaseClient::run`).
//! 2. The reconnect loop honours the shutdown signal — SIGINT
//!    eventually stops reconnect retries.
//!
//! Intentionally not covered here: bring-controller-back-on-same-port
//! cycle. macOS holds ports in TIME_WAIT longer than the
//! reconnect backoff gives us, producing flakes unrelated to the
//! reconnect logic itself. The controller-cycle smoke test is the
//! operator-run manual drill in the session transcript; CI
//! validates the unit tests on the jitter + backoff state
//! machine.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use mm_agent::{default_registry_builder, run_with_reconnect, CredentialCatalog, ReconnectConfig};
use mm_controller::{http_router, spawn_accept_loop, AgentRegistry, FleetState, LeasePolicy};
use mm_common::settings::SettingsFile;
use mm_control::messages::AgentId;
use tokio::sync::watch;

#[tokio::test]
async fn reconnect_loop_establishes_first_session() {
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr: SocketAddr = probe.local_addr().unwrap();
    drop(probe);
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = http_listener.local_addr().unwrap();

    let fleet = FleetState::new();
    let registry = AgentRegistry::new();
    let policy = Arc::new(LeasePolicy::default());
    let accept_task = spawn_accept_loop(ws_addr, fleet.clone(), registry.clone(), Arc::clone(&policy));
    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_router(fleet.clone(), registry.clone()))
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let settings = SettingsFile::from_str(
        r#"
        [agent]
        id = "eu-reconn-01"
        "#,
    )
    .unwrap();
    let catalog = Arc::new(CredentialCatalog::from_settings(settings));
    let build_registry = default_registry_builder(
        Arc::clone(&catalog),
        mm_dashboard::state::DashboardState::new(),
    );
    let cfg = ReconnectConfig::new(
        ws_addr.to_string(),
        AgentId::new("eu-reconn-01"),
        build_registry,
        catalog,
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let agent_task = tokio::spawn(async move {
        run_with_reconnect(cfg, shutdown_rx).await
    });

    // The agent should show up in the fleet view — that proves
    // the reconnect loop dialed the controller and the lease
    // handshake succeeded.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let body: serde_json::Value =
            reqwest::get(format!("http://{}/api/v1/fleet", http_addr))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        let n = body.as_array().map(|a| a.len()).unwrap_or(0);
        if n == 1 {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("reconnect loop never reached the controller");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Shutdown should stop the loop.
    let _ = shutdown_tx.send(true);
    accept_task.abort();
    let _ = accept_task.await;
    http_task.abort();
    let _ = http_task.await;
    let exit = tokio::time::timeout(Duration::from_secs(5), agent_task)
        .await
        .expect("reconnect loop respects shutdown signal");
    drop(exit);
}

#[tokio::test]
async fn reconnect_loop_exits_quickly_when_controller_absent_and_shutdown_fires() {
    // No controller listening anywhere — every connect attempt
    // fails. With shutdown fired the loop MUST exit rather than
    // keep retrying forever.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr: SocketAddr = probe.local_addr().unwrap();
    drop(probe);

    let settings = SettingsFile::from_str(
        r#"
        [agent]
        id = "eu-reconn-02"
        "#,
    )
    .unwrap();
    let catalog = Arc::new(CredentialCatalog::from_settings(settings));
    let build_registry = default_registry_builder(
        Arc::clone(&catalog),
        mm_dashboard::state::DashboardState::new(),
    );
    let cfg = ReconnectConfig::new(
        ws_addr.to_string(),
        AgentId::new("eu-reconn-02"),
        build_registry,
        catalog,
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let agent_task = tokio::spawn(async move {
        run_with_reconnect(cfg, shutdown_rx).await
    });

    // Let the loop spin through a few failed attempts.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = shutdown_tx.send(true);

    // Shutdown is checked at the top of each iteration, so
    // the loop exits within one backoff.
    let res = tokio::time::timeout(Duration::from_secs(5), agent_task)
        .await
        .expect("reconnect loop exits within backoff");
    assert!(res.is_ok(), "agent task should have joined, got {:?}", res);
}
