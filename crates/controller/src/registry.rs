//! Agent registry — the map HTTP handlers use to reach a
//! specific connected agent and push commands at it.
//!
//! Every accepted [`AgentSession`] owns a command-in mpsc
//! receiver; the sender half lives in the registry keyed by
//! agent id. When an HTTP handler receives a deploy request it
//! looks up the agent, clones the sender, and pushes a
//! [`CommandPayload`] through. The session's pump loop forwards
//! that payload to the transport as a signed command envelope.
//!
//! The registry is distinct from [`crate::FleetState`]:
//! - FleetState = "what's the observable state of each agent"
//!   (snapshot-friendly, read-mostly, exposed to UI readers).
//! - AgentRegistry = "how do I send to a specific agent"
//!   (write-once on connect, looked up by controllers).
//!
//! Keeping them separate means the HTTP readers can snapshot
//! FleetState without taking any write-locks, and the deploy
//! path stays decoupled from the observability path.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use mm_control::messages::{AgentId, CommandPayload};
use tokio::sync::mpsc;

/// Controller-internal event that flips the admission-control
/// state on an active session. Distinct from [`CommandPayload`]
/// because these events never travel on the wire — they're
/// HTTP → session nudges that tell the session "do the
/// lease-grant dance now" or "revoke authority now".
#[derive(Debug, Clone)]
pub enum SessionLifecycleEvent {
    /// Operator approved this session's fingerprint. Session
    /// issues a fresh lease + pushes every authorised credential.
    ApprovalGranted,
    /// Operator revoked authority. Session sends LeaseRevoke +
    /// drops its held lease; agent walks the fail-ladder.
    ApprovalRevoked { reason: String },
}

/// Sender half of a session's command-in channel. Cheap to
/// clone so HTTP handlers can send to the same agent from
/// multiple concurrent request handlers without contention.
#[derive(Debug, Clone)]
pub struct AgentCommandTx {
    inner: mpsc::UnboundedSender<CommandPayload>,
    lifecycle: mpsc::UnboundedSender<SessionLifecycleEvent>,
}

impl AgentCommandTx {
    pub fn new(
        tx: mpsc::UnboundedSender<CommandPayload>,
        lifecycle: mpsc::UnboundedSender<SessionLifecycleEvent>,
    ) -> Self {
        Self { inner: tx, lifecycle }
    }

    /// Push a command. Returns `Err` when the session on the
    /// other end has dropped (disconnected agent). Callers
    /// translate that into a 503 / 404 as appropriate.
    pub fn push(&self, cmd: CommandPayload) -> Result<(), RegistryError> {
        self.inner
            .send(cmd)
            .map_err(|_| RegistryError::AgentGone)
    }

    /// Push a session-lifecycle event. Same error semantics as
    /// [`push`] — session gone means nothing happened.
    pub fn push_lifecycle(&self, ev: SessionLifecycleEvent) -> Result<(), RegistryError> {
        self.lifecycle
            .send(ev)
            .map_err(|_| RegistryError::AgentGone)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("agent is not connected")]
    NotFound,
    #[error("agent session is shutting down")]
    AgentGone,
}

/// Resolved payload for a [`CommandPayload::FetchDeploymentDetails`]
/// request, routed to the waiting HTTP handler via its oneshot.
#[derive(Debug, Clone)]
pub struct DetailsReply {
    pub deployment_id: String,
    pub topic: String,
    pub payload: serde_json::Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentRegistry {
    inner: Arc<RwLock<HashMap<String, AgentCommandTx>>>,
    /// Pending details requests, keyed by `request_id`. HTTP
    /// handler stores a oneshot sender here, the telemetry loop
    /// resolves it when a matching `DetailsReply` arrives.
    /// Entries expire via the HTTP handler's timeout — the
    /// handler removes its own entry on timeout so a late reply
    /// just logs + drops.
    pending_details:
        Arc<RwLock<HashMap<uuid::Uuid, tokio::sync::oneshot::Sender<DetailsReply>>>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, id: &AgentId, tx: AgentCommandTx) {
        if let Ok(mut guard) = self.inner.write() {
            guard.insert(id.as_str().to_string(), tx);
        }
    }

    pub fn deregister(&self, id: &AgentId) {
        if let Ok(mut guard) = self.inner.write() {
            guard.remove(id.as_str());
        }
    }

    pub fn send(&self, id: &str, cmd: CommandPayload) -> Result<(), RegistryError> {
        let tx = {
            let guard = self.inner.read().map_err(|_| RegistryError::AgentGone)?;
            guard.get(id).cloned()
        };
        match tx {
            Some(tx) => tx.push(cmd),
            None => Err(RegistryError::NotFound),
        }
    }

    pub fn send_lifecycle(&self, id: &str, ev: SessionLifecycleEvent) -> Result<(), RegistryError> {
        let tx = {
            let guard = self.inner.read().map_err(|_| RegistryError::AgentGone)?;
            guard.get(id).cloned()
        };
        match tx {
            Some(tx) => tx.push_lifecycle(ev),
            None => Err(RegistryError::NotFound),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Register a pending details request. The HTTP handler
    /// awaits on `rx`; the telemetry loop resolves via
    /// [`Self::resolve_pending_details`] when a matching reply
    /// lands.
    pub fn pending_details_register(
        &self,
        request_id: uuid::Uuid,
        tx: tokio::sync::oneshot::Sender<DetailsReply>,
    ) {
        if let Ok(mut guard) = self.pending_details.write() {
            guard.insert(request_id, tx);
        }
    }

    /// HTTP handler calls this on its own timeout to reclaim the
    /// slot — prevents a late-arriving reply from lingering.
    pub fn pending_details_forget(&self, request_id: uuid::Uuid) {
        if let Ok(mut guard) = self.pending_details.write() {
            guard.remove(&request_id);
        }
    }

    /// Telemetry loop hands matching replies back to the waiter.
    /// Returns `true` when a waiter was resolved (the HTTP
    /// handler should then ignore its timeout). Returns `false`
    /// when no waiter is registered — handler already timed out
    /// or never existed. Late replies log at debug.
    pub fn resolve_pending_details(&self, reply: DetailsReply, request_id: uuid::Uuid) -> bool {
        let waiter = {
            let Ok(mut guard) = self.pending_details.write() else {
                return false;
            };
            guard.remove(&request_id)
        };
        match waiter {
            Some(tx) => tx.send(reply).is_ok(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_control::messages::CommandPayload;

    #[test]
    fn register_and_send_happy_path() {
        let reg = AgentRegistry::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (lc_tx, _lc_rx) = mpsc::unbounded_channel();
        reg.register(&AgentId::new("eu-01"), AgentCommandTx::new(tx, lc_tx));

        assert_eq!(reg.len(), 1);
        reg.send("eu-01", CommandPayload::Heartbeat).unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn send_to_unknown_id_returns_not_found() {
        let reg = AgentRegistry::new();
        let err = reg.send("ghost", CommandPayload::Heartbeat).unwrap_err();
        assert!(matches!(err, RegistryError::NotFound));
    }

    #[test]
    fn deregister_removes_agent() {
        let reg = AgentRegistry::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (lc_tx, _lc_rx) = mpsc::unbounded_channel();
        let id = AgentId::new("eu-02");
        reg.register(&id, AgentCommandTx::new(tx, lc_tx));
        reg.deregister(&id);
        assert!(reg.is_empty());
        assert!(matches!(
            reg.send("eu-02", CommandPayload::Heartbeat),
            Err(RegistryError::NotFound)
        ));
    }

    #[test]
    fn send_after_session_dropped_returns_gone() {
        let reg = AgentRegistry::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let (lc_tx, _lc_rx) = mpsc::unbounded_channel();
        reg.register(&AgentId::new("eu-03"), AgentCommandTx::new(tx, lc_tx));
        drop(rx);
        let err = reg.send("eu-03", CommandPayload::Heartbeat).unwrap_err();
        assert!(matches!(err, RegistryError::AgentGone));
    }
}
