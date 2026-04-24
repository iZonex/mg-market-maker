//! Agent-side control-plane library.
//!
//! The agent runs one [`LeaseClient`] per control-plane
//! connection. It registers, accepts a lease, refreshes it at
//! 1/3 of its lifetime, and emits telemetry/acks in response to
//! controller commands. On lease expiry or revocation it flips its
//! [`LeaseState`] to the terminal state — engines then walk the
//! fail-ladder.
//!
//! PR-1 scope: the lease loop + the terminal-state signal. Fail-
//! ladder execution (hooking into the real engine kill switch)
//! lands with PR-2 where the agent owns engine tasks. For now
//! the loop just emits a tracing event and exits when authority
//! is lost.

pub mod app_config;
pub mod catalog;
pub mod connector_factory;
pub mod engine_runner;
pub mod fail_ladder_walker;
pub mod graph_replay;
pub mod market_maker_runner;
pub mod reconnect;
pub mod registry;

pub use app_config::build_agent_config;
pub use market_maker_runner::{product_fallback, MarketMakerRunner};
pub use reconnect::{default_registry_builder, run_with_reconnect, ReconnectConfig};

// `AuthorityHandle` is defined below in this file; re-exported
// here so the binary (and future PR-2c-iii callers) can name it
// without referring to the hidden private struct position.

pub use catalog::CredentialCatalog;
pub use connector_factory::build_connector;
pub use engine_runner::{RealEngineFactory, SubscribeOnlyRunner};
pub use fail_ladder_walker::{FailAction, FailLadderWalker};
pub use registry::{EngineFactory, MockEngineFactory, SpawnedEngine, StrategyRegistry};

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::sync::watch;

use mm_control::envelope::{Envelope, SignedEnvelope};
use mm_control::identity::IdentityKey;
use mm_control::lease::{LeaderLease, LeaseState};
use mm_control::messages::{AgentId, CommandPayload, TelemetryPayload};
use mm_control::seq::Cursor;
use mm_control::transport::{Transport, TransportError};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub id: AgentId,
    /// Fraction of a lease's lifetime at which the agent asks
    /// for a refresh. Default 1/3 gives a 3× safety margin
    /// against a missed refresh round.
    pub refresh_at_fraction: f32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            id: AgentId::new("agent-unnamed"),
            refresh_at_fraction: 1.0 / 3.0,
        }
    }
}

/// Handle the agent binary uses to observe its own authority
/// state. Engines subscribe to this to learn when to quote and
/// when to walk the fail-ladder.
#[derive(Clone)]
pub struct AuthorityHandle {
    rx: watch::Receiver<LeaseState>,
}

impl AuthorityHandle {
    pub fn current(&self) -> LeaseState {
        self.rx.borrow().clone()
    }

    /// Wait until authority state transitions. Returns the new
    /// state; returns an error only if the underlying publisher
    /// has been dropped (binary is shutting down).
    pub async fn changed(&mut self) -> Result<LeaseState, watch::error::RecvError> {
        self.rx.changed().await?;
        Ok(self.rx.borrow().clone())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("controller rejected registration")]
    RegistrationRejected,
    #[error("unexpected envelope from controller")]
    BadEnvelope,
    #[error("authority lost: {0}")]
    AuthorityLost(String),
}

/// Drives a single control-plane session from the agent side.
/// Owns the transport, the command cursor, and the authority
/// state publisher. Run via [`LeaseClient::run`] — returns when
/// the controller closes cleanly or authority is lost.
pub struct LeaseClient<T: Transport> {
    transport: T,
    config: AgentConfig,
    cursor: Cursor,
    state_tx: watch::Sender<LeaseState>,
    /// Shared view of the current lease — the refresh loop reads
    /// it to decide when to request an extension, the recv loop
    /// writes it when a LeaseGrant/LeaseRefresh lands.
    current_lease: Arc<Mutex<Option<LeaderLease>>>,
    /// Strategy registry the client drives via reconcile when a
    /// `SetDesiredStrategies` command lands. `None` disables the
    /// reconcile path — tests that only exercise the lease loop
    /// don't need a registry attached, and the binary wires one
    /// in via [`LeaseClient::with_registry`].
    registry: Option<StrategyRegistry>,
    /// Shared credential catalog — populated from controller-pushed
    /// PushCredential commands. Both the LeaseClient and the
    /// RealEngineFactory hold an Arc into the same map so a
    /// credential pushed during a session is visible to the
    /// reconcile loop's engine factory on the next deploy.
    catalog: Option<Arc<CredentialCatalog>>,
    /// Ed25519 identity key. Persisted on disk between restarts
    /// so the fingerprint is stable — the controller keys
    /// admission-control decisions by fingerprint, rotating the
    /// key means re-running the approval flow. `None` for
    /// transport-only tests; production agents always attach one.
    identity: Option<IdentityKey>,
    /// Agent-local in-memory `DashboardState`. Engines spawned
    /// by the attached `RealEngineFactory` publish their
    /// operator-facing state (atomic bundles, funding-arb pair
    /// state, SOR decisions, rebalance advisories, ...) into
    /// this instance; the `FetchDeploymentDetails` command
    /// handler reads from the same instance to serve per-topic
    /// replies without having to reach into engine internals.
    /// `None` on transport-only / test clients.
    dashboard: Option<mm_dashboard::state::DashboardState>,
}

impl<T: Transport> LeaseClient<T> {
    pub fn new(transport: T, config: AgentConfig) -> (Self, AuthorityHandle) {
        let (state_tx, state_rx) = watch::channel(LeaseState::Unclaimed);
        let client = Self {
            transport,
            config,
            cursor: Cursor::fresh(),
            state_tx,
            current_lease: Arc::new(Mutex::new(None)),
            registry: None,
            catalog: None,
            identity: None,
            dashboard: None,
        };
        (client, AuthorityHandle { rx: state_rx })
    }

    /// Attach the agent's Ed25519 identity key. Controller uses
    /// the fingerprint of the advertised public key to drive its
    /// approval store — so this is effectively "tell the controller
    /// who you are, stably across restarts".
    pub fn with_identity(mut self, key: IdentityKey) -> Self {
        self.identity = Some(key);
        self
    }

    /// Attach the shared credential catalog. Controller-pushed
    /// credentials land here on `PushCredential` commands; the
    /// `RealEngineFactory` reads from the same Arc when
    /// reconciling deployments.
    pub fn with_catalog(mut self, catalog: Arc<CredentialCatalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    /// Builder-style — attach a registry so `SetDesiredStrategies`
    /// commands trigger the reconcile diff. The registry is owned
    /// by the client for the duration of the session; on
    /// authority loss [`LeaseClient::run`] aborts every running
    /// strategy before returning.
    pub fn with_registry(mut self, registry: StrategyRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Attach the agent's shared in-memory `DashboardState`.
    /// Must be the same instance the attached `RealEngineFactory`
    /// carries via `with_dashboard`, so engines write and the
    /// details handler reads through a single map.
    pub fn with_dashboard(
        mut self,
        dashboard: mm_dashboard::state::DashboardState,
    ) -> Self {
        self.dashboard = Some(dashboard);
        self
    }

    /// Drive the control channel until controller closes or authority
    /// is lost. Returns `Ok(())` on clean shutdown, `Err(_)` on
    /// transport failure or authority revocation.
    ///
    /// On any error exit — transport close, lease expiry, controller
    /// revocation — the registry's live strategies are aborted
    /// before the function returns. That is the agent-side hook
    /// the fail-ladder will plug into in PR-2c (abort becomes
    /// "walk the ladder"); for PR-2a the mock strategies just go
    /// away.
    pub async fn run(mut self) -> Result<(), AgentError> {
        self.send_register().await?;
        let result = self.pump_until_disconnect().await;
        if result.is_err() {
            if let Some(reg) = self.registry.as_mut() {
                reg.abort_all().await;
            }
        }
        result
    }

    async fn send_register(&mut self) -> Result<(), AgentError> {
        let payload = TelemetryPayload::Register {
            agent_id: self.config.id.clone(),
            last_applied: self.cursor.last_applied,
            protocol_version: mm_control::PROTOCOL_VERSION,
            pubkey: self.identity.as_ref().map(|k| k.public()),
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        self.send_telemetry(payload).await
    }

    async fn send_telemetry(&mut self, payload: TelemetryPayload) -> Result<(), AgentError> {
        // Telemetry uses its own independent seq; PR-1 keeps it
        // trivial (monotonic counter on self.cursor). Real impl
        // will split command/telemetry cursors into two fields.
        let seq = self.cursor.last_applied.next();
        self.cursor.advance(seq);
        let env = Envelope::telemetry(seq, payload);
        self.transport.send(SignedEnvelope::unsigned(env)).await?;
        Ok(())
    }

    async fn pump_until_disconnect(&mut self) -> Result<(), AgentError> {
        // Last-sent RefreshRequest tracked by the lease's
        // `issued_seq` — suppresses the "send again immediately"
        // re-fire that otherwise happens while the response
        // hasn't arrived yet. `issued_seq` is strictly monotonic
        // on every grant/refresh (`lease_id` is not — it's
        // deliberately reused across refreshes so agents track
        // a stable identity).
        let mut refresh_requested_for: Option<mm_control::seq::Seq> = None;
        // Periodic deployment snapshot cadence. Without this the
        // agent only emits `DeploymentState` on SetDesiredStrategies
        // + PatchDeploymentVariables — i.e. frozen from the moment
        // of deploy. Operator UI stayed visually stale forever:
        // mid_price / spread_bps / VPIN / all the book-derived
        // fields filled by the engine's per-tick publish never
        // made it onto the wire. 2s cadence matches the typical
        // engine refresh (~500ms) × 4 so we don't flood the
        // controller but still see sub-live UI latency.
        const SNAPSHOT_EVERY: Duration = Duration::from_secs(2);
        let mut next_snapshot = tokio::time::Instant::now() + SNAPSHOT_EVERY;
        loop {
            // Check lease expiry at the top of every loop turn.
            // The pump must detect expiry even when the controller has
            // gone silent — the whole point of the dead-man
            // layer is that silence doesn't mean safety. Clone
            // the lease out of the guard in a scoped block so
            // the MutexGuard drops before we touch `self` again.
            let held_lease = self.current_lease.lock().await.clone();
            if let Some(lease) = held_lease {
                if !lease.is_valid_at(chrono::Utc::now()) {
                    self.expire_lease().await;
                    return Err(AgentError::AuthorityLost("lease expired".into()));
                }
            }

            // Compute the earliest wake-up: whichever of
            // refresh-deadline or lease-expiry comes first. Both
            // cause the loop to re-enter the expiry check above.
            let (refresh_dl, expires_at) = {
                let guard = self.current_lease.lock().await;
                match guard.as_ref() {
                    Some(lease) => {
                        let total_ms =
                            (lease.expires_at - lease.issued_at).num_milliseconds().max(1);
                        let refresh_offset_ms =
                            (total_ms as f32 * self.config.refresh_at_fraction) as i64;
                        (
                            Some(lease.issued_at + chrono::Duration::milliseconds(refresh_offset_ms)),
                            Some(lease.expires_at),
                        )
                    }
                    None => (None, None),
                }
            };

            let next_wake = match (refresh_dl, expires_at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };

            // `tokio::time::timeout` on `recv()` rather than a
            // select!-over-self pair so the recv future fully
            // drops before we touch `self` again — avoids the
            // overlapping-borrow pitfall in tokio::select!.
            let current_issued_seq = self
                .current_lease
                .lock()
                .await
                .as_ref()
                .map(|l| l.issued_seq);

            // Wake up at whichever is earliest of: lease-refresh
            // deadline, lease-expiry, or the next periodic
            // snapshot. Saturating to zero when a deadline is in
            // the past makes `timeout(0, ...)` fire the Elapsed
            // branch immediately so we run the catch-up actions.
            let now = tokio::time::Instant::now();
            let snapshot_remaining = next_snapshot
                .checked_duration_since(now)
                .unwrap_or(Duration::from_millis(0));
            let wait_std = match next_wake {
                Some(dl) => dl - chrono::Utc::now(),
                None => chrono::Duration::seconds(3600),
            }
            .to_std()
            .unwrap_or(Duration::from_millis(0))
            .min(snapshot_remaining);

            let signed = match tokio::time::timeout(wait_std, self.transport.recv()).await {
                Ok(Ok(Some(env))) => env,
                Ok(Ok(None)) => return self.on_transport_closed().await,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => {
                    // Timeout fired — figure out which deadline
                    // actually triggered it and service it.
                    if tokio::time::Instant::now() >= next_snapshot {
                        if let Some(reg) = self.registry.as_ref() {
                            let rows = reg.snapshot_rows();
                            let _ = self
                                .send_telemetry(TelemetryPayload::DeploymentState {
                                    deployments: rows,
                                })
                                .await;
                        }
                        next_snapshot = tokio::time::Instant::now() + SNAPSHOT_EVERY;
                    }
                    if let (Some(rd), Some(seq)) = (refresh_dl, current_issued_seq) {
                        if chrono::Utc::now() >= rd
                            && refresh_requested_for != Some(seq)
                        {
                            self.request_refresh().await?;
                            refresh_requested_for = Some(seq);
                        }
                    }
                    continue;
                }
            };
            self.on_command(signed).await?;
            // Clear the in-flight marker once we've processed
            // the controller's reply and the lease has a newer
            // `issued_seq` — that's our signal that the refresh
            // landed (lease_id stays the same across refreshes
            // deliberately, only issued_seq advances).
            let new_issued_seq = self
                .current_lease
                .lock()
                .await
                .as_ref()
                .map(|l| l.issued_seq);
            if new_issued_seq != current_issued_seq {
                refresh_requested_for = None;
            }
        }
    }

    /// Called when `recv()` returns `Ok(None)` — the controller has
    /// closed the channel cleanly. If we currently hold a lease
    /// this is a fatal authority event: the controller cannot refresh
    /// through a closed channel, so we are about to lose
    /// authority regardless. Return immediately so the binary can
    /// walk the fail-ladder instead of waiting out the lease TTL.
    async fn on_transport_closed(&mut self) -> Result<(), AgentError> {
        let held_lease = self.current_lease.lock().await.is_some();
        if held_lease {
            self.expire_lease().await;
            Err(AgentError::AuthorityLost(
                "controller transport closed while holding lease".into(),
            ))
        } else {
            Ok(())
        }
    }

    async fn request_refresh(&mut self) -> Result<(), AgentError> {
        let (lease_id, expires_at) = {
            let guard = self.current_lease.lock().await;
            guard
                .as_ref()
                .map(|l| (l.lease_id, l.expires_at))
                .unzip()
        };
        if let Some(lease_id) = lease_id {
            // Debug-visible so operators can correlate "no
            // refresh" symptoms with "request never sent" vs
            // "controller never replied". expires_at is logged so
            // the gap between request and expiry is obvious.
            let now = chrono::Utc::now();
            let ttl_remaining_ms = expires_at
                .map(|e| (e - now).num_milliseconds())
                .unwrap_or(0);
            tracing::debug!(
                %lease_id,
                ttl_remaining_ms,
                "requesting lease refresh from controller"
            );
            self.send_telemetry(TelemetryPayload::LeaseRefreshRequest {
                current_lease_id: lease_id,
            })
            .await
        } else {
            Ok(())
        }
    }

    async fn on_command(&mut self, signed: SignedEnvelope) -> Result<(), AgentError> {
        let Envelope {
            command,
            telemetry,
            seq,
            ..
        } = signed.envelope;
        if telemetry.is_some() {
            return Err(AgentError::BadEnvelope);
        }
        let Some(cmd) = command else {
            return Err(AgentError::BadEnvelope);
        };
        match cmd {
            CommandPayload::LeaseGrant { lease } | CommandPayload::LeaseRefresh { lease } => {
                self.install_lease(lease).await;
            }
            CommandPayload::LeaseRevoke { reason } => {
                self.revoke_lease(reason).await;
                return Err(AgentError::AuthorityLost("lease revoked by controller".into()));
            }
            CommandPayload::PushCredential { credential } => {
                if let Some(cat) = self.catalog.as_ref() {
                    match cat.insert(credential.clone()) {
                        Ok(()) => tracing::info!(
                            credential = %credential.id,
                            "credential installed from controller push"
                        ),
                        Err(e) => tracing::warn!(
                            credential = %credential.id,
                            error = %e,
                            "PushCredential failed — unknown exchange/product or malformed"
                        ),
                    }
                } else {
                    tracing::debug!(
                        credential = %credential.id,
                        "PushCredential received but no catalog attached — dropping"
                    );
                }
            }
            CommandPayload::SetDesiredStrategies { strategies } => {
                if let Some(reg) = self.registry.as_mut() {
                    reg.reconcile(&strategies).await;
                    // Push a fresh deployment snapshot so the
                    // controller's fleet view reflects the change
                    // immediately. Best-effort — if the transport
                    // fails here the next reconcile or heartbeat
                    // retries.
                    let rows = reg.snapshot_rows();
                    let _ = self
                        .send_telemetry(TelemetryPayload::DeploymentState {
                            deployments: rows,
                        })
                        .await;
                } else {
                    tracing::debug!(
                        count = strategies.len(),
                        "SetDesiredStrategies received but no registry attached — dropping"
                    );
                }
            }
            CommandPayload::PatchDeploymentVariables {
                deployment_id,
                patch,
            } => {
                if let Some(reg) = self.registry.as_mut() {
                    let ok = reg.patch_variables(&deployment_id, &patch);
                    if ok {
                        tracing::info!(
                            deployment = %deployment_id,
                            fields = patch.len(),
                            "patched deployment variables — hot-reload hooks run per-strategy"
                        );
                        // Fresh telemetry frame so the drilldown
                        // UI sees the new `variables` immediately.
                        let rows = reg.snapshot_rows();
                        let _ = self
                            .send_telemetry(TelemetryPayload::DeploymentState {
                                deployments: rows,
                            })
                            .await;
                    } else {
                        tracing::warn!(
                            deployment = %deployment_id,
                            "patch targeted unknown deployment — dropping"
                        );
                    }
                } else {
                    tracing::debug!(
                        deployment = %deployment_id,
                        "PatchDeploymentVariables received but no registry attached — dropping"
                    );
                }
            }
            CommandPayload::FetchDeploymentDetails {
                deployment_id,
                topic,
                request_id,
                args,
            } => {
                // Map deployment_id → symbol via registry, then
                // service the topic from the process-global
                // details store. Unknown topic / stale
                // deployment_id both yield a reply with an
                // `error` field so the controller surfaces a
                // 400 instead of timing out.
                let (payload, error) = if let Some(reg) = self.registry.as_ref() {
                    match reg.deployment_symbol(&deployment_id) {
                        Some(symbol) => match topic.as_str() {
                            "funding_arb_recent_events" => {
                                let events = mm_dashboard::details_store::global()
                                    .funding_arb_events(&symbol);
                                (
                                    serde_json::json!({ "events": events }),
                                    None,
                                )
                            }
                            "audit_tail" => {
                                // Engine writes to data/audit/{symbol}.jsonl
                                // inside the agent's working directory.
                                // Args accepted:
                                //   from_ms / until_ms — inclusive range
                                //     on the entry's `timestamp` / `ts_ms`
                                //     field (parsed as i64 or RFC3339).
                                //   limit — cap entries returned (default
                                //     200, max 5000). Range queries can
                                //     bump this for MiCA monthly export
                                //     flows at the cost of a bigger reply.
                                //
                                // Missing file = empty array (fresh
                                // deployment, no events written yet).
                                let from_ms = args.get("from_ms").and_then(|v| v.as_i64());
                                let until_ms = args.get("until_ms").and_then(|v| v.as_i64());
                                let limit = args
                                    .get("limit")
                                    .and_then(|v| v.as_u64())
                                    .map(|n| n as usize)
                                    .unwrap_or(200)
                                    .min(5000);
                                let path = format!(
                                    "data/audit/{}.jsonl",
                                    symbol.to_lowercase()
                                );
                                let events = match std::fs::read_to_string(&path) {
                                    Ok(content) => content
                                        .lines()
                                        .rev()
                                        .filter_map(|line| {
                                            serde_json::from_str::<serde_json::Value>(
                                                line,
                                            )
                                            .ok()
                                        })
                                        .filter(|v| {
                                            let ts = audit_entry_ts(v);
                                            if let (Some(ts), Some(from)) = (ts, from_ms) {
                                                if ts < from { return false; }
                                            }
                                            if let (Some(ts), Some(until)) = (ts, until_ms) {
                                                if ts > until { return false; }
                                            }
                                            true
                                        })
                                        .take(limit)
                                        .collect::<Vec<_>>(),
                                    Err(_) => Vec::new(),
                                };
                                (
                                    serde_json::json!({ "events": events }),
                                    None,
                                )
                            }
                            "sor_decisions_recent" => {
                                // Prefer the shared DashboardState
                                // (full engine-written state) when
                                // available; fall back to the
                                // process-global details_store ring
                                // so pre-dashboard agents still
                                // surface whatever they mirrored.
                                let decisions = if let Some(dash) = self.dashboard.as_ref() {
                                    dash.sor_decisions_recent(100)
                                        .into_iter()
                                        .filter_map(|r| serde_json::to_value(&r).ok())
                                        .collect()
                                } else {
                                    mm_dashboard::details_store::global()
                                        .sor_decisions(&symbol)
                                };
                                (
                                    serde_json::json!({ "decisions": decisions }),
                                    None,
                                )
                            }
                            "atomic_bundles_inflight" => {
                                let bundles = self
                                    .dashboard
                                    .as_ref()
                                    .map(|dash| dash.atomic_bundles_inflight())
                                    .unwrap_or_default();
                                let values: Vec<serde_json::Value> = bundles
                                    .into_iter()
                                    .filter_map(|b| serde_json::to_value(&b).ok())
                                    .collect();
                                (
                                    serde_json::json!({ "bundles": values }),
                                    None,
                                )
                            }
                            "funding_arb_pairs" => {
                                let pairs = self
                                    .dashboard
                                    .as_ref()
                                    .map(|dash| dash.funding_arb_pairs())
                                    .unwrap_or_default();
                                let values: Vec<serde_json::Value> = pairs
                                    .into_iter()
                                    .filter_map(|p| serde_json::to_value(&p).ok())
                                    .collect();
                                (
                                    serde_json::json!({ "pairs": values }),
                                    None,
                                )
                            }
                            "rebalance_recommendations" => {
                                let recs = self
                                    .dashboard
                                    .as_ref()
                                    .map(|dash| dash.rebalance_recommendations())
                                    .unwrap_or_default();
                                let values: Vec<serde_json::Value> = recs
                                    .into_iter()
                                    .filter_map(|r| serde_json::to_value(&r).ok())
                                    .collect();
                                (
                                    serde_json::json!({ "recommendations": values }),
                                    None,
                                )
                            }
                            "onchain_scores" => {
                                let snaps = self
                                    .dashboard
                                    .as_ref()
                                    .map(|dash| dash.onchain_snapshots())
                                    .unwrap_or_default();
                                let values: Vec<serde_json::Value> = snaps
                                    .into_iter()
                                    .filter_map(|s| serde_json::to_value(&s).ok())
                                    .collect();
                                (
                                    serde_json::json!({ "snapshots": values }),
                                    None,
                                )
                            }
                            "adverse_selection" => {
                                let row = self
                                    .dashboard
                                    .as_ref()
                                    .and_then(|dash| dash.get_symbol(&symbol))
                                    .map(|s| serde_json::json!({
                                        "symbol": s.symbol,
                                        "adverse_bps": s.adverse_bps,
                                        "as_prob_bid": s.as_prob_bid,
                                        "as_prob_ask": s.as_prob_ask,
                                    }));
                                (
                                    serde_json::json!({ "row": row }),
                                    None,
                                )
                            }
                            "venue_inventory" => {
                                // Symbol-scoped — only this deployment's
                                // legs. The fleet-wide view is the
                                // frontend's fan-out join.
                                let legs = self
                                    .dashboard
                                    .as_ref()
                                    .map(|dash| dash.venue_balances(&symbol))
                                    .unwrap_or_default();
                                let values: Vec<serde_json::Value> = legs
                                    .into_iter()
                                    .filter_map(|v| serde_json::to_value(&v).ok())
                                    .collect();
                                (
                                    serde_json::json!({ "legs": values }),
                                    None,
                                )
                            }
                            "decisions_recent" => {
                                // DecisionLedger mirror — engine pushes
                                // `ledger.recent(200)` into details_store
                                // on every publish tick so the agent here
                                // reads a fresh list without locking the
                                // engine's own state.
                                let decisions = mm_dashboard::details_store::global()
                                    .decisions_snapshot(&symbol);
                                (
                                    serde_json::json!({ "decisions": decisions }),
                                    None,
                                )
                            }
                            "graph_trace_recent" => {
                                // M1-GOBS — strategy-graph live trace ring.
                                // Engine pushes a `TickTrace` on every
                                // `refresh_quotes`; the UI polls this
                                // topic every 2s while Live mode is open.
                                // Optional `limit` arg caps the returned
                                // slice (default 20).
                                let limit = args
                                    .get("limit")
                                    .and_then(|v| v.as_u64())
                                    .map(|n| n as usize)
                                    .unwrap_or(20);
                                let store = mm_dashboard::details_store::global();
                                let traces = store.graph_traces(&symbol, Some(limit));
                                let analysis = store.graph_analysis(&symbol);
                                (
                                    serde_json::json!({
                                        "traces": traces,
                                        "graph_analysis": analysis,
                                    }),
                                    None,
                                )
                            }
                            "graph_replay" => {
                                // M5-GOBS — replay a candidate graph
                                // against the last N captured TickTraces
                                // for this deployment's symbol. The
                                // candidate graph JSON arrives inside
                                // `args.candidate_graph`; `ticks`
                                // caps the replay window (default 20).
                                //
                                // Runs entirely on the agent so the
                                // trace ring (process-global on the
                                // agent) is a direct read — no fan-
                                // out. Divergences are sink-set diffs
                                // between the original capture and
                                // what the candidate would fire given
                                // the same source-node values.
                                let candidate_json = args
                                    .get("candidate_graph")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                let ticks = args
                                    .get("ticks")
                                    .and_then(|v| v.as_u64())
                                    .map(|n| n.min(256) as usize)
                                    .unwrap_or(20);
                                let candidate: Result<mm_strategy_graph::Graph, _> =
                                    serde_json::from_value(candidate_json);
                                let payload = match candidate {
                                    Err(e) => serde_json::json!({
                                        "summary": format!("candidate parse failed: {e}"),
                                        "ticks_replayed": 0,
                                        "divergence_count": 0,
                                        "divergences": [],
                                        "candidate_issues": [format!("{e}")],
                                    }),
                                    Ok(candidate) => {
                                        match mm_strategy_graph::Evaluator::build(&candidate) {
                                            Err(e) => serde_json::json!({
                                                "summary": format!("candidate graph rejected: {e}"),
                                                "ticks_replayed": 0,
                                                "divergence_count": 0,
                                                "divergences": [],
                                                "candidate_issues": [format!("{e}")],
                                            }),
                                            Ok(mut replay_ev) => {
                                                let store = mm_dashboard::details_store::global();
                                                // Oldest→newest so the
                                                // divergence list preserves
                                                // tick ordering.
                                                let original: Vec<_> = store
                                                    .graph_traces(&symbol, Some(ticks))
                                                    .into_iter()
                                                    .rev()
                                                    .collect();
                                                crate::graph_replay::compute_replay_payload(
                                                    &symbol,
                                                    &original,
                                                    &mut replay_ev,
                                                )
                                            }
                                        }
                                    }
                                };
                                (payload, None)
                            }
                            "graph_analysis" => {
                                // M1-GOBS — static topology analysis,
                                // populated on every `swap_strategy_graph`.
                                // Cheap read; UI asks once when entering
                                // Live mode and again on detected swap.
                                let analysis = mm_dashboard::details_store::global()
                                    .graph_analysis(&symbol);
                                match analysis {
                                    Some(a) => (
                                        serde_json::to_value(&a)
                                            .unwrap_or(serde_json::Value::Null),
                                        None,
                                    ),
                                    None => (
                                        serde_json::json!({}),
                                        Some(
                                            "no graph analysis yet — no graph swapped"
                                                .to_string(),
                                        ),
                                    ),
                                }
                            }
                            "audit_chain_verify" => {
                                // Fix #2 — real SHA-256 chain
                                // verify on the agent's local
                                // JSONL file. Reuses
                                // `mm_risk::audit::verify_chain`
                                // which already computes the
                                // exact write-path invariant:
                                // every row's prev_hash equals
                                // the SHA-256 of the previous
                                // line's raw bytes. Over-the-
                                // wire re-serialisation would
                                // change the bytes so the
                                // controller can't verify by
                                // itself — it has to fan out.
                                let path = format!(
                                    "data/audit/{}.jsonl",
                                    symbol.to_lowercase()
                                );
                                let p = std::path::Path::new(&path);
                                let payload = if !p.exists() {
                                    serde_json::json!({
                                        "exists": false,
                                        "rows_checked": 0,
                                        "last_hash": null,
                                    })
                                } else {
                                    match mm_risk::audit::verify_chain(p) {
                                        Ok(report) => serde_json::json!({
                                            "exists": true,
                                            "rows_checked": report.rows_checked,
                                            "last_hash": report.last_hash,
                                            "valid": true,
                                        }),
                                        Err(err) => {
                                            let (row, expected, got, kind) = match err {
                                                mm_risk::audit::ChainVerifyError::MalformedRow(r) => {
                                                    (r, None, None, "malformed_row")
                                                }
                                                mm_risk::audit::ChainVerifyError::ChainBroken {
                                                    row,
                                                    expected,
                                                    got,
                                                } => (row, expected, got, "chain_broken"),
                                            };
                                            serde_json::json!({
                                                "exists": true,
                                                "valid": false,
                                                "error_kind": kind,
                                                "row": row,
                                                "expected": expected,
                                                "got": got,
                                            })
                                        }
                                    }
                                };
                                (payload, None)
                            }
                            "alerts_recent" => {
                                // Wave D4 — agent-local alert
                                // ring. Controller fans out
                                // across the fleet and dedupes
                                // on (severity, title_hash).
                                let limit = args
                                    .get("limit")
                                    .and_then(|v| v.as_u64())
                                    .map(|n| n as usize)
                                    .unwrap_or(100);
                                let records = self
                                    .dashboard
                                    .as_ref()
                                    .map(|d| d.alerts_recent(limit))
                                    .unwrap_or_default();
                                let values: Vec<serde_json::Value> = records
                                    .into_iter()
                                    .filter_map(|r| serde_json::to_value(&r).ok())
                                    .collect();
                                (serde_json::json!({ "alerts": values }), None)
                            }
                            "flatten_preview" => {
                                // Wave C8 — pre-flatten preview so
                                // the operator sees how big the
                                // position is and roughly how deep
                                // it will have to sweep before
                                // dispatching the L4 kill. Best-
                                // effort estimate off the
                                // DashboardState book_depth_levels
                                // ring: we walk outward until the
                                // accumulated quote depth covers
                                // the position and return the
                                // widest pct_from_mid reached.
                                // Empty book → unknown estimate.
                                let row = self
                                    .dashboard
                                    .as_ref()
                                    .and_then(|d| d.get_symbol(&symbol));
                                let payload = match row {
                                    Some(s) => {
                                        let qty = s.inventory.abs();
                                        let side = if s.inventory.is_sign_negative() {
                                            "buy"
                                        } else if s.inventory.is_zero() {
                                            "flat"
                                        } else {
                                            "sell"
                                        };
                                        // Estimate slippage — walk the opposite side.
                                        let mut cum = rust_decimal::Decimal::ZERO;
                                        let mut worst_pct: Option<rust_decimal::Decimal> = None;
                                        let notional = qty * s.mid_price;
                                        for lvl in &s.book_depth_levels {
                                            let available = if side == "sell" {
                                                lvl.bid_depth_quote
                                            } else {
                                                lvl.ask_depth_quote
                                            };
                                            cum += available;
                                            worst_pct = Some(lvl.pct_from_mid);
                                            if cum >= notional {
                                                break;
                                            }
                                        }
                                        let covered = cum >= notional;
                                        serde_json::json!({
                                            "symbol": s.symbol,
                                            "side": side,
                                            "quantity": qty.to_string(),
                                            "mid_price": s.mid_price.to_string(),
                                            "inventory_value_quote": notional.to_string(),
                                            "estimated_slippage_pct": worst_pct.map(|p| p.to_string()),
                                            "book_depth_covers_position": covered,
                                            "book_levels": s
                                                .book_depth_levels
                                                .iter()
                                                .map(|l| serde_json::json!({
                                                    "pct_from_mid": l.pct_from_mid.to_string(),
                                                    "bid_depth_quote": l.bid_depth_quote.to_string(),
                                                    "ask_depth_quote": l.ask_depth_quote.to_string(),
                                                }))
                                                .collect::<Vec<_>>(),
                                        })
                                    }
                                    None => serde_json::json!(null),
                                };
                                (payload, None)
                            }
                            "reconciliation_snapshot" => {
                                // Wave C1 — latest order/balance
                                // reconciliation outcome for this
                                // deployment's symbol. Controller
                                // fans out across the fleet for
                                // the ReconciliationPage.
                                let snap = self
                                    .dashboard
                                    .as_ref()
                                    .and_then(|d| d.get_reconciliation(&symbol));
                                let payload = match snap {
                                    Some(s) => serde_json::to_value(&s)
                                        .unwrap_or(serde_json::Value::Null),
                                    None => serde_json::Value::Null,
                                };
                                (payload, None)
                            }
                            "client_metrics" => {
                                // Wave B3 — compact metrics slice the
                                // controller needs to answer /pnl,
                                // /positions, /sla, /client/{id}/*
                                // without parsing the whole SymbolState
                                // blob. All decimals as strings so
                                // rust_decimal round-trips precisely.
                                let row = self
                                    .dashboard
                                    .as_ref()
                                    .and_then(|d| d.get_symbol(&symbol));
                                let payload = match row {
                                    Some(s) => {
                                        let bid_depth: rust_decimal::Decimal = s
                                            .book_depth_levels
                                            .iter()
                                            .map(|l| l.bid_depth_quote)
                                            .sum();
                                        let ask_depth: rust_decimal::Decimal = s
                                            .book_depth_levels
                                            .iter()
                                            .map(|l| l.ask_depth_quote)
                                            .sum();
                                        // 2026-04-21 journey smoke — tenants
                                        // saw PnL totals but the "Recent
                                        // fills" card was always empty
                                        // because fill rows never crossed
                                        // the agent→controller boundary. The
                                        // FleetClientMetricsFetcher only
                                        // aggregated scalars; now we also
                                        // emit the last 50 per-symbol fill
                                        // rows so ClientPortalPage renders
                                        // real activity. Controller filters
                                        // by client_id again on the other
                                        // side in case the agent serves
                                        // multiple tenants.
                                        let recent = self
                                            .dashboard
                                            .as_ref()
                                            .map(|d| {
                                                d.get_recent_fills(Some(&s.symbol), 50)
                                            })
                                            .unwrap_or_default();
                                        serde_json::json!({
                                            "symbol": s.symbol,
                                            "venue": s.venue,
                                            "product": s.product,
                                            "mode": s.mode,
                                            "strategy": s.strategy,
                                            "inventory": s.inventory.to_string(),
                                            "inventory_value": s.inventory_value.to_string(),
                                            "mid_price": s.mid_price.to_string(),
                                            "spread_bps": s.spread_bps.to_string(),
                                            "total_fills": s.total_fills,
                                            "pnl_total": s.pnl.total.to_string(),
                                            "pnl_spread": s.pnl.spread.to_string(),
                                            "pnl_inventory": s.pnl.inventory.to_string(),
                                            "pnl_rebates": s.pnl.rebates.to_string(),
                                            "pnl_fees": s.pnl.fees.to_string(),
                                            "pnl_volume": s.pnl.volume.to_string(),
                                            "pnl_round_trips": s.pnl.round_trips,
                                            "pnl_fill_count": s.pnl.fill_count,
                                            "sla_uptime_pct": s.sla_uptime_pct.to_string(),
                                            "sla_max_spread_bps": s.sla_max_spread_bps.to_string(),
                                            "sla_min_depth_quote": s.sla_min_depth_quote.to_string(),
                                            "spread_compliance_pct": s.spread_compliance_pct.to_string(),
                                            "presence_pct_24h": s.presence_pct_24h.to_string(),
                                            "two_sided_pct_24h": s.two_sided_pct_24h.to_string(),
                                            "minutes_with_data_24h": s.minutes_with_data_24h,
                                            "bid_depth_quote": bid_depth.to_string(),
                                            "ask_depth_quote": ask_depth.to_string(),
                                            "kill_level": s.kill_level,
                                            "recent_fills": recent,
                                            // CalibrationStatus fleet fan-out
                                            // (2026-04-22) — ship the per-symbol
                                            // calibration snapshot inline so the
                                            // controller's `calibration_status`
                                            // endpoint can read it via the same
                                            // fleet fetcher used for PnL/SLA
                                            // aggregation. Before this, the
                                            // CalibrationStatus panel read
                                            // controller-local state that nothing
                                            // writes in distributed mode —
                                            // operators saw an empty table even
                                            // when GLFT had a real (a, k, N).
                                            "calibration": self
                                                .dashboard
                                                .as_ref()
                                                .map(|d| d.calibration_snapshots()
                                                    .into_iter()
                                                    .find(|c| c.symbol == s.symbol))
                                                .and_then(|opt| opt)
                                                .and_then(|cs| serde_json::to_value(cs).ok()),
                                        })
                                    }
                                    None => serde_json::json!(null),
                                };
                                (payload, None)
                            }
                            other => (
                                serde_json::Value::Null,
                                Some(format!("unknown topic '{other}'")),
                            ),
                        },
                        None => (
                            serde_json::Value::Null,
                            Some(format!(
                                "unknown deployment '{deployment_id}' — not in registry"
                            )),
                        ),
                    }
                } else {
                    (
                        serde_json::Value::Null,
                        Some("agent has no registry attached".to_string()),
                    )
                };
                let _ = self
                    .send_telemetry(TelemetryPayload::DetailsReply {
                        request_id,
                        deployment_id,
                        topic,
                        payload,
                        error,
                    })
                    .await;
            }
            CommandPayload::AddClient { client_id, symbols } => {
                // Wave B5 — hot-register a tenant on our local
                // DashboardState. `register_client` is idempotent
                // (duplicate id keeps existing state, refreshes
                // symbol set) so repeated pushes on reconnect
                // stay safe.
                if let Some(dashboard) = self.dashboard.as_ref() {
                    dashboard.register_client(&client_id, &symbols);
                    tracing::info!(
                        client_id = %client_id,
                        symbol_count = symbols.len(),
                        "registered client"
                    );
                } else {
                    tracing::warn!(
                        client_id = %client_id,
                        "AddClient received but agent has no dashboard attached — ignored"
                    );
                }
            }
            CommandPayload::Heartbeat => {
                self.send_telemetry(TelemetryPayload::Heartbeat {
                    agent_clock_ms: chrono::Utc::now().timestamp_millis(),
                })
                .await?;
            }
        }
        // ACK every command we successfully applied so the controller
        // can advance its per-agent cursor.
        self.send_telemetry(TelemetryPayload::Ack { applied_seq: seq })
            .await?;
        self.cursor.advance(seq);
        Ok(())
    }

    async fn install_lease(&mut self, lease: LeaderLease) {
        // Surface lease install for operators — quiet install
        // hid the reason behind silent sessions. Info-level so
        // the default `RUST_LOG=info` picks it up without
        // tuning.
        tracing::info!(
            lease_id = %lease.lease_id,
            issued_at = %lease.issued_at,
            expires_at = %lease.expires_at,
            issued_seq = ?lease.issued_seq,
            "lease held"
        );
        {
            let mut guard = self.current_lease.lock().await;
            *guard = Some(lease.clone());
        }
        let _ = self.state_tx.send(LeaseState::Held(lease));
    }

    async fn expire_lease(&mut self) {
        let expired = self.current_lease.lock().await.clone();
        if let Some(lease) = expired {
            let _ = self.state_tx.send(LeaseState::Expired(lease));
        }
    }

    async fn revoke_lease(&mut self, reason: String) {
        let previous = self.current_lease.lock().await.clone();
        if let Some(lease) = previous {
            let _ = self.state_tx.send(LeaseState::Revoked {
                previous: lease,
                reason,
            });
        }
    }
}

/// Extract a millisecond timestamp from an audit log entry.
/// The writer uses different shapes across event types — some
/// carry `timestamp` as RFC3339, some `ts_ms` as an integer.
/// Returns `None` when neither is present so range filtering
/// falls back to "keep the entry" (no false negatives).
fn audit_entry_ts(v: &serde_json::Value) -> Option<i64> {
    if let Some(n) = v.get("ts_ms").and_then(|x| x.as_i64()) {
        return Some(n);
    }
    if let Some(s) = v.get("timestamp").and_then(|x| x.as_str()) {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(dt.timestamp_millis());
        }
    }
    None
}
