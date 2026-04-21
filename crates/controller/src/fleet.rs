//! Fleet state — the controller's aggregated view of every connected
//! agent.
//!
//! Each [`AgentSession`] owns a shared reference to a
//! [`FleetState`] and writes into it on meaningful events:
//! first register, every lease refresh, every heartbeat, and on
//! clean or forced disconnect. Readers (HTTP endpoint, dashboard
//! aggregator, future telemetry subscribers) snapshot the map in
//! one read lock.
//!
//! This is the controller's **eventually-consistent** view — never
//! the source of truth for trading decisions. Controller lag behind
//! agent ground truth is acceptable and expected under WAN
//! conditions. Operators use the fleet view to answer "what's
//! connected, is it healthy", not "what position does agent X
//! hold right now" (that's an agent-direct query).
//!
//! No per-strategy telemetry yet — that arrives when the
//! SetDesiredStrategies → reconcile ack loop from PR-2a starts
//! flowing state upstream in a follow-up PR.
//!
//! [`AgentSession`]: crate::AgentSession

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use chrono::Utc;
use mm_control::lease::LeaderLease;
use mm_control::messages::{AgentId, DeploymentStateRow};
use mm_control::seq::Seq;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    pub agent_id: String,
    /// UTC millis of the last telemetry we saw from this agent.
    /// Operators read this as "is this agent alive" without
    /// having to trust the lease (the lease is a separate
    /// authority question; an agent can be sending heartbeats
    /// long after its lease expired, which is a distinct class
    /// of problem).
    pub last_seen_ms: i64,
    /// Protocol version the agent advertised at register time.
    pub protocol_version: u16,
    /// Agent binary version (`CARGO_PKG_VERSION` on the agent side).
    /// Empty for agents built before the handshake carried it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_version: String,
    /// Stable short fingerprint derived from the agent's Ed25519
    /// public key. Admission-control key — operators approve by
    /// fingerprint, not by self-advertised `agent_id`. Empty when
    /// the agent registered without a pubkey (legacy path).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pubkey_fingerprint: String,
    /// Approval-store state for this fingerprint. One of
    /// `pending`, `approved`, `revoked`, or `unknown` (when no
    /// approval store is attached). UI uses this to decide
    /// whether to show approve/reject controls or lease chips.
    #[serde(default)]
    pub approval_state: String,
    /// Currently-issued lease. `None` before the first
    /// [`CommandPayload::LeaseGrant`] fires; operators see this
    /// as "pending handshake".
    pub current_lease: Option<LeaderLease>,
    /// Last command seq the agent ACK'd. Advances with every
    /// applied command — lets operators detect an agent that
    /// receives commands but cannot apply them.
    pub last_applied_seq: Seq,
    /// Agent-reported wall clock at the last heartbeat. Useful
    /// for clock-skew detection against the controller's own clock
    /// without having to run PTP everywhere.
    pub agent_clock_ms: Option<i64>,
    /// Latest deployment snapshot the agent pushed. Updated
    /// in-place on each DeploymentState telemetry — controller does
    /// not retain history, HTTP readers always see the newest
    /// frame. Empty when the agent has not pushed any yet.
    pub deployments: Vec<DeploymentStateRow>,
}

impl AgentView {
    fn bare(agent_id: &str, protocol_version: u16, last_applied_seq: Seq) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            last_seen_ms: Utc::now().timestamp_millis(),
            protocol_version,
            agent_version: String::new(),
            pubkey_fingerprint: String::new(),
            approval_state: String::new(),
            current_lease: None,
            last_applied_seq,
            agent_clock_ms: None,
            deployments: Vec::new(),
        }
    }
}

/// Shared fleet state. Cheaply cloneable [`Arc`] — both the
/// per-agent session writers and the HTTP endpoint reader hold
/// the same underlying map.
#[derive(Debug, Clone, Default)]
pub struct FleetState {
    inner: Arc<RwLock<HashMap<String, AgentView>>>,
}

impl FleetState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh agent OR refresh its `last_seen_ms` on a
    /// re-register. Does not overwrite lease / seq state — those
    /// land via [`FleetState::update_lease`] etc.
    pub fn on_register(&self, id: &AgentId, protocol_version: u16, last_applied: Seq) {
        self.on_register_full(id, protocol_version, last_applied, "", "", "");
    }

    /// Extended register with admission-control metadata. All
    /// three additional fields may be empty strings — the UI
    /// handles them as "unknown" which is fine for legacy /
    /// test paths that don't set them.
    pub fn on_register_full(
        &self,
        id: &AgentId,
        protocol_version: u16,
        last_applied: Seq,
        agent_version: &str,
        pubkey_fingerprint: &str,
        approval_state: &str,
    ) {
        if let Ok(mut guard) = self.inner.write() {
            let entry = guard
                .entry(id.as_str().to_string())
                .or_insert_with(|| AgentView::bare(id.as_str(), protocol_version, last_applied));
            entry.last_seen_ms = Utc::now().timestamp_millis();
            entry.protocol_version = protocol_version;
            entry.last_applied_seq = last_applied;
            if !agent_version.is_empty() {
                entry.agent_version = agent_version.to_string();
            }
            if !pubkey_fingerprint.is_empty() {
                entry.pubkey_fingerprint = pubkey_fingerprint.to_string();
            }
            if !approval_state.is_empty() {
                entry.approval_state = approval_state.to_string();
            }
        }
    }

    /// Update the cached approval state for an agent that has
    /// already registered. Called when an operator approves or
    /// revokes via the admission-control HTTP surface so the
    /// fleet view flips without waiting for the next register.
    pub fn update_approval_state(&self, id: &AgentId, state: &str) {
        if let Ok(mut guard) = self.inner.write() {
            if let Some(entry) = guard.get_mut(id.as_str()) {
                entry.approval_state = state.to_string();
                entry.last_seen_ms = Utc::now().timestamp_millis();
            }
        }
    }

    pub fn update_lease(&self, id: &AgentId, lease: LeaderLease) {
        if let Ok(mut guard) = self.inner.write() {
            if let Some(entry) = guard.get_mut(id.as_str()) {
                entry.current_lease = Some(lease);
                entry.last_seen_ms = Utc::now().timestamp_millis();
            }
        }
    }

    pub fn on_heartbeat(&self, id: &AgentId, agent_clock_ms: i64) {
        if let Ok(mut guard) = self.inner.write() {
            if let Some(entry) = guard.get_mut(id.as_str()) {
                entry.agent_clock_ms = Some(agent_clock_ms);
                entry.last_seen_ms = Utc::now().timestamp_millis();
            }
        }
    }

    pub fn on_ack(&self, id: &AgentId, applied_seq: Seq) {
        if let Ok(mut guard) = self.inner.write() {
            if let Some(entry) = guard.get_mut(id.as_str()) {
                entry.last_applied_seq = applied_seq;
                entry.last_seen_ms = Utc::now().timestamp_millis();
            }
        }
    }

    /// Replace the agent's deployment snapshot vec wholesale.
    /// Frames are self-contained — each one advertises the
    /// complete running set + per-deployment live state — so the
    /// controller does not reconcile per-row, it just takes the
    /// newest frame as ground truth.
    pub fn on_deployment_state(&self, id: &AgentId, rows: Vec<DeploymentStateRow>) {
        if let Ok(mut guard) = self.inner.write() {
            if let Some(entry) = guard.get_mut(id.as_str()) {
                entry.deployments = rows;
                entry.last_seen_ms = Utc::now().timestamp_millis();
            }
        }
    }

    /// Remove an agent from the fleet view — called on clean
    /// disconnect. Keeps the map tight in the common case; for
    /// audit-level "historical agents" we'd keep a separate
    /// on-disk log, not pile them up in live memory.
    pub fn on_disconnect(&self, id: &AgentId) {
        if let Ok(mut guard) = self.inner.write() {
            guard.remove(id.as_str());
        }
    }

    /// Snapshot for HTTP / dashboard readers. Sorted by agent_id
    /// so the client-side UI doesn't jitter between polls.
    pub fn snapshot(&self) -> Vec<AgentView> {
        let mut out: Vec<AgentView> = self
            .inner
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default();
        out.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        out
    }

    pub fn get(&self, id: &str) -> Option<AgentView> {
        self.inner
            .read()
            .ok()
            .and_then(|g| g.get(id).cloned())
    }

    /// Returns the internal shared reference so tests /
    /// long-running observers can watch the map. Avoids handing
    /// out the RwLock directly.
    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl FleetState {
    #[cfg(test)]
    fn test_view(&self, id: &str) -> Option<AgentView> {
        self.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use mm_control::seq::Seq;
    use uuid::Uuid;

    fn agent(id: &str) -> AgentId {
        AgentId::new(id)
    }

    fn lease(expires_in_secs: i64) -> LeaderLease {
        let now = Utc::now();
        LeaderLease {
            lease_id: Uuid::nil(),
            agent_id: "test".into(),
            issued_at: now,
            expires_at: now + Duration::seconds(expires_in_secs),
            issued_seq: Seq(1),
        }
    }

    #[test]
    fn register_creates_bare_view() {
        let state = FleetState::new();
        state.on_register(&agent("eu-01"), 1, Seq::ZERO);
        let v = state.test_view("eu-01").unwrap();
        assert_eq!(v.agent_id, "eu-01");
        assert_eq!(v.protocol_version, 1);
        assert!(v.current_lease.is_none());
    }

    #[test]
    fn lease_update_persists() {
        let state = FleetState::new();
        state.on_register(&agent("eu-01"), 1, Seq::ZERO);
        state.update_lease(&agent("eu-01"), lease(30));
        let v = state.test_view("eu-01").unwrap();
        assert!(v.current_lease.is_some());
    }

    #[test]
    fn ack_advances_seq() {
        let state = FleetState::new();
        state.on_register(&agent("eu-01"), 1, Seq::ZERO);
        state.on_ack(&agent("eu-01"), Seq(42));
        assert_eq!(state.test_view("eu-01").unwrap().last_applied_seq, Seq(42));
    }

    #[test]
    fn heartbeat_records_clock() {
        let state = FleetState::new();
        state.on_register(&agent("eu-01"), 1, Seq::ZERO);
        state.on_heartbeat(&agent("eu-01"), 1_700_000_000_000);
        assert_eq!(
            state.test_view("eu-01").unwrap().agent_clock_ms,
            Some(1_700_000_000_000)
        );
    }

    #[test]
    fn disconnect_removes_entry() {
        let state = FleetState::new();
        state.on_register(&agent("eu-01"), 1, Seq::ZERO);
        assert_eq!(state.len(), 1);
        state.on_disconnect(&agent("eu-01"));
        assert!(state.is_empty());
    }

    #[test]
    fn snapshot_is_sorted_by_id() {
        let state = FleetState::new();
        state.on_register(&agent("zulu-01"), 1, Seq::ZERO);
        state.on_register(&agent("alpha-01"), 1, Seq::ZERO);
        state.on_register(&agent("mike-01"), 1, Seq::ZERO);
        let ids: Vec<String> = state.snapshot().into_iter().map(|v| v.agent_id).collect();
        assert_eq!(ids, vec!["alpha-01", "mike-01", "zulu-01"]);
    }

    #[test]
    fn update_lease_noop_on_unknown_agent() {
        // Robustness: a stray lease update for an agent that
        // never registered (bug elsewhere) must not create a
        // phantom entry — the fleet is driven by registration,
        // full stop.
        let state = FleetState::new();
        state.update_lease(&agent("ghost"), lease(30));
        assert!(state.is_empty());
    }
}
