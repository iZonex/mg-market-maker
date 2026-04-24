//! WS accept loop — one task binds the listener and spawns a
//! dedicated [`AgentSession`] task per accepted agent
//! connection.
//!
//! Kept separate from `main.rs` so integration tests can drive
//! the accept loop against an ephemeral port that the test
//! itself manages.
//!
//! Graceful shutdown: dropping the [`JoinHandle`] returned by
//! [`spawn_accept_loop`] aborts the listener + every in-flight
//! session. For PR-2e that is enough — PR-2f layers a
//! `CancellationToken` so sessions drain in-flight commands
//! before hanging up.

use std::net::SocketAddr;
use std::sync::Arc;

use mm_control::ws_transport::WsListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::{AgentRegistry, AgentSession, ApprovalStore, FleetState, LeasePolicy, VaultStore};

/// Bind `addr`, accept incoming WS connections, spawn one
/// [`AgentSession`] per accepted socket. Each session runs until
/// its agent disconnects or it returns an error — both of which
/// are logged and do not affect siblings.
pub async fn run_accept_loop(
    addr: SocketAddr,
    fleet: FleetState,
    registry: AgentRegistry,
    credentials: Option<VaultStore>,
    approvals: Option<ApprovalStore>,
    policy: Arc<LeasePolicy>,
    tls: Option<TlsAcceptor>,
) -> anyhow::Result<()> {
    let bare = WsListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let local = bare.local_addr().map_err(|e| anyhow::anyhow!("{e}"))?;
    let listener = match tls {
        Some(acceptor) => {
            tracing::info!(addr = %local, scheme = "wss", "controller WS listening (TLS enabled)");
            bare.with_tls(acceptor)
        }
        None => {
            tracing::info!(addr = %local, scheme = "ws", "controller WS listening (plain)");
            bare
        }
    };

    loop {
        let (transport, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "controller WS accept failed — continuing");
                continue;
            }
        };
        let mut session = AgentSession::new(transport, (*policy).clone())
            .with_fleet(fleet.clone())
            .with_command_channel(registry.clone());
        if let Some(ref store) = credentials {
            session = session.with_credentials(store.clone());
        }
        if let Some(ref store) = approvals {
            session = session.with_approvals(store.clone());
        }
        tokio::spawn(async move {
            tracing::info!(peer = %peer, "controller accepted agent session");
            match session.run_until_disconnect().await {
                Ok(()) => tracing::info!(peer = %peer, "agent session ended cleanly"),
                Err(e) => tracing::warn!(peer = %peer, error = %e, "agent session error"),
            }
        });
    }
}

/// Convenience wrapper: spawn the accept loop in its own task so
/// the binary / test can drive other work in parallel. Dropping
/// the returned handle aborts the loop; no sessions are drained.
pub fn spawn_accept_loop(
    addr: SocketAddr,
    fleet: FleetState,
    registry: AgentRegistry,
    policy: Arc<LeasePolicy>,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(run_accept_loop(
        addr, fleet, registry, None, None, policy, None,
    ))
}

pub fn spawn_accept_loop_with_credentials(
    addr: SocketAddr,
    fleet: FleetState,
    registry: AgentRegistry,
    credentials: VaultStore,
    policy: Arc<LeasePolicy>,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(run_accept_loop(
        addr,
        fleet,
        registry,
        Some(credentials),
        None,
        policy,
        None,
    ))
}

pub fn spawn_accept_loop_with_credentials_and_approvals(
    addr: SocketAddr,
    fleet: FleetState,
    registry: AgentRegistry,
    credentials: Option<VaultStore>,
    approvals: ApprovalStore,
    policy: Arc<LeasePolicy>,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(run_accept_loop(
        addr,
        fleet,
        registry,
        credentials,
        Some(approvals),
        policy,
        None,
    ))
}

/// TLS-aware variant. Operators pass a `TlsAcceptor` (built via
/// [`mm_control::build_acceptor`] from their PEM cert + key);
/// the returned task serves wss:// instead of ws://.
pub fn spawn_accept_loop_tls(
    addr: SocketAddr,
    fleet: FleetState,
    registry: AgentRegistry,
    policy: Arc<LeasePolicy>,
    tls: TlsAcceptor,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(run_accept_loop(
        addr,
        fleet,
        registry,
        None,
        None,
        policy,
        Some(tls),
    ))
}

pub fn spawn_accept_loop_tls_with_credentials(
    addr: SocketAddr,
    fleet: FleetState,
    registry: AgentRegistry,
    credentials: VaultStore,
    policy: Arc<LeasePolicy>,
    tls: TlsAcceptor,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(run_accept_loop(
        addr,
        fleet,
        registry,
        Some(credentials),
        None,
        policy,
        Some(tls),
    ))
}

pub fn spawn_accept_loop_tls_full(
    addr: SocketAddr,
    fleet: FleetState,
    registry: AgentRegistry,
    credentials: Option<VaultStore>,
    approvals: ApprovalStore,
    policy: Arc<LeasePolicy>,
    tls: TlsAcceptor,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(run_accept_loop(
        addr,
        fleet,
        registry,
        credentials,
        Some(approvals),
        policy,
        Some(tls),
    ))
}
