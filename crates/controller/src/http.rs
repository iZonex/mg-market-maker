//! Controller HTTP observability + deploy endpoints.
//!
//! PR-2d-lite shipped `GET /api/v1/fleet` + `/health`. PR-2g
//! adds `POST /api/v1/agents/{id}/deployments` so an operator
//! (or future dashboard UI) can push a desired-strategy slice
//! at a named agent without going through a WS-RPC test harness.
//!
//! Authorisation: `router_full` (used by tests) is deliberately
//! auth-less so integration tests can exercise CRUD without
//! standing up a full AuthState. Production callers MUST use
//! [`router_full_authed`] which layers `auth_middleware` +
//! role gates over the same routes. The binary (mm-server)
//! calls the authed variant — closing the 2026-04-21 journey
//! smoke finding where every controller route was anonymously
//! reachable.

use std::net::SocketAddr;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

use mm_control::messages::{CommandPayload, DesiredStrategy};
use mm_dashboard::auth::{
    admin_middleware, auth_middleware, tenant_scope_middleware, AuthState,
};

use crate::approvals::{AgentProfilePatch, ApprovalRecord, ApprovalStore};
use crate::registry::SessionLifecycleEvent;
use crate::tunables::{TunableField, Tunables, TunablesStore};
use crate::vault::{CredentialCheck, CredentialDescriptor, VaultEntry, VaultError, VaultStore, VaultSummary};
use crate::{AgentRegistry, FleetState, RegistryError};

#[derive(Clone)]
struct AppState {
    fleet: FleetState,
    registry: AgentRegistry,
    vault: Option<VaultStore>,
    approvals: Option<ApprovalStore>,
    tunables: Option<TunablesStore>,
}

pub fn router(fleet: FleetState, registry: AgentRegistry) -> Router {
    router_full(fleet, registry, None, None, None)
}

/// SEC-1 — production variant of [`router_full`] that layers
/// `auth_middleware` + role gates over every endpoint. Reads
/// (GET) require an authenticated non-ClientReader role (admin
/// / operator / viewer). Writes (POST / PATCH / DELETE) that
/// mutate trading state require `can_control()` (admin /
/// operator). Security-critical writes (vault, approvals,
/// tunables PUT) require admin.
///
/// The SPA static fallback stays anonymous — HTML / JS / CSS
/// is public by design. Anonymous callers hitting any `/api/*`
/// path get a 401 from `auth_middleware` without ever reaching
/// the handler.
pub fn router_full_authed(
    fleet: FleetState,
    registry: AgentRegistry,
    vault: Option<VaultStore>,
    approvals: Option<ApprovalStore>,
    tunables: Option<TunablesStore>,
    auth_state: AuthState,
) -> Router {
    let state = AppState {
        fleet,
        registry,
        vault,
        approvals,
        tunables,
    };

    let frontend_dir = resolve_frontend_dir();
    let spa_index = format!("{frontend_dir}/index.html");
    let static_spa = ServeDir::new(&frontend_dir)
        .append_index_html_on_directories(true)
        .not_found_service(ServeFile::new(spa_index));

    // Tier 1 — internal view (admin / operator / viewer). Returns
    // operational state, never mutates. ClientReader is blocked
    // from the entire controller surface — it has its own portal.
    let internal_view = Router::new()
        .route("/api/v1/fleet", get(get_fleet))
        .route("/api/v1/vault", get(list_vault_entries))
        .route("/api/v1/agents/{agent_id}/deployments", get(get_deployments))
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/variables",
            axum::routing::get(get_deployment_variables),
        )
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/details/{topic}",
            get(get_deployment_details),
        )
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/replay",
            axum::routing::post(post_deployment_replay),
        )
        .route(
            "/api/v1/agents/{agent_id}/credentials",
            get(get_agent_credentials),
        )
        .route("/api/v1/templates", get(get_templates))
        .route("/api/v1/tunables", get(get_tunables))
        .route("/api/v1/tunables/schema", get(get_tunables_schema))
        .route("/api/v1/approvals", get(list_approvals))
        .route("/api/v1/surveillance/fleet", get(get_surveillance_fleet))
        .route("/api/v1/reconciliation/fleet", get(get_reconciliation_fleet))
        .route("/api/v1/alerts/fleet", get(get_alerts_fleet))
        // Tenant-scope gate blocks ClientReader (their token is
        // tenant-scoped, controller surface carries no `{id}`
        // so it cannot be scoped to a single tenant). Admin /
        // Operator / Viewer are untenanted and pass through.
        .route_layer(middleware::from_fn(tenant_scope_middleware))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

    // Tier 2 — control surface (admin / operator). Mutates a
    // running deployment's variables, fires ops, deploys new
    // strategies, dispatches fleet-wide actions, kicks off
    // audit verification. Viewer cannot hit these; ClientReader
    // already filtered by the auth layer failing on role.
    //
    // We reuse admin_middleware-semantics via a small adapter
    // that accepts either Admin or Operator. No new middleware
    // needed — wrap handlers that themselves check
    // claims.can_control() via the shared auth_middleware +
    // control_role_middleware pair.
    let control = Router::new()
        .route(
            "/api/v1/agents/{agent_id}/deployments",
            post(post_deployments),
        )
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/variables",
            axum::routing::patch(patch_deployment_variables),
        )
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/ops/{op}",
            post(post_deployment_op),
        )
        .route("/api/v1/ops/fleet/{op}", post(post_fleet_op))
        .route("/api/v1/audit/verify", post(post_audit_verify))
        .route(
            "/api/admin/sentiment/headline",
            post(post_sentiment_headline),
        )
        .route("/api/admin/config/{symbol}", post(post_admin_config_proxy))
        .route_layer(middleware::from_fn(control_role_middleware))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

    // Tier 3 — admin-only. Vault writes, approval lifecycle,
    // tunables PUT, agent profile PUT. Stealing any one of
    // these gives an attacker the ability to exfiltrate
    // credentials or accept a rogue agent into the fleet.
    let admin_only = Router::new()
        .route("/api/v1/vault", post(post_vault_entry))
        .route(
            "/api/v1/vault/{name}",
            axum::routing::put(put_vault_entry).delete(delete_vault_entry),
        )
        .route("/api/v1/tunables", axum::routing::put(put_tunables))
        .route(
            "/api/v1/agents/{fingerprint}/profile",
            axum::routing::put(put_agent_profile),
        )
        .route("/api/v1/approvals/pre-approve", post(pre_approve_agent))
        .route(
            "/api/v1/approvals/{fingerprint}",
            axum::routing::delete(delete_approval),
        )
        .route("/api/v1/approvals/{fingerprint}/accept", post(accept_agent))
        .route("/api/v1/approvals/{fingerprint}/reject", post(reject_agent))
        .route("/api/v1/approvals/{fingerprint}/revoke", post(revoke_agent))
        .route_layer(middleware::from_fn(admin_middleware))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(state);

    internal_view
        .merge(control)
        .merge(admin_only)
        .fallback_service(static_spa)
}

/// Role gate used by the control tier. Allows Admin + Operator,
/// rejects Viewer / ClientReader with 403. Runs after
/// `auth_middleware` so `TokenClaims` is present.
async fn control_role_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use mm_dashboard::auth::TokenClaims;
    match req.extensions().get::<TokenClaims>() {
        Some(c) if c.role.can_control() => next.run(req).await,
        Some(_) => StatusCode::FORBIDDEN.into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub fn router_full(
    fleet: FleetState,
    registry: AgentRegistry,
    vault: Option<VaultStore>,
    approvals: Option<ApprovalStore>,
    tunables: Option<TunablesStore>,
) -> Router {
    let state = AppState { fleet, registry, vault, approvals, tunables };

    // API routes match first, everything else hits the Svelte
    // SPA. Resolve the `frontend/dist` location in this order:
    //   1. `MM_FRONTEND_DIR` env — explicit operator override
    //   2. `./frontend/dist` — repo root (typical dev)
    //   3. `../frontend/dist` — running from `target/debug`
    //   4. `../../frontend/dist` — running deeper
    //   5. compile-time `CARGO_MANIFEST_DIR/../../frontend/dist`
    //      — the workspace path baked into the binary
    // First match that has an `index.html` wins.
    let frontend_dir = resolve_frontend_dir();
    let spa_index = format!("{frontend_dir}/index.html");
    let static_spa = ServeDir::new(&frontend_dir)
        .append_index_html_on_directories(true)
        .not_found_service(ServeFile::new(spa_index));

    Router::new()
        .route("/api/v1/fleet", get(get_fleet))
        // Unified vault — exchange credentials + generic service
        // secrets. Kind tag on each entry gates push-to-agent.
        .route(
            "/api/v1/vault",
            get(list_vault_entries).post(post_vault_entry),
        )
        .route(
            "/api/v1/vault/{name}",
            axum::routing::put(put_vault_entry).delete(delete_vault_entry),
        )
        .route(
            "/api/v1/agents/{agent_id}/deployments",
            post(post_deployments).get(get_deployments),
        )
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/variables",
            axum::routing::get(get_deployment_variables)
                .patch(patch_deployment_variables),
        )
        // Per-deployment operational endpoint. "Strategy = a
        // deployment" in the distributed model: kill / pause /
        // emulator / DCA / graph-swap all target a specific
        // (agent, deployment) tuple, not a symbol (symbols
        // aren't unique when the same venue pair runs on
        // different agents).
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/ops/{op}",
            post(post_deployment_op),
        )
        // On-demand details fetch. Controller fires a
        // FetchDeploymentDetails command with a fresh request_id,
        // parks a oneshot in the registry, and awaits the agent's
        // DetailsReply. Topics recognised today:
        //   * `funding_arb_recent_events`
        // Timeout 5s — any slower and it's safer for the UI to
        // show "stale" than block indefinitely.
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/details/{topic}",
            get(get_deployment_details),
        )
        .route(
            "/api/v1/agents/{agent_id}/deployments/{deployment_id}/replay",
            axum::routing::post(post_deployment_replay),
        )
        // Legacy thin-proxy for the pre-distributed `/api/admin/config/{symbol}`
        // endpoint. Tools and scripts that already hit this URL
        // (hyperopt drivers, ad-hoc jq one-liners, external
        // monitoring) keep working — we route the (field, value)
        // pair to the matching deployment's variables PATCH.
        // See `post_admin_config_proxy` for the translation table.
        .route(
            "/api/admin/config/{symbol}",
            post(post_admin_config_proxy),
        )
        // Sentiment headline broadcast — fleet-wide fan-out.
        // Iterates every live deployment and PATCHes a `news`
        // variable into it; the agent translator maps that into
        // `ConfigOverride::News(text)` which the engine's
        // NewsRetreatStateMachine consumes on its next tick.
        .route(
            "/api/admin/sentiment/headline",
            post(post_sentiment_headline),
        )
        .route(
            "/api/v1/agents/{agent_id}/credentials",
            get(get_agent_credentials),
        )
        .route("/api/v1/templates", get(get_templates))
        // Platform-level runtime tunables (lease TTL, version
        // pinning, deploy defaults). `GET /schema` tells the UI
        // how to render the form — field types, categories,
        // min/max ranges. `PUT` replaces the whole blob.
        .route("/api/v1/tunables", get(get_tunables).put(put_tunables))
        .route("/api/v1/tunables/schema", get(get_tunables_schema))
        .route("/api/v1/approvals", get(list_approvals))
        // Wave F2 — pre-approve a fingerprint before the agent
        // has ever registered. Admin pastes the FP that the
        // trading-box operator read off the agent's boot log;
        // when the agent actually connects, the handshake is
        // silent (no pending step).
        .route(
            "/api/v1/approvals/pre-approve",
            post(pre_approve_agent),
        )
        .route(
            "/api/v1/approvals/{fingerprint}",
            axum::routing::delete(delete_approval),
        )
        .route(
            "/api/v1/approvals/{fingerprint}/accept",
            post(accept_agent),
        )
        .route(
            "/api/v1/approvals/{fingerprint}/reject",
            post(reject_agent),
        )
        .route(
            "/api/v1/approvals/{fingerprint}/revoke",
            post(revoke_agent),
        )
        // Agent profile — description / labels / client / region.
        // Distinct from approval: editing profile never touches
        // admission state and vice versa.
        .route(
            "/api/v1/agents/{fingerprint}/profile",
            axum::routing::put(put_agent_profile),
        )
        // Fleet-wide surveillance aggregate. Rolls up the
        // per-deployment `manipulation_*` fields on every live
        // DeploymentStateRow into a single array the UI can
        // table without fanning out to each agent. Gauges are
        // engine-emitted as of Wave 1 R follow-up; this endpoint
        // just joins them across the fleet.
        .route(
            "/api/v1/surveillance/fleet",
            get(get_surveillance_fleet),
        )
        .route(
            "/api/v1/reconciliation/fleet",
            get(get_reconciliation_fleet),
        )
        // Wave D4 — fleet-wide alert stream with dedup.
        .route(
            "/api/v1/alerts/fleet",
            get(get_alerts_fleet),
        )
        // Fix #2 — real hash-chain verification. Fans out to
        // every running deployment, each agent verifies its
        // own JSONL file (computes SHA-256 row-by-row against
        // the stored `prev_hash`), controller aggregates.
        .route(
            "/api/v1/audit/verify",
            post(post_audit_verify),
        )
        // Wave C2 — fleet-wide ops. Applies an op to every
        // running deployment on every accepted agent. Today's
        // surface: `pause` / `resume`. Same body shape as the
        // per-deployment route, same translator so
        // pause/resume go through `paused` variable.
        .route(
            "/api/v1/ops/fleet/{op}",
            post(post_fleet_op),
        )
        // `/health` is owned by the dashboard router — when the
        // controller is merged alongside it (mm-server main),
        // duplicating here panics on overlapping routes.
        .with_state(state)
        .fallback_service(static_spa)
}

async fn list_approvals(State(state): State<AppState>) -> Json<Vec<ApprovalRecord>> {
    match &state.approvals {
        Some(store) => Json(store.list()),
        None => Json(Vec::new()),
    }
}

#[derive(Deserialize, Default)]
struct ApprovalActionBody {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    actor: Option<String>,
}

fn approval_error(e: crate::approvals::ApprovalStoreError) -> (StatusCode, String) {
    use crate::approvals::ApprovalStoreError::*;
    match e {
        Io(ref err) if err.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, e.to_string())
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct PreApproveBody {
    fingerprint: String,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

async fn pre_approve_agent(
    State(state): State<AppState>,
    Json(body): Json<PreApproveBody>,
) -> Result<Json<ApprovalRecord>, (StatusCode, String)> {
    let Some(store) = &state.approvals else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no approval store configured".into(),
        ));
    };
    let fp = body.fingerprint.trim();
    if fp.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "fingerprint is required".into()));
    }
    // Fingerprints are 16 hex chars (SHA-256[..8]). Reject
    // obviously wrong input early — saves typos from creating
    // dead records.
    if fp.len() < 8 || !fp.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("'{fp}' is not a valid fingerprint — expected hex string ≥ 8 chars"),
        ));
    }
    let actor = body.actor.unwrap_or_else(|| "operator".into());
    let record = store
        .pre_approve(fp, &actor, body.notes.as_deref())
        .map_err(approval_error)?;
    Ok(Json(record))
}

async fn accept_agent(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    Json(body): Json<ApprovalActionBody>,
) -> Result<Json<ApprovalRecord>, (StatusCode, String)> {
    let Some(store) = &state.approvals else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no approval store configured".into(),
        ));
    };
    let actor = body.actor.unwrap_or_else(|| "operator".into());
    let record = store.accept(&fingerprint, &actor).map_err(approval_error)?;
    // Nudge the connected session (if any) so the lease-grant
    // dance fires without waiting for the agent's next frame.
    let _ = state
        .registry
        .send_lifecycle(&record.agent_id, SessionLifecycleEvent::ApprovalGranted);
    Ok(Json(record))
}

async fn reject_agent(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    Json(body): Json<ApprovalActionBody>,
) -> Result<Json<ApprovalRecord>, (StatusCode, String)> {
    let Some(store) = &state.approvals else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no approval store configured".into(),
        ));
    };
    let actor = body.actor.unwrap_or_else(|| "operator".into());
    let reason = body.reason.unwrap_or_else(|| "operator reject".into());
    let record = store
        .reject(&fingerprint, &actor, &reason)
        .map_err(approval_error)?;
    // If the agent is currently connected we may or may not have
    // issued a lease (if they were previously Accepted). Either
    // way sending Revoke is safe — it's a no-op if the agent
    // holds no lease today.
    let _ = state.registry.send_lifecycle(
        &record.agent_id,
        SessionLifecycleEvent::ApprovalRevoked {
            reason: reason.clone(),
        },
    );
    Ok(Json(record))
}

async fn revoke_agent(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    Json(body): Json<ApprovalActionBody>,
) -> Result<Json<ApprovalRecord>, (StatusCode, String)> {
    let Some(store) = &state.approvals else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no approval store configured".into(),
        ));
    };
    let actor = body.actor.unwrap_or_else(|| "operator".into());
    let reason = body.reason.unwrap_or_else(|| "operator revoke".into());
    let record = store
        .revoke(&fingerprint, &actor, &reason)
        .map_err(approval_error)?;
    let _ = state.registry.send_lifecycle(
        &record.agent_id,
        SessionLifecycleEvent::ApprovalRevoked {
            reason: reason.clone(),
        },
    );
    Ok(Json(record))
}

async fn put_agent_profile(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    Json(patch): Json<AgentProfilePatch>,
) -> Result<Json<ApprovalRecord>, (StatusCode, String)> {
    let Some(store) = &state.approvals else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no approval store configured".into(),
        ));
    };
    let rec = store
        .update_profile(&fingerprint, patch)
        .map_err(approval_error)?;
    Ok(Json(rec))
}

async fn delete_approval(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let Some(store) = &state.approvals else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no approval store configured".into(),
        ));
    };
    store.remove(&fingerprint).map_err(approval_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_fleet(State(state): State<AppState>) -> Json<Vec<crate::AgentView>> {
    Json(state.fleet.snapshot())
}

/// One row in the fleet-aggregate surveillance response. Pins
/// every detector score to the concrete (agent, deployment,
/// symbol) triple so the UI can table or filter by any of them.
#[derive(Debug, Serialize)]
struct SurveillanceFleetRow {
    agent_id: String,
    deployment_id: String,
    symbol: String,
    pump_dump: String,
    wash: String,
    thin_book: String,
    combined: String,
    /// `0` unless the kill-switch is escalated on this deployment.
    /// Useful to flag rows where a detector already tripped a
    /// response — the UI renders them in a distinct band.
    kill_level: u8,
    sampled_at_ms: i64,
}

async fn get_surveillance_fleet(
    State(state): State<AppState>,
) -> Json<Vec<SurveillanceFleetRow>> {
    let mut out = Vec::new();
    for view in state.fleet.snapshot() {
        for dep in view.deployments {
            // Skip rows with no manipulation sample yet —
            // detectors warm up, and empty strings would render
            // as 0.000 which looks like "totally safe" rather
            // than "no data". Better to omit.
            if dep.manipulation_combined.is_empty() {
                continue;
            }
            out.push(SurveillanceFleetRow {
                agent_id: view.agent_id.clone(),
                deployment_id: dep.deployment_id,
                symbol: dep.symbol,
                pump_dump: dep.manipulation_pump_dump,
                wash: dep.manipulation_wash,
                thin_book: dep.manipulation_thin_book,
                combined: dep.manipulation_combined,
                kill_level: dep.kill_level,
                sampled_at_ms: dep.sampled_at_ms,
            });
        }
    }
    // Sort by combined score desc so the UI's first page is the
    // rows an operator most likely needs to look at. Ties break
    // by (agent, symbol) so the order is stable tick-to-tick.
    out.sort_by(|a, b| {
        let av: f64 = a.combined.parse().unwrap_or(0.0);
        let bv: f64 = b.combined.parse().unwrap_or(0.0);
        bv.partial_cmp(&av)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    Json(out)
}

async fn get_deployments(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<Vec<mm_control::DeploymentStateRow>>, (StatusCode, String)> {
    match state.fleet.get(&agent_id) {
        Some(view) => Ok(Json(view.deployments)),
        None => Err((
            StatusCode::NOT_FOUND,
            format!("agent {agent_id} is not currently connected"),
        )),
    }
}

fn resolve_frontend_dir() -> String {
    if let Ok(explicit) = std::env::var("MM_FRONTEND_DIR") {
        return explicit;
    }
    let candidates = [
        "frontend/dist".to_string(),
        "../frontend/dist".to_string(),
        "../../frontend/dist".to_string(),
        format!("{}/../../frontend/dist", env!("CARGO_MANIFEST_DIR")),
    ];
    for c in &candidates {
        let index = format!("{c}/index.html");
        if std::path::Path::new(&index).exists() {
            return c.clone();
        }
    }
    tracing::warn!(
        "frontend/dist not found in any default location — set MM_FRONTEND_DIR explicitly. \
         API endpoints still work; UI won't load."
    );
    "frontend/dist".to_string()
}

/// Request body for `POST /api/v1/vault` and `PUT …/{name}`.
///
/// Unified shape for every kind of secret the vault holds. For
/// exchange-kind entries, operators supply `api_key` + `api_secret`
/// in `values` and `exchange` + `product` in `metadata`; for
/// Telegram, `token` in `values` and `chat_id` in `metadata`;
/// etc. The server validates kind-specific required fields
/// (see `vault::validate`).
#[derive(Deserialize, Debug)]
struct VaultCreate {
    name: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    values: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    metadata: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    allowed_agents: Vec<String>,
    /// Wave 2b tenant tag. Empty / absent = shared infra; any
    /// non-empty string gates the credential to agents whose
    /// profile `client_id` matches. See `VaultEntry::client_id`.
    #[serde(default)]
    client_id: Option<String>,
    /// Wave C6 — optional operator-supplied expiry (epoch millis).
    /// `None` / absent = never expires.
    #[serde(default)]
    expires_at_ms: Option<i64>,
}

fn default_kind() -> String {
    crate::vault::kinds::GENERIC.into()
}

fn create_to_entry(body: VaultCreate) -> VaultEntry {
    VaultEntry {
        name: body.name,
        kind: body.kind,
        description: body.description,
        values: body.values,
        metadata: body.metadata,
        allowed_agents: body.allowed_agents,
        client_id: body.client_id.filter(|s| !s.is_empty()),
        created_at_ms: 0,
        updated_at_ms: 0,
        rotated_at_ms: None,
        expires_at_ms: body.expires_at_ms,
    }
}

fn vault_error(e: VaultError) -> (StatusCode, String) {
    match e {
        VaultError::Duplicate(_) => (StatusCode::CONFLICT, e.to_string()),
        VaultError::NotFound(_) => (StatusCode::NOT_FOUND, e.to_string()),
        VaultError::Invalid(_) => (StatusCode::BAD_REQUEST, e.to_string()),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn post_vault_entry(
    State(state): State<AppState>,
    Json(body): Json<VaultCreate>,
) -> Result<Json<VaultSummary>, (StatusCode, String)> {
    let Some(store) = &state.vault else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no vault configured — set MM_VAULT".into(),
        ));
    };
    let cred_name = body.name.clone();
    let summary = store
        .insert(create_to_entry(body))
        .map_err(vault_error)?;
    push_credential_to_connected_agents(&state, &cred_name);
    Ok(Json(summary))
}

async fn put_vault_entry(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(mut body): Json<VaultCreate>,
) -> Result<Json<VaultSummary>, (StatusCode, String)> {
    let Some(store) = &state.vault else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no vault configured".into(),
        ));
    };
    body.name = name;
    let cred_name = body.name.clone();
    let summary = store
        .upsert(create_to_entry(body))
        .map_err(vault_error)?;
    push_credential_to_connected_agents(&state, &cred_name);
    Ok(Json(summary))
}

/// Live-smoke gap fix — when an operator adds or rotates a
/// vault entry while agents are already connected, push the
/// new shape to every accepted agent whose `allowed_agents`
/// whitelist admits it. Without this, operators who deploy
/// immediately after creating a credential saw "credential
/// not found in settings" on the agent side; the credential
/// reached the agent only on the next reconnect.
fn push_credential_to_connected_agents(state: &AppState, cred_name: &str) {
    use mm_control::messages::CommandPayload;
    let Some(store) = &state.vault else { return };
    for view in state.fleet.snapshot() {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        // Filter by each agent's authorisation shape.
        let pushable = store.pushable_exchange_for_agent(&view.agent_id);
        let Some(cred) = pushable.into_iter().find(|c| c.id == cred_name) else {
            // Either not an exchange kind, or the whitelist
            // excludes this agent — legitimate, silent skip.
            continue;
        };
        match state.registry.send(
            &view.agent_id,
            CommandPayload::PushCredential { credential: cred },
        ) {
            Ok(()) => {
                tracing::info!(
                    agent = %view.agent_id,
                    credential = %cred_name,
                    "pushed credential to live agent after vault upsert"
                );
            }
            Err(e) => {
                tracing::warn!(
                    agent = %view.agent_id,
                    credential = %cred_name,
                    error = ?e,
                    "credential push failed — agent will pick it up on next reconnect"
                );
            }
        }
    }
}

async fn delete_vault_entry(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let Some(store) = &state.vault else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no vault configured".into(),
        ));
    };
    store.remove(&name).map_err(vault_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_vault_entries(State(state): State<AppState>) -> Json<Vec<VaultSummary>> {
    match &state.vault {
        Some(store) => Json(store.list_summaries()),
        None => Json(Vec::new()),
    }
}

#[derive(Debug, Deserialize)]
struct DeployRequest {
    strategies: Vec<DesiredStrategy>,
}

#[derive(Debug, Serialize)]
struct DeployResponse {
    agent_id: String,
    accepted: usize,
}

async fn post_deployments(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(body): Json<DeployRequest>,
) -> Result<Json<DeployResponse>, (StatusCode, String)> {
    // Pre-validation — catches common deploy mistakes BEFORE the
    // SetDesiredStrategies frame hits the agent. Silent no-op on
    // the agent side was worse than a loud 412: operator thinks
    // the strategy is running when actually the catalog resolve
    // failed. We refuse early, loud.
    if let Err((code, msg)) = pre_validate_deploy(&state, &agent_id, &body.strategies) {
        return Err((code, msg));
    }

    // Fix #3 — auto-inject `variables.client_id` from the
    // agent's approval profile so fills land in the right
    // tenant bucket without the operator having to set it in
    // every deploy. Explicit `client_id` in the request wins;
    // agent profile fallback kicks in only when the deploy
    // request doesn't set it.
    let agent_tenant: Option<String> = state
        .fleet
        .get(&agent_id)
        .and_then(|v| {
            if v.pubkey_fingerprint.is_empty() {
                None
            } else {
                Some(v.pubkey_fingerprint.clone())
            }
        })
        .and_then(|fp| state.approvals.as_ref().and_then(|a| a.get(&fp)))
        .and_then(|rec| rec.profile.client_id.clone());
    let mut strategies = body.strategies;
    if let Some(ref tenant) = agent_tenant {
        for s in strategies.iter_mut() {
            if !s.variables.contains_key("client_id") {
                s.variables.insert(
                    "client_id".into(),
                    serde_json::Value::String(tenant.clone()),
                );
            }
        }
    }

    let count = strategies.len();
    match state.registry.send(
        &agent_id,
        CommandPayload::SetDesiredStrategies { strategies },
    ) {
        Ok(()) => Ok(Json(DeployResponse {
            agent_id,
            accepted: count,
        })),
        Err(RegistryError::NotFound) => Err((
            StatusCode::NOT_FOUND,
            format!("agent {agent_id} is not currently connected"),
        )),
        Err(RegistryError::AgentGone) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("agent {agent_id} session is shutting down"),
        )),
    }
}

fn pre_validate_deploy(
    state: &AppState,
    agent_id: &str,
    strategies: &[DesiredStrategy],
) -> Result<(), (StatusCode, String)> {
    // Agent must be `Accepted` in the approval store if one is
    // attached — deploying to Pending / Rejected / Revoked makes
    // no sense: the agent holds no lease so it can't execute.
    //
    // We also resolve the agent's tenant (`profile.client_id`)
    // here so the downstream vault check can enforce the Wave 2b
    // tenant gate. Untagged agents (`None`) map to "shared infra"
    // and only pass credentials that are themselves untagged.
    let mut agent_tenant: Option<String> = None;
    if let Some(approvals) = state.approvals.as_ref() {
        if let Some(view) = state.fleet.get(agent_id) {
            if !view.approval_state.is_empty() && view.approval_state != "accepted" {
                return Err((
                    StatusCode::PRECONDITION_FAILED,
                    format!(
                        "agent '{agent_id}' is in approval state '{}' — deploy requires 'accepted'",
                        view.approval_state
                    ),
                ));
            }
            if !view.pubkey_fingerprint.is_empty() {
                agent_tenant = approvals
                    .get(&view.pubkey_fingerprint)
                    .and_then(|rec| rec.profile.client_id.clone());
            }
        }
    }

    // Every referenced credential must exist in the store AND
    // the target agent must be authorised to receive it.
    // Simultaneously scan for cross-tenant credential mix within
    // a single deployment — two credentials with different
    // `Some(x)` client_ids can never legitimately coexist in the
    // same strategy descriptor.
    if let Some(store) = state.vault.as_ref() {
        for strategy in strategies {
            let mut seen_tenant: Option<String> = None;
            for cred_id in strategy.credential_ids() {
                match store.can_exchange_access(cred_id, agent_id, agent_tenant.as_deref()) {
                    CredentialCheck::Ok { .. } => {
                        if let Some(entry) = store.get(cred_id) {
                            if let Some(t) = entry.client_id.clone() {
                                match &seen_tenant {
                                    Some(prev) if prev != &t => {
                                        return Err((
                                            StatusCode::PRECONDITION_FAILED,
                                            format!(
                                                "deployment '{}' mixes credentials from two tenants ('{prev}' and '{t}') — refuse to avoid cross-tenant leak",
                                                strategy.deployment_id
                                            ),
                                        ));
                                    }
                                    None => seen_tenant = Some(t),
                                    _ => {}
                                }
                            }
                        }
                    }
                    CredentialCheck::Unknown => {
                        return Err((
                            StatusCode::PRECONDITION_FAILED,
                            format!(
                                "deployment '{}' references credential '{cred_id}' which does not exist in the vault",
                                strategy.deployment_id
                            ),
                        ));
                    }
                    CredentialCheck::WrongKind { actual } => {
                        return Err((
                            StatusCode::PRECONDITION_FAILED,
                            format!(
                                "deployment '{}' references '{cred_id}' which is kind '{actual}', not 'exchange'",
                                strategy.deployment_id
                            ),
                        ));
                    }
                    CredentialCheck::NotAuthorised { whitelist } => {
                        return Err((
                            StatusCode::PRECONDITION_FAILED,
                            format!(
                                "deployment '{}' references credential '{cred_id}' which is not authorised for agent '{agent_id}' (allowed agents: {:?})",
                                strategy.deployment_id, whitelist,
                            ),
                        ));
                    }
                    CredentialCheck::TenantMismatch { cred_tenant, agent_tenant } => {
                        let agent_desc = if agent_tenant.is_empty() {
                            "<untagged>".to_string()
                        } else {
                            format!("'{agent_tenant}'")
                        };
                        return Err((
                            StatusCode::PRECONDITION_FAILED,
                            format!(
                                "deployment '{}' references credential '{cred_id}' belonging to tenant '{cred_tenant}', but agent '{agent_id}' is tenant {agent_desc} — refuse to avoid cross-tenant leak",
                                strategy.deployment_id
                            ),
                        ));
                    }
                    CredentialCheck::Expired { expired_at_ms } => {
                        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                            expired_at_ms,
                        )
                        .map(|d| d.to_rfc3339())
                        .unwrap_or_else(|| format!("{expired_at_ms}"));
                        return Err((
                            StatusCode::PRECONDITION_FAILED,
                            format!(
                                "deployment '{}' references credential '{cred_id}' which expired at {dt} — rotate the credential before deploying",
                                strategy.deployment_id
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct PatchVariablesResponse {
    agent_id: String,
    deployment_id: String,
    patched_fields: usize,
}

#[derive(serde::Serialize)]
struct DeploymentVariablesView {
    agent_id: String,
    deployment_id: String,
    symbol: String,
    template: String,
    variables: serde_json::Map<String, serde_json::Value>,
    active_graph: Option<serde_json::Value>,
}

/// Introspect the currently-applied variables for a deployment.
/// Returns the freshest snapshot the controller has from fleet
/// telemetry — same data that lives inside `DeploymentStateRow`,
/// surfaced on its own URL so the StrategyPage / DeployDialog
/// "preview before patch" flow doesn't have to parse the whole
/// fleet blob. Mirrors the shape `patch_deployment_variables`
/// accepts so a round-trip `GET → edit → PATCH` works naturally.
async fn get_deployment_variables(
    State(state): State<AppState>,
    Path((agent_id, deployment_id)): Path<(String, String)>,
) -> Result<Json<DeploymentVariablesView>, (StatusCode, String)> {
    let view = state.fleet.get(&agent_id).ok_or((
        StatusCode::NOT_FOUND,
        format!("agent {agent_id} is not currently connected"),
    ))?;
    let row = view
        .deployments
        .iter()
        .find(|d| d.deployment_id == deployment_id)
        .ok_or((
            StatusCode::NOT_FOUND,
            format!(
                "deployment {deployment_id} not found on agent {agent_id}"
            ),
        ))?;
    Ok(Json(DeploymentVariablesView {
        agent_id,
        deployment_id,
        symbol: row.symbol.clone(),
        template: row.template.clone(),
        variables: row.variables.clone(),
        active_graph: row.active_graph.clone(),
    }))
}

async fn patch_deployment_variables(
    State(state): State<AppState>,
    Path((agent_id, deployment_id)): Path<(String, String)>,
    Json(body): Json<serde_json::Map<String, serde_json::Value>>,
) -> Result<Json<PatchVariablesResponse>, (StatusCode, String)> {
    // Validate target agent is Accepted — deploying / editing
    // on a Pending / Rejected agent makes no sense.
    if let Some(approvals) = state.approvals.as_ref() {
        let _ = approvals;
        if let Some(view) = state.fleet.get(&agent_id) {
            if !view.approval_state.is_empty() && view.approval_state != "accepted" {
                return Err((
                    StatusCode::PRECONDITION_FAILED,
                    format!(
                        "agent '{agent_id}' is '{}' — patch requires 'accepted'",
                        view.approval_state
                    ),
                ));
            }
        }
    }
    let count = body.len();
    match state.registry.send(
        &agent_id,
        CommandPayload::PatchDeploymentVariables {
            deployment_id: deployment_id.clone(),
            patch: body,
        },
    ) {
        Ok(()) => Ok(Json(PatchVariablesResponse {
            agent_id,
            deployment_id,
            patched_fields: count,
        })),
        Err(RegistryError::NotFound) => Err((
            StatusCode::NOT_FOUND,
            format!("agent {agent_id} is not currently connected"),
        )),
        Err(RegistryError::AgentGone) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("agent {agent_id} session is shutting down"),
        )),
    }
}

/// Legacy `/api/admin/config/{symbol}` body shape — a single
/// `{ field, value }` pair. Pre-distributed single-engine
/// clients use this format; keep it as-is so they don't need
/// to change when pointing at the new controller.
#[derive(Deserialize, Debug)]
struct LegacyConfigOverride {
    /// `ConfigOverride` variant name in Pascal case — e.g.
    /// `"Gamma"`, `"MinSpreadBps"`, `"MomentumEnabled"`.
    field: String,
    /// Value as a string. Numeric knobs parse on the agent;
    /// booleans accept `"true"` / `"false"`. Ignored for
    /// `PauseQuoting` / `ResumeQuoting` (the field name carries
    /// the intent).
    #[serde(default)]
    value: String,
}

/// Translate one legacy `{field, value}` override into a
/// variables-PATCH pair. Mirrors the agent's
/// `translate_variable_override` — if a match arm lands there, a
/// matching branch goes here. `None` means the legacy field has
/// no modern equivalent and the caller should surface a 400.
fn legacy_config_to_variable(
    field: &str,
    value: &str,
) -> Option<(String, serde_json::Value)> {
    match field {
        "Gamma" => Some(("gamma".into(), serde_json::json!(value))),
        "MinSpreadBps" => Some(("min_spread_bps".into(), serde_json::json!(value))),
        "OrderSize" => Some(("order_size".into(), serde_json::json!(value))),
        "MaxDistanceBps" => Some(("max_distance_bps".into(), serde_json::json!(value))),
        "NumLevels" => value
            .parse::<u64>()
            .ok()
            .map(|n| ("num_levels".into(), serde_json::json!(n))),
        "MomentumEnabled" => Some((
            "momentum_enabled".into(),
            serde_json::json!(value == "true" || value == "1"),
        )),
        "MarketResilienceEnabled" => Some((
            "market_resilience_enabled".into(),
            serde_json::json!(value == "true" || value == "1"),
        )),
        "AmendEnabled" => Some((
            "amend_enabled".into(),
            serde_json::json!(value == "true" || value == "1"),
        )),
        "AmendMaxTicks" => value
            .parse::<u64>()
            .ok()
            .map(|n| ("amend_max_ticks".into(), serde_json::json!(n))),
        "OtrEnabled" => Some((
            "otr_enabled".into(),
            serde_json::json!(value == "true" || value == "1"),
        )),
        "MaxInventory" => Some(("max_inventory".into(), serde_json::json!(value))),
        "PauseQuoting" => Some(("paused".into(), serde_json::json!(true))),
        "ResumeQuoting" => Some(("paused".into(), serde_json::json!(false))),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
struct LegacyConfigProxyResponse {
    agent_id: String,
    deployment_id: String,
    symbol: String,
    field: String,
    applied: bool,
}

/// Resolve `symbol` → `(agent_id, deployment_id)` by scanning the
/// fleet. Returns the FIRST match — symbol uniqueness is an
/// operator invariant, not a controller-enforced one, so if two
/// deployments quote the same symbol a legacy client can't
/// disambiguate. Modern clients should use the per-deployment
/// PATCH endpoint instead.
fn find_deployment_for_symbol(state: &AppState, symbol: &str) -> Option<(String, String)> {
    for view in state.fleet.snapshot() {
        for dep in &view.deployments {
            if dep.symbol == symbol {
                return Some((view.agent_id.clone(), dep.deployment_id.clone()));
            }
        }
    }
    None
}

/// Legacy thin-proxy for `/api/admin/config/{symbol}`. Translates
/// the old single-engine `ConfigOverride` shape into a
/// per-deployment variables PATCH and forwards it via the same
/// registry send path modern clients use.
async fn post_admin_config_proxy(
    State(state): State<AppState>,
    Path(symbol): Path<String>,
    Json(body): Json<LegacyConfigOverride>,
) -> Result<Json<LegacyConfigProxyResponse>, (StatusCode, String)> {
    let Some((key, value)) = legacy_config_to_variable(&body.field, &body.value) else {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "legacy field '{}' has no variables-PATCH mapping — use the per-deployment variables PATCH endpoint directly",
                body.field
            ),
        ));
    };
    let Some((agent_id, deployment_id)) = find_deployment_for_symbol(&state, &symbol) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!(
                "no running deployment matches symbol '{symbol}' in the fleet"
            ),
        ));
    };

    let mut patch = serde_json::Map::new();
    patch.insert(key, value);

    match state.registry.send(
        &agent_id,
        CommandPayload::PatchDeploymentVariables {
            deployment_id: deployment_id.clone(),
            patch,
        },
    ) {
        Ok(()) => Ok(Json(LegacyConfigProxyResponse {
            agent_id,
            deployment_id,
            symbol,
            field: body.field,
            applied: true,
        })),
        Err(RegistryError::NotFound) => Err((
            StatusCode::NOT_FOUND,
            format!("agent {agent_id} is not currently connected"),
        )),
        Err(RegistryError::AgentGone) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("agent {agent_id} session is shutting down"),
        )),
    }
}

/// Body for POST /api/v1/agents/{a}/deployments/{d}/ops/{op}.
/// Every field is optional — each op variant picks what it
/// cares about. Unknown fields are ignored (forward-compatible).
#[derive(Debug, Deserialize, Default)]
struct OpBody {
    /// Kill reason audit string. Defaults to "dashboard
    /// operator" when empty.
    #[serde(default)]
    reason: String,
    /// JSON spec for emulator-register (EmulatorOrderSpec) and
    /// dca-start (DcaSpec). Forwarded opaquely; the engine
    /// parses.
    #[serde(default)]
    spec: Option<serde_json::Value>,
    /// Emulator id for emulator-cancel.
    #[serde(default)]
    id: Option<u64>,
    /// Full strategy-graph JSON body for graph-swap.
    #[serde(default)]
    graph: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct OpResponse {
    agent_id: String,
    deployment_id: String,
    op: String,
    applied: bool,
}

/// Translate an `op` name (path segment) + body into the
/// `variables`-PATCH shape the agent's registry understands.
/// Kept here rather than in the agent because controller is
/// the policy layer and the ops catalogue is part of the HTTP
/// contract — changing the name means changing the URL.
fn op_to_variables_patch(
    op: &str,
    body: &OpBody,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut m = serde_json::Map::new();
    let reason = if body.reason.trim().is_empty() {
        "dashboard operator".to_string()
    } else {
        body.reason.clone()
    };
    match op {
        // Kill ladder L1–L5.
        "widen"       => { m.insert("kill_level".into(), serde_json::json!(1)); m.insert("kill_reason".into(), serde_json::json!(reason)); }
        "stop"        => { m.insert("kill_level".into(), serde_json::json!(2)); m.insert("kill_reason".into(), serde_json::json!(reason)); }
        "cancel-all"  => { m.insert("kill_level".into(), serde_json::json!(3)); m.insert("kill_reason".into(), serde_json::json!(reason)); }
        "flatten"     => { m.insert("kill_level".into(), serde_json::json!(4)); m.insert("kill_reason".into(), serde_json::json!(reason)); }
        "disconnect"  => { m.insert("kill_level".into(), serde_json::json!(5)); m.insert("kill_reason".into(), serde_json::json!(reason)); }
        "reset"       => { m.insert("kill_reset_reason".into(), serde_json::json!(reason)); }
        // Pause / resume — already wired via the `paused`
        // variable translator.
        "pause"       => { m.insert("paused".into(), serde_json::json!(true)); }
        "resume"      => { m.insert("paused".into(), serde_json::json!(false)); }
        // Emulator + DCA + graph-swap — specs pass through
        // opaquely; agent translator emits the right variant.
        "emulator-register" => {
            let spec = body.spec.as_ref().ok_or_else(|| "emulator-register requires body.spec".to_string())?;
            m.insert("emulator_spec".into(), serde_json::json!(spec.to_string()));
        }
        "emulator-cancel" => {
            let id = body.id.ok_or_else(|| "emulator-cancel requires body.id".to_string())?;
            m.insert("emulator_cancel_id".into(), serde_json::json!(id));
        }
        "dca-start" => {
            let spec = body.spec.as_ref().ok_or_else(|| "dca-start requires body.spec".to_string())?;
            m.insert("dca_spec".into(), serde_json::json!(spec.to_string()));
        }
        "dca-cancel" => {
            m.insert("dca_cancel".into(), serde_json::json!(true));
        }
        "graph-swap" => {
            let graph = body.graph.as_ref().ok_or_else(|| "graph-swap requires body.graph".to_string())?;
            m.insert("strategy_graph".into(), serde_json::json!(graph.to_string()));
        }
        other => return Err(format!("unknown op '{other}'")),
    }
    Ok(m)
}

#[derive(serde::Serialize)]
struct FleetOpResult {
    agent_id: String,
    deployment_id: String,
    symbol: String,
    applied: bool,
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct FleetOpResponse {
    op: String,
    attempted: usize,
    succeeded: usize,
    failed: usize,
    results: Vec<FleetOpResult>,
}

/// Wave C2 — fleet-wide op dispatcher. Fans out the per-deployment
/// op (e.g. `pause`, `resume`) to every running deployment on every
/// accepted agent. Same translator as the per-deployment route so
/// behaviour is identical; ACK-accept is best-effort per target.
async fn post_fleet_op(
    State(state): State<AppState>,
    Path(op): Path<String>,
    body: Option<Json<OpBody>>,
) -> Result<Json<FleetOpResponse>, (StatusCode, String)> {
    // Only pause/resume are fleet-safe today. Kill-ladder and
    // graph-swap are deliberately scoped to a single deployment
    // — a fleet-wide kill L5 would trigger simultaneous
    // disconnects and is more useful as a per-agent decision
    // than a single click. Reopen if operator pain says
    // otherwise.
    const ALLOWED: &[&str] = &["pause", "resume"];
    if !ALLOWED.contains(&op.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "op '{op}' is not fleet-safe — allowed: {}",
                ALLOWED.join(", ")
            ),
        ));
    }

    let body = body.map(|Json(b)| b).unwrap_or_default();
    let patch = op_to_variables_patch(&op, &body)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let mut results = Vec::new();
    for view in state.fleet.snapshot() {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        for dep in &view.deployments {
            if !dep.running {
                continue;
            }
            let send_result = state.registry.send(
                &view.agent_id,
                CommandPayload::PatchDeploymentVariables {
                    deployment_id: dep.deployment_id.clone(),
                    patch: patch.clone(),
                },
            );
            let (applied, error) = match send_result {
                Ok(()) => (true, None),
                Err(e) => (false, Some(format!("{e:?}"))),
            };
            results.push(FleetOpResult {
                agent_id: view.agent_id.clone(),
                deployment_id: dep.deployment_id.clone(),
                symbol: dep.symbol.clone(),
                applied,
                error,
            });
        }
    }
    let attempted = results.len();
    let succeeded = results.iter().filter(|r| r.applied).count();
    let failed = attempted - succeeded;
    Ok(Json(FleetOpResponse {
        op,
        attempted,
        succeeded,
        failed,
        results,
    }))
}

async fn post_deployment_op(
    State(state): State<AppState>,
    Path((agent_id, deployment_id, op)): Path<(String, String, String)>,
    body: Option<Json<OpBody>>,
) -> Result<Json<OpResponse>, (StatusCode, String)> {
    // Agent admission gate — same as variables PATCH.
    if let Some(view) = state.fleet.get(&agent_id) {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            return Err((
                StatusCode::PRECONDITION_FAILED,
                format!(
                    "agent '{agent_id}' is '{}' — op requires 'accepted'",
                    view.approval_state
                ),
            ));
        }
    }
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let patch = op_to_variables_patch(&op, &body)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    match state.registry.send(
        &agent_id,
        CommandPayload::PatchDeploymentVariables {
            deployment_id: deployment_id.clone(),
            patch,
        },
    ) {
        Ok(()) => Ok(Json(OpResponse {
            agent_id,
            deployment_id,
            op,
            applied: true,
        })),
        Err(RegistryError::NotFound) => Err((
            StatusCode::NOT_FOUND,
            format!("agent {agent_id} is not currently connected"),
        )),
        Err(RegistryError::AgentGone) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("agent {agent_id} session is shutting down"),
        )),
    }
}

#[derive(Deserialize, Debug)]
struct HeadlinePayload {
    text: String,
}

#[derive(Debug, Serialize)]
struct HeadlineResponse {
    recipients: usize,
    failed: Vec<String>,
}

/// Port of the legacy dashboard sentiment broadcast. In the old
/// single-engine model it broadcast `ConfigOverride::News` to
/// the in-process engine. Distributed: iterate the fleet, PATCH
/// a `news` variable into each running deployment. The agent's
/// translator routes that to the engine's NewsRetreat state
/// machine via the same variables hot-reload path as any other
/// per-deployment tune.
async fn post_sentiment_headline(
    State(state): State<AppState>,
    Json(body): Json<HeadlinePayload>,
) -> Result<Json<HeadlineResponse>, (StatusCode, String)> {
    if body.text.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty text".into()));
    }
    let mut patch = serde_json::Map::new();
    patch.insert("news".into(), serde_json::json!(body.text));
    let mut recipients = 0usize;
    let mut failed = Vec::new();
    for view in state.fleet.snapshot() {
        // Skip non-accepted agents — no lease, commands won't
        // be applied. Matches the deploy + ops gate.
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        for dep in &view.deployments {
            if !dep.running {
                continue;
            }
            match state.registry.send(
                &view.agent_id,
                CommandPayload::PatchDeploymentVariables {
                    deployment_id: dep.deployment_id.clone(),
                    patch: patch.clone(),
                },
            ) {
                Ok(()) => recipients += 1,
                Err(e) => failed.push(format!(
                    "{}/{}: {}",
                    view.agent_id, dep.deployment_id, e
                )),
            }
        }
    }
    tracing::info!(
        chars = body.text.len(),
        recipients,
        failed = failed.len(),
        "sentiment headline fan-out complete"
    );
    Ok(Json(HeadlineResponse { recipients, failed }))
}

#[derive(Debug, Serialize)]
struct DetailsResponse {
    agent_id: String,
    deployment_id: String,
    topic: String,
    payload: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

const DETAILS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Wave C1 — fleet-wide reconciliation rollup. Fans out the
/// `reconciliation_snapshot` details topic to every running
/// deployment on every accepted agent, collects results, and
/// returns a flat list. Each row carries the `(agent_id,
/// deployment_id)` tuple so the UI can sort/filter/group.
/// Deployments that haven't run their first reconcile cycle
/// yet reply with a null payload and are skipped.
#[derive(serde::Serialize)]
struct ReconciliationFleetRow {
    agent_id: String,
    deployment_id: String,
    symbol: String,
    cycle: u64,
    last_cycle_ms: i64,
    internal_orders: u32,
    venue_orders: u32,
    ghost_orders: Vec<String>,
    phantom_orders: Vec<String>,
    balance_mismatches: Vec<serde_json::Value>,
    orders_fetch_failed: bool,
    /// `true` when ghosts / phantoms / balance_mismatches is
    /// non-empty. The UI uses this to flag rows needing
    /// attention at the top of the page.
    has_drift: bool,
}

async fn get_reconciliation_fleet(
    State(state): State<AppState>,
) -> Json<Vec<ReconciliationFleetRow>> {
    use mm_control::messages::CommandPayload;
    const PER_DEPLOYMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

    let mut handles = Vec::new();
    for view in state.fleet.snapshot() {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        let agent_id = view.agent_id.clone();
        for dep in &view.deployments {
            if !dep.running {
                continue;
            }
            let request_id = uuid::Uuid::new_v4();
            let (tx, rx) = tokio::sync::oneshot::channel();
            state.registry.pending_details_register(request_id, tx);
            if state
                .registry
                .send(
                    &agent_id,
                    CommandPayload::FetchDeploymentDetails {
                        deployment_id: dep.deployment_id.clone(),
                        topic: "reconciliation_snapshot".into(),
                        request_id,
                        args: serde_json::Map::new(),
                    },
                )
                .is_err()
            {
                state.registry.pending_details_forget(request_id);
                continue;
            }
            handles.push((
                request_id,
                agent_id.clone(),
                dep.deployment_id.clone(),
                tokio::time::timeout(PER_DEPLOYMENT_TIMEOUT, rx),
            ));
        }
    }

    let mut out = Vec::new();
    for (request_id, agent_id, deployment_id, future) in handles {
        match future.await {
            Ok(Ok(reply)) => {
                if reply.payload.is_null() {
                    continue;
                }
                let ghost: Vec<String> = reply
                    .payload
                    .get("ghost_orders")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let phantom: Vec<String> = reply
                    .payload
                    .get("phantom_orders")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let bal_mm: Vec<serde_json::Value> = reply
                    .payload
                    .get("balance_mismatches")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let orders_fetch_failed = reply
                    .payload
                    .get("orders_fetch_failed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let has_drift = !ghost.is_empty()
                    || !phantom.is_empty()
                    || !bal_mm.is_empty()
                    || orders_fetch_failed;
                out.push(ReconciliationFleetRow {
                    agent_id,
                    deployment_id,
                    symbol: reply
                        .payload
                        .get("symbol")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    cycle: reply.payload.get("cycle").and_then(|v| v.as_u64()).unwrap_or(0),
                    last_cycle_ms: reply
                        .payload
                        .get("last_cycle_ms")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    internal_orders: reply
                        .payload
                        .get("internal_orders")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    venue_orders: reply
                        .payload
                        .get("venue_orders")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    ghost_orders: ghost,
                    phantom_orders: phantom,
                    balance_mismatches: bal_mm,
                    orders_fetch_failed,
                    has_drift,
                });
            }
            _ => {
                state.registry.pending_details_forget(request_id);
            }
        }
    }
    // Drift rows first (attention-grabbing), then by agent/symbol.
    out.sort_by(|a, b| {
        b.has_drift
            .cmp(&a.has_drift)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    Json(out)
}

#[derive(serde::Serialize)]
struct AlertFleetRow {
    ts_ms: i64,
    severity: String,
    title: String,
    message: String,
    symbol: Option<String>,
    /// Agents that emitted an equivalent alert within the dedup
    /// window. First entry is the freshest source.
    agents: Vec<String>,
    /// Total occurrences across the fleet in the dedup window.
    count: u64,
}

/// Wave D4 — fleet-wide alert feed with cross-agent dedup.
/// Fans out the `alerts_recent` details topic to every
/// accepted agent, then collapses `(severity, title)` pairs
/// that land within 60s of each other into a single row,
/// carrying a `count` and the distinct `agents` that fired.
#[derive(serde::Serialize)]
struct AuditVerifyRow {
    agent_id: String,
    deployment_id: String,
    symbol: String,
    exists: bool,
    valid: bool,
    rows_checked: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    break_row: Option<u64>,
}

#[derive(serde::Serialize)]
struct AuditVerifyResponse {
    total_deployments: usize,
    valid: usize,
    broken: usize,
    missing: usize,
    rows: Vec<AuditVerifyRow>,
}

/// Fix #2 — fan-out chain verify. Each agent re-computes
/// SHA-256 row by row against the stored `prev_hash`, so the
/// controller doesn't have to worry about re-serialisation
/// changing the bytes. Returns per-deployment status + a
/// rollup (valid / broken / missing-file counts).
async fn post_audit_verify(
    State(state): State<AppState>,
) -> Json<AuditVerifyResponse> {
    use mm_control::messages::CommandPayload;
    const PER_DEPLOYMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    let mut handles = Vec::new();
    for view in state.fleet.snapshot() {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        let agent_id = view.agent_id.clone();
        for dep in &view.deployments {
            if !dep.running {
                continue;
            }
            let request_id = uuid::Uuid::new_v4();
            let (tx, rx) = tokio::sync::oneshot::channel();
            state.registry.pending_details_register(request_id, tx);
            if state
                .registry
                .send(
                    &agent_id,
                    CommandPayload::FetchDeploymentDetails {
                        deployment_id: dep.deployment_id.clone(),
                        topic: "audit_chain_verify".into(),
                        request_id,
                        args: serde_json::Map::new(),
                    },
                )
                .is_err()
            {
                state.registry.pending_details_forget(request_id);
                continue;
            }
            handles.push((
                request_id,
                agent_id.clone(),
                dep.deployment_id.clone(),
                dep.symbol.clone(),
                tokio::time::timeout(PER_DEPLOYMENT_TIMEOUT, rx),
            ));
        }
    }

    let mut rows = Vec::new();
    for (request_id, agent_id, deployment_id, symbol, future) in handles {
        match future.await {
            Ok(Ok(reply)) => {
                let p = &reply.payload;
                let exists = p.get("exists").and_then(|v| v.as_bool()).unwrap_or(false);
                let valid = p.get("valid").and_then(|v| v.as_bool()).unwrap_or(false);
                let rows_checked = p
                    .get("rows_checked")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let last_hash = p
                    .get("last_hash")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let error_kind = p
                    .get("error_kind")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let break_row = p.get("row").and_then(|v| v.as_u64());
                rows.push(AuditVerifyRow {
                    agent_id,
                    deployment_id,
                    symbol,
                    exists,
                    valid,
                    rows_checked,
                    last_hash,
                    error_kind,
                    break_row,
                });
            }
            _ => {
                state.registry.pending_details_forget(request_id);
            }
        }
    }

    let total_deployments = rows.len();
    let valid_count = rows.iter().filter(|r| r.exists && r.valid).count();
    let broken_count = rows.iter().filter(|r| r.exists && !r.valid).count();
    let missing_count = rows.iter().filter(|r| !r.exists).count();
    // Broken first so the UI's top rows are the ones that need
    // attention.
    rows.sort_by(|a, b| {
        let a_bad = a.exists && !a.valid;
        let b_bad = b.exists && !b.valid;
        b_bad.cmp(&a_bad)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    Json(AuditVerifyResponse {
        total_deployments,
        valid: valid_count,
        broken: broken_count,
        missing: missing_count,
        rows,
    })
}

async fn get_alerts_fleet(
    State(state): State<AppState>,
) -> Json<Vec<AlertFleetRow>> {
    use mm_control::messages::CommandPayload;
    const PER_DEPLOYMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
    const DEDUP_WINDOW_MS: i64 = 60_000;

    let mut handles = Vec::new();
    for view in state.fleet.snapshot() {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            continue;
        }
        let agent_id = view.agent_id.clone();
        // Alerts are agent-local (pushed by any of the agent's
        // engines into its shared DashboardState). Use any
        // running deployment as the target; the agent handler
        // ignores the deployment_id for this topic.
        let Some(dep) = view.deployments.iter().find(|d| d.running) else {
            continue;
        };
        let request_id = uuid::Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.registry.pending_details_register(request_id, tx);
        let mut args = serde_json::Map::new();
        args.insert("limit".into(), serde_json::json!(100));
        if state
            .registry
            .send(
                &agent_id,
                CommandPayload::FetchDeploymentDetails {
                    deployment_id: dep.deployment_id.clone(),
                    topic: "alerts_recent".into(),
                    request_id,
                    args,
                },
            )
            .is_err()
        {
            state.registry.pending_details_forget(request_id);
            continue;
        }
        handles.push((
            request_id,
            agent_id,
            tokio::time::timeout(PER_DEPLOYMENT_TIMEOUT, rx),
        ));
    }

    // Collect (agent_id, alert) tuples.
    let mut raw: Vec<(String, serde_json::Value)> = Vec::new();
    for (request_id, agent_id, future) in handles {
        match future.await {
            Ok(Ok(reply)) => {
                if let Some(alerts) = reply
                    .payload
                    .get("alerts")
                    .and_then(|v| v.as_array())
                {
                    for a in alerts {
                        raw.push((agent_id.clone(), a.clone()));
                    }
                }
            }
            _ => {
                state.registry.pending_details_forget(request_id);
            }
        }
    }

    // Dedup: group `(severity, title)` within DEDUP_WINDOW_MS
    // of the newest occurrence. Seen newest-first so the
    // representative row carries the freshest timestamp.
    raw.sort_by(|a, b| {
        let at = a.1.get("ts_ms").and_then(|v| v.as_i64()).unwrap_or(0);
        let bt = b.1.get("ts_ms").and_then(|v| v.as_i64()).unwrap_or(0);
        bt.cmp(&at)
    });

    let mut out: Vec<AlertFleetRow> = Vec::new();
    'outer: for (agent_id, alert) in raw {
        let ts_ms = alert.get("ts_ms").and_then(|v| v.as_i64()).unwrap_or(0);
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
        for row in out.iter_mut() {
            if row.severity == severity
                && row.title == title
                && (row.ts_ms - ts_ms).abs() <= DEDUP_WINDOW_MS
            {
                row.count += 1;
                if !row.agents.contains(&agent_id) {
                    row.agents.push(agent_id.clone());
                }
                continue 'outer;
            }
        }
        let message = alert
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let symbol = alert
            .get("symbol")
            .and_then(|v| v.as_str())
            .map(String::from);
        out.push(AlertFleetRow {
            ts_ms,
            severity,
            title,
            message,
            symbol,
            agents: vec![agent_id],
            count: 1,
        });
    }

    Json(out)
}

async fn get_deployment_details(
    State(state): State<AppState>,
    Path((agent_id, deployment_id, topic)): Path<(String, String, String)>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<DetailsResponse>, (StatusCode, String)> {
    // Validate target agent is Accepted before sending a
    // round-trip command — keeps the correlation map from
    // gaining entries that can never be resolved.
    if let Some(view) = state.fleet.get(&agent_id) {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            return Err((
                StatusCode::PRECONDITION_FAILED,
                format!(
                    "agent '{agent_id}' is in approval state '{}' — details fetch requires 'accepted'",
                    view.approval_state
                ),
            ));
        }
    }

    let request_id = uuid::Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.registry.pending_details_register(request_id, tx);

    // Query params pass through as topic args. Numeric values
    // are coerced to JSON numbers when they parse — lets
    // audit-range queries (`?from_ms=...&until_ms=...&limit=200`)
    // and other topic-specific filters ride through without a
    // richer typed API. Non-numeric strings stay as strings.
    let mut args = serde_json::Map::new();
    for (k, v) in query {
        let parsed = if let Ok(n) = v.parse::<i64>() {
            serde_json::json!(n)
        } else if let Ok(f) = v.parse::<f64>() {
            serde_json::json!(f)
        } else {
            serde_json::json!(v)
        };
        args.insert(k, parsed);
    }

    if let Err(e) = state.registry.send(
        &agent_id,
        CommandPayload::FetchDeploymentDetails {
            deployment_id: deployment_id.clone(),
            topic: topic.clone(),
            request_id,
            args,
        },
    ) {
        // Command-send failed — clean up the pending entry so
        // it doesn't linger.
        state.registry.pending_details_forget(request_id);
        return Err(match e {
            RegistryError::NotFound => (
                StatusCode::NOT_FOUND,
                format!("agent {agent_id} is not currently connected"),
            ),
            RegistryError::AgentGone => (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("agent {agent_id} session is shutting down"),
            ),
        });
    }

    match tokio::time::timeout(DETAILS_TIMEOUT, rx).await {
        Ok(Ok(reply)) => Ok(Json(DetailsResponse {
            agent_id,
            deployment_id: reply.deployment_id,
            topic: reply.topic,
            payload: reply.payload,
            error: reply.error,
        })),
        Ok(Err(_)) => {
            // Sender dropped (session terminated mid-wait).
            state.registry.pending_details_forget(request_id);
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                format!("agent {agent_id} session dropped while waiting for details"),
            ))
        }
        Err(_) => {
            // Timeout — reclaim the slot so a late reply doesn't
            // leak, then surface 504 so the UI can retry or fall
            // back to stale telemetry.
            state.registry.pending_details_forget(request_id);
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "no reply for details '{topic}' on deployment '{deployment_id}' within {}s",
                    DETAILS_TIMEOUT.as_secs()
                ),
            ))
        }
    }
}

#[derive(serde::Deserialize)]
struct ReplayRequestBody {
    candidate_graph: serde_json::Value,
    #[serde(default)]
    ticks: Option<u32>,
}

/// M5-GOBS — POST sibling of `get_deployment_details`. Exists
/// because the replay request body carries a full candidate
/// strategy graph JSON — too bulky to smuggle through
/// `?args=` on the GET path. Otherwise the plumbing is
/// identical: fan-out to the target agent with topic
/// `graph_replay`, wait on the oneshot, return the reply.
async fn post_deployment_replay(
    State(state): State<AppState>,
    Path((agent_id, deployment_id)): Path<(String, String)>,
    axum::Json(body): axum::Json<ReplayRequestBody>,
) -> Result<Json<DetailsResponse>, (StatusCode, String)> {
    if let Some(view) = state.fleet.get(&agent_id) {
        if !view.approval_state.is_empty() && view.approval_state != "accepted" {
            return Err((
                StatusCode::PRECONDITION_FAILED,
                format!(
                    "agent '{agent_id}' is in approval state '{}' — replay requires 'accepted'",
                    view.approval_state
                ),
            ));
        }
    }

    let request_id = uuid::Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.registry.pending_details_register(request_id, tx);

    let mut args = serde_json::Map::new();
    args.insert("candidate_graph".to_string(), body.candidate_graph);
    if let Some(t) = body.ticks {
        args.insert("ticks".to_string(), serde_json::json!(t));
    }

    if let Err(e) = state.registry.send(
        &agent_id,
        CommandPayload::FetchDeploymentDetails {
            deployment_id: deployment_id.clone(),
            topic: "graph_replay".to_string(),
            request_id,
            args,
        },
    ) {
        state.registry.pending_details_forget(request_id);
        return Err(match e {
            RegistryError::NotFound => (
                StatusCode::NOT_FOUND,
                format!("agent {agent_id} is not currently connected"),
            ),
            RegistryError::AgentGone => (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("agent {agent_id} session is shutting down"),
            ),
        });
    }

    match tokio::time::timeout(DETAILS_TIMEOUT, rx).await {
        Ok(Ok(reply)) => Ok(Json(DetailsResponse {
            agent_id,
            deployment_id: reply.deployment_id,
            topic: reply.topic,
            payload: reply.payload,
            error: reply.error,
        })),
        Ok(Err(_)) => {
            state.registry.pending_details_forget(request_id);
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                format!("agent {agent_id} session dropped while waiting for replay"),
            ))
        }
        Err(_) => {
            state.registry.pending_details_forget(request_id);
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "no reply for replay on deployment '{deployment_id}' within {}s",
                    DETAILS_TIMEOUT.as_secs()
                ),
            ))
        }
    }
}

async fn get_agent_credentials(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Json<Vec<CredentialDescriptor>> {
    let rows = match &state.vault {
        Some(store) => store.exchange_descriptors_for_agent(&agent_id),
        None => Vec::new(),
    };
    Json(rows)
}

async fn get_templates() -> Json<Vec<crate::templates::TemplateRow>> {
    Json(crate::templates::catalog())
}

async fn get_tunables_schema() -> Json<Vec<TunableField>> {
    Json(crate::tunables::schema())
}

async fn get_tunables(State(state): State<AppState>) -> Json<Tunables> {
    match &state.tunables {
        Some(t) => Json(t.current()),
        None => Json(Tunables::default()),
    }
}

async fn put_tunables(
    State(state): State<AppState>,
    Json(body): Json<Tunables>,
) -> Result<Json<Tunables>, (StatusCode, String)> {
    let Some(store) = &state.tunables else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "controller has no tunables store configured".into(),
        ));
    };
    store
        .replace(body)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// Bind and serve until the process exits. Binary uses this;
/// tests use [`router`] + their own listener.
pub async fn run_http_server(
    fleet: FleetState,
    registry: AgentRegistry,
    addr: SocketAddr,
) -> anyhow::Result<()> {
    run_http_server_full(fleet, registry, None, None, None, addr).await
}

pub async fn run_http_server_full(
    fleet: FleetState,
    registry: AgentRegistry,
    vault: Option<VaultStore>,
    approvals: Option<ApprovalStore>,
    tunables: Option<TunablesStore>,
    addr: SocketAddr,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %addr, "controller HTTP server listening");
    axum::serve(listener, router_full(fleet, registry, vault, approvals, tunables)).await?;
    Ok(())
}

impl IntoResponse for RegistryError {
    fn into_response(self) -> axum::response::Response {
        match self {
            RegistryError::NotFound => {
                (StatusCode::NOT_FOUND, self.to_string()).into_response()
            }
            RegistryError::AgentGone => {
                (StatusCode::SERVICE_UNAVAILABLE, self.to_string()).into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests;
