use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, Method};
use axum::middleware;
use axum::routing::{get, post};
use axum::{Json, Router};
use prometheus::TextEncoder;
use std::net::SocketAddr;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{info, warn};

use crate::auth::{
    admin_middleware, auth_middleware, auth_status_handler, bootstrap_handler,
    change_password_handler, client_signup_handler, create_invite_handler,
    create_password_reset_handler, internal_view_middleware, login_handler, logout_handler,
    me_handler, password_reset_handler, tenant_scope_middleware, totp_disable_handler,
    totp_enroll_handler, totp_verify_handler, ApiUser, AuthState, Role,
};
use crate::rate_limit::{rate_limit_middleware, RateLimiter};
use crate::state::{DashboardState, SymbolState};
use crate::websocket::{ws_handler, WsBroadcast};

/// Start the dashboard HTTP + WebSocket server with authentication.
///
/// Layer cake per route group:
///   - public (`/health`, `/api/auth/login`): no auth; login is
///     IP-rate-limited to blunt brute-force.
///   - k8s probes (`/ready`, `/startup`): no auth — must be
///     callable by the orchestrator.
///   - protected operator/client API (`/api/status`, `/api/v1/*`):
///     requires valid Bearer token or `X-API-Key` header.
///   - metrics (`/metrics`): auth + `Admin|Operator` role (Viewer
///     cannot see inventory/PnL gauges).
///   - admin (`/api/admin/*`): auth + `Admin` role only + per-user
///     rate limit.
///   - WebSocket (`/ws`): token passed as `?token=` query param,
///     verified inside `ws_handler` (browsers cannot set headers on
///     the WS upgrade request).
/// Build the full dashboard Router without binding. Exposed so
/// the `mm-server` controller can merge these routes into its
/// own HTTP surface + serve the Svelte frontend on the same
/// port. The thin `start` wrapper below preserves the
/// bind-and-serve behaviour for any caller that still wants it.
pub fn build_app(
    state: DashboardState,
    ws_broadcast: Arc<WsBroadcast>,
    auth_state: AuthState,
) -> Router {
    crate::metrics::init();
    build_router_inner(state, ws_broadcast, auth_state)
}

pub async fn start(
    state: DashboardState,
    ws_broadcast: Arc<WsBroadcast>,
    auth_state: AuthState,
    port: u16,
) -> anyhow::Result<()> {
    let app = build_app(state, ws_broadcast, auth_state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%addr, "dashboard server starting");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn build_router_inner(
    state: DashboardState,
    ws_broadcast: Arc<WsBroadcast>,
    auth_state: AuthState,
) -> Router {
    // Rate limiters — admin gets a stricter budget; login is
    // throttled by source IP to slow credential-stuffing.
    let admin_rl = RateLimiter::new(300); // 300 req/min per user
    let login_rl = RateLimiter::new(20); // 20/min per source IP
                                         // Sprint 5c — strategy-graph deploy is far more expensive than
                                         // most admin reads (parse + validate + persist + broadcast to
                                         // every engine) and a deploy burst is a plausible misuse / DoS
                                         // path, so it gets its own much tighter budget. Reads (graphs
                                         // list, templates, history) stay on the generic admin limiter.
    let graph_deploy_rl = RateLimiter::new(10);

    // Public routes — no auth. Login is IP-rate-limited.
    let public = Router::new()
        .route("/health", get(health))
        .with_state(auth_state.clone());

    let login = Router::new()
        .route("/api/auth/login", post(login_handler))
        .route("/api/auth/bootstrap", post(bootstrap_handler))
        .route("/api/auth/status", get(auth_status_handler))
        // Wave E4 — public client-signup endpoint. Verifies a
        // signed invite token + creates the ClientReader user.
        // Rate-limited same as login because brute-forcing
        // random tokens is the attack surface.
        .route("/api/auth/client-signup", post(client_signup_handler))
        // Wave H1 — public password-reset consumer. Takes a
        // signed reset_token (minted by an admin) + a new
        // password. Same rate limit as login since random
        // reset_id brute-force is the attack surface.
        .route("/api/auth/password-reset", post(password_reset_handler))
        .route_layer(middleware::from_fn_with_state(
            login_rl,
            rate_limit_middleware,
        ))
        .with_state(auth_state.clone());

    // K8s probes (no auth, need DashboardState).
    let probes = Router::new()
        .route("/ready", get(readiness))
        .route("/startup", get(readiness))
        .with_state(state.clone());

    // Protected API routes — any authenticated role.
    let protected_api = Router::new()
        .route("/api/status", get(get_status))
        .route("/api/v1/venues/status", get(venues_status))
        // `/api/v1/inventory/venues` endpoints removed —
        // per-deployment venue legs are served via the
        // `venue_inventory` details topic. Frontend fans out
        // across fleet and merges.
        .route("/api/v1/clients/loss-state", get(clients_loss_state))
        // LEGACY-1 (2026-04-21) — `/api/v1/surveillance/scores`
        // and `/api/v1/active-graphs` removed. Both read the
        // controller-local DashboardState which is never
        // populated in distributed mode; frontend reads the
        // fleet-aware `/api/v1/surveillance/fleet` + the
        // per-deployment `active_graph` field on the fleet row.
        // `/api/v1/decisions/recent` removed — DecisionLedger
        // lives inside each engine. DecisionsLedger.svelte now
        // fans out per-deployment via the details endpoint
        // (topic `decisions_recent`). Engine mirrors its
        // ledger.recent(200) into the process-global details
        // store on every publish tick so the agent can serve
        // the reply without reaching into engine state.
        .route("/api/v1/plans/active", get(active_plans))
        .route("/api/v1/otr/tiered", get(otr_tiered))
        .route("/api/v1/portfolio/cross_venue", get(portfolio_cross_venue))
        .route("/api/v1/venues/latency_p95", get(venues_latency_p95))
        .route("/api/v1/venues/funding_state", get(venues_funding_state))
        .route(
            "/api/v1/history/inventory/per_leg",
            get(per_leg_inventory_history),
        )
        .route("/api/v1/basis", get(basis_monitor))
        .route("/api/v1/venues/book_state", get(venues_book_state))
        .route("/api/v1/clients", get(list_clients_public))
        // `/api/v1/sor/decisions/recent` removed — SOR
        // decisions live in each engine. SorDecisions.svelte
        // fans out per-deployment via the `sor_decisions_recent`
        // details topic.
        // `/api/v1/atomic-bundles/inflight`, `/api/v1/funding-arb/pairs`,
        // `/api/v1/rebalance/recommendations` removed in the
        // 2026-04 stabilization pass — all three read engine-
        // local state that doesn't exist in distributed mode.
        // Frontend now fans out per-deployment via the details
        // protocol (topics: `atomic_bundles_inflight`,
        // `funding_arb_pairs`, `rebalance_recommendations`).
        .route("/api/v1/rebalance/execute", post(rebalance_execute))
        .route("/api/v1/rebalance/log", get(rebalance_log))
        // `/api/v1/adverse-selection` removed — per-deployment
        // `adverse_selection` details topic serves distributed
        // clients (agent reads from its own shared DashboardState
        // populated by the running engine).
        .route("/api/v1/calibration/status", get(calibration_status))
        // /api/v1/active-graphs and /api/v1/manipulation/scores
        // removed as part of LEGACY-1. Frontend reads
        // /api/v1/surveillance/fleet (ManipulationScores) and
        // per-deployment `active_graph` on the fleet row.
        // `/api/v1/onchain/scores` removed — served via the
        // `onchain_scores` details topic per deployment.
        .merge(crate::client_api::client_routes())
        .merge(crate::client_portal::client_portal_routes())
        // Wave E2 — tenant scope enforcement runs AFTER auth
        // middleware (TokenClaims already in extensions). Rejects
        // cross-tenant access and blocks ClientReader role from
        // hitting non-client endpoints like /api/v1/fleet or
        // /api/v1/pnl. Admin/operator tokens pass through.
        .route_layer(middleware::from_fn(tenant_scope_middleware))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

    // Logout — auth-protected so we know who is walking out the
    // door; the endpoint itself only emits an audit event since
    // tokens are stateless HMAC.
    let logout = Router::new()
        .route("/api/auth/logout", post(logout_handler))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(auth_state.clone());

    // Authenticated profile self-service: current-user info,
    // password change, 2FA enrollment / verify / disable. All
    // gated by the same token middleware as the rest of the
    // protected API surface.
    let profile = Router::new()
        .route("/api/auth/me", get(me_handler))
        .route("/api/auth/password", post(change_password_handler))
        .route("/api/auth/totp/enroll", post(totp_enroll_handler))
        .route("/api/auth/totp/verify", post(totp_verify_handler))
        .route("/api/auth/totp/disable", post(totp_disable_handler))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(auth_state.clone());

    // Prometheus metrics — auth + internal-view role gate.
    let metrics_route = Router::new()
        .route("/metrics", get(prometheus_metrics))
        .route_layer(middleware::from_fn(internal_view_middleware))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ));

    // Admin config routes — hot-reload, symbol control, webhooks,
    // alerts, loans, optimization, clients. Admin role ONLY plus
    // user-scoped rate limit.
    let admin_config = Router::new()
        // Per-symbol kill / pause / emulator / DCA endpoints were
        // removed in the 2026-04 stabilization pass. In the
        // distributed model a symbol isn't unique across agents,
        // so the canonical ops surface is per-deployment:
        //   POST /api/v1/agents/{agent_id}/deployments/{deployment_id}/ops/{op}
        // served by the controller crate. Frontend uses the
        // DeploymentDrilldown panel to target a specific
        // deployment; there's no operator path that still needs
        // symbol-based routing.
        .route(
            "/api/v1/ops/client-reset/{client_id}",
            post(ops_client_reset),
        )
        // 23-UX-6 — venue-scoped kill switch control. Operators
        // quench one venue without disturbing sibling venues
        // on the same engine. Read endpoint is admin-only too
        // because elevated venue state is PII-adjacent
        // (positions exposure).
        .route(
            "/api/v1/ops/venue-kill/{venue}",
            post(ops_set_venue_kill_level),
        )
        .route("/api/v1/kill/venues", get(list_venue_kill_levels))
        // `/api/admin/config/*` routes removed post controller/agent
        // split — they broadcast to a server-embedded engine that
        // no longer exists. Per-deployment config overrides land in
        // Wave 2 as PATCH /api/v1/agents/{id}/deployments/{dep_id}/variables.
        .route("/api/admin/webhooks", get(admin_list_webhooks))
        .route("/api/admin/webhooks", post(admin_add_webhook))
        .route("/api/admin/alerts", get(admin_list_alerts))
        .route("/api/admin/alerts", post(admin_add_alert))
        .route("/api/admin/alerts/check", get(admin_check_alerts))
        .route("/api/admin/symbols", get(admin_list_symbols))
        .route("/api/admin/loans", axum::routing::post(admin_create_loan))
        .route("/api/admin/loans", get(admin_list_loans))
        // Hyperopt review loop — parameter auto-calibration.
        // Worker records a JSONL of market events, sweeps params,
        // suggests the best trial; operator reviews + applies.
        // In the distributed 2026-04 model the worker observes
        // DashboardState that no local engine populates, and
        // `apply` routes via a dead ConfigOverride channel —
        // surfaced in the response as `applied: 0 / skipped: [..]`
        // so the operator sees the gap rather than silent failure.
        // Full distributed hyperopt (worker observes agent
        // telemetry + applies via per-deployment variables PATCH)
        // is tracked as a follow-up; endpoints + UI stay wired
        // so the catalog and manual trigger flow are reachable.
        .route("/api/admin/optimize/status", get(admin_optimize_status))
        .route("/api/admin/optimize/results", get(admin_optimize_results))
        .route("/api/admin/optimize/trigger", post(admin_optimize_trigger))
        .route("/api/admin/optimize/pending", get(admin_optimize_pending))
        .route("/api/admin/optimize/apply", post(admin_optimize_apply))
        .route("/api/admin/optimize/discard", post(admin_optimize_discard))
        // `/api/admin/sentiment/headline` lives on the controller
        // crate (needs FleetState for fan-out) — see
        // `post_sentiment_headline` there.
        .merge(crate::admin_clients::admin_client_routes())
        .with_state(state.clone());

    // Strategy-graph deploy endpoint — same auth + admin middleware
    // as the rest of the admin surface but a tighter rate limit.
    // Split into its own router so the stricter limiter layers
    // cleanly without double-counting against `admin_rl`.
    let admin_graph_deploy = Router::new()
        // Saves the graph + returns a content hash. No longer
        // broadcasts — distributed engines receive the graph via
        // the per-deployment `ops/graph-swap` endpoint on the
        // controller. Frontend calls the save, then fires
        // graph-swap at a specific (agent, deployment) target.
        .route("/api/admin/strategy/graph", post(admin_save_strategy_graph))
        // Per-node config patch — edits a single node's config
        // in the stored graph catalog. Save-only in distributed
        // mode (no in-process engine to hot-swap); operator
        // redeploys via `ops/graph-swap` to push the patched
        // graph to a specific deployment.
        .route(
            "/api/admin/strategy/graph/{name}/nodes/{node_id}/config",
            axum::routing::patch(admin_patch_strategy_node_config),
        )
        .route_layer(middleware::from_fn_with_state(
            graph_deploy_rl,
            rate_limit_middleware,
        ))
        .route_layer(middleware::from_fn(admin_middleware))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

    // Admin user-management — also admin-only.
    let admin_users = Router::new()
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/users", post(create_user))
        // Wave E4 — admin mints a client invite URL.
        .route(
            "/api/admin/clients/{id}/invite",
            post(create_invite_handler),
        )
        // Wave H1 — admin mints a password-reset URL for a
        // target user. Returned URL is one-shot, 1h-expiring,
        // signed. Admin delivers out-of-band.
        .route(
            "/api/admin/users/{id}/reset-password",
            post(create_password_reset_handler),
        )
        .with_state(auth_state.clone());

    // Wave H4 — admin auth-audit readback. Reads login /
    // logout / password-reset rows from the shared MiCA audit
    // trail so operators can spot credential-stuffing
    // patterns and account for recovery flows. Needs
    // DashboardState so it can resolve the audit file path.
    let admin_auth_audit = Router::new()
        .route("/api/admin/auth/audit", get(admin_auth_audit_handler))
        .with_state(state.clone());

    let admin = admin_config
        .merge(admin_users)
        .merge(admin_auth_audit)
        .route_layer(middleware::from_fn_with_state(
            admin_rl,
            rate_limit_middleware,
        ))
        .route_layer(middleware::from_fn(admin_middleware))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ));

    // WebSocket — auth via query param, verified inside handler.
    let ws_routes = Router::new().route("/ws", get(ws_handler)).with_state((
        state.clone(),
        ws_broadcast,
        auth_state,
    ));

    let cors = build_cors_layer();

    Router::new()
        .merge(public)
        .merge(login)
        .merge(logout)
        .merge(profile)
        .merge(probes)
        .merge(protected_api)
        .merge(metrics_route)
        .merge(admin)
        .merge(admin_graph_deploy)
        .merge(ws_routes)
        .layer(cors)
}

/// Build a CORS layer from `MM_DASHBOARD_CORS_ORIGINS` — a
/// comma-separated list of allowed origins (e.g.,
/// `https://dash.example.com,https://admin.example.com`). Unset
/// defaults to localhost dev origins only; `*` enables wildcard
/// (dangerous — only for closed networks). Allowing credentials
/// together with `*` is illegal per the CORS spec and we refuse
/// to configure it that way.
fn build_cors_layer() -> CorsLayer {
    let raw = std::env::var("MM_DASHBOARD_CORS_ORIGINS").unwrap_or_default();
    let origins: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let allow = if raw.trim() == "*" {
        warn!(
            "MM_DASHBOARD_CORS_ORIGINS=* enables wildcard CORS — \
             do NOT use this outside closed/dev networks"
        );
        AllowOrigin::any()
    } else if origins.is_empty() {
        // Dev default: Vite + common local ports. Production
        // deployments MUST set MM_DASHBOARD_CORS_ORIGINS.
        let defaults = [
            "http://localhost:5173",
            "http://127.0.0.1:5173",
            "http://localhost:3000",
            "http://127.0.0.1:3000",
        ];
        info!(
            defaults = ?defaults,
            "MM_DASHBOARD_CORS_ORIGINS not set — using localhost dev defaults"
        );
        AllowOrigin::list(
            defaults
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect::<Vec<_>>(),
        )
    } else {
        info!(allowed = ?origins, "CORS allowed origins");
        AllowOrigin::list(
            origins
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect::<Vec<_>>(),
        )
    };

    CorsLayer::new()
        .allow_origin(allow)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-api-key"),
        ])
        .allow_credentials(false)
}

async fn health() -> &'static str {
    "ok"
}

/// K8s readiness probe — 200 when at least one symbol has
/// received market data (mid_price > 0). Returns 503 during
/// startup before the first book snapshot lands.
async fn readiness(State(state): State<DashboardState>) -> axum::http::StatusCode {
    let symbols = state.get_all();
    if symbols
        .iter()
        .any(|s| s.mid_price > rust_decimal::Decimal::ZERO)
    {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn get_status(State(state): State<DashboardState>) -> Json<Vec<SymbolState>> {
    Json(state.get_all())
}

/// Per-venue inventory drilldown for a single symbol. Returns the
// Per-venue inventory readback handlers removed — distributed
// deployments serve these via the `venue_inventory` details
// topic (per deployment). The frontend panel does the cross-
// symbol join.

async fn prometheus_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder
        .encode_to_string(&metric_families)
        .unwrap_or_default()
}

// ── Sprint 4 companion — surveillance scores JSON endpoint ─
//
// `GET /api/v1/surveillance/scores` — frontend-friendly view
// into the per-pattern detector state. Reads directly from the
// Prometheus registry (single source of truth), so values here
// never drift from `/metrics`. Shape:
//
//     {
//         "patterns": {
//             "spoofing": {
//                 "BTCUSDT": { "score": 0.73, "alerts_total": 4 },
//                 "ETHUSDT": { "score": 0.12, "alerts_total": 0 }
//             },
//             ...
//         }
//     }

// LEGACY-1 (2026-04-21) — surveillance_scores / active_graphs /
// manipulation_scores handlers removed. They read the
// controller-local DashboardState + Prometheus registry, both of
// which stay empty in distributed mode (engine gauges live on
// agents). Frontend now fans out via `/api/v1/surveillance/fleet`
// and reads per-deployment `active_graph` directly off the
// fleet row.

// INT-1 decision-ledger readback removed — replaced by the
// per-deployment `decisions_recent` details topic (see agent's
// FetchDeploymentDetails handler). DecisionsLedger.svelte fans
// out across fleet and merges client-side.

#[derive(serde::Serialize)]
struct ActivePlansResponse {
    plans: Vec<crate::state::PlanSnapshot>,
}

#[derive(serde::Serialize, Default)]
struct TieredOtrRow {
    tob_cumulative: f64,
    tob_rolling_5min: f64,
    top20_cumulative: f64,
    top20_rolling_5min: f64,
}

#[derive(serde::Serialize)]
struct TieredOtrResponse {
    /// Per-symbol 4-way OTR breakdown. Values mirror the
    /// `mm_otr_tiered{symbol,tier,window}` gauge that engines
    /// publish every minute.
    symbols: std::collections::BTreeMap<String, TieredOtrRow>,
}

#[derive(serde::Serialize)]
struct CrossVenueLeg {
    venue: String,
    symbol: String,
    inventory: rust_decimal::Decimal,
    /// Mark price in the leg's native quote currency. `null`
    /// while the engine's book is still warming up.
    #[serde(skip_serializing_if = "Option::is_none")]
    mark_price: Option<rust_decimal::Decimal>,
    /// `inventory × mark` in the leg's native quote currency.
    #[serde(skip_serializing_if = "Option::is_none")]
    notional_quote: Option<rust_decimal::Decimal>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Serialize)]
struct CrossVenueAsset {
    /// Inferred base asset (e.g. `BTC` for `BTCUSDT` /
    /// `BTC-USDT` / `BTCUSDC`). Matches the aggregator's own
    /// bucketing so the `net_delta` figure lines up with
    /// `Portfolio.CrossVenueNetDelta` evaluated on the same
    /// asset string.
    base: String,
    net_delta: rust_decimal::Decimal,
    /// Sum of each leg's `notional_quote`. Mixes quote
    /// currencies if the same base trades against different
    /// quotes across venues — the frontend renders this raw
    /// and leaves FX to the caller.
    net_notional_quote: rust_decimal::Decimal,
    legs: Vec<CrossVenueLeg>,
}

#[derive(serde::Serialize)]
struct CrossVenuePortfolioResponse {
    assets: Vec<CrossVenueAsset>,
}

async fn portfolio_cross_venue(
    State(state): State<DashboardState>,
) -> Json<CrossVenuePortfolioResponse> {
    let assets = state
        .cross_venue_by_asset()
        .into_iter()
        .map(|agg| CrossVenueAsset {
            base: agg.base,
            net_delta: agg.net_delta,
            net_notional_quote: agg.net_notional_quote,
            legs: agg
                .legs
                .into_iter()
                .map(|l| CrossVenueLeg {
                    venue: l.venue,
                    symbol: l.symbol,
                    inventory: l.inventory,
                    mark_price: l.mark_price,
                    notional_quote: l.notional_quote,
                    updated_at: l.updated_at,
                })
                .collect(),
        })
        .collect();
    Json(CrossVenuePortfolioResponse { assets })
}

/// OBS-2 — per-venue p95 book-update latency derived from the
/// `mm_book_update_latency_ms` histogram. Reads Prometheus
/// histogram buckets, aggregates across symbols for each venue,
/// and returns one row per venue with an approximated p95 using
/// the histogram's native bucket boundaries (no external
/// quantile estimator).
#[derive(serde::Serialize)]
struct VenueLatencyRow {
    venue: String,
    /// Approximated p95 in milliseconds. `null` when the venue
    /// has no samples yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    p95_ms: Option<f64>,
    sample_count: u64,
}

#[derive(serde::Serialize)]
struct VenueLatencyResponse {
    venues: Vec<VenueLatencyRow>,
}

async fn venues_latency_p95() -> Json<VenueLatencyResponse> {
    use prometheus::proto::MetricType;
    // bucket upper bound → cumulative count per venue.
    let mut per_venue: std::collections::BTreeMap<String, Vec<(f64, u64)>> =
        std::collections::BTreeMap::new();
    let mut total_counts: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    let families = prometheus::gather();
    for fam in &families {
        if fam.get_name() != "mm_book_update_latency_ms"
            || fam.get_field_type() != MetricType::HISTOGRAM
        {
            continue;
        }
        for m in fam.get_metric() {
            let venue = m
                .get_label()
                .iter()
                .find(|lbl| lbl.get_name() == "venue")
                .map(|lbl| lbl.get_value().to_string());
            let Some(venue) = venue else { continue };
            let h = m.get_histogram();
            let buckets = per_venue.entry(venue.clone()).or_default();
            // Histograms share bucket boundaries across all
            // metrics, so we accumulate cumulative_count
            // per boundary.
            for b in h.get_bucket() {
                let ub = b.get_upper_bound();
                let count = b.get_cumulative_count();
                if let Some(existing) = buckets
                    .iter_mut()
                    .find(|(boundary, _)| (boundary - ub).abs() < f64::EPSILON)
                {
                    existing.1 += count;
                } else {
                    buckets.push((ub, count));
                }
            }
            *total_counts.entry(venue).or_insert(0) += h.get_sample_count();
        }
    }

    let venues: Vec<VenueLatencyRow> = per_venue
        .into_iter()
        .map(|(venue, mut buckets)| {
            // Sort by upper bound ascending for a monotone cdf.
            buckets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let total = total_counts.get(&venue).copied().unwrap_or(0);
            let p95_ms = if total == 0 {
                None
            } else {
                let target = ((total as f64) * 0.95).ceil() as u64;
                // First bucket whose cumulative count ≥ target
                // is the p95 upper-bound approximation.
                buckets
                    .iter()
                    .find(|(_, cum)| *cum >= target)
                    .map(|(ub, _)| *ub)
            };
            VenueLatencyRow {
                venue,
                p95_ms,
                sample_count: total,
            }
        })
        .collect();
    Json(VenueLatencyResponse { venues })
}

// S1.3 SOR routing decision log — handler removed. See
// details-protocol `sor_decisions_recent` topic on the agent.

/// S2.2 — inflight atomic bundle snapshot for the monitor
/// panel. Returns every bundle currently tracked on the
// Atomic-bundles / rebalance-recommendations snapshot handlers
// removed — distributed clients fetch per-deployment via the
// details protocol (topics: `atomic_bundles_inflight`,
// `rebalance_recommendations`). See agent's FetchDetails
// handler in `crates/agent/src/lib.rs`.

/// S6.4 — rebalance execute request. Operator-approved transfer.
/// Intra-venue wallet transfers dispatch through the venue's
/// `internal_transfer` immediately; cross-venue are logged as
/// `accepted` but not dispatched (V1 deliberately excludes
/// on-chain withdrawals pending deposit-address whitelisting).
#[derive(Debug, serde::Deserialize)]
struct RebalanceExecuteRequest {
    from_venue: String,
    to_venue: String,
    asset: String,
    qty: rust_decimal::Decimal,
    #[serde(default)]
    from_wallet: Option<String>,
    #[serde(default)]
    to_wallet: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct RebalanceExecuteResponse {
    transfer_id: String,
    status: String,
    venue_tx_id: Option<String>,
    error: Option<String>,
}

async fn rebalance_execute(
    State(state): State<DashboardState>,
    axum::Extension(claims): axum::Extension<crate::auth::TokenClaims>,
    axum::Json(req): axum::Json<RebalanceExecuteRequest>,
) -> (axum::http::StatusCode, Json<RebalanceExecuteResponse>) {
    use axum::http::StatusCode;
    use mm_persistence::transfer_log::{TransferRecord, TransferStatus};

    let Some(log) = state.transfer_log() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(RebalanceExecuteResponse {
                transfer_id: String::new(),
                status: "unconfigured".into(),
                venue_tx_id: None,
                error: Some("transfer log not registered".into()),
            }),
        );
    };

    let now = chrono::Utc::now();
    let transfer_id = uuid::Uuid::new_v4().to_string();

    // Kill-switch gate — any level above Normal blocks an
    // operator transfer. Prevents a toxic-flow widen / pause from
    // being dismissed by an accidental Execute click during the
    // same incident.
    let kl = state.max_kill_level();
    if kl > 0 {
        let rec = TransferRecord {
            transfer_id: transfer_id.clone(),
            ts: now,
            from_venue: req.from_venue.to_lowercase(),
            to_venue: req.to_venue.to_lowercase(),
            asset: req.asset.clone(),
            qty: req.qty,
            from_wallet: req.from_wallet.clone(),
            to_wallet: req.to_wallet.clone(),
            reason: req.reason.clone(),
            operator: claims.user_id.clone(),
            status: TransferStatus::RejectedKillSwitch,
            venue_tx_id: None,
            error: Some(format!("kill_level={kl}")),
        };
        if let Err(e) = log.append(&rec) {
            warn!(error = %e, "transfer log append failed; returning error anyway");
        }
        return (
            StatusCode::FORBIDDEN,
            Json(RebalanceExecuteResponse {
                transfer_id,
                status: "rejected_kill_switch".into(),
                venue_tx_id: None,
                error: Some(format!("kill_level={kl}")),
            }),
        );
    }

    // Intra-venue: dispatch through the venue connector.
    if req.from_venue.eq_ignore_ascii_case(&req.to_venue) {
        let Some(conn) = state.venue_connector(&req.from_venue) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(RebalanceExecuteResponse {
                    transfer_id,
                    status: "no_connector".into(),
                    venue_tx_id: None,
                    error: Some(format!(
                        "no connector registered for venue {}",
                        req.from_venue
                    )),
                }),
            );
        };
        let from = req.from_wallet.as_deref().unwrap_or("SPOT");
        let to = req.to_wallet.as_deref().unwrap_or("SPOT");
        let outcome = conn.internal_transfer(&req.asset, req.qty, from, to).await;
        let (status_enum, tx_id, err_text, http) = match &outcome {
            Ok(tx) => (
                TransferStatus::Executed,
                Some(tx.clone()),
                None,
                StatusCode::OK,
            ),
            Err(e) => (
                TransferStatus::Failed,
                None,
                Some(e.to_string()),
                StatusCode::BAD_GATEWAY,
            ),
        };
        let rec = TransferRecord {
            transfer_id: transfer_id.clone(),
            ts: now,
            from_venue: req.from_venue.to_lowercase(),
            to_venue: req.to_venue.to_lowercase(),
            asset: req.asset.clone(),
            qty: req.qty,
            from_wallet: Some(from.to_string()),
            to_wallet: Some(to.to_string()),
            reason: req.reason.clone(),
            operator: claims.user_id.clone(),
            status: status_enum,
            venue_tx_id: tx_id.clone(),
            error: err_text.clone(),
        };
        if let Err(e) = log.append(&rec) {
            warn!(error = %e, "transfer log append failed after dispatch");
        }
        return (
            http,
            Json(RebalanceExecuteResponse {
                transfer_id,
                status: match status_enum {
                    TransferStatus::Executed => "executed".into(),
                    TransferStatus::Failed => "failed".into(),
                    _ => "unknown".into(),
                },
                venue_tx_id: tx_id,
                error: err_text,
            }),
        );
    }

    // Cross-venue (V1): log intent but don't dispatch. On-chain
    // withdraws need deposit-address whitelisting that isn't yet
    // wired — the operator owns the transfer via the venue UI
    // and the audit row here documents the decision.
    let rec = TransferRecord {
        transfer_id: transfer_id.clone(),
        ts: now,
        from_venue: req.from_venue.to_lowercase(),
        to_venue: req.to_venue.to_lowercase(),
        asset: req.asset.clone(),
        qty: req.qty,
        from_wallet: req.from_wallet.clone(),
        to_wallet: req.to_wallet.clone(),
        reason: req.reason.clone(),
        operator: claims.user_id.clone(),
        status: TransferStatus::Accepted,
        venue_tx_id: None,
        error: None,
    };
    if let Err(e) = log.append(&rec) {
        warn!(error = %e, "transfer log append failed");
    }
    (
        StatusCode::ACCEPTED,
        Json(RebalanceExecuteResponse {
            transfer_id,
            status: "accepted".into(),
            venue_tx_id: None,
            error: Some(
                "cross-venue transfers require manual venue-side action; decision logged".into(),
            ),
        }),
    )
}

/// S6.4 — tail of the transfer log for the history panel.
#[derive(Debug, serde::Serialize)]
struct RebalanceLogResponse {
    records: Vec<mm_persistence::transfer_log::TransferRecord>,
}

async fn rebalance_log(State(state): State<DashboardState>) -> Json<RebalanceLogResponse> {
    let records = state
        .transfer_log()
        .and_then(|log| mm_persistence::transfer_log::read_all(log.path()).ok())
        .unwrap_or_default();
    Json(RebalanceLogResponse { records })
}

// S5.2 funding-arb pairs snapshot handler removed — distributed
// clients fetch per-deployment via the `funding_arb_pairs`
// details topic.

// S5.3 adverse-selection view replaced by the per-deployment
// `adverse_selection` details topic. Frontend panel fans out.

/// S5.4 — live-calibration status endpoint. Returns one row per
/// symbol whose active strategy publishes calibration
/// (`GlftStrategy` today). Stateless-strategy symbols don't
/// appear; the panel renders "no rows" as expected.
#[derive(Debug, serde::Serialize)]
struct CalibrationStatusResponse {
    rows: Vec<crate::state::CalibrationSnapshot>,
}

async fn calibration_status(
    State(state): State<DashboardState>,
) -> Json<CalibrationStatusResponse> {
    // CalibrationStatus fleet fan-out (2026-04-22) — in
    // distributed mode the controller's local
    // `calibration_snapshots` is never populated (calibration
    // lives on agents). Fan out via the `client_metrics` topic,
    // collect the `calibration` field each agent embeds, and
    // merge newest-first. Falls back to controller-local state
    // when no fetcher is installed (unit tests).
    let mut rows: Vec<crate::state::CalibrationSnapshot> = Vec::new();
    if let Some(fetcher) = state.fleet_client_metrics_fetcher() {
        let metrics = fetcher(None).await;
        for row in metrics {
            if let Some(cal) = row.get("calibration") {
                if !cal.is_null() {
                    if let Ok(snap) =
                        serde_json::from_value::<crate::state::CalibrationSnapshot>(cal.clone())
                    {
                        rows.push(snap);
                    }
                }
            }
        }
    }
    if rows.is_empty() {
        rows = state.calibration_snapshots();
    }
    rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Json(CalibrationStatusResponse { rows })
}

/// S6.1 — which graph is driving each symbol. Compact projection
/// of `SymbolState.active_graph` for a dedicated widget so the
/// frontend doesn't need to fetch the full symbol snapshot to
/// show "Graph: X (hash abc…)". Symbols without a deployed graph
/// are skipped — empty response means the whole system is on
/// legacy strategies.
// LEGACY-1 (2026-04-21) — active_graphs_snapshot +
// manipulation_scores handlers removed. See top-of-file note;
// frontend reads fleet endpoints instead.

async fn otr_tiered() -> Json<TieredOtrResponse> {
    use prometheus::proto::MetricType;
    let mut out: std::collections::BTreeMap<String, TieredOtrRow> =
        std::collections::BTreeMap::new();
    let families = prometheus::gather();
    for fam in &families {
        if fam.get_name() != "mm_otr_tiered" || fam.get_field_type() != MetricType::GAUGE {
            continue;
        }
        for m in fam.get_metric() {
            let mut symbol = None;
            let mut tier = None;
            let mut window = None;
            for lbl in m.get_label() {
                match lbl.get_name() {
                    "symbol" => symbol = Some(lbl.get_value().to_string()),
                    "tier" => tier = Some(lbl.get_value().to_string()),
                    "window" => window = Some(lbl.get_value().to_string()),
                    _ => {}
                }
            }
            let (Some(symbol), Some(tier), Some(window)) = (symbol, tier, window) else {
                continue;
            };
            let v = m.get_gauge().get_value();
            let row = out.entry(symbol).or_default();
            match (tier.as_str(), window.as_str()) {
                ("tob", "cumulative") => row.tob_cumulative = v,
                ("tob", "rolling_5min") => row.tob_rolling_5min = v,
                ("top20", "cumulative") => row.top20_cumulative = v,
                ("top20", "rolling_5min") => row.top20_rolling_5min = v,
                _ => {}
            }
        }
    }
    Json(TieredOtrResponse { symbols: out })
}

async fn active_plans(State(state): State<DashboardState>) -> Json<ActivePlansResponse> {
    Json(ActivePlansResponse {
        plans: state.active_plans_all(),
    })
}

// `label_pair` helper removed with surveillance_scores handler.

// --- Admin: User Management ---

// Wave H4 — admin auth-audit readback.
//
// Returns login / logout / password-reset rows from the
// shared MiCA audit trail so operators can review who
// signed in from where, spot credential-stuffing patterns,
// and correlate password-reset activity with account
// recoveries. Admin-only, rate-limited with the rest of the
// admin surface. Uses the plain local file reader — events
// live on the controller's filesystem already (agents
// stream their audit events to the controller via the
// pump). No fleet fan-out needed.
#[derive(serde::Deserialize, Default)]
struct AuthAuditQuery {
    /// Inclusive upper bound (default: now). Milliseconds since epoch.
    until_ms: Option<i64>,
    /// Inclusive lower bound (default: 24h before `until_ms`).
    from_ms: Option<i64>,
    /// Max rows returned (default 200, hard cap 5000).
    limit: Option<usize>,
    /// Filter by substring in detail (e.g. user_id=u-abc, ip=1.2.3.4).
    contains: Option<String>,
}

async fn admin_auth_audit_handler(
    State(state): State<DashboardState>,
    Query(q): Query<AuthAuditQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (axum::http::StatusCode, String)> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let until_ms = q.until_ms.unwrap_or(now_ms);
    let from_ms = q.from_ms.unwrap_or_else(|| until_ms - 24 * 60 * 60 * 1000);
    if from_ms > until_ms {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "from_ms must be <= until_ms".into(),
        ));
    }
    let limit = q.limit.unwrap_or(200).min(5000);
    let contains = q.contains.as_deref().map(|s| s.to_string());

    let Some(path) = state.audit_log_path() else {
        return Ok(Json(Vec::new()));
    };
    let from_dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(from_ms).ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        "from_ms out of range".into(),
    ))?;
    let until_dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(until_ms).ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        "until_ms out of range".into(),
    ))?;

    use mm_risk::audit::AuditEventType as T;
    let types = [
        T::LoginSucceeded,
        T::LoginFailed,
        T::LogoutSucceeded,
        T::PasswordResetIssued,
        T::PasswordResetCompleted,
    ];
    let events =
        mm_risk::audit_reader::read_audit_filtered(&path, from_dt, until_dt, Some(&types), None)
            .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Filter against both the event_type tag and the detail
    // string so operators can narrow by "password_reset" /
    // "login_failed" AND by user_id / ip / role within the same
    // query. Event-type value is rendered via serde as the
    // snake_case tag so a simple Debug-based substring match
    // works on both the enum name and the canonical tag.
    let filtered: Vec<serde_json::Value> = events
        .into_iter()
        .rev() // newest first
        .filter(|ev| {
            contains.as_deref().is_none_or(|needle| {
                let tag = serde_json::to_value(&ev.event_type)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                tag.contains(needle) || ev.detail.as_deref().is_some_and(|d| d.contains(needle))
            })
        })
        .take(limit)
        .filter_map(|ev| serde_json::to_value(ev).ok())
        .collect();
    Ok(Json(filtered))
}

async fn list_users(State(auth): State<AuthState>) -> Json<Vec<UserInfo>> {
    let users = auth.list_users();
    Json(
        users
            .into_iter()
            .map(|u| UserInfo {
                id: u.id,
                name: u.name,
                role: u.role,
                allowed_symbols: u.allowed_symbols,
                // Don't expose full API key — only last 4 chars.
                api_key_hint: format!("...{}", &u.api_key[u.api_key.len().saturating_sub(4)..]),
            })
            .collect(),
    )
}

async fn create_user(
    State(auth): State<AuthState>,
    Json(req): Json<CreateUserRequest>,
) -> Json<CreateUserResponse> {
    let api_key = generate_api_key();
    let role = match req.role.as_str() {
        "admin" => Role::Admin,
        "operator" => Role::Operator,
        _ => Role::Viewer,
    };

    let user = ApiUser {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name.clone(),
        role,
        api_key: api_key.clone(),
        password_hash: None,
        totp_secret: None,
        totp_pending: None,
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        allowed_symbols: if req.allowed_symbols.is_empty() {
            None
        } else {
            Some(req.allowed_symbols)
        },
        client_id: None, // set via client onboarding API
    };

    info!(name = %req.name, role = ?role, "user created");
    auth.add_user(user.clone());

    Json(CreateUserResponse {
        id: user.id,
        name: user.name,
        role: user.role,
        api_key, // Show full key ONCE on creation.
    })
}

/// Generate a 64-char hex API key backed by the OS CSPRNG (256
/// bits of entropy). `getrandom` pulls from `/dev/urandom`,
/// `getentropy`, or `BCryptGenRandom` depending on the platform —
/// each is cryptographically strong and non-blocking after boot.
fn generate_api_key() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS CSPRNG must be available");
    hex::encode(bytes)
}

#[derive(serde::Serialize)]
struct UserInfo {
    id: String,
    name: String,
    role: Role,
    allowed_symbols: Option<Vec<String>>,
    api_key_hint: String,
}

#[derive(serde::Deserialize)]
struct CreateUserRequest {
    name: String,
    role: String,
    #[serde(default)]
    allowed_symbols: Vec<String>,
}

#[derive(serde::Serialize)]
struct CreateUserResponse {
    id: String,
    name: String,
    role: Role,
    api_key: String,
}

// Per-symbol ops (kill / pause / resume / emulator / DCA) were
// removed in the 2026-04 stabilization pass. Canonical path:
// POST /api/v1/agents/{a}/deployments/{d}/ops/{op} served by
// the controller. Nothing in this module dispatches per-symbol
// ConfigOverride any more.

#[derive(serde::Serialize)]
struct WebhookListResponse {
    url_count: usize,
    events_sent: u64,
    events_failed: u64,
}

async fn admin_list_webhooks(State(state): State<DashboardState>) -> Json<WebhookListResponse> {
    let wh = state.webhook_dispatcher();
    Json(match wh {
        Some(w) => WebhookListResponse {
            url_count: w.url_count(),
            events_sent: w.events_sent(),
            events_failed: w.events_failed(),
        },
        None => WebhookListResponse {
            url_count: 0,
            events_sent: 0,
            events_failed: 0,
        },
    })
}

#[derive(serde::Deserialize)]
struct AddWebhookRequest {
    url: String,
}

async fn admin_add_webhook(
    State(state): State<DashboardState>,
    Json(req): Json<AddWebhookRequest>,
) -> Json<WebhookListResponse> {
    if let Some(wh) = state.webhook_dispatcher() {
        wh.add_url(req.url);
        Json(WebhookListResponse {
            url_count: wh.url_count(),
            events_sent: wh.events_sent(),
            events_failed: wh.events_failed(),
        })
    } else {
        Json(WebhookListResponse {
            url_count: 0,
            events_sent: 0,
            events_failed: 0,
        })
    }
}

async fn admin_list_alerts(
    State(state): State<DashboardState>,
) -> Json<Vec<crate::state::AlertRule>> {
    Json(state.get_alert_rules())
}

async fn admin_add_alert(
    State(state): State<DashboardState>,
    Json(rule): Json<crate::state::AlertRule>,
) -> Json<Vec<crate::state::AlertRule>> {
    state.add_alert_rule(rule);
    Json(state.get_alert_rules())
}

#[derive(serde::Serialize)]
struct AlertCheckResponse {
    triggered: Vec<(String, String)>,
}

async fn admin_check_alerts(State(state): State<DashboardState>) -> Json<AlertCheckResponse> {
    Json(AlertCheckResponse {
        triggered: state.check_alert_rules(),
    })
}

/// List all active symbols with their current state summary.
async fn admin_list_symbols(State(state): State<DashboardState>) -> Json<Vec<serde_json::Value>> {
    let symbols = state.get_all();
    let config_syms = state.config_symbols();
    Json(
        symbols
            .iter()
            .map(|s| {
                serde_json::json!({
                    "symbol": s.symbol,
                    "mid_price": s.mid_price.to_string(),
                    "spread_bps": s.spread_bps.to_string(),
                    "inventory": s.inventory.to_string(),
                    "kill_level": s.kill_level,
                    "live_orders": s.live_orders,
                    "total_fills": s.total_fills,
                    "pnl": s.pnl.total.to_string(),
                    "uptime_pct": s.sla_uptime_pct.to_string(),
                    "has_config_channel": config_syms.contains(&s.symbol),
                    "regime": s.regime,
                })
            })
            .collect(),
    )
}

// ── Loan admin endpoints (Epic 2) ───────────────────────────

#[derive(serde::Deserialize)]
struct CreateLoanRequest {
    symbol: String,
    #[serde(default)]
    client_id: Option<String>,
    total_qty: rust_decimal::Decimal,
    #[serde(default)]
    cost_basis_per_token: rust_decimal::Decimal,
    #[serde(default)]
    annual_rate_pct: rust_decimal::Decimal,
    #[serde(default)]
    counterparty: String,
    start_date: String,
    end_date: String,
    #[serde(default)]
    installments: Vec<CreateInstallment>,
}

#[derive(serde::Deserialize)]
struct CreateInstallment {
    due_date: String,
    qty: rust_decimal::Decimal,
}

async fn admin_create_loan(
    State(state): State<DashboardState>,
    axum::Json(req): axum::Json<CreateLoanRequest>,
) -> axum::Json<serde_json::Value> {
    let id = uuid::Uuid::new_v4().to_string();
    let start = chrono::NaiveDate::parse_from_str(&req.start_date, "%Y-%m-%d")
        .unwrap_or_else(|_| chrono::Utc::now().date_naive());
    let end = chrono::NaiveDate::parse_from_str(&req.end_date, "%Y-%m-%d")
        .unwrap_or_else(|_| chrono::Utc::now().date_naive());

    let installments: Vec<mm_persistence::loan::ReturnInstallment> = req
        .installments
        .iter()
        .map(|i| {
            let due = chrono::NaiveDate::parse_from_str(&i.due_date, "%Y-%m-%d").unwrap_or(end);
            mm_persistence::loan::ReturnInstallment {
                due_date: due,
                qty: i.qty,
                status: mm_persistence::loan::InstallmentStatus::Pending,
                completed_at: None,
            }
        })
        .collect();

    let agreement = mm_persistence::loan::LoanAgreement {
        id: id.clone(),
        symbol: req.symbol.clone(),
        client_id: req.client_id,
        terms: mm_persistence::loan::LoanTerms {
            total_qty: req.total_qty,
            cost_basis_per_token: req.cost_basis_per_token,
            annual_rate_pct: req.annual_rate_pct,
            option_strike: None,
            option_expiry: None,
            start_date: start,
            end_date: end,
            counterparty: req.counterparty,
        },
        schedule: mm_persistence::loan::ReturnSchedule { installments },
        status: mm_persistence::loan::LoanStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    state.set_loan_agreement(agreement);

    axum::Json(serde_json::json!({
        "id": id,
        "symbol": req.symbol,
        "status": "created"
    }))
}

async fn admin_list_loans(
    State(state): State<DashboardState>,
) -> axum::Json<Vec<mm_persistence::loan::LoanAgreement>> {
    axum::Json(state.get_all_loan_agreements())
}

// ── Optimization admin endpoints (Epic 6 + 33) ──────────────
//
// Hyperopt review loop. Worker lives as a tokio task in
// `mm-server` / `mm-dashboard`; it reads `HyperoptTrigger`
// payloads off a channel registered at startup, runs the
// optimiser against a pre-recorded JSONL, and stages the
// best trial as `PendingCalibration` in DashboardState.
//
// The `apply` path translates pending entries into
// `ConfigOverride::*` variants and dispatches through
// `state.send_config_override`, which is a no-op in the
// distributed deployment (engines live on agents, the
// process-local ConfigOverride channel map is empty). The
// response surfaces this as `applied: 0, skipped: [...]` so
// the operator sees the gap rather than silent failure.
//
// Distributed port (follow-up): rewire `apply` to iterate
// the fleet and PATCH `gamma` / `order_size` / etc. variables
// into the target deployment(s); rewire `trigger` to kick a
// worker that observes agent telemetry + audit-log recordings.
// Endpoints + frontend card stay wired meanwhile so the
// catalog + history flow remain reachable.

async fn admin_optimize_status(
    State(state): State<DashboardState>,
) -> axum::Json<Option<crate::state::OptimizationState>> {
    axum::Json(state.get_optimization_state())
}

async fn admin_optimize_results(
    State(state): State<DashboardState>,
) -> axum::Json<serde_json::Value> {
    let opt = state.get_optimization_state();
    match opt {
        Some(o) => axum::Json(serde_json::json!({
            "status": o.status,
            "trials_completed": o.trials_completed,
            "trials_total": o.trials_total,
            "best_params": o.best_params,
            "best_loss": o.best_loss,
        })),
        None => axum::Json(serde_json::json!({"status": "no optimization run"})),
    }
}

async fn admin_optimize_trigger(
    State(state): State<DashboardState>,
    axum::Json(trigger): axum::Json<crate::state::HyperoptTrigger>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    if !state.send_hyperopt_trigger(trigger.clone()) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "hyperopt worker not registered — server startup may still be racing",
            })),
        )
            .into_response();
    }
    axum::Json(serde_json::json!({
        "status": "queued",
        "symbol": trigger.symbol,
        "trials": trigger.num_trials,
        "recording_path": trigger.recording_path,
    }))
    .into_response()
}

async fn admin_optimize_pending(
    State(state): State<DashboardState>,
) -> axum::Json<Vec<crate::state::PendingCalibration>> {
    axum::Json(state.all_calibrations())
}

#[derive(serde::Deserialize)]
struct SymbolBody {
    symbol: String,
}

async fn admin_optimize_apply(
    State(state): State<DashboardState>,
    axum::Json(body): axum::Json<SymbolBody>,
) -> axum::Json<serde_json::Value> {
    use crate::state::ConfigOverride;
    let Some(pending) = state.get_calibration(&body.symbol) else {
        return axum::Json(serde_json::json!({
            "status": "none_pending",
            "symbol": body.symbol,
        }));
    };
    let mut applied = 0u32;
    let mut skipped: Vec<String> = Vec::new();
    for (key, value) in &pending.suggested {
        let ovr = match key.as_str() {
            "gamma" => ConfigOverride::Gamma(*value),
            "min_spread_bps" => ConfigOverride::MinSpreadBps(*value),
            "order_size" => ConfigOverride::OrderSize(*value),
            "max_distance_bps" => ConfigOverride::MaxDistanceBps(*value),
            "num_levels" => {
                use rust_decimal::prelude::ToPrimitive;
                let n = value.to_u64().unwrap_or(0) as usize;
                ConfigOverride::NumLevels(n)
            }
            "max_inventory" => ConfigOverride::MaxInventory(*value),
            _ => {
                skipped.push(key.clone());
                continue;
            }
        };
        if state.send_config_override(&body.symbol, ovr) {
            applied += 1;
        } else {
            skipped.push(format!(
                "{key} (no local engine — distributed apply not yet wired)"
            ));
        }
    }
    state.clear_calibration(&body.symbol);
    axum::Json(serde_json::json!({
        "status": "applied",
        "symbol": body.symbol,
        "applied": applied,
        "skipped": skipped,
    }))
}

async fn admin_optimize_discard(
    State(state): State<DashboardState>,
    axum::Json(body): axum::Json<SymbolBody>,
) -> axum::Json<serde_json::Value> {
    let cleared = state.clear_calibration(&body.symbol).is_some();
    axum::Json(serde_json::json!({
        "status": if cleared { "discarded" } else { "none_pending" },
        "symbol": body.symbol,
    }))
}

// ── Epic H — strategy graph save ───────────────────────────
//
// `POST /api/admin/strategy/graph` body: the full graph JSON.
// Flow:
//   1. parse + compile (runs full validation via `Evaluator::build`).
//   2. persist via `GraphStore::save` — atomic tmp+rename + deploy
//      log append + SHA-256 hash.
//   3. respond with `{ hash, name }`.
//
// No broadcast — distributed engines live on agents. After a
// save, the frontend fires a per-deployment graph-swap op
// against a specific (agent, deployment) target. That's the
// correct shape: "strategy" means a single deployment, not a
// fleet-wide scope string.

/// `?rollback_from=<prev_hash>` query parameter distinguishes a
/// rollback-to-a-known-historical-version from a fresh forward
/// deploy. The server can derive the fact itself (prev live hash
/// != new hash) but rollback is *intent* — the UI flags it so the
/// audit row tells regulators it was a deliberate reversion rather
/// than a rewrite that happened to match an old hash.
#[derive(serde::Deserialize)]
struct DeployQuery {
    #[serde(default)]
    rollback_from: Option<String>,
    /// UI-6 — operator acknowledgement of a restricted-node
    /// deploy. When the env gate is ON (`MM_ALLOW_RESTRICTED`)
    /// AND the graph references pentest kinds, the deploy must
    /// carry `restricted_ack=yes-pentest-mode` so a routine
    /// deploy can't silently promote a pentest template into
    /// the engine. Without the ack we return `412 Precondition
    /// Required` with the offender list and the operator UI
    /// opens a confirmation modal.
    #[serde(default)]
    restricted_ack: Option<String>,
}

/// Sprint 5b — derive the set of venue names a graph may
/// reference. One entry per configured `ExchangeType`, produced
/// exactly the way the engine stringifies its own venue
/// (`Debug::fmt(&exchange_type).to_lowercase()`), so validator
/// strings match what the engine compares against at tick time.
fn known_venue_names(cfg: &mm_common::config::AppConfig) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    out.push(format!("{:?}", cfg.exchange.exchange_type).to_lowercase());
    for v in &cfg.sor_extra_venues {
        out.push(format!("{:?}", v.exchange.exchange_type).to_lowercase());
    }
    out.sort();
    out.dedup();
    out
}

async fn admin_save_strategy_graph(
    State(state): State<DashboardState>,
    Query(q): Query<DeployQuery>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    // Operator identity from the auth layer's request header
    // (set by the auth middleware). Falls back to "unknown" if
    // the header is absent — the admin router enforces admin
    // role via a layer so the endpoint shouldn't reach this point
    // anonymously, but we don't want a panic on header drift.
    let operator = headers
        .get("X-MM-User")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    // Parse + validate.
    let graph = match mm_strategy_graph::Graph::from_json(&body) {
        Ok(g) => g,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "error": "parse",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    // Compile runs full DAG + type + cycle + sink checks.
    if let Err(e) = mm_strategy_graph::Evaluator::build(&graph) {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": "validate",
                "detail": e.to_string(),
            })),
        )
            .into_response();
    }

    // Sprint 5b — venue existence check. Walks every venue-typed
    // config field and refuses the deploy if the string doesn't
    // match a configured venue on the server. Skipped when the
    // app config isn't attached (test / minimal-mode servers).
    if let Some(cfg) = state.app_config() {
        let known = known_venue_names(&cfg);
        if let Err(e) = graph.validate_venues(known.iter().map(String::as_str)) {
            if let Some(audit) = state.audit_log() {
                audit.strategy_graph_deploy_rejected(
                    &graph.name,
                    &format!("unknown venue: {e}"),
                    &operator,
                );
            }
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "error": "unknown_venue",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    }

    // Restricted-kind gate. `MM_ALLOW_RESTRICTED=yes-pentest-mode`
    // opts in deploying graphs that reference pentest-only nodes;
    // any other value (including the default absent case) refuses
    // the deploy and emits an audit row so regulators can confirm
    // the gate actually fired. Intentional literal value check:
    // a shell-dotfile `MM_ALLOW_RESTRICTED=1` left from a prior
    // session must NOT silently unlock production. MUST match the
    // graph evaluator's `graph::allow_restricted_env` byte-for-byte
    // — Sprint 14 R8 caught these drifting apart.
    let allow_restricted = std::env::var("MM_ALLOW_RESTRICTED")
        .map(|v| v == "yes-pentest-mode")
        .unwrap_or(false);
    let offenders: Vec<String> = graph
        .nodes
        .iter()
        .filter(|n| {
            mm_strategy_graph::catalog::shape(&n.kind)
                .map(|s| s.restricted)
                .unwrap_or(false)
        })
        .map(|n| n.kind.clone())
        .collect();
    if !allow_restricted && !offenders.is_empty() {
        let reason = format!(
            "restricted nodes without MM_ALLOW_RESTRICTED=yes-pentest-mode: {}",
            offenders.join(",")
        );
        if let Some(audit) = state.audit_log() {
            audit.strategy_graph_deploy_rejected(&graph.name, &reason, &operator);
        }
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "restricted",
                "detail": reason,
            })),
        )
            .into_response();
    }
    // UI-6 — even with the env gate ON, operator must ack each
    // restricted deploy. Returns `412 Precondition Required`
    // with the offender list so the UI knows to open the ack
    // modal and retry with the explicit token.
    if allow_restricted && !offenders.is_empty() {
        let acked = q.restricted_ack.as_deref() == Some("yes-pentest-mode");
        if !acked {
            if let Some(audit) = state.audit_log() {
                audit.strategy_graph_deploy_rejected(
                    &graph.name,
                    &format!(
                        "restricted deploy awaiting operator ack: {}",
                        offenders.join(",")
                    ),
                    &operator,
                );
            }
            return (
                StatusCode::PRECONDITION_REQUIRED,
                axum::Json(serde_json::json!({
                    "error": "restricted_ack_required",
                    "detail": "operator must explicitly acknowledge the restricted nodes before deploy",
                    "restricted_nodes": offenders,
                })),
            )
                .into_response();
        }
    }

    let Some(store) = state.strategy_graph_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "strategy graphs not configured",
            })),
        )
            .into_response();
    };

    let hash = match store.save(&graph, Some(&operator)) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({
                    "error": "persist",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };

    // Scope key string — compact, matches the form the engine
    // once used when routing ConfigOverride messages. Still
    // audit-logged because the catalog entry is scoped even
    // though actual engine dispatch is now per-deployment.
    let scope_key = match &graph.scope {
        mm_strategy_graph::Scope::Symbol(s) => format!("Symbol({s})"),
        mm_strategy_graph::Scope::AssetClass(c) => format!("AssetClass({c})"),
        mm_strategy_graph::Scope::Client(c) => format!("Client({c})"),
        mm_strategy_graph::Scope::Global => "Global".to_string(),
    };

    // Audit trail — rollback precedes the save row so a grepper
    // reading top-to-bottom sees intent before result.
    // `recipients` is left at 0 because the save itself doesn't
    // dispatch; the per-deployment graph-swap op records its
    // own audit event at dispatch time.
    if let Some(audit) = state.audit_log() {
        if let Some(from_hash) = q.rollback_from.as_deref() {
            audit.strategy_graph_rolled_back(&graph.name, from_hash, &hash, &operator);
        }
        audit.strategy_graph_deployed(&graph.name, &hash, &scope_key, &operator, 0);
        if !offenders.is_empty() {
            audit.strategy_graph_restricted_deploy_acked(&graph.name, &hash, &operator, &offenders);
        }
    }

    tracing::info!(
        name = %graph.name,
        hash = %hash,
        operator = %operator,
        rollback_from = ?q.rollback_from,
        "strategy graph saved — operator still needs to dispatch to a deployment via /ops/graph-swap"
    );

    (
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "status": "saved",
            "hash": hash,
            "name": graph.name,
        })),
    )
        .into_response()
}

/// `PATCH /api/admin/strategy/graph/{name}/nodes/{node_id}/config`.
/// Edits one node's config in the stored graph catalog.
/// Save-only in distributed mode — the historical broadcast
/// through ConfigOverride is gone; after patching, the
/// operator redeploys via `POST .../deployments/.../ops/graph-swap`
/// to push the patched graph onto a specific deployment.
/// Restricted-kind + env gate still apply because a patch can
/// introduce pentest kinds that weren't in the original graph.
async fn admin_patch_strategy_node_config(
    State(state): State<DashboardState>,
    Path((name, node_id_str)): Path<(String, String)>,
    Query(q): Query<DeployQuery>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let operator = headers
        .get("X-MM-User")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let new_config: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "error": "parse",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    let target_node_id = match uuid::Uuid::parse_str(&node_id_str) {
        Ok(u) => mm_strategy_graph::NodeId(u),
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "error": "bad_node_id",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    let Some(store) = state.strategy_graph_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "strategy graphs not configured",
            })),
        )
            .into_response();
    };
    let mut graph = match store.load(&name) {
        Ok(g) => g,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({
                    "error": "graph_not_found",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    let from_hash = graph.content_hash();
    let Some(node) = graph.nodes.iter_mut().find(|n| n.id == target_node_id) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": "node_not_found",
                "detail": format!("node {node_id_str} not in graph {name}"),
            })),
        )
            .into_response();
    };
    node.config = new_config;
    if let Err(e) = mm_strategy_graph::Evaluator::build(&graph) {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": "validate",
                "detail": e.to_string(),
            })),
        )
            .into_response();
    }
    // Restricted-kind gate — mirrors the full-deploy path.
    let allow_restricted = std::env::var("MM_ALLOW_RESTRICTED")
        .map(|v| v == "yes-pentest-mode")
        .unwrap_or(false);
    let offenders: Vec<String> = graph
        .nodes
        .iter()
        .filter(|n| {
            mm_strategy_graph::catalog::shape(&n.kind)
                .map(|s| s.restricted)
                .unwrap_or(false)
        })
        .map(|n| n.kind.clone())
        .collect();
    if !allow_restricted && !offenders.is_empty() {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "restricted",
                "detail": format!("patch introduced restricted nodes: {}", offenders.join(",")),
            })),
        )
            .into_response();
    }
    if allow_restricted && !offenders.is_empty() {
        let acked = q.restricted_ack.as_deref() == Some("yes-pentest-mode");
        if !acked {
            return (
                StatusCode::PRECONDITION_REQUIRED,
                axum::Json(serde_json::json!({
                    "error": "restricted_ack_required",
                    "detail": "operator must explicitly acknowledge the restricted nodes before patch",
                    "restricted_nodes": offenders,
                })),
            )
                .into_response();
        }
    }
    let to_hash = match store.save(&graph, Some(&operator)) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({
                    "error": "persist",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    if let Some(audit) = state.audit_log() {
        audit.strategy_graph_node_patched(
            &graph.name,
            &from_hash,
            &to_hash,
            &node_id_str,
            &operator,
        );
    }
    tracing::info!(
        name = %graph.name,
        from_hash = %from_hash,
        to_hash = %to_hash,
        node_id = %node_id_str,
        operator = %operator,
        "strategy graph node patched (save-only — redeploy via ops/graph-swap to push)"
    );
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "status": "patched",
            "from_hash": from_hash,
            "to_hash": to_hash,
            "node_id": node_id_str,
            "name": graph.name,
        })),
    )
        .into_response()
}

/// 23-UX-6 — set the kill-switch level for a single venue. Body
/// carries `{level: u8}` on the 0..=5 scale (same as the
/// per-symbol kill). `level = 0` clears the entry. The engine
/// consumes this via `DashboardState::venue_kill_level` on every
/// connector dispatch; level ≥ 3 short-circuits placement +
/// triggers a cancel-all on that venue's connector.
#[derive(serde::Deserialize)]
struct VenueKillRequest {
    level: u8,
    #[serde(default)]
    reason: String,
}

#[derive(serde::Serialize)]
struct VenueKillResponse {
    venue: String,
    level: u8,
    applied: bool,
}

async fn ops_set_venue_kill_level(
    State(state): State<DashboardState>,
    Path(venue): Path<String>,
    Json(req): Json<VenueKillRequest>,
) -> Json<VenueKillResponse> {
    if req.level > 5 {
        return Json(VenueKillResponse {
            venue,
            level: req.level,
            applied: false,
        });
    }
    state.set_venue_kill_level(&venue, req.level);
    if let Some(audit) = state.audit_log() {
        audit.risk_event(
            "",
            mm_risk::audit::AuditEventType::KillSwitchEscalated,
            &format!(
                "venue_kill venue={venue} level={} reason={}",
                req.level,
                if req.reason.is_empty() {
                    "n/a"
                } else {
                    &req.reason
                }
            ),
        );
    }
    Json(VenueKillResponse {
        venue,
        level: req.level,
        applied: true,
    })
}

async fn list_venue_kill_levels(State(state): State<DashboardState>) -> Json<serde_json::Value> {
    let levels = state.all_venue_kill_levels();
    // Sort by venue name so the response is deterministic.
    let mut rows: Vec<(String, u8)> = levels.into_iter().collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Json(serde_json::json!({
        "venues": rows
            .into_iter()
            .map(|(v, lvl)| serde_json::json!({"venue": v, "level": lvl}))
            .collect::<Vec<_>>(),
    }))
}

// ── Connectivity matrix ─────────────────────────────────────
//
// GET /api/v1/venues/status — returns a snapshot of every
// tracked venue/symbol connection: WS state, last update age,
// accumulated reconnects, sequence-gap count. Operators see this
// as a grid in the dashboard's connectivity panel.

#[derive(serde::Serialize)]
struct VenueStatusRow {
    symbol: String,
    /// `true` when the engine has a live mid price on this
    /// symbol — proxy for "WS feed healthy enough to quote".
    has_data: bool,
    /// Current mid price (empty string when has_data is false).
    mid_price: String,
    kill_level: u8,
    /// Human-readable kill-switch label derived from kill_level
    /// (NORMAL / WIDEN_SPREADS / STOP_NEW_ORDERS / CANCEL_ALL /
    /// FLATTEN_ALL / DISCONNECT) so the UI does not have to
    /// hardcode the mapping.
    kill_label: &'static str,
    live_orders: u32,
    total_fills: u64,
    /// SLA uptime % over the current SLA window.
    sla_uptime_pct: String,
    /// True when kill level >= StopNewOrders — the engine is
    /// currently refusing to place new orders.
    quoting_halted: bool,
    /// S2.4 — latest margin ratio (MM / equity) reported by
    /// the venue. `null` on spot or pre-first-poll perp.
    #[serde(skip_serializing_if = "Option::is_none")]
    margin_ratio: Option<String>,
    /// S2.4 — highest ADL quantile across this symbol's
    /// positions (0..=4). `null` on venues that don't
    /// publish ADL (HyperLiquid) or before the first snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    adl_quantile: Option<u8>,
}

fn kill_label(level: u8) -> &'static str {
    match level {
        0 => "NORMAL",
        1 => "WIDEN_SPREADS",
        2 => "STOP_NEW_ORDERS",
        3 => "CANCEL_ALL",
        4 => "FLATTEN_ALL",
        5 => "DISCONNECT",
        _ => "UNKNOWN",
    }
}

/// 23-UX-4 — one snapshot row per (venue, symbol, product)
/// with a funding-rate entry on the data bus. The frontend's
/// funding-countdown panel renders these so operators see the
/// next 8h settlement approaching per perp leg + current rate.
#[derive(serde::Serialize)]
struct FundingStateRow {
    venue: String,
    symbol: String,
    product: String,
    rate: Option<rust_decimal::Decimal>,
    next_funding_ts: Option<i64>,
}

/// 23-UX-2 — per-leg inventory time-series endpoint. Optional
/// `?base=BTC` filters to legs whose inferred base asset matches.
#[derive(serde::Deserialize)]
struct PerLegHistoryQuery {
    base: Option<String>,
}

async fn per_leg_inventory_history(
    State(state): State<DashboardState>,
    axum::extract::Query(q): axum::extract::Query<PerLegHistoryQuery>,
) -> Json<Vec<crate::state::PerLegInventoryHistory>> {
    Json(state.per_leg_inventory_timeseries(q.base.as_deref()))
}

/// 23-UX-5 — cross-venue / spot-vs-perp basis snapshot. For
/// every base asset with ≥ 2 L1 entries, compute each pairwise
/// basis in bps against the reference (the cheapest spot leg
/// if present, else the lowest-mid leg).
#[derive(serde::Serialize)]
struct BasisRow {
    base_asset: String,
    reference_venue: String,
    reference_symbol: String,
    reference_product: String,
    reference_mid: rust_decimal::Decimal,
    legs: Vec<BasisLeg>,
}

#[derive(serde::Serialize)]
struct BasisLeg {
    venue: String,
    symbol: String,
    product: String,
    mid: rust_decimal::Decimal,
    /// Basis vs reference, in bps. `(mid - ref_mid) / ref_mid *
    /// 10_000`. Sign preserved — positive = premium.
    basis_bps: rust_decimal::Decimal,
}

/// 23-UX-12 — lightweight read-only client listing for the
/// Overview page's client dropdown. Does NOT include
/// registration fields, webhook URLs, or jurisdiction — the
/// admin endpoint at /api/admin/clients owns those. Operators +
/// viewers can see which clients map to which symbols so the
/// dashboard scope selector works.
#[derive(serde::Serialize)]
struct ClientPublicRow {
    id: String,
    symbols: Vec<String>,
}

async fn list_clients_public(State(state): State<DashboardState>) -> Json<Vec<ClientPublicRow>> {
    let ids = state.client_ids();
    let rows: Vec<ClientPublicRow> = ids
        .into_iter()
        .map(|id| {
            let syms = state
                .get_client_symbols(&id)
                .into_iter()
                .map(|s| s.symbol)
                .collect();
            ClientPublicRow { id, symbols: syms }
        })
        .collect();
    Json(rows)
}

/// Per-venue book-state row. One snapshot per (venue, symbol,
/// product) with L1 mid/spread plus feed age so the Overview can
/// show operators "what market each venue sees" without them
/// having to scroll through four different panels to compare.
#[derive(serde::Serialize)]
struct VenueBookStateRow {
    venue: String,
    symbol: String,
    product: String,
    bid: Option<rust_decimal::Decimal>,
    ask: Option<rust_decimal::Decimal>,
    mid: Option<rust_decimal::Decimal>,
    spread_bps: Option<rust_decimal::Decimal>,
    /// Milliseconds since last L1 update. `null` when no update
    /// has been seen yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    age_ms: Option<i64>,
    /// UX-VENUE-2 — regime label for THIS venue's mid stream
    /// (`Quiet` / `Trending` / `Volatile` / `MeanReverting`).
    /// `None` when the engine's per-venue classifier has not
    /// yet published a snapshot for this key — fresh start, or
    /// `market_maker.venue_regime_classify_secs = 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    regime: Option<String>,
    /// UX-VENUE-2 — milliseconds since the regime label was
    /// last published. Lets the UI flag a stale classifier
    /// (e.g. a venue whose feed stopped) without having to
    /// infer it from `age_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    regime_age_ms: Option<i64>,
}

async fn venues_book_state(State(state): State<DashboardState>) -> Json<Vec<VenueBookStateRow>> {
    let now = chrono::Utc::now();
    let regime_map: std::collections::HashMap<_, _> =
        state.data_bus().regime_entries().into_iter().collect();
    let mut rows: Vec<VenueBookStateRow> = state
        .data_bus()
        .l1_entries()
        .into_iter()
        .map(|(key, snap)| {
            let regime_snap = regime_map.get(&key);
            VenueBookStateRow {
                venue: key.0.clone(),
                symbol: key.1.clone(),
                product: format!("{:?}", key.2).to_lowercase(),
                bid: snap.bid_px,
                ask: snap.ask_px,
                mid: snap.mid,
                spread_bps: snap.spread_bps,
                age_ms: snap.ts.map(|t| (now - t).num_milliseconds().max(0)),
                regime: regime_snap.map(|r| r.label.clone()),
                regime_age_ms: regime_snap
                    .and_then(|r| r.ts)
                    .map(|t| (now - t).num_milliseconds().max(0)),
            }
        })
        .collect();
    // Deterministic ordering: spot first, then perp classes, then
    // alphabetical by venue within a product class. Makes the
    // frontend render stable between polls.
    rows.sort_by(|a, b| {
        a.product
            .cmp(&b.product)
            .then_with(|| a.venue.cmp(&b.venue))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    Json(rows)
}

async fn basis_monitor(State(state): State<DashboardState>) -> Json<Vec<BasisRow>> {
    use rust_decimal::Decimal;
    let entries = state.data_bus().l1_entries();

    // Group legs by inferred base asset.
    let mut by_base: std::collections::HashMap<String, Vec<(crate::data_bus::StreamKey, Decimal)>> =
        std::collections::HashMap::new();
    for (key, snap) in entries {
        let Some(mid) = snap.mid else { continue };
        if mid <= Decimal::ZERO {
            continue;
        }
        let base = mm_portfolio::infer_base_asset(&key.1);
        // Prefix-trim common quote suffixes so BTCUSDT and
        // BTCUSDC group together. infer_base_asset returns
        // the whole symbol when no separator, so strip the
        // trailing USDT/USDC/USD/BUSD if present.
        let base_trimmed = trim_quote_suffix(&base);
        by_base
            .entry(base_trimmed.to_string())
            .or_default()
            .push((key, mid));
    }

    let mut out = Vec::new();
    for (base, mut legs) in by_base {
        if legs.len() < 2 {
            continue;
        }
        // Pick reference: prefer spot (lowest product ordinal),
        // then cheapest mid.
        legs.sort_by(|a, b| {
            let ord = product_ordinal(a.0 .2).cmp(&product_ordinal(b.0 .2));
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
            a.1.cmp(&b.1)
        });
        let (ref_key, ref_mid) = legs[0].clone();
        let leg_rows: Vec<BasisLeg> = legs
            .into_iter()
            .map(|(k, mid)| {
                let basis_bps = (mid - ref_mid) / ref_mid * Decimal::from(10_000);
                BasisLeg {
                    venue: k.0,
                    symbol: k.1,
                    product: format!("{:?}", k.2).to_lowercase(),
                    mid,
                    basis_bps,
                }
            })
            .collect();
        out.push(BasisRow {
            base_asset: base,
            reference_venue: ref_key.0,
            reference_symbol: ref_key.1,
            reference_product: format!("{:?}", ref_key.2).to_lowercase(),
            reference_mid: ref_mid,
            legs: leg_rows,
        });
    }
    out.sort_by(|a, b| a.base_asset.cmp(&b.base_asset));
    Json(out)
}

fn trim_quote_suffix(sym: &str) -> &str {
    for q in ["USDT", "USDC", "USD", "BUSD", "DAI"] {
        if let Some(stripped) = sym.strip_suffix(q) {
            if !stripped.is_empty() {
                return stripped;
            }
        }
    }
    sym
}

fn product_ordinal(p: mm_common::config::ProductType) -> u8 {
    use mm_common::config::ProductType;
    match p {
        ProductType::Spot => 0,
        ProductType::LinearPerp => 1,
        ProductType::InversePerp => 2,
    }
}

async fn venues_funding_state(State(state): State<DashboardState>) -> Json<Vec<FundingStateRow>> {
    let entries = state.data_bus().funding_entries();
    let rows: Vec<FundingStateRow> = entries
        .into_iter()
        .map(|(key, f)| FundingStateRow {
            venue: key.0,
            symbol: key.1,
            product: format!("{:?}", key.2).to_lowercase(),
            rate: f.rate,
            next_funding_ts: f.next_funding_ts.map(|ts| ts.timestamp_millis()),
        })
        .collect();
    Json(rows)
}

async fn venues_status(State(state): State<DashboardState>) -> Json<Vec<VenueStatusRow>> {
    let rows: Vec<VenueStatusRow> = state
        .get_all()
        .into_iter()
        .map(|s| {
            let margin_ratio = state.margin_ratio(&s.symbol).map(|v| v.to_string());
            let adl_quantile = state.adl_quantile(&s.symbol);
            VenueStatusRow {
                has_data: s.mid_price > rust_decimal::Decimal::ZERO,
                mid_price: s.mid_price.to_string(),
                kill_label: kill_label(s.kill_level),
                kill_level: s.kill_level,
                live_orders: s.live_orders as u32,
                total_fills: s.total_fills,
                sla_uptime_pct: s.sla_uptime_pct.to_string(),
                quoting_halted: s.kill_level >= 2,
                margin_ratio,
                adl_quantile,
                symbol: s.symbol,
            }
        })
        .collect();
    Json(rows)
}

// ── Per-client loss circuit (Epic 6/7) ───────────────────────
//
// GET /api/v1/clients/loss-state — snapshot every registered
// client's aggregate daily PnL, configured limit, and trip flag.
// Powers the dashboard's client-circuit panel.

#[derive(serde::Serialize)]
struct ClientLossRow {
    client_id: String,
    daily_pnl: String,
    limit_usd: Option<String>,
    tripped: bool,
}

async fn clients_loss_state(State(state): State<DashboardState>) -> Json<Vec<ClientLossRow>> {
    let circuit = match state.per_client_circuit() {
        Some(c) => c,
        None => return Json(Vec::new()),
    };
    let snap = circuit.snapshot_all();
    let mut rows: Vec<ClientLossRow> = snap
        .into_iter()
        .map(|(client_id, s)| ClientLossRow {
            client_id,
            daily_pnl: s.daily_pnl.to_string(),
            limit_usd: s.limit.map(|l| l.to_string()),
            tripped: s.tripped,
        })
        .collect();
    rows.sort_by(|a, b| a.client_id.cmp(&b.client_id));
    Json(rows)
}

/// Manual reset of a client's loss circuit — operator ack after
/// investigating the breach. The individual engines' kill
/// switches are NOT reset here; operators call the existing
/// `POST /api/v1/ops/reset/{symbol}` for each sibling engine so
/// every post-incident escalation is acknowledged.
async fn ops_client_reset(
    State(state): State<DashboardState>,
    Path(client_id): Path<String>,
) -> Json<serde_json::Value> {
    match state.per_client_circuit() {
        Some(circuit) => {
            circuit.reset_client(&client_id);
            Json(serde_json::json!({
                "client_id": client_id,
                "status": "reset",
                "note": "each engine's kill switch must also be reset via /api/v1/ops/reset/{symbol}"
            }))
        }
        None => Json(serde_json::json!({
            "client_id": client_id,
            "status": "no_circuit",
            "error": "per-client loss circuit not registered on this server"
        })),
    }
}

// LEGACY-1 (2026-04-21) — `surveillance_tests` module removed
// with its handler. Fleet endpoint coverage lives in
// `mm-controller` integration tests.
