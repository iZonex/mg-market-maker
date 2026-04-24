//! [`RealEngineFactory`] — the PR-2c-i [`EngineFactory`] that
//! resolves a [`DesiredStrategy`]'s credential bindings through
//! a [`CredentialCatalog`], builds a concrete
//! [`ExchangeConnector`], and spawns a minimal subscribe-only
//! task.
//!
//! **PR-2c-i intentional scope cut**: the spawned task is a
//! *subscriber*, not a full `MarketMaker`. It:
//!
//! 1. calls `connector.subscribe(&[symbol])` to start the book
//!    feed,
//! 2. pumps the receiver logging a rolling mid-price snapshot,
//! 3. exits cleanly when the registry aborts it.
//!
//! Full MarketMaker integration — strategy graph, risk manager,
//! kill switch, audit log, portfolio-risk wire-up — is the
//! subject of PR-2c-ii. Keeping the first cut subscribe-only
//! lets PR-2c-i prove the credential → connector → live feed
//! pipeline end-to-end without dragging in `mm-engine` / `mm-risk`
//! / `mm-strategy` deps.
//!
//! Fallback: if the desired credential cannot be resolved
//! (missing env, unknown id) the factory logs at warn and
//! returns a no-op task so reconcile does not panic. Real
//! bindings in PR-2c-ii will surface errors up to a
//! "deployment rejected" telemetry event.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mm_exchange_core::connector::ExchangeConnector;
use mm_exchange_core::events::MarketEvent;

use mm_control::fail_ladder::{FailLadder, StrategyClass};
use mm_control::lease::LeaseState;
use mm_control::messages::DesiredStrategy;

/// Extract a credential-id string from the strategy's `variables`
/// map. Returns `None` when the key is missing, null, or not a
/// string. Kept out-of-line so every caller has identical
/// extraction semantics — no surprise `""` vs `null` bugs.
fn variable_credential<'a>(desired: &'a DesiredStrategy, key: &str) -> Option<&'a str> {
    desired
        .variables
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

use crate::app_config::build_agent_config;
use crate::catalog::CredentialCatalog;
use crate::connector_factory::build_connector;
use crate::fail_ladder_walker::FailLadderWalker;
use crate::market_maker_runner::MarketMakerRunner;
use crate::registry::{EngineFactory, SpawnedEngine};
use crate::AuthorityHandle;

/// PR-2c-i engine factory: builds a real exchange connector, then
/// runs a subscribe-only pump. Held by the agent binary and
/// handed to the `StrategyRegistry` via `with_registry`.
pub struct RealEngineFactory {
    catalog: Arc<CredentialCatalog>,
    /// How often the subscribe-only task logs its latest mid
    /// snapshot. Keeps noise bounded without having to wire
    /// metrics yet.
    log_interval: Duration,
    /// Shared authority view. Spawned runners clone a receiver
    /// per task and start walking the fail-ladder as soon as
    /// authority transitions to [`LeaseState::Expired`] or
    /// [`LeaseState::Revoked`]. Absent on constructions that
    /// pre-date the controller handshake (e.g. tests that exercise
    /// only the connector path) — in that case spawned runners
    /// behave as in PR-2c-i, indefinitely.
    authority: Option<AuthorityHandle>,
    /// When `true`, spawned deployments run a full
    /// [`MarketMakerRunner`] (real orders, real risk, real audit).
    /// When `false` (default), they run the subscribe-only pump
    /// so dev + smoke environments stay observation-only until
    /// the operator explicitly opts in.
    trading_enabled: bool,
    /// Shared in-memory `DashboardState`. Engines spawned by
    /// this factory populate it with their operator-facing
    /// state (atomic bundles, funding-arb pairs, SOR
    /// decisions, rebalance advisories, ...) and the agent's
    /// FetchDetails handler reads from the same instance to
    /// serve per-deployment details topics. Distinct from
    /// the controller's fleet-wide view (which remains
    /// aggregated in `FleetState`). `None` on test factories
    /// that don't need the distributed panel surface.
    dashboard: Option<mm_dashboard::state::DashboardState>,
}

impl RealEngineFactory {
    pub fn new(catalog: Arc<CredentialCatalog>) -> Self {
        Self {
            catalog,
            log_interval: Duration::from_secs(5),
            authority: None,
            trading_enabled: false,
            dashboard: None,
        }
    }

    /// Attach the process-global in-memory dashboard state.
    /// Engines spawned by this factory will publish per-symbol
    /// operator-facing state into it; the agent's details
    /// endpoint reads back from the same instance.
    pub fn with_dashboard(mut self, dashboard: mm_dashboard::state::DashboardState) -> Self {
        self.dashboard = Some(dashboard);
        self
    }

    pub fn with_log_interval(mut self, dur: Duration) -> Self {
        self.log_interval = dur;
        self
    }

    /// Attach the authority handle the factory should clone into
    /// each runner it spawns. Builder-style so callers can chain:
    /// `RealEngineFactory::new(c).with_authority(h).with_log_interval(...)`.
    pub fn with_authority(mut self, handle: AuthorityHandle) -> Self {
        self.authority = Some(handle);
        self
    }

    /// Enable full-trading mode. Every deployment the factory
    /// spawns builds a real [`MarketMakerEngine`] instead of the
    /// subscribe-only pump. Off by default so dev / smoke
    /// environments stay observation-only until the operator
    /// deliberately flips this.
    pub fn with_trading_enabled(mut self, enabled: bool) -> Self {
        self.trading_enabled = enabled;
        self
    }
}

#[async_trait]
impl EngineFactory for RealEngineFactory {
    async fn spawn(&self, desired: &DesiredStrategy) -> SpawnedEngine {
        let label = format!(
            "{}/{}@{}",
            desired.template, desired.symbol, desired.deployment_id
        );

        // Resolve primary credential. The wire protocol carries a
        // flat `credentials` allow-list (what this deployment is
        // permitted to touch); the role assignment lives in
        // `variables` — each template reads the names it cares
        // about. Convention: `primary_credential` is the main
        // quoting venue. Templates without hedge/extras simply
        // never read those keys.
        let cred_id = variable_credential(desired, "primary_credential")
            .or_else(|| desired.credentials.first().map(|s| s.as_str()))
            .unwrap_or("");
        if cred_id.is_empty() {
            tracing::warn!(
                deployment = %desired.deployment_id,
                "no primary credential — variables.primary_credential unset and credentials list empty; deployment runs as no-op"
            );
            return SpawnedEngine::without_hot_reload(tokio::spawn(async {}), label);
        }
        // Wave 2b — allow-list enforcement at the catalog boundary.
        // Refuses to hand back a credential that isn't in
        // `desired.credentials`, even if the agent happens to hold
        // it for another deployment. Prevents a bug in the
        // variables map from resolving tenant-A's key into
        // tenant-B's runtime on a shared agent.
        let resolved = match self.catalog.resolve_for(cred_id, &desired.credentials) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    deployment = %desired.deployment_id,
                    credential = %cred_id,
                    allowlist = ?desired.credentials,
                    error = %e,
                    "could not resolve primary credential (either unknown or outside deployment allow-list) — deployment runs as no-op"
                );
                return SpawnedEngine::without_hot_reload(tokio::spawn(async {}), label);
            }
        };

        // Build the connector.
        let connector = match build_connector(&resolved) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    deployment = %desired.deployment_id,
                    credential = %cred_id,
                    error = %e,
                    "could not build connector — deployment runs as no-op"
                );
                return SpawnedEngine::without_hot_reload(tokio::spawn(async {}), label);
            }
        };

        let symbol = desired.symbol.clone();
        let deployment_id = desired.deployment_id.clone();
        let log_interval = self.log_interval;
        // Fail-ladder selection: explicit override on the
        // DesiredStrategy wins; otherwise fall back to the
        // template-class default. PR-2c-ii always assumes the
        // `Maker` class because we haven't wired class detection
        // into templates yet — tighten in PR-2c-iii when the
        // template metadata is declared.
        let ladder = desired
            .fail_ladder_override
            .clone()
            .unwrap_or_else(|| FailLadder::default_for(StrategyClass::Maker));
        let authority = self.authority.clone();

        // PR-2c-iii-b fork: trading_enabled flips this deployment
        // from the subscribe-only pump to a real MarketMakerEngine.
        // Config comes from the shared adapter so variables,
        // rails, and feature flags all apply identically to
        // dashboard previews.
        if self.trading_enabled {
            // Resolve optional hedge binding. Missing credential
            // is a hard error — the operator set a hedge but the
            // catalog can't resolve it — because silently falling
            // through to primary-only quoting breaks cross-
            // exchange strategies in a dangerous way (the "hedge"
            // leg never fires and inventory builds unchecked).
            // Hedge credential — optional, template driven. Only
            // templates that actually post a hedge leg read this
            // variable; single-venue makers leave it unset and
            // the engine quotes primary-only.
            let hedge_id_opt = variable_credential(desired, "hedge_credential");
            let hedge_resolved = match hedge_id_opt {
                None => None,
                Some(hedge_id) => match self.catalog.resolve_for(hedge_id, &desired.credentials) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::warn!(
                            deployment = %desired.deployment_id,
                            credential = %hedge_id,
                            allowlist = ?desired.credentials,
                            error = %e,
                            "could not resolve hedge credential (either unknown or outside deployment allow-list) — deployment runs as no-op"
                        );
                        return SpawnedEngine::without_hot_reload(tokio::spawn(async {}), label);
                    }
                },
            };
            // SOR extras — one or more read-only credentials the
            // router may route through. Unresolved extras are a
            // hard no-op for the same reason hedge is: routing
            // with a stale/missing key is a silent degradation we
            // don't want to paper over.
            // Extras — array of credential ids for SOR targets
            // and signal-only feeds. Sourced from
            // `variables.extras_credentials` (JSON array of
            // strings). Empty / absent = no extras.
            let extras_ids: Vec<String> = desired
                .variables
                .get("extras_credentials")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let mut extras_resolved: Vec<mm_common::settings::ResolvedCredential> = Vec::new();
            for extra_id in &extras_ids {
                match self.catalog.resolve_for(extra_id, &desired.credentials) {
                    Ok(r) => extras_resolved.push(r),
                    Err(e) => {
                        tracing::warn!(
                            deployment = %desired.deployment_id,
                            credential = %extra_id,
                            allowlist = ?desired.credentials,
                            error = %e,
                            "could not resolve SOR-extra credential (either unknown or outside deployment allow-list) — deployment runs as no-op"
                        );
                        return SpawnedEngine::without_hot_reload(tokio::spawn(async {}), label);
                    }
                }
            }
            let config = match build_agent_config(
                desired,
                &resolved,
                hedge_resolved.as_ref(),
                &extras_resolved,
                self.catalog.settings(),
            ) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        deployment = %desired.deployment_id,
                        error = %e,
                        "failed to build AppConfig — deployment runs as no-op"
                    );
                    return SpawnedEngine::without_hot_reload(tokio::spawn(async {}), label);
                }
            };
            // Build the hedge connector lazily from the resolved
            // credential. Reusing the same factory as the primary
            // connector keeps behaviour identical across legs
            // (same testnet handling, same product dispatch).
            let hedge_connector = match hedge_resolved.as_ref() {
                None => None,
                Some(hr) => match build_connector(hr) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        tracing::warn!(
                            deployment = %desired.deployment_id,
                            credential = %hr.id,
                            error = %e,
                            "could not build hedge connector — deployment runs as no-op"
                        );
                        return SpawnedEngine::without_hot_reload(tokio::spawn(async {}), label);
                    }
                },
            };
            // Build SOR-extra connectors. Same factory, same
            // failure policy as hedge — a busted extra bails the
            // whole deployment rather than silently dropping the
            // router's routing pool.
            let mut extra_connectors: Vec<Arc<dyn ExchangeConnector>> = Vec::new();
            for er in &extras_resolved {
                match build_connector(er) {
                    Ok(c) => extra_connectors.push(c),
                    Err(e) => {
                        tracing::warn!(
                            deployment = %desired.deployment_id,
                            credential = %er.id,
                            error = %e,
                            "could not build SOR-extra connector — deployment runs as no-op"
                        );
                        return SpawnedEngine::without_hot_reload(tokio::spawn(async {}), label);
                    }
                }
            }
            // Hot-reload channel — the receiver goes into the
            // engine's select loop via `with_config_overrides`; the
            // sender lives on the returned `SpawnedEngine` so the
            // registry's `patch_variables` path can ship live
            // ConfigOverride variants at deployment-level granularity.
            let (override_tx, override_rx) =
                tokio::sync::mpsc::unbounded_channel::<mm_dashboard::state::ConfigOverride>();
            // Fix #3 — tenant tagging. The controller auto-
            // injects `variables.client_id` from the agent's
            // approval profile at deploy time; operators can
            // override per-deployment by setting it explicitly.
            // Absent = shared infra (untagged fills).
            let client_id = desired
                .variables
                .get("client_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            let runner = MarketMakerRunner {
                symbol: symbol.clone(),
                deployment_id: deployment_id.clone(),
                template: desired.template.clone(),
                config,
                connector: connector.clone(),
                hedge_connector,
                extra_connectors,
                authority: authority.clone(),
                config_override_rx: Some(override_rx),
                dashboard: self.dashboard.clone(),
                client_id,
            };
            let runner_label = label.clone();
            let handle = tokio::spawn(async move {
                if let Err(e) = runner.run().await {
                    tracing::warn!(
                        deployment = %deployment_id,
                        label = %runner_label,
                        error = %e,
                        "MarketMakerRunner exited with error"
                    );
                }
            });
            return SpawnedEngine {
                handle,
                label,
                config_override_tx: Some(override_tx),
            };
        }

        let runner_label = label.clone();
        let handle = tokio::spawn(async move {
            let mut runner = SubscribeOnlyRunner {
                connector,
                symbol: symbol.clone(),
                deployment_id: deployment_id.clone(),
                log_interval,
                authority,
                ladder,
            };
            if let Err(e) = runner.run().await {
                tracing::warn!(
                    deployment = %deployment_id,
                    label = %runner_label,
                    error = %e,
                    "runner exited with error"
                );
            }
        });

        SpawnedEngine::without_hot_reload(handle, label)
    }
}

/// Per-deployment task that owns a connector, a subscription, a
/// rolling mid-price log, AND — as of PR-2c-ii — watches the
/// shared [`AuthorityHandle`] so it can walk the fail-ladder
/// when the agent loses authority.
///
/// Phases:
/// 1. **Held** — normal subscribe-and-log. Orders would be
///    placed here in PR-2c-iii once an OrderManager is attached.
/// 2. **Walking** — authority transitioned to
///    [`LeaseState::Expired`] or [`LeaseState::Revoked`]; the
///    runner drives a [`FailLadderWalker`] against a real-time
///    clock, firing each rung's action exactly once. Market
///    data pump keeps running so rung handlers have current
///    mid prices to quote against (matters for the future
///    "Widen" rung).
/// 3. **Complete** — every rung has fired; runner exits.
pub struct SubscribeOnlyRunner {
    pub connector: Arc<dyn ExchangeConnector>,
    pub symbol: String,
    pub deployment_id: String,
    pub log_interval: Duration,
    /// Observer for the deployment's authority. `None` means
    /// "assume held forever" (test / PR-2c-i compatibility).
    pub authority: Option<AuthorityHandle>,
    /// Ladder this deployment walks after authority loss.
    pub ladder: FailLadder,
}

impl SubscribeOnlyRunner {
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let mut rx = self
            .connector
            .subscribe(std::slice::from_ref(&self.symbol))
            .await
            .map_err(|e| anyhow::anyhow!("subscribe failed: {e}"))?;

        let mut log_ticker = tokio::time::interval(self.log_interval);
        log_ticker.tick().await; // prime
        let mut latest_mid: Option<rust_decimal::Decimal> = None;

        // If we hold an authority handle, wait until state is
        // `Held(_)` before pumping. An unclaimed or already-
        // expired handle should trigger the fail-ladder
        // immediately, not start normal operations.
        let mut walker: Option<FailLadderWalker> = if let Some(ref h) = self.authority {
            classify_initial(&h.current(), &self.ladder)
        } else {
            None
        };

        tracing::info!(
            deployment = %self.deployment_id,
            symbol = %self.symbol,
            venue = ?self.connector.venue_id(),
            product = ?self.connector.product(),
            authority_attached = self.authority.is_some(),
            initial_phase = if walker.is_some() { "walking" } else { "held" },
            "runner started"
        );

        loop {
            // Done walking? No further action possible.
            if let Some(ref w) = walker {
                if w.is_complete() {
                    tracing::info!(
                        deployment = %self.deployment_id,
                        "fail-ladder complete — runner exiting"
                    );
                    return Ok(());
                }
            }

            // Compute sleep until next rung, capped by
            // log_interval. When not walking we just wait on
            // events and the log ticker.
            let next_rung_dl = walker
                .as_ref()
                .and_then(next_rung_sleep)
                .unwrap_or(self.log_interval);
            let rung_ticker = tokio::time::sleep(next_rung_dl);
            tokio::pin!(rung_ticker);

            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Some(ev) => self.on_event(ev, &mut latest_mid),
                        None => {
                            tracing::info!(
                                deployment = %self.deployment_id,
                                "market event stream closed"
                            );
                            return Ok(());
                        }
                    }
                }
                _ = log_ticker.tick() => {
                    if walker.is_none() {
                        if let Some(m) = latest_mid {
                            tracing::info!(
                                deployment = %self.deployment_id,
                                symbol = %self.symbol,
                                mid = %m,
                                "runner heartbeat"
                            );
                        }
                    }
                }
                _ = &mut rung_ticker, if walker.is_some() => {
                    if let Some(w) = walker.as_mut() {
                        while let Some(action) = w.poll_at(std::time::Instant::now()) {
                            tracing::warn!(
                                deployment = %self.deployment_id,
                                symbol = %self.symbol,
                                rung = action.rung_index,
                                level = ?action.level,
                                "fail-ladder action (PR-2c-ii stub — real connector call lands in PR-2c-iii)"
                            );
                        }
                    }
                }
                changed = wait_auth_change(self.authority.as_mut()),
                    if walker.is_none() && self.authority.is_some() =>
                {
                    if let Some(state) = changed {
                        if let Some(w) = classify_transition(&state, &self.ladder) {
                            tracing::warn!(
                                deployment = %self.deployment_id,
                                symbol = %self.symbol,
                                state = ?state,
                                "authority lost — walking fail-ladder"
                            );
                            walker = Some(w);
                        }
                    }
                }
            }
        }
    }

    fn on_event(&self, ev: MarketEvent, latest_mid: &mut Option<rust_decimal::Decimal>) {
        use mm_exchange_core::events::MarketEvent::*;
        match ev {
            BookSnapshot { bids, asks, .. } | BookDelta { bids, asks, .. } => {
                // Derive mid from first non-empty level on each
                // side; skip updates that cannot produce a valid
                // mid (one-sided book at startup).
                let best_bid = bids.first().map(|l| l.price);
                let best_ask = asks.first().map(|l| l.price);
                if let (Some(b), Some(a)) = (best_bid, best_ask) {
                    if b > rust_decimal::Decimal::ZERO && a > rust_decimal::Decimal::ZERO {
                        *latest_mid = Some((b + a) / rust_decimal::Decimal::from(2u32));
                    }
                }
            }
            Fill { fill, .. } => {
                tracing::info!(
                    deployment = %self.deployment_id,
                    trade_id = %fill.trade_id,
                    price = %fill.price,
                    qty = %fill.qty,
                    "fill observed (subscribe-only runner does not act on it)"
                );
            }
            _ => {}
        }
    }
}

/// Start the runner directly in "walking" mode if the authority
/// is already in a terminal state at construction time. Covers
/// the case where the controller revokes + restarts the agent before
/// the runner loop ever entered Held.
fn classify_initial(state: &LeaseState, ladder: &FailLadder) -> Option<FailLadderWalker> {
    classify_transition(state, ladder)
}

/// Transition classifier — returns `Some(walker)` iff the given
/// state has no trading authority. Watch-change events surface
/// every transition so we gate on the terminal variants.
fn classify_transition(state: &LeaseState, ladder: &FailLadder) -> Option<FailLadderWalker> {
    match state {
        LeaseState::Held(_) => None,
        LeaseState::Unclaimed => None,
        LeaseState::Expired(_) | LeaseState::Revoked { .. } => Some(FailLadderWalker::start(
            ladder.clone(),
            std::time::Instant::now(),
        )),
    }
}

async fn wait_auth_change(handle: Option<&mut AuthorityHandle>) -> Option<LeaseState> {
    match handle {
        Some(h) => h.changed().await.ok(),
        None => std::future::pending().await,
    }
}

/// Sleep duration until the walker's next rung would fire.
/// Capped so we don't pass a huge Duration into tokio::sleep.
fn next_rung_sleep(walker: &FailLadderWalker) -> Option<Duration> {
    let next = walker.next_rung_at()?;
    // The walker tracks absolute offsets from `entered_at`, so
    // the caller needs to subtract elapsed silence. PR-2c-ii
    // keeps it simple — poll at the next absolute offset and let
    // `poll_at` consume what's ready; over-polling is cheap.
    Some(next.min(Duration::from_secs(30)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::{ExchangeType, ProductType};
    use mm_common::settings::{CredentialSpec, SettingsFile};
    use mm_control::messages::DesiredStrategy;

    fn settings_with(credentials: Vec<CredentialSpec>) -> SettingsFile {
        let mut base = SettingsFile::from_str(
            r#"
            [agent]
            id = "test-agent"
            "#,
        )
        .unwrap();
        base.credentials = credentials;
        base
    }

    fn desired(primary: &str, symbol: &str) -> DesiredStrategy {
        let mut vars = serde_json::Map::new();
        vars.insert("primary_credential".into(), serde_json::json!(primary));
        DesiredStrategy {
            deployment_id: "dep-1".into(),
            template: "subscribe-only".into(),
            symbol: symbol.into(),
            credentials: vec![primary.to_string()],
            variables: vars,
            fail_ladder_override: None,
        }
    }

    #[tokio::test]
    async fn unknown_primary_binding_yields_noop_task() {
        let catalog = Arc::new(CredentialCatalog::from_settings(settings_with(vec![])));
        let factory = RealEngineFactory::new(catalog);
        let spawned = factory.spawn(&desired("missing-cred", "BTCUSDT")).await;
        // The task should be a pure no-op that completes
        // immediately.
        spawned.handle.abort();
        let _ = spawned.handle.await;
    }

    #[test]
    fn classify_transition_maps_terminals_to_walker() {
        let ladder = FailLadder::default_for(StrategyClass::Maker);
        // Non-terminal states keep the runner in "held" phase.
        assert!(classify_transition(&LeaseState::Unclaimed, &ladder).is_none());

        let now = chrono::Utc::now();
        let lease = mm_control::lease::LeaderLease {
            lease_id: uuid::Uuid::nil(),
            agent_id: "a".into(),
            issued_at: now,
            expires_at: now + chrono::Duration::seconds(30),
            issued_seq: mm_control::seq::Seq(1),
        };
        assert!(classify_transition(&LeaseState::Held(lease.clone()), &ladder).is_none());

        // Terminal states trigger the walker.
        assert!(classify_transition(&LeaseState::Expired(lease.clone()), &ladder).is_some());
        assert!(classify_transition(
            &LeaseState::Revoked {
                previous: lease,
                reason: "test".into()
            },
            &ladder
        )
        .is_some());
    }

    #[test]
    fn classify_initial_mirrors_transition_for_terminal_states() {
        let ladder = FailLadder::default_for(StrategyClass::Maker);
        let now = chrono::Utc::now();
        let lease = mm_control::lease::LeaderLease {
            lease_id: uuid::Uuid::nil(),
            agent_id: "a".into(),
            issued_at: now,
            expires_at: now + chrono::Duration::seconds(30),
            issued_seq: mm_control::seq::Seq(1),
        };
        assert!(classify_initial(&LeaseState::Unclaimed, &ladder).is_none());
        assert!(classify_initial(&LeaseState::Held(lease.clone()), &ladder).is_none());
        assert!(classify_initial(&LeaseState::Expired(lease), &ladder).is_some());
    }

    #[tokio::test]
    async fn primary_credential_outside_allowlist_yields_noop() {
        // Wave 2b regression — desired.credentials is the
        // authoritative allow-list. A variables.primary_credential
        // that names something NOT in credentials must not resolve,
        // even if the catalog happens to hold that credential for a
        // different deployment on the same agent.
        use mm_common::settings::CredentialSpec;
        let cred = CredentialSpec {
            id: "allowed-cred".into(),
            exchange: ExchangeType::Binance,
            product: ProductType::Spot,
            api_key_env: "MM_DEFINITELY_UNSET_KEY_Z9Z9Z9".into(),
            api_secret_env: "MM_DEFINITELY_UNSET_SECRET_Z9Z9Z9".into(),
            max_notional_quote: None,
            default_symbol: None,
            allowed_agents: Vec::new(),
        };
        let catalog = Arc::new(CredentialCatalog::from_settings(settings_with(vec![cred])));
        // Inject "other-cred" directly into the catalog as if it
        // had been pushed for a different deployment.
        catalog
            .insert(mm_control::messages::PushedCredential {
                id: "other-cred".into(),
                exchange: "binance".into(),
                product: "spot".into(),
                api_key: "leaked-key".into(),
                api_secret: "leaked-secret".into(),
                max_notional_quote: None,
                default_symbol: None,
            })
            .unwrap();
        let factory = RealEngineFactory::new(catalog);
        // Desired only allows "allowed-cred", but variables points
        // at "other-cred" — the allow-list gate must refuse.
        let mut vars = serde_json::Map::new();
        vars.insert("primary_credential".into(), serde_json::json!("other-cred"));
        let desired = DesiredStrategy {
            deployment_id: "dep-x".into(),
            template: "subscribe-only".into(),
            symbol: "BTCUSDT".into(),
            credentials: vec!["allowed-cred".into()],
            variables: vars,
            fail_ladder_override: None,
        };
        let spawned = factory.spawn(&desired).await;
        // No-op task — the allow-list refusal produces an empty
        // spawn per the existing failure-is-no-op contract.
        spawned.handle.abort();
        let _ = spawned.handle.await;
    }

    #[tokio::test]
    async fn env_missing_for_primary_also_yields_noop() {
        let cred = CredentialSpec {
            id: "c1".into(),
            exchange: ExchangeType::Binance,
            product: ProductType::Spot,
            api_key_env: "MM_DEFINITELY_UNSET_KEY_Z9Z9Z9".into(),
            api_secret_env: "MM_DEFINITELY_UNSET_SECRET_Z9Z9Z9".into(),
            max_notional_quote: None,
            default_symbol: None,
            allowed_agents: Vec::new(),
        };
        let catalog = Arc::new(CredentialCatalog::from_settings(settings_with(vec![cred])));
        let factory = RealEngineFactory::new(catalog);
        let spawned = factory.spawn(&desired("c1", "BTCUSDT")).await;
        spawned.handle.abort();
        let _ = spawned.handle.await;
    }
}
