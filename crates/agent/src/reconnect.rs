//! Reconnect loop for the agent's control-plane client.
//!
//! The subscribe-only and MarketMaker runners already handle
//! their own market-data reconnects (via the connector's retry
//! loops). This module handles the *control-plane* reconnect:
//! the controller ↔ agent WS-RPC session.
//!
//! **Policy (PR-2f-reconnect):**
//! - Full replay on reconnect — agent sends `Register` with
//!   `last_applied = Seq::ZERO` and the controller re-issues a fresh
//!   lease. Sticky-cursor resume (`last_applied` advertising the
//!   real seq) lands in PR-2f-resume alongside a controller-side
//!   replay buffer.
//! - Exponential backoff with jitter: 1s, 2s, 4s, 8s, 16s, cap
//!   30s. Jitter ±25 % so a fleet-wide outage doesn't reconnect
//!   in a thundering herd.
//! - Any `AgentError` other than `Transport` / clean close
//!   propagates up — authority-loss from a revoke is a
//!   deliberate controller action, not a reconnect trigger.
//!
//! **Not handled here:**
//! - Market-data reconnect — lives in each exchange connector.
//! - Registry preservation across reconnects — a new
//!   `StrategyRegistry` is built on every reconnect because the
//!   previous LeaseClient owned it. Engines running in-process
//!   keep going until `abort_all` fires (via authority loss on
//!   the old session's final frame) — so a fast reconnect keeps
//!   trading live through the blip.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::watch;

use mm_control::identity::IdentityKey;
use mm_control::messages::AgentId;
use mm_control::ws_transport::WsTransport;

use crate::{
    AgentConfig, AgentError, AuthorityHandle, CredentialCatalog, LeaseClient,
    RealEngineFactory, StrategyRegistry,
};

/// Build-registry callback — lets the reconnect loop rebuild a
/// fresh registry + factory per-session. Defined as a closure so
/// the binary controls factory selection (mock vs real + trading
/// flag) without the reconnect loop needing to know.
pub type RegistryBuilder = Arc<dyn Fn(AuthorityHandle) -> StrategyRegistry + Send + Sync>;

pub struct ReconnectConfig {
    pub controller_addr: String,
    pub agent_id: AgentId,
    pub build_registry: RegistryBuilder,
    /// Shared credential catalog — survives across reconnects
    /// so credentials the controller pushed in a prior session are
    /// still there when the agent comes back. Controller re-pushes
    /// on each register too, so stale entries refresh.
    pub catalog: Arc<CredentialCatalog>,
    /// Agent's Ed25519 identity key. Cloned into every new
    /// LeaseClient session so the controller sees a stable
    /// fingerprint across reconnects — without this, reconnects
    /// would drop the agent back to Pending every time.
    pub identity: Option<IdentityKey>,
    /// Initial backoff on the first failure. Doubles each retry.
    pub initial_backoff: Duration,
    /// Maximum backoff — we cap here to avoid multi-minute
    /// silent periods that trigger controller-side watchdog flaps.
    pub max_backoff: Duration,
    /// Agent-local shared `DashboardState`. Threaded into every
    /// new `LeaseClient` session so the `FetchDetails` handler
    /// sees the same state instance the engines (via the
    /// factory) populate.
    pub dashboard: Option<mm_dashboard::state::DashboardState>,
}

impl ReconnectConfig {
    pub fn new(
        controller_addr: String,
        agent_id: AgentId,
        build_registry: RegistryBuilder,
        catalog: Arc<CredentialCatalog>,
    ) -> Self {
        Self {
            controller_addr,
            agent_id,
            build_registry,
            catalog,
            identity: None,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            dashboard: None,
        }
    }

    pub fn with_identity(mut self, identity: IdentityKey) -> Self {
        self.identity = Some(identity);
        self
    }

    pub fn with_dashboard(
        mut self,
        dashboard: mm_dashboard::state::DashboardState,
    ) -> Self {
        self.dashboard = Some(dashboard);
        self
    }
}

/// Run the agent control-plane loop with transparent reconnect.
/// Returns when authority is lost (controller revoke, or fail-ladder
/// completed and the agent caller decides to give up) or when
/// the caller's shutdown signal fires.
pub async fn run_with_reconnect(cfg: ReconnectConfig, shutdown: watch::Receiver<bool>) -> Result<()> {
    let mut backoff = cfg.initial_backoff;
    let mut consecutive_failures = 0u32;

    loop {
        if *shutdown.borrow() {
            tracing::info!("reconnect loop exiting on shutdown signal");
            return Ok(());
        }

        match connect_and_run(&cfg, shutdown.clone()).await {
            SessionOutcome::Shutdown => {
                tracing::info!("reconnect loop exiting on shutdown signal (mid-session)");
                return Ok(());
            }
            SessionOutcome::CleanClose => {
                tracing::info!(
                    consecutive_failures,
                    "control-plane session ended cleanly"
                );
                // Clean end (controller closed) — reset backoff and
                // try again immediately. A clean close at this
                // level is almost always controller restart.
                backoff = cfg.initial_backoff;
                consecutive_failures = 0;
                // Still tiny sleep to avoid reconnect storms on
                // bad controller state (e.g. a controller that accepts but
                // immediately closes).
                sleep_or_shutdown(Duration::from_millis(250), shutdown.clone()).await;
            }
            SessionOutcome::AuthorityLost(reason) => {
                // Authority loss is deliberate (controller revoke /
                // lease expiry). Do NOT reconnect — return to
                // the caller which decides whether to retry or
                // exit. Usually the binary exits so a supervisor
                // investigates why authority went away.
                tracing::error!(reason = %reason, "control-plane authority lost — reconnect loop exiting");
                return Err(anyhow::anyhow!("authority lost: {reason}"));
            }
            SessionOutcome::TransportError(e) => {
                consecutive_failures += 1;
                let sleep = jittered(backoff);
                tracing::warn!(
                    error = %e,
                    consecutive_failures,
                    backoff_ms = sleep.as_millis() as u64,
                    "control-plane session failed — reconnecting after backoff"
                );
                sleep_or_shutdown(sleep, shutdown.clone()).await;
                backoff = (backoff * 2).min(cfg.max_backoff);
            }
        }
    }
}

/// Sleep for `dur` OR wake early when shutdown fires. Returns on
/// either. Caller re-checks shutdown at the top of the loop.
async fn sleep_or_shutdown(dur: Duration, mut shutdown: watch::Receiver<bool>) {
    tokio::select! {
        _ = tokio::time::sleep(dur) => {}
        _ = shutdown.changed() => {}
    }
}

enum SessionOutcome {
    Shutdown,
    CleanClose,
    AuthorityLost(String),
    TransportError(AgentError),
}

async fn connect_and_run(
    cfg: &ReconnectConfig,
    mut shutdown: watch::Receiver<bool>,
) -> SessionOutcome {
    let transport = match WsTransport::connect(&cfg.controller_addr).await {
        Ok(t) => t,
        Err(e) => return SessionOutcome::TransportError(e.into()),
    };
    tracing::info!(
        agent = %cfg.agent_id.as_str(),
        controller = %cfg.controller_addr,
        "control-plane session connected"
    );
    let (client, authority) = LeaseClient::new(
        transport,
        AgentConfig {
            id: cfg.agent_id.clone(),
            ..Default::default()
        },
    );
    let registry = (cfg.build_registry)(authority);
    let mut client = client
        .with_registry(registry)
        .with_catalog(Arc::clone(&cfg.catalog));
    if let Some(id) = cfg.identity.as_ref() {
        client = client.with_identity(id.clone());
    }
    if let Some(dash) = cfg.dashboard.as_ref() {
        client = client.with_dashboard(dash.clone());
    }
    tokio::select! {
        res = client.run() => match res {
            Ok(()) => SessionOutcome::CleanClose,
            Err(AgentError::AuthorityLost(r)) => SessionOutcome::AuthorityLost(r),
            Err(e) => SessionOutcome::TransportError(e),
        },
        _ = shutdown.changed() => SessionOutcome::Shutdown,
    }
}

/// Jitter backoff by ±25 % to break fleet-wide lockstep during
/// shared-cause outages (controller restart, WAN flap).
fn jittered(base: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut h = RandomState::new().build_hasher();
    h.write_u64(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0));
    let rand_u64 = h.finish();
    // Map to [-0.25, +0.25).
    let frac = ((rand_u64 as f64) / (u64::MAX as f64)) * 0.5 - 0.25;
    let base_ms = base.as_millis() as f64;
    let out_ms = (base_ms * (1.0 + frac)).max(100.0) as u64;
    Duration::from_millis(out_ms)
}

/// Convenience builder used by `mm-agent` main. Trading is
/// always on — there is no separate "subscribe-only" mode on
/// the agent. The deploy mode (paper vs live fills) comes from
/// each `DesiredStrategy`'s template config that the controller
/// pushes. Credentials arrive over the wire; the catalog starts
/// empty and fills as `PushCredential` commands land.
pub fn default_registry_builder(
    catalog: Arc<CredentialCatalog>,
    dashboard: mm_dashboard::state::DashboardState,
) -> RegistryBuilder {
    Arc::new(move |authority: AuthorityHandle| -> StrategyRegistry {
        let factory = RealEngineFactory::new(Arc::clone(&catalog))
            .with_authority(authority)
            .with_trading_enabled(true)
            .with_dashboard(dashboard.clone());
        StrategyRegistry::new(Arc::new(factory))
            .with_dashboard(dashboard.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_window() {
        for _ in 0..50 {
            let j = jittered(Duration::from_secs(4));
            let ms = j.as_millis();
            // 4s × [0.75, 1.25] = [3000, 5000], clamp at 100.
            assert!((2_900..=5_100).contains(&(ms as i64)), "jitter out of band: {ms}ms");
        }
    }

    #[test]
    fn jitter_respects_floor() {
        let j = jittered(Duration::from_millis(10));
        assert!(j.as_millis() >= 100, "floor keeps us off zero-sleep storms");
    }
}
