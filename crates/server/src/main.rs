//! `mm-server` — controller + dashboard binary.
//!
//! Single operator process for the fleet:
//! - Serves the Svelte UI from `frontend/dist` (configurable via
//!   `MM_FRONTEND_DIR`).
//! - Legacy dashboard endpoints (`/api/v1/status`, `/inventory`,
//!   `/pnl`, `/metrics`, auth flow, WebSocket broadcast) via
//!   `mm_dashboard` — their backing `DashboardState` is fed by
//!   an adapter that reads fleet + per-deployment telemetry from
//!   connected agents.
//! - Controller surface (`/api/v1/fleet`, `/api/v1/credentials`,
//!   `/api/v1/agents/{id}/deployments`, WS accept for agents) via
//!   `mm_controller`.
//! - Trading lives in `mm-agent`. Server has NO engine.
//!
//! Single-machine trading = one `mm-server` + one `mm-agent` on
//! the same host. Distributed = server central, agents per colo.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::Router;
use mm_controller::{
    http_router_full_authed, spawn_accept_loop_tls_full,
    spawn_accept_loop_with_credentials_and_approvals, AgentRegistry, ApprovalStore, FleetState,
    LeasePolicy, MasterKey, TunablesStore, VaultStore,
};
use mm_dashboard::{auth::AuthState, state::DashboardState, websocket::WsBroadcast};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

mod adapter;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // UI + dashboard + controller API — one port, operator-facing.
    // Default 9090 matches the legacy `dashboard_port` config so
    // operators bookmark the same URL.
    let http_addr_str =
        std::env::var("MM_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:9090".to_string());
    let http_addr = SocketAddr::from_str(&http_addr_str)
        .map_err(|e| anyhow::anyhow!("invalid MM_HTTP_ADDR={http_addr_str}: {e}"))?;

    // Agent WS — agents dial this to join the fleet. Port 9091 is
    // the HTTP's neighbour so both cluster on one mental block.
    let ws_addr_str =
        std::env::var("MM_AGENT_WS_ADDR").unwrap_or_else(|_| "127.0.0.1:9091".to_string());
    let ws_addr = SocketAddr::from_str(&ws_addr_str)
        .map_err(|e| anyhow::anyhow!("invalid MM_AGENT_WS_ADDR={ws_addr_str}: {e}"))?;

    let fleet = FleetState::new();
    let registry = AgentRegistry::new();
    // Lease policy — seeded from `Tunables` so operator-edited
    // values apply on next session. Live sessions keep whichever
    // TTL was in effect when they were issued; no hot-swap of
    // in-flight leases.
    let policy = Arc::new(LeasePolicy::default());
    // NB: full wire-up of tunables → LeasePolicy lands when
    // `AgentSession` takes `Arc<TunablesStore>` instead of a
    // baked policy (follow-up). For now the tunables file is
    // persisted + UI-editable, and a server restart applies
    // the new values.

    // Master key for vault encryption-at-rest. Preferred:
    // `MM_MASTER_KEY=<64-hex>` (systemd `Credentials=`, K8s
    // secret mount). Otherwise read/generate `./master-key`
    // (0600 perms) so local dev just works. Auto-generated on
    // first start; operator backs it up separately from the
    // vault file — an attacker who grabs only `vault.json`
    // can't decrypt without this key.
    let master_key_path = PathBuf::from(
        std::env::var("MM_MASTER_KEY_FILE").unwrap_or_else(|_| "master-key".to_string()),
    );
    let master_key = MasterKey::resolve(
        std::env::var("MM_MASTER_KEY").ok().as_deref(),
        master_key_path,
    )
    .map_err(|e| anyhow::anyhow!("master key resolve: {e}"))?;

    let vault_path = std::env::var("MM_VAULT").unwrap_or_else(|_| "vault.json".to_string());
    let vault = match VaultStore::load_from_path(&vault_path, master_key.clone()) {
        Ok(v) => {
            info!(path = %vault_path, entries = v.len(), "vault loaded");
            Some(v)
        }
        Err(e) => {
            return Err(anyhow::anyhow!("vault load failed at {vault_path}: {e}"));
        }
    };

    // Runtime tunables — lease policy, version pinning, deploy
    // defaults. Operator-editable from the UI, persisted to
    // `MM_TUNABLES=./tunables.json`. Missing fields in the file
    // get the code default so adding a new tunable is
    // backwards-compatible with existing deployments.
    let tunables_path =
        std::env::var("MM_TUNABLES").unwrap_or_else(|_| "tunables.json".to_string());
    let tunables = match TunablesStore::load_from_path(&tunables_path) {
        Ok(t) => {
            info!(path = %tunables_path, "tunables loaded");
            Some(t)
        }
        Err(e) => {
            warn!(error = %e, "failed to load tunables — using code defaults");
            Some(TunablesStore::in_memory())
        }
    };

    // Admission-control store. `MM_APPROVALS` controls the
    // backing file; unset now defaults to `./approvals.json`
    // so operator maintenance-restart of the server keeps the
    // accepted-agent list intact (previously rejected
    // fingerprints also stay rejected — critical). Set to
    // the literal string "memory" to opt into the old
    // in-memory behaviour (tests / ephemeral smokes).
    // Unknown fingerprints always land as Pending — operator
    // has to explicitly accept before the session gets a lease
    // or credentials.
    let approvals_path =
        std::env::var("MM_APPROVALS").unwrap_or_else(|_| "approvals.json".to_string());
    let approvals = if approvals_path == "memory" || approvals_path.is_empty() {
        info!(
            "approval store held in memory only (MM_APPROVALS={}); operator decisions lost on restart",
            approvals_path
        );
        ApprovalStore::in_memory()
    } else {
        let store = ApprovalStore::load_from_path(&approvals_path)
            .map_err(|e| anyhow::anyhow!("load approval store {approvals_path}: {e}"))?;
        info!(
            path = %approvals_path,
            entries = store.len(),
            "approval store loaded (persisted)"
        );
        store
    };

    // Dashboard state + auth + WS broadcast. State starts empty;
    // the adapter task below fills it from fleet telemetry.
    let dashboard_state = DashboardState::new();
    // M-SAVE GOBS — attach a GraphStore so `/api/v1/strategy/custom_templates`
    // has a root to write under. Distributed mode doesn't run an
    // engine locally, but the custom-template persistence lives
    // on the controller box so operators can save graphs from
    // any browser and the library persists across restarts.
    match mm_strategy_graph::GraphStore::new("data/strategy_graphs") {
        Ok(store) => {
            dashboard_state.set_strategy_graph_store(std::sync::Arc::new(store));
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "strategy graph store init failed — custom-template saves will 503",
            );
        }
    }
    let ws_broadcast = Arc::new(WsBroadcast::new(1024));

    // JWT signing secret. Must be stable across restarts or
    // every in-flight session token is invalidated. If unset we
    // auto-generate a 32-byte random secret and persist it to
    // `./auth-secret` (0600). Production: set MM_AUTH_SECRET
    // explicitly so you control key rotation.
    let jwt_secret = match std::env::var("MM_AUTH_SECRET").ok() {
        Some(s) => s,
        None => {
            let path = PathBuf::from("auth-secret");
            load_or_generate_auth_secret(&path)?
        }
    };

    // User store. First run: no users file → UI shows the
    // bootstrap form; operator creates the root admin there. No
    // hardcoded `admin-key-change-me` anywhere in the binary.
    let users_path = std::env::var("MM_USERS").unwrap_or_else(|_| "users.json".to_string());
    let mut auth_state = AuthState::new(&jwt_secret)
        .with_users_path(&users_path)
        .map_err(|e| anyhow::anyhow!("load users from {users_path}: {e}"))?;
    // TOTP issuer — shown to operators in their authenticator app
    // next to the account name. Default is the product brand; a
    // multi-tenant deploy overrides via `MM_TOTP_ISSUER` so each
    // controller's enrolled users see a deployment-specific label.
    if let Ok(issuer) = std::env::var("MM_TOTP_ISSUER") {
        auth_state = auth_state.with_totp_issuer(issuer);
    }
    // Controller-local audit sink — receives auth events
    // (login succeeded/failed, logout, password-reset issued/
    // completed) plus any other events the dashboard emits on
    // the controller side. Agent engines write their own audit
    // files; the fleet-aware fetcher (below) aggregates them
    // for MiCA monthly exports. The H4 `/api/admin/auth/audit`
    // readback reads only this local file because auth events
    // are controller-scoped.
    let audit_path_str =
        std::env::var("MM_AUDIT_PATH").unwrap_or_else(|_| "data/audit.jsonl".to_string());
    let audit_path = PathBuf::from(&audit_path_str);
    if let Some(parent) = audit_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    let shared_audit = match mm_risk::audit::AuditLog::new(&audit_path) {
        Ok(log) => {
            info!(path = %audit_path.display(), "controller audit log opened");
            Some(Arc::new(log))
        }
        Err(e) => {
            warn!(
                path = %audit_path.display(),
                error = %e,
                "failed to open controller audit log — auth events will not persist"
            );
            None
        }
    };
    if let Some(audit) = shared_audit.as_ref() {
        auth_state = auth_state.with_audit(audit.clone());
        dashboard_state.set_audit_log_path(audit_path.clone());
    }
    // Wave H3 — optional hard-gate: admin login requires TOTP
    // armed. `MM_REQUIRE_TOTP_FOR_ADMIN=1|true|yes` flips the
    // switch. Default off so deployments migrate intentionally
    // (admin enrolls TOTP first, operator flips the flag).
    let require_totp_admin = std::env::var("MM_REQUIRE_TOTP_FOR_ADMIN")
        .ok()
        .map(|v| {
            let s = v.trim().to_ascii_lowercase();
            matches!(s.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false);
    if require_totp_admin {
        info!("admin login hardened: TOTP required (MM_REQUIRE_TOTP_FOR_ADMIN)");
        // H5 GOBS — lockout-safe preflight. When the hard-gate
        // is armed but NO admin user has a TOTP secret enrolled,
        // every admin login will fail the 2FA check and nobody
        // can reach the dashboard to enrol. We refuse to start
        // in that state — the operator's only options are (a)
        // roll back the env flag, (b) run the binary once with
        // MM_REQUIRE_TOTP_ADMIN_BYPASS=yes-i-understand so they
        // can log in, enrol TOTP, log out, then remove the
        // bypass + restart. Fail-stop at boot is strictly
        // safer than bricking the auth surface mid-session.
        let bypass = std::env::var("MM_REQUIRE_TOTP_ADMIN_BYPASS")
            .ok()
            .map(|v| v.trim() == "yes-i-understand")
            .unwrap_or(false);
        let any_admin_has_totp = auth_state.any_admin_has_totp();
        if !any_admin_has_totp && !auth_state.needs_bootstrap() && !bypass {
            return Err(anyhow::anyhow!(
                "MM_REQUIRE_TOTP_FOR_ADMIN is set but no admin user has TOTP enrolled. \
                 Starting would lock every admin out. Fix by:\n  \
                 1. unset MM_REQUIRE_TOTP_FOR_ADMIN, restart, log in, enrol TOTP, restart with the flag; OR\n  \
                 2. set MM_REQUIRE_TOTP_ADMIN_BYPASS=yes-i-understand for a single recovery run, enrol TOTP, then remove the bypass.",
            ));
        }
        if bypass {
            warn!(
                "MM_REQUIRE_TOTP_ADMIN_BYPASS active — TOTP gate is soft for this boot; \
                 enrol TOTP on the admin user, log out, remove the bypass env, restart.",
            );
        }
    }
    auth_state = auth_state.with_require_totp_for_admin(require_totp_admin);
    if auth_state.needs_bootstrap() {
        warn!(
            path = %users_path,
            "no users configured — open the dashboard and create the root admin via the bootstrap form"
        );
    } else {
        info!(path = %users_path, "user store loaded");
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        http_addr = %http_addr,
        ws_addr = %ws_addr,
        has_vault = vault.is_some(),
        "mm-server starting"
    );

    // Adapter — every 1s, project FleetState + per-agent
    // deployment snapshots into the DashboardState so the
    // legacy endpoints (status, inventory, pnl, venues, …) have
    // something to serve.
    adapter::spawn_fleet_to_dashboard_adapter(
        fleet.clone(),
        dashboard_state.clone(),
        Duration::from_secs(1),
    );

    // Fleet audit fetcher — wire the controller registry + fleet
    // snapshot into the dashboard so MiCA monthly reports read
    // audit events from every agent instead of the (empty)
    // local audit.jsonl. Build once as an Arc closure and
    // install; dashboard's `build_monthly_report` picks it up.
    {
        let fleet_c = fleet.clone();
        let registry_c = registry.clone();
        let fetcher: mm_dashboard::state::AuditRangeFetcher =
            std::sync::Arc::new(move |from_ms: i64, until_ms: i64, limit: usize| {
                let fleet = fleet_c.clone();
                let registry = registry_c.clone();
                Box::pin(async move {
                    fetch_fleet_audit_range(&fleet, &registry, from_ms, until_ms, limit).await
                })
            });
        dashboard_state.set_audit_range_fetcher(fetcher);
    }

    // Wave B1 — fleet client-metrics fetcher. Fans out the
    // `client_metrics` details topic across every running
    // deployment, optionally filtered by the agent's tenant
    // (`approvals.get(fp).profile.client_id`). Dashboard's
    // /positions, /pnl, /sla, /client/{id}/* handlers prefer
    // this over their narrow local projection.
    {
        let fleet_c = fleet.clone();
        let registry_c = registry.clone();
        let approvals_c = approvals.clone();
        let fetcher: mm_dashboard::state::FleetClientMetricsFetcher =
            std::sync::Arc::new(move |client_filter: Option<String>| {
                let fleet = fleet_c.clone();
                let registry = registry_c.clone();
                let approvals = approvals_c.clone();
                Box::pin(async move {
                    fetch_fleet_client_metrics(
                        &fleet,
                        &registry,
                        &approvals,
                        client_filter.as_deref(),
                    )
                    .await
                })
            });
        dashboard_state.set_fleet_client_metrics_fetcher(fetcher);
    }

    // Wave B5 — hot-register AddClient broadcaster. Admin
    // `create_client` invokes this so a new tenant lands on
    // every accepted agent's DashboardState immediately.
    {
        let fleet_c = fleet.clone();
        let registry_c = registry.clone();
        let broadcaster: mm_dashboard::state::FleetAddClientBroadcaster =
            std::sync::Arc::new(move |client_id: String, symbols: Vec<String>| {
                let fleet = fleet_c.clone();
                let registry = registry_c.clone();
                Box::pin(
                    async move { broadcast_add_client(&fleet, &registry, &client_id, &symbols) },
                )
            });
        dashboard_state.set_fleet_add_client_broadcaster(broadcaster);
    }

    // Compose HTTP routes:
    //   * controller routes (fleet, credentials, deployments) +
    //     Svelte ServeDir fallback
    //   * dashboard routes (status, inventory, pnl, /ws, /metrics,
    //     auth flow, admin)
    // axum Router::merge handles non-overlapping routes from
    // both; each keeps its own state.
    // Fix #6 — fleet-wide Telegram bridge. Controller-level
    // dedup sender: agents don't fire Telegram (their
    // AlertManager is constructed with `None` by the agent
    // runner), so the single hop is here. Pulls the same
    // fan-out the /api/v1/alerts/fleet endpoint uses, tracks
    // which (severity, title) pairs it has already sent in a
    // 10-minute window so a recurring kill doesn't spam the
    // chat. Telegram creds come from env (MM_TELEGRAM_TOKEN +
    // MM_TELEGRAM_CHAT); unset = task still runs but only
    // logs, no network.
    {
        let fleet_c = fleet.clone();
        let registry_c = registry.clone();
        tokio::spawn(async move {
            telegram_bridge_loop(fleet_c, registry_c).await;
        });
    }

    // Fix #5 — violations auto-actions. Polls the same
    // rollup the UI's ViolationsPanel consumes; when
    // `tunables.auto_widen_on_violation` is set and a
    // high-severity breach lands on a deployment that isn't
    // already kill-escalated, dispatches L1 widen. Default
    // off — operator must opt in via Platform tunables.
    if let Some(tun) = tunables.clone() {
        let fleet_c = fleet.clone();
        let registry_c = registry.clone();
        tokio::spawn(async move {
            violation_auto_action_loop(fleet_c, registry_c, tun).await;
        });
    }

    // I3 (2026-04-21) — distributed webhook fan-out. Every
    // few seconds walks tenants with a registered dispatcher,
    // pulls their fills from the fleet fetcher, and fires a
    // `WebhookEvent::Fill` for every fill newer than the
    // tenant's dispatch cursor. The tenant's dispatcher lives
    // on the controller; fills live on agents; this loop is
    // the missing edge that connects the two. Not gated behind
    // a tunable — tenants who registered a URL already consented.
    {
        let ds_c = dashboard_state.clone();
        tokio::spawn(async move {
            webhook_fanout_loop(ds_c).await;
        });
    }

    // SEC-1 — controller routes MUST layer auth + role gates.
    // The 2026-04-21 product-journey smoke caught /api/v1/fleet,
    // /api/v1/vault, /api/v1/approvals, and deploy POSTs all
    // reachable anonymously. `http_router_full_authed` wraps
    // read-tier in internal_view, control-tier in can_control,
    // admin-tier (vault / approvals / tunables PUT) in admin.
    let controller_router = http_router_full_authed(
        fleet.clone(),
        registry.clone(),
        vault.clone(),
        Some(approvals.clone()),
        tunables.clone(),
        auth_state.clone(),
    );
    let dashboard_router = mm_dashboard::server::build_app(
        dashboard_state.clone(),
        ws_broadcast.clone(),
        auth_state.clone(),
    );
    let app: Router = controller_router.merge(dashboard_router);

    let http_task = tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(http_addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(error = %e, "HTTP bind failed");
                return;
            }
        };
        info!(addr = %http_addr, "HTTP (Svelte + controller + dashboard) listening");
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            error!(error = %e, "HTTP server exited with error");
        }
    });

    let tls_cert = std::env::var("MM_TLS_CERT").ok().map(PathBuf::from);
    let tls_key = std::env::var("MM_TLS_KEY").ok().map(PathBuf::from);

    // Every accept-loop variant gets the ApprovalStore — admission
    // control is not optional in this binary. Tests and
    // library-level callers that want the bare behaviour use the
    // lower-level `spawn_accept_loop*` helpers directly.
    let ws_task = match (tls_cert, tls_key) {
        (Some(cert), Some(key)) => {
            let acceptor = mm_control::build_acceptor(&cert, &key)
                .map_err(|e| anyhow::anyhow!("TLS acceptor build failed: {e}"))?;
            info!(
                cert = %cert.display(),
                has_vault = vault.is_some(),
                "TLS enabled + admission control"
            );
            spawn_accept_loop_tls_full(
                ws_addr,
                fleet.clone(),
                registry.clone(),
                vault.clone(),
                approvals.clone(),
                policy,
                acceptor,
            )
        }
        (None, None) => {
            warn!(
                has_vault = vault.is_some(),
                "plain ws:// — admission control enabled, ensure network boundary is trusted"
            );
            spawn_accept_loop_with_credentials_and_approvals(
                ws_addr,
                fleet.clone(),
                registry.clone(),
                vault.clone(),
                approvals.clone(),
                policy,
            )
        }
        _ => anyhow::bail!("MM_TLS_CERT and MM_TLS_KEY must be set together"),
    };

    tokio::signal::ctrl_c().await?;
    info!("SIGINT — shutting down");
    ws_task.abort();
    http_task.abort();
    let _ = ws_task.await;
    let _ = http_task.await;
    Ok(())
}

/// Fan out an `audit_tail` request to every running deployment
/// across the fleet, collect replies, merge + cap. Called by
/// the dashboard's `build_monthly_report` via the
/// `AuditRangeFetcher` closure installed at boot. The fan-out
/// is parallel (one command per deployment) with a 5s per-call
/// timeout — `build_monthly_report` is operator-initiated, so
/// we prefer a thorough read over a low-latency one.
async fn fetch_fleet_audit_range(
    fleet: &mm_controller::FleetState,
    registry: &mm_controller::AgentRegistry,
    from_ms: i64,
    until_ms: i64,
    limit: usize,
) -> Vec<serde_json::Value> {
    use mm_control::messages::CommandPayload;

    const PER_DEPLOYMENT_TIMEOUT: Duration = Duration::from_secs(5);

    let snapshot = fleet.snapshot();
    let mut handles = Vec::new();
    for view in snapshot {
        let agent_id = view.agent_id.clone();
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        for dep in &view.deployments {
            if !dep.running {
                continue;
            }
            let request_id = uuid::Uuid::new_v4();
            let (tx, rx) = tokio::sync::oneshot::channel();
            registry.pending_details_register(request_id, tx);
            let mut args = serde_json::Map::new();
            args.insert("from_ms".into(), serde_json::json!(from_ms));
            args.insert("until_ms".into(), serde_json::json!(until_ms));
            args.insert("limit".into(), serde_json::json!(limit));
            if registry
                .send(
                    &agent_id,
                    CommandPayload::FetchDeploymentDetails {
                        deployment_id: dep.deployment_id.clone(),
                        topic: "audit_tail".into(),
                        request_id,
                        args,
                    },
                )
                .is_err()
            {
                // Command dispatch failed — agent session gone.
                // Reclaim the pending slot so a late reply
                // doesn't linger.
                registry.pending_details_forget(request_id);
                continue;
            }
            handles.push((request_id, tokio::time::timeout(PER_DEPLOYMENT_TIMEOUT, rx)));
        }
    }

    let mut merged: Vec<serde_json::Value> = Vec::new();
    for (request_id, future) in handles {
        match future.await {
            Ok(Ok(reply)) => {
                if let Some(events) = reply.payload.get("events").and_then(|v| v.as_array()) {
                    merged.extend(events.iter().cloned());
                }
            }
            _ => {
                registry.pending_details_forget(request_id);
            }
        }
    }

    // Cap at `limit` after merge — per-deployment caps add up
    // fast on a big fleet. Sort newest-first using millisecond
    // `ts_ms` or RFC3339 `timestamp`, keep top-N.
    fn ts_of(v: &serde_json::Value) -> i64 {
        if let Some(n) = v.get("ts_ms").and_then(|x| x.as_i64()) {
            return n;
        }
        if let Some(s) = v.get("timestamp").and_then(|x| x.as_str()) {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                return dt.timestamp_millis();
            }
        }
        0
    }
    merged.sort_by_key(|r| std::cmp::Reverse(ts_of(r)));
    merged.truncate(limit);
    merged
}

/// Wave B1 implementation — fan-out the `client_metrics`
/// details topic across every live deployment in the fleet,
/// optionally filtered to agents whose approval profile
/// carries a matching `client_id`. Each reply is the compact
/// JSON slice the agent's dashboard emits (decimal-string
/// PnL fields, SLA scalars, book-depth sums). Returns one
/// `Value` per deployment with `agent_id` + `deployment_id`
/// + `client_id` injected so callers can group by tenant
/// without another lookup.
async fn fetch_fleet_client_metrics(
    fleet: &mm_controller::FleetState,
    registry: &mm_controller::AgentRegistry,
    approvals: &mm_controller::ApprovalStore,
    client_filter: Option<&str>,
) -> Vec<serde_json::Value> {
    use mm_control::messages::CommandPayload;

    const PER_DEPLOYMENT_TIMEOUT: Duration = Duration::from_secs(3);

    let snapshot = fleet.snapshot();
    let mut handles = Vec::new();
    for view in snapshot {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        // Resolve agent tenant via fingerprint. Matching
        // `AgentProfile.client_id` is how the UI groups rows
        // by tenant today; we mirror that on the server.
        let tenant = if view.pubkey_fingerprint.is_empty() {
            None
        } else {
            approvals
                .get(&view.pubkey_fingerprint)
                .and_then(|rec| rec.profile.client_id.clone())
        };
        if let Some(filter) = client_filter {
            if tenant.as_deref() != Some(filter) {
                continue;
            }
        }
        let agent_id = view.agent_id.clone();
        for dep in &view.deployments {
            if !dep.running {
                continue;
            }
            let request_id = uuid::Uuid::new_v4();
            let (tx, rx) = tokio::sync::oneshot::channel();
            registry.pending_details_register(request_id, tx);
            if registry
                .send(
                    &agent_id,
                    CommandPayload::FetchDeploymentDetails {
                        deployment_id: dep.deployment_id.clone(),
                        topic: "client_metrics".into(),
                        request_id,
                        args: serde_json::Map::new(),
                    },
                )
                .is_err()
            {
                registry.pending_details_forget(request_id);
                continue;
            }
            handles.push((
                request_id,
                agent_id.clone(),
                dep.deployment_id.clone(),
                tenant.clone(),
                tokio::time::timeout(PER_DEPLOYMENT_TIMEOUT, rx),
            ));
        }
    }

    let mut merged: Vec<serde_json::Value> = Vec::new();
    for (request_id, agent_id, deployment_id, tenant, future) in handles {
        match future.await {
            Ok(Ok(reply)) => {
                // The agent emits a full JSON object (or null
                // when it has no SymbolState yet). Skip nulls;
                // inject ownership fields on the rest.
                if !reply.payload.is_null() {
                    if let Some(obj) = reply.payload.as_object() {
                        let mut row = obj.clone();
                        row.insert("agent_id".into(), serde_json::json!(agent_id));
                        row.insert("deployment_id".into(), serde_json::json!(deployment_id));
                        if let Some(t) = tenant {
                            row.insert("client_id".into(), serde_json::json!(t));
                        }
                        merged.push(serde_json::Value::Object(row));
                    }
                }
            }
            _ => {
                registry.pending_details_forget(request_id);
            }
        }
    }
    merged
}

/// I3 (2026-04-21) — webhook fan-out loop. Runs on the
/// controller, polls tenants with registered dispatchers,
/// fires `WebhookEvent::Fill` per new fill discovered in the
/// fleet snapshot. Cursors advance per-tenant so a restart of
/// the loop (but not of the controller itself — cursors live
/// in DashboardState, in-memory) doesn't replay fills.
/// On first pass for a fresh dispatcher the cursor is set to
/// the newest-seen timestamp so historical fills don't flood
/// a freshly registered endpoint.
async fn webhook_fanout_loop(state: mm_dashboard::state::DashboardState) {
    use mm_dashboard::webhooks::WebhookEvent;
    const TICK: Duration = Duration::from_secs(5);
    loop {
        tokio::time::sleep(TICK).await;
        let clients = state.client_ids_with_webhooks();
        for client_id in clients {
            let Some(dispatcher) = state.get_client_webhook_dispatcher(&client_id) else {
                continue;
            };
            // `get_client_fills` uses the same fleet-aware path
            // that powers `/api/v1/client/self/fills` so we never
            // read stale controller-local state.
            let fills = if let Some(f) = state.fleet_client_metrics_fetcher() {
                let metrics = f(Some(client_id.clone())).await;
                let mut out = Vec::new();
                for row in metrics {
                    if let Some(arr) = row.get("recent_fills").and_then(|v| v.as_array()) {
                        for raw in arr {
                            if let Ok(fill) = serde_json::from_value::<
                                mm_dashboard::state::FillRecord,
                            >(raw.clone())
                            {
                                out.push(fill);
                            }
                        }
                    }
                }
                out.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                out
            } else {
                Vec::new()
            };
            if fills.is_empty() {
                continue;
            }
            let cursor = state.webhook_fill_cursor(&client_id);
            let newest = fills.last().map(|f| f.timestamp);
            match cursor {
                None => {
                    // First pass for this dispatcher — mark the
                    // cursor at the newest fill without firing,
                    // so a tenant who just registered doesn't get
                    // a burst of pre-existing events.
                    if let Some(ts) = newest {
                        state.set_webhook_fill_cursor(&client_id, ts);
                    }
                }
                Some(last_ts) => {
                    let mut max_ts = last_ts;
                    for fill in fills.iter().filter(|f| f.timestamp > last_ts) {
                        dispatcher.dispatch(WebhookEvent::Fill {
                            symbol: fill.symbol.clone(),
                            side: fill.side.clone(),
                            price: fill.price,
                            qty: fill.qty,
                            timestamp: fill.timestamp.to_rfc3339(),
                            is_maker: fill.is_maker,
                            fee: fill.fee,
                        });
                        if fill.timestamp > max_ts {
                            max_ts = fill.timestamp;
                        }
                    }
                    if max_ts > last_ts {
                        state.set_webhook_fill_cursor(&client_id, max_ts);
                    }
                }
            }
        }
    }
}

/// Fix #5 — violations auto-action loop. Walks the fleet
/// snapshot every 10s, finds high-severity breaches (SLA
/// uptime < 90%, manipulation combined ≥ 0.95) on
/// deployments that are still RUNNING without kill
/// escalation, and dispatches an L1 widen. Gated behind
/// `tunables.auto_widen_on_violation` — when the flag is
/// false the loop sleeps without touching anything.
/// Tracks what's already been actioned in a 15-minute
/// window so one breach doesn't trigger escalating widens.
async fn violation_auto_action_loop(
    fleet: mm_controller::FleetState,
    registry: mm_controller::AgentRegistry,
    tunables: mm_controller::TunablesStore,
) {
    use mm_control::messages::CommandPayload;
    use std::collections::HashMap;
    const TICK: Duration = Duration::from_secs(10);
    const COOLDOWN_MS: i64 = 15 * 60 * 1000;

    let mut last_action: HashMap<String, i64> = HashMap::new();

    loop {
        tokio::time::sleep(TICK).await;

        let cur = tunables.current();
        if !cur.auto_widen_on_violation {
            continue;
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        last_action.retain(|_, ts| now_ms - *ts < COOLDOWN_MS);

        let snapshot = fleet.snapshot();
        for view in snapshot {
            if !view.approval_state.is_empty() && view.approval_state != "accepted" {
                continue;
            }
            for dep in &view.deployments {
                if !dep.running {
                    continue;
                }
                // Already escalated — nothing more to auto-do.
                if dep.kill_level > 0 {
                    continue;
                }
                let sla_uptime = dep.sla_uptime_pct.parse::<f64>().unwrap_or(100.0);
                let manip_score = dep.manipulation_combined.parse::<f64>().unwrap_or(0.0);
                // Wave G3 — per-category gating. Each flag
                // controls one trigger; order here is priority
                // (SLA first, then manipulation) so if both
                // breaches fire on the same tick, the SLA
                // reason wins the audit message.
                let breach = if cur.auto_widen_sla
                    && !dep.sla_uptime_pct.is_empty()
                    && sla_uptime > 0.0
                    && sla_uptime < 90.0
                {
                    Some(format!("SLA uptime {:.2}% < 90%", sla_uptime))
                } else if cur.auto_widen_manip && manip_score >= 0.95 {
                    Some(format!(
                        "manipulation combined {:.0}% ≥ 95%",
                        manip_score * 100.0
                    ))
                } else {
                    None
                };
                let Some(reason) = breach else { continue };
                let key = format!("{}/{}", view.agent_id, dep.deployment_id);
                if last_action.contains_key(&key) {
                    continue;
                }

                let mut patch = serde_json::Map::new();
                patch.insert("kill_level".into(), serde_json::json!(1));
                patch.insert(
                    "kill_reason".into(),
                    serde_json::json!(format!("auto-widen: {reason}")),
                );
                if registry
                    .send(
                        &view.agent_id,
                        CommandPayload::PatchDeploymentVariables {
                            deployment_id: dep.deployment_id.clone(),
                            patch,
                        },
                    )
                    .is_ok()
                {
                    last_action.insert(key, now_ms);
                    warn!(
                        agent = %view.agent_id,
                        deployment = %dep.deployment_id,
                        symbol = %dep.symbol,
                        reason = %reason,
                        "auto-widen dispatched (L1 kill)"
                    );
                }
            }
        }
    }
}

/// Fix #6 — controller-side Telegram bridge loop. Wakes
/// every 15 seconds, fan-outs `alerts_recent` to every agent,
/// collapses duplicate `(severity, title)` entries, and fires
/// Telegram once per unique key within a 10-minute window.
/// Quiet-log no-op when `MM_TELEGRAM_TOKEN` is unset.
async fn telegram_bridge_loop(
    fleet: mm_controller::FleetState,
    registry: mm_controller::AgentRegistry,
) {
    use mm_control::messages::CommandPayload;
    use std::collections::HashMap;
    const SEND_WINDOW_MS: i64 = 10 * 60 * 1000;
    const TICK: Duration = Duration::from_secs(15);

    let token = std::env::var("MM_TELEGRAM_TOKEN").ok();
    let chat = std::env::var("MM_TELEGRAM_CHAT").ok();
    let client = reqwest::Client::new();
    // Track the last ts we sent for each (severity, title).
    let mut last_sent: HashMap<(String, String), i64> = HashMap::new();

    loop {
        tokio::time::sleep(TICK).await;

        // Fan out alerts_recent to every accepted agent's first
        // running deployment (alerts live on agent-local
        // DashboardState so any deployment works).
        let mut collected: Vec<serde_json::Value> = Vec::new();
        for view in fleet.snapshot() {
            if !view.approval_state.is_empty() && view.approval_state != "accepted" {
                continue;
            }
            let Some(dep) = view.deployments.iter().find(|d| d.running) else {
                continue;
            };
            let request_id = uuid::Uuid::new_v4();
            let (tx, rx) = tokio::sync::oneshot::channel();
            registry.pending_details_register(request_id, tx);
            let mut args = serde_json::Map::new();
            args.insert("limit".into(), serde_json::json!(50));
            if registry
                .send(
                    &view.agent_id,
                    CommandPayload::FetchDeploymentDetails {
                        deployment_id: dep.deployment_id.clone(),
                        topic: "alerts_recent".into(),
                        request_id,
                        args,
                    },
                )
                .is_err()
            {
                registry.pending_details_forget(request_id);
                continue;
            }
            if let Ok(Ok(reply)) = tokio::time::timeout(Duration::from_secs(3), rx).await {
                if let Some(arr) = reply.payload.get("alerts").and_then(|v| v.as_array()) {
                    collected.extend(arr.iter().cloned());
                }
            }
        }

        // Dedup + send.
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Drop old keys out of the window.
        last_sent.retain(|_, ts| now_ms - *ts < SEND_WINDOW_MS);

        for alert in &collected {
            let ts_ms = alert.get("ts_ms").and_then(|v| v.as_i64()).unwrap_or(0);
            // Only consider alerts younger than the window.
            if ts_ms < now_ms - SEND_WINDOW_MS {
                continue;
            }
            let severity = alert
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("info")
                .to_string();
            let title = alert
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let key = (severity.clone(), title.clone());
            if last_sent.contains_key(&key) {
                continue;
            }
            last_sent.insert(key, now_ms);
            let message = alert
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let symbol = alert
                .get("symbol")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let emoji = match severity.as_str() {
                "critical" => "🚨",
                "high" | "warning" => "⚠️",
                _ => "ℹ️",
            };
            let text = format!(
                "{emoji} *{title}*{}\n{message}",
                if symbol.is_empty() {
                    String::new()
                } else {
                    format!(" [{symbol}]")
                },
            );

            if let (Some(tok), Some(ch)) = (token.as_deref(), chat.as_deref()) {
                let url = format!("https://api.telegram.org/bot{tok}/sendMessage");
                let body = serde_json::json!({
                    "chat_id": ch,
                    "text": text,
                    "parse_mode": "Markdown",
                });
                if let Err(e) = client.post(&url).json(&body).send().await {
                    warn!(error = %e, title = %title, "telegram bridge: send failed");
                }
            } else {
                info!(severity = %severity, title = %title, "telegram bridge: would send (no token configured)");
            }
        }
    }
}

/// Wave B5 — push an `AddClient` command to every accepted
/// agent in the fleet. Returns the number of successful
/// dispatches (command enqueued; not an end-to-end ACK).
/// `send` is synchronous in the registry so the broadcast
/// returns as soon as every session's outbound queue has
/// accepted the message.
fn broadcast_add_client(
    fleet: &mm_controller::FleetState,
    registry: &mm_controller::AgentRegistry,
    client_id: &str,
    symbols: &[String],
) -> usize {
    use mm_control::messages::CommandPayload;
    let mut count = 0usize;
    for view in fleet.snapshot() {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        if registry
            .send(
                &view.agent_id,
                CommandPayload::AddClient {
                    client_id: client_id.to_string(),
                    symbols: symbols.to_vec(),
                },
            )
            .is_ok()
        {
            count += 1;
        }
    }
    count
}

fn load_or_generate_auth_secret(path: &PathBuf) -> Result<String> {
    if path.exists() {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read auth secret {}: {e}", path.display()))?;
        let trimmed = raw.trim().to_string();
        if trimmed.len() >= 32 {
            return Ok(trimmed);
        }
        warn!(
            path = %path.display(),
            len = trimmed.len(),
            "auth secret file too short — regenerating a stronger one"
        );
    }
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    let secret = hex::encode(buf);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("mkdir for auth secret: {e}"))?;
        }
    }
    std::fs::write(path, &secret)
        .map_err(|e| anyhow::anyhow!("write auth secret {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    warn!(
        path = %path.display(),
        "generated fresh auth secret — persist this file, losing it invalidates all session tokens"
    );
    Ok(secret)
}
