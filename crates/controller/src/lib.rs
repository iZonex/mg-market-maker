//! Controller-side control-plane library.
//!
//! The controller runs one [`AgentSession`] per connected agent. Each
//! session owns that agent's transport, issues the initial lease
//! on connect, honours refresh requests while the agent remains
//! well-behaved, and walks away cleanly when either side closes.
//!
//! Kept library-shaped so integration tests can drive a session
//! directly against an in-memory transport without standing up
//! a real WS-RPC listener. The binary (`src/main.rs`) is the
//! entry point that will, in follow-up PRs, own the WS-RPC
//! accept loop + per-agent session spawn. For PR-1 the binary
//! is a stub that exists only to prove the workspace wires up.

pub mod approvals;
pub mod fleet;
pub mod http;
pub mod master_key;
pub mod registry;
pub mod replay;
pub mod templates;
pub mod tunables;
pub mod vault;
pub mod ws_accept;

pub use approvals::{ApprovalRecord, ApprovalState, ApprovalStore, ApprovalStoreError};
pub use fleet::{AgentView, FleetState};
pub use http::{
    router as http_router, router_full as http_router_full,
    router_full_authed as http_router_full_authed, run_http_server, run_http_server_full,
};
pub use master_key::{MasterKey, MasterKeyError};
pub use registry::{AgentCommandTx, AgentRegistry, RegistryError, SessionLifecycleEvent};
pub use replay::ReplayStore;
pub use tunables::{Tunables, TunablesError, TunablesStore};
pub use vault::{
    CredentialCheck, CredentialDescriptor, VaultEntry, VaultError, VaultStore, VaultSummary,
};
pub use ws_accept::{
    run_accept_loop, spawn_accept_loop, spawn_accept_loop_tls, spawn_accept_loop_tls_full,
    spawn_accept_loop_tls_with_credentials, spawn_accept_loop_with_credentials,
    spawn_accept_loop_with_credentials_and_approvals,
};

use std::time::Duration;

use mm_control::envelope::{Envelope, SignedEnvelope};
use mm_control::lease::LeaderLease;
use mm_control::messages::{AgentId, CommandPayload, TelemetryPayload};
use mm_control::seq::Seq;
use mm_control::transport::{Transport, TransportError};
use tokio::sync::mpsc;

/// Policy knobs the controller uses when issuing leases. Defaults
/// follow the research-informed 30s-expiry / 10s-refresh shape;
/// tests set shorter windows to keep runs fast.
#[derive(Debug, Clone)]
pub struct LeasePolicy {
    pub lease_ttl: Duration,
    /// Hard cap on how often an agent may request a refresh —
    /// cheap guard against a misbehaving agent burning CPU. Honest
    /// agents refresh once per 1/3 of `lease_ttl`.
    pub min_refresh_interval: Duration,
    /// Agent-binary-version compatibility policy. Controller refuses
    /// to lease agents whose advertised `agent_version` falls
    /// outside this range. `None` on either bound disables that
    /// side — a range of `(None, None)` accepts any version.
    /// Production deployments set both bounds to pin a rollout
    /// window.
    pub min_agent_version: Option<semver::Version>,
    pub max_agent_version: Option<semver::Version>,
}

impl Default for LeasePolicy {
    fn default() -> Self {
        // 120s TTL with the agent's 1/3 refresh fraction gives a
        // ~40s window to refresh — enough headroom for a slow
        // startup burst (engine spawn, WS handshake, credential
        // push) without the first lease expiring mid-deploy.
        // Previous 30s default (from the handshake-loop tests)
        // kept failing the real deploy loop because the main
        // refresh cycle couldn't race a chatty boot sequence.
        Self {
            lease_ttl: Duration::from_secs(120),
            min_refresh_interval: Duration::from_secs(3),
            min_agent_version: None,
            max_agent_version: None,
        }
    }
}

/// Produce a semver-style version policy from `CARGO_PKG_VERSION`
/// of the running controller. Fleet best practice: lock agents to the
/// same major.minor as controller and accept any patch revision above
/// that. Operators can override via the binary's env.
pub fn default_version_policy() -> (Option<semver::Version>, Option<semver::Version>) {
    let raw = env!("CARGO_PKG_VERSION");
    match semver::Version::parse(raw) {
        Ok(v) => {
            let min = semver::Version::new(v.major, v.minor, 0);
            let max = semver::Version::new(v.major, v.minor.saturating_add(1), 0);
            (Some(min), Some(max))
        }
        Err(_) => (None, None),
    }
}

/// One agent's view from the controller side. Owns the transport,
/// the per-agent command sequence, and the currently issued
/// lease (if any).
pub struct AgentSession<T: Transport> {
    transport: T,
    cmd_seq: Seq,
    policy: LeasePolicy,
    /// Populated once the agent has registered. Before that we
    /// refuse to issue a lease — the controller doesn't authorise a
    /// peer it can't address.
    agent_id: Option<AgentId>,
    /// Most recent lease issued to this agent. Retained so a
    /// subsequent refresh can re-use the same `lease_id` (an
    /// agent tracks its current lease by id and expects the
    /// same one on extension).
    current_lease: Option<LeaderLease>,
    /// Optional shared fleet view. When attached, the session
    /// writes register / lease / heartbeat / ack events into
    /// the map so HTTP readers + dashboards can observe the
    /// session without holding the transport themselves. None
    /// for transport-only tests that don't care about the fleet
    /// view.
    fleet: Option<FleetState>,
    /// Optional credential store — when attached, the session
    /// pushes every resolved credential to the agent after the
    /// first LeaseGrant so reconcile-time deploys find them in
    /// the agent's in-memory catalog.
    credentials: Option<VaultStore>,
    /// Optional approval store — admission-control gate. When
    /// attached, the session only issues LeaseGrant + pushes
    /// credentials for fingerprints in the `Approved` state;
    /// `Pending` / `Revoked` fingerprints stay connected and
    /// visible in the fleet but hold no authority. When absent
    /// the session accepts every register (legacy / test path).
    approvals: Option<ApprovalStore>,
    /// Command-in channel. When attached, the session pumps any
    /// `CommandPayload` that arrives on `cmd_in_rx` out through
    /// the transport as a fresh signed command envelope. This is
    /// the hook HTTP handlers use to reach the agent — they push
    /// into the mirrored sender half held in [`AgentRegistry`].
    cmd_in_rx: Option<mpsc::UnboundedReceiver<CommandPayload>>,
    /// Session-lifecycle events channel. HTTP handlers push
    /// `ApprovalGranted` / `ApprovalRevoked` here when an
    /// operator flips admission state so the session can act
    /// without the operator having to kick the agent.
    lifecycle_rx: Option<mpsc::UnboundedReceiver<SessionLifecycleEvent>>,
    /// Optional registry shared with the accept loop. The session
    /// registers itself on first Register telemetry + removes
    /// itself on disconnect so HTTP handlers see only live agents.
    agent_registry: Option<AgentRegistry>,
    /// Sender side retained by the session so it can register
    /// itself with the registry once the agent id is known.
    /// Present when `cmd_in_rx` is set.
    cmd_in_tx: Option<mpsc::UnboundedSender<CommandPayload>>,
    /// Mirror of `lifecycle_rx` — sender half kept so the
    /// session can register itself with the registry.
    lifecycle_tx: Option<mpsc::UnboundedSender<SessionLifecycleEvent>>,
    /// Pubkey fingerprint of the registered agent. Set by
    /// `on_register` once the pubkey has been checked against
    /// the approval store. Operators see this on the UI.
    pubkey_fingerprint: Option<String>,
    /// Cached approval state — controls whether subsequent
    /// grant / revoke paths fire.
    approval_state: ApprovalState,
}

/// Disambiguation for the run loop's three-way select. Kept
/// private to the crate because it's purely an implementation
/// detail of [`AgentSession::run_until_disconnect`].
// The `Incoming` branch intentionally carries the envelope by
// value — boxing would force an allocation for every frame on
// the hot receive path, where the transport already pools its
// buffer. The ~480 byte variant size is the SignedEnvelope
// cost, not a miss.
#[allow(clippy::large_enum_variant)]
enum TransportSelect {
    Incoming(Result<Option<SignedEnvelope>, TransportError>),
    OutboundCommand(Option<CommandPayload>),
    Lifecycle(Option<SessionLifecycleEvent>),
}

/// Validate an agent's advertised binary version against the
/// controller's configured compatibility range. Empty / missing
/// version strings are treated as "unknown" — rejected when
/// either bound is set (explicit opt-in needed for production),
/// accepted otherwise.
fn check_agent_version(policy: &LeasePolicy, raw: &str) -> Result<(), String> {
    let has_bound = policy.min_agent_version.is_some() || policy.max_agent_version.is_some();
    if raw.is_empty() {
        return if has_bound {
            Err("agent did not advertise a version".into())
        } else {
            Ok(())
        };
    }
    let parsed = semver::Version::parse(raw)
        .map_err(|e| format!("agent version '{raw}' failed to parse: {e}"))?;
    if let Some(min) = &policy.min_agent_version {
        if parsed < *min {
            return Err(format!("agent version {parsed} is below min {min}"));
        }
    }
    if let Some(max) = &policy.max_agent_version {
        if parsed >= *max {
            return Err(format!(
                "agent version {parsed} is at/above exclusive max {max}"
            ));
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("agent sent payload before registering")]
    NotRegistered,
    #[error("agent refresh arrived too soon (min interval is {min_secs}s)")]
    RefreshTooFast { min_secs: u64 },
    #[error("agent sent unexpected envelope kind")]
    BadEnvelope,
}

impl<T: Transport> AgentSession<T> {
    pub fn new(transport: T, policy: LeasePolicy) -> Self {
        Self {
            transport,
            cmd_seq: Seq::ZERO,
            policy,
            agent_id: None,
            current_lease: None,
            fleet: None,
            credentials: None,
            approvals: None,
            cmd_in_rx: None,
            lifecycle_rx: None,
            agent_registry: None,
            cmd_in_tx: None,
            lifecycle_tx: None,
            pubkey_fingerprint: None,
            approval_state: ApprovalState::Accepted,
        }
    }

    /// Attach the admission-control store. When set, the session
    /// gates LeaseGrant + credential push on the fingerprint's
    /// approval state. When absent the session accepts every
    /// register unconditionally (legacy / test path).
    pub fn with_approvals(mut self, store: ApprovalStore) -> Self {
        self.approvals = Some(store);
        self
    }

    /// Attach the controller-side credential store. After the session
    /// issues its first LeaseGrant it pushes every resolved
    /// credential to the agent. Rotation: operators hot-replace
    /// the store's entries and nudge affected agents to reconnect
    /// (or wait for the next normal heartbeat cycle; future PR
    /// adds proactive push-on-rotate).
    pub fn with_credentials(mut self, store: VaultStore) -> Self {
        self.credentials = Some(store);
        self
    }

    /// Attach the shared fleet view. Builder-style so the controller
    /// binary can thread one shared [`FleetState`] through every
    /// session it accepts.
    pub fn with_fleet(mut self, fleet: FleetState) -> Self {
        self.fleet = Some(fleet);
        self
    }

    /// Attach a command-in channel + the shared registry. Once
    /// the agent registers, the session inserts `cmd_in_tx` +
    /// `lifecycle_tx` into the registry keyed by agent id so
    /// HTTP handlers can route commands AND admission-control
    /// events to this session. On disconnect the session
    /// deregisters itself.
    pub fn with_command_channel(mut self, registry: AgentRegistry) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (lc_tx, lc_rx) = mpsc::unbounded_channel();
        self.cmd_in_rx = Some(rx);
        self.cmd_in_tx = Some(tx);
        self.lifecycle_rx = Some(lc_rx);
        self.lifecycle_tx = Some(lc_tx);
        self.agent_registry = Some(registry);
        self
    }

    pub fn agent_id(&self) -> Option<&AgentId> {
        self.agent_id.as_ref()
    }

    pub fn current_lease(&self) -> Option<&LeaderLease> {
        self.current_lease.as_ref()
    }

    /// Drive the session until the peer closes. Pumps both
    /// directions:
    /// - inbound transport → telemetry handlers (register,
    ///   refresh-request, heartbeat, ack)
    /// - inbound command channel → outbound transport (HTTP
    ///   handlers push `SetDesiredStrategies` / kill / heartbeat
    ///   commands this way)
    pub async fn run_until_disconnect(mut self) -> Result<(), SessionError> {
        loop {
            // Extract the two channel receivers for the duration
            // of a single select! iteration. Take and restore so
            // the borrow checker is happy across branches that
            // borrow `self.transport` and `self` simultaneously.
            let mut cmd_rx = self.cmd_in_rx.take();
            let mut lc_rx = self.lifecycle_rx.take();

            let event = async {
                match (cmd_rx.as_mut(), lc_rx.as_mut()) {
                    (Some(cr), Some(lr)) => tokio::select! {
                        incoming = self.transport.recv() => TransportSelect::Incoming(incoming),
                        cmd = cr.recv() => TransportSelect::OutboundCommand(cmd),
                        ev = lr.recv() => TransportSelect::Lifecycle(ev),
                    },
                    (Some(cr), None) => tokio::select! {
                        incoming = self.transport.recv() => TransportSelect::Incoming(incoming),
                        cmd = cr.recv() => TransportSelect::OutboundCommand(cmd),
                    },
                    (None, Some(lr)) => tokio::select! {
                        incoming = self.transport.recv() => TransportSelect::Incoming(incoming),
                        ev = lr.recv() => TransportSelect::Lifecycle(ev),
                    },
                    (None, None) => TransportSelect::Incoming(self.transport.recv().await),
                }
            }
            .await;

            self.cmd_in_rx = cmd_rx;
            self.lifecycle_rx = lc_rx;

            match event {
                TransportSelect::Incoming(Ok(None)) => {
                    tracing::info!(
                        agent = ?self.agent_id.as_ref().map(|a| a.as_str()),
                        "agent disconnected"
                    );
                    if let (Some(fleet), Some(id)) = (self.fleet.as_ref(), self.agent_id.as_ref()) {
                        fleet.on_disconnect(id);
                    }
                    if let (Some(reg), Some(id)) =
                        (self.agent_registry.as_ref(), self.agent_id.as_ref())
                    {
                        reg.deregister(id);
                    }
                    return Ok(());
                }
                TransportSelect::Incoming(Err(e)) => return Err(e.into()),
                TransportSelect::Incoming(Ok(Some(signed))) => {
                    self.on_envelope(signed).await?;
                }
                TransportSelect::OutboundCommand(None) => {
                    self.cmd_in_rx = None;
                }
                TransportSelect::OutboundCommand(Some(cmd)) => {
                    self.push_command(cmd).await?;
                }
                TransportSelect::Lifecycle(None) => {
                    self.lifecycle_rx = None;
                }
                TransportSelect::Lifecycle(Some(ev)) => {
                    self.on_lifecycle(ev).await?;
                }
            }
        }
    }

    async fn on_lifecycle(&mut self, ev: SessionLifecycleEvent) -> Result<(), SessionError> {
        match ev {
            SessionLifecycleEvent::ApprovalGranted => {
                if self.approval_state == ApprovalState::Accepted {
                    // Duplicate approve click — session already
                    // granted. Idempotent no-op.
                    return Ok(());
                }
                self.approval_state = ApprovalState::Accepted;
                if let (Some(fleet), Some(id)) = (self.fleet.as_ref(), self.agent_id.as_ref()) {
                    fleet.update_approval_state(id, ApprovalState::Accepted.as_str());
                }
                tracing::info!(
                    agent = ?self.agent_id.as_ref().map(|a| a.as_str()),
                    fingerprint = ?self.pubkey_fingerprint,
                    "agent approved at runtime — issuing lease + pushing credentials"
                );
                self.issue_initial_lease_and_credentials().await
            }
            SessionLifecycleEvent::ApprovalRevoked { reason } => {
                if self.approval_state == ApprovalState::Revoked {
                    return Ok(());
                }
                self.approval_state = ApprovalState::Revoked;
                if let (Some(fleet), Some(id)) = (self.fleet.as_ref(), self.agent_id.as_ref()) {
                    fleet.update_approval_state(id, ApprovalState::Revoked.as_str());
                }
                tracing::warn!(
                    agent = ?self.agent_id.as_ref().map(|a| a.as_str()),
                    fingerprint = ?self.pubkey_fingerprint,
                    reason = %reason,
                    "agent approval revoked at runtime — sending LeaseRevoke"
                );
                self.current_lease = None;
                self.push_command(CommandPayload::LeaseRevoke { reason })
                    .await
            }
        }
    }

    async fn on_envelope(&mut self, signed: SignedEnvelope) -> Result<(), SessionError> {
        let Envelope {
            telemetry, command, ..
        } = signed.envelope;
        if command.is_some() {
            // Agents never send commands back at the controller.
            return Err(SessionError::BadEnvelope);
        }
        let Some(payload) = telemetry else {
            return Err(SessionError::BadEnvelope);
        };
        match payload {
            TelemetryPayload::Register {
                agent_id,
                last_applied,
                protocol_version,
                pubkey,
                agent_version,
            } => {
                self.on_register(
                    agent_id,
                    protocol_version,
                    last_applied,
                    agent_version,
                    pubkey,
                )
                .await
            }
            TelemetryPayload::LeaseRefreshRequest { current_lease_id } => {
                self.on_refresh_request(current_lease_id).await
            }
            TelemetryPayload::Heartbeat { agent_clock_ms } => {
                if let (Some(fleet), Some(id)) = (self.fleet.as_ref(), self.agent_id.as_ref()) {
                    fleet.on_heartbeat(id, agent_clock_ms);
                }
                Ok(())
            }
            TelemetryPayload::Ack { applied_seq } => {
                if let (Some(fleet), Some(id)) = (self.fleet.as_ref(), self.agent_id.as_ref()) {
                    fleet.on_ack(id, applied_seq);
                }
                Ok(())
            }
            TelemetryPayload::DeploymentState { deployments } => {
                if let (Some(fleet), Some(id)) = (self.fleet.as_ref(), self.agent_id.as_ref()) {
                    fleet.on_deployment_state(id, deployments);
                }
                Ok(())
            }
            TelemetryPayload::DetailsReply {
                request_id,
                deployment_id,
                topic,
                payload,
                error,
            } => {
                if let Some(reg) = self.agent_registry.as_ref() {
                    let resolved = reg.resolve_pending_details(
                        crate::registry::DetailsReply {
                            deployment_id,
                            topic,
                            payload,
                            error,
                        },
                        request_id,
                    );
                    if !resolved {
                        tracing::debug!(
                            %request_id,
                            "DetailsReply arrived with no pending waiter — HTTP handler likely timed out"
                        );
                    }
                }
                Ok(())
            }
        }
    }

    async fn on_register(
        &mut self,
        agent_id: AgentId,
        protocol_version: u16,
        last_applied: Seq,
        agent_version: String,
        pubkey: Option<mm_control::identity::PublicKey>,
    ) -> Result<(), SessionError> {
        if protocol_version != mm_control::PROTOCOL_VERSION {
            tracing::warn!(
                requested = protocol_version,
                expected = mm_control::PROTOCOL_VERSION,
                "agent protocol version mismatch — refusing lease"
            );
            return Err(SessionError::BadEnvelope);
        }
        if let Err(reason) = check_agent_version(&self.policy, &agent_version) {
            tracing::warn!(
                agent = %agent_id.as_str(),
                reported = %agent_version,
                reason = %reason,
                "agent binary version outside compatibility range — refusing lease"
            );
            return Err(SessionError::BadEnvelope);
        }

        // Admission control — consult the approval store if one
        // is attached. Unknown fingerprint → the store inserts a
        // Pending record and returns Pending; operator approves
        // via HTTP to flip state and trigger lease grant.
        let (fingerprint, approval_state) = match (self.approvals.as_ref(), pubkey.as_ref()) {
            (Some(store), Some(pk)) => {
                let fp = pk.fingerprint();
                let pubkey_hex = pk.to_hex();
                let st = store.record_register(&fp, agent_id.as_str(), &pubkey_hex);
                (Some(fp), st)
            }
            (Some(_), None) => {
                // Store is attached but the agent did not send a
                // pubkey — refuse the session outright. Legacy
                // unsigned agents belong behind a feature flag,
                // not on a production controller.
                tracing::warn!(
                    agent = %agent_id.as_str(),
                    "approval store is enabled but agent registered without a pubkey — refusing session"
                );
                return Err(SessionError::BadEnvelope);
            }
            (None, _) => (None, ApprovalState::Accepted),
        };
        self.pubkey_fingerprint = fingerprint.clone();
        self.approval_state = approval_state;

        let fp_str = fingerprint.clone().unwrap_or_default();
        if let Some(fleet) = self.fleet.as_ref() {
            fleet.on_register_full(
                &agent_id,
                protocol_version,
                last_applied,
                &agent_version,
                &fp_str,
                approval_state.as_str(),
            );
        }
        // Register into the command-in registry so HTTP handlers
        // can reach this session — needed BEFORE admission check
        // so operators can approve pending agents via the registry.
        if let (Some(reg), Some(tx), Some(lc_tx)) = (
            self.agent_registry.as_ref(),
            self.cmd_in_tx.as_ref(),
            self.lifecycle_tx.as_ref(),
        ) {
            reg.register(&agent_id, AgentCommandTx::new(tx.clone(), lc_tx.clone()));
        }
        self.agent_id = Some(agent_id);

        // Admission gate — only Approved agents get a lease +
        // credentials at register time. Pending / Revoked stay
        // connected and visible; lease grant fires later via
        // the lifecycle channel when the operator flips state.
        if !approval_state.is_accepted() {
            tracing::warn!(
                agent = ?self.agent_id.as_ref().map(|a| a.as_str()),
                fingerprint = ?self.pubkey_fingerprint,
                state = approval_state.as_str(),
                "agent registered but not approved — no lease / no credentials pushed"
            );
            return Ok(());
        }

        self.issue_initial_lease_and_credentials().await
    }

    /// Fire the first LeaseGrant + push every authorised
    /// credential. Called from `on_register` for already-approved
    /// agents, and from the lifecycle handler when an operator
    /// approves a pending agent mid-session.
    async fn issue_initial_lease_and_credentials(&mut self) -> Result<(), SessionError> {
        let lease = self.issue_fresh_lease();
        self.current_lease = Some(lease.clone());
        if let (Some(fleet), Some(id)) = (self.fleet.as_ref(), self.agent_id.as_ref()) {
            fleet.update_lease(id, lease.clone());
        }
        self.push_command(CommandPayload::LeaseGrant { lease })
            .await?;

        let batch: Vec<_> = match (self.credentials.as_ref(), self.agent_id.as_ref()) {
            (Some(store), Some(id)) => store.pushable_exchange_for_agent(id.as_str()),
            _ => Vec::new(),
        };
        let count = batch.len();
        for cred in batch {
            self.push_command(CommandPayload::PushCredential { credential: cred })
                .await?;
        }
        if count > 0 {
            tracing::info!(
                agent = ?self.agent_id.as_ref().map(|a| a.as_str()),
                credentials = count,
                "pushed {} credentials to agent",
                count
            );
        }
        Ok(())
    }

    async fn on_refresh_request(&mut self, lease_id: uuid::Uuid) -> Result<(), SessionError> {
        tracing::debug!(
            agent = ?self.agent_id.as_ref().map(|a| a.as_str()),
            %lease_id,
            "received lease refresh request"
        );
        if !self.approval_state.is_accepted() {
            // Agent is asking for a refresh, but operator has
            // revoked authority. Tell it explicitly so it walks
            // the fail-ladder instead of trying again in 3s.
            self.push_command(CommandPayload::LeaseRevoke {
                reason: "approval revoked".into(),
            })
            .await?;
            return Ok(());
        }
        let current = self
            .current_lease
            .as_ref()
            .ok_or(SessionError::NotRegistered)?;
        if current.lease_id != lease_id {
            tracing::warn!(
                current = %current.lease_id,
                requested = %lease_id,
                "refresh request cites unknown lease id — refusing"
            );
            return Err(SessionError::BadEnvelope);
        }
        // Simple rate limit: refuse refreshes issued closer than
        // `min_refresh_interval` to the last issuance. Real code
        // will also audit per-agent. Returning the typed error
        // used to bubble up through `on_envelope` and drop the
        // whole session — catastrophic for a chatty boot that
        // sends a stray refresh too early. Log + swallow so the
        // session survives; the next valid refresh fires at the
        // agent's 1/3 mark anyway.
        let since_issued = chrono::Utc::now() - current.issued_at;
        if since_issued.to_std().unwrap_or_default() < self.policy.min_refresh_interval {
            tracing::warn!(
                agent = ?self.agent_id.as_ref().map(|a| a.as_str()),
                %lease_id,
                since_issued_ms = since_issued.num_milliseconds(),
                min_interval_ms = self.policy.min_refresh_interval.as_millis() as i64,
                "refresh request rate-limited — ignoring (session stays alive)"
            );
            return Ok(());
        }
        let refreshed = self.issue_extended_lease(current.lease_id);
        self.current_lease = Some(refreshed.clone());
        if let (Some(fleet), Some(id)) = (self.fleet.as_ref(), self.agent_id.as_ref()) {
            fleet.update_lease(id, refreshed.clone());
        }
        tracing::debug!(
            agent = ?self.agent_id.as_ref().map(|a| a.as_str()),
            %lease_id,
            new_expires_at = %refreshed.expires_at,
            "issuing lease refresh"
        );
        self.push_command(CommandPayload::LeaseRefresh { lease: refreshed })
            .await
    }

    fn issue_fresh_lease(&self) -> LeaderLease {
        let issued = chrono::Utc::now();
        let expires = issued
            + chrono::Duration::from_std(self.policy.lease_ttl).expect("ttl fits chrono::Duration");
        LeaderLease {
            lease_id: uuid::Uuid::new_v4(),
            agent_id: self
                .agent_id
                .as_ref()
                .map(|a| a.as_str().to_string())
                .unwrap_or_default(),
            issued_at: issued,
            expires_at: expires,
            issued_seq: self.cmd_seq.next(),
        }
    }

    fn issue_extended_lease(&self, reuse_lease_id: uuid::Uuid) -> LeaderLease {
        let issued = chrono::Utc::now();
        let expires = issued
            + chrono::Duration::from_std(self.policy.lease_ttl).expect("ttl fits chrono::Duration");
        LeaderLease {
            lease_id: reuse_lease_id,
            agent_id: self
                .agent_id
                .as_ref()
                .map(|a| a.as_str().to_string())
                .unwrap_or_default(),
            issued_at: issued,
            expires_at: expires,
            issued_seq: self.cmd_seq.next(),
        }
    }

    async fn push_command(&mut self, payload: CommandPayload) -> Result<(), SessionError> {
        self.cmd_seq = self.cmd_seq.next();
        let envelope = Envelope::command(self.cmd_seq, payload);
        let signed = SignedEnvelope::unsigned(envelope);
        self.transport.send(signed).await?;
        Ok(())
    }
}
