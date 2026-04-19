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
    admin_middleware, auth_middleware, internal_view_middleware, login_handler, logout_handler,
    ApiUser, AuthState, Role,
};
use crate::rate_limit::{rate_limit_middleware, RateLimiter};
use crate::state::{ConfigOverride, DashboardState, SymbolState, VenueBalanceSnapshot};
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
pub async fn start(
    state: DashboardState,
    ws_broadcast: Arc<WsBroadcast>,
    auth_state: AuthState,
    port: u16,
) -> anyhow::Result<()> {
    crate::metrics::init();

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
        .route("/api/v1/inventory/venues", get(inventory_venues_all))
        .route(
            "/api/v1/inventory/venues/{symbol}",
            get(inventory_venues_symbol),
        )
        .route("/api/v1/clients/loss-state", get(clients_loss_state))
        .route("/api/v1/surveillance/scores", get(surveillance_scores))
        .route("/api/v1/decisions/recent", get(decisions_recent))
        .route("/api/v1/plans/active", get(active_plans))
        .route("/api/v1/otr/tiered", get(otr_tiered))
        .route("/api/v1/portfolio/cross_venue", get(portfolio_cross_venue))
        .route("/api/v1/venues/latency_p95", get(venues_latency_p95))
        .route("/api/v1/sor/decisions/recent", get(sor_decisions_recent))
        .route("/api/v1/atomic-bundles/inflight", get(atomic_bundles_inflight))
        .merge(crate::client_api::client_routes())
        .merge(crate::client_portal::client_portal_routes())
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
        .route("/api/v1/ops/widen/{symbol}", post(ops_widen))
        .route("/api/v1/ops/stop/{symbol}", post(ops_stop))
        .route("/api/v1/ops/cancel-all/{symbol}", post(ops_cancel_all))
        .route("/api/v1/ops/flatten/{symbol}", post(ops_flatten))
        .route("/api/v1/ops/disconnect/{symbol}", post(ops_disconnect))
        .route("/api/v1/ops/reset/{symbol}", post(ops_kill_reset))
        .route("/api/v1/ops/client-reset/{client_id}", post(ops_client_reset))
        .route("/api/admin/config/{symbol}", post(admin_config_override))
        .route("/api/admin/config", post(admin_config_broadcast))
        .route("/api/admin/config/bulk", post(admin_config_bulk))
        .route(
            "/api/admin/symbols/{symbol}/pause",
            post(admin_pause_symbol),
        )
        .route(
            "/api/admin/symbols/{symbol}/resume",
            post(admin_resume_symbol),
        )
        .route("/api/admin/webhooks", get(admin_list_webhooks))
        .route("/api/admin/webhooks", post(admin_add_webhook))
        .route("/api/admin/alerts", get(admin_list_alerts))
        .route("/api/admin/alerts", post(admin_add_alert))
        .route("/api/admin/alerts/check", get(admin_check_alerts))
        .route("/api/admin/symbols", get(admin_list_symbols))
        .route("/api/admin/loans", axum::routing::post(admin_create_loan))
        .route("/api/admin/loans", get(admin_list_loans))
        .route("/api/admin/optimize/status", get(admin_optimize_status))
        .route("/api/admin/optimize/results", get(admin_optimize_results))
        .route("/api/admin/optimize/trigger", post(admin_optimize_trigger))
        .route("/api/admin/optimize/pending", get(admin_optimize_pending))
        .route("/api/admin/optimize/apply", post(admin_optimize_apply))
        .route("/api/admin/optimize/discard", post(admin_optimize_discard))
        .route("/api/admin/sentiment/headline", post(admin_sentiment_headline))
        .merge(crate::admin_clients::admin_client_routes())
        .with_state(state.clone());

    // Strategy-graph deploy endpoint — same auth + admin middleware
    // as the rest of the admin surface but a tighter rate limit.
    // Split into its own router so the stricter limiter layers
    // cleanly without double-counting against `admin_rl`.
    let admin_graph_deploy = Router::new()
        .route("/api/admin/strategy/graph", post(admin_deploy_strategy_graph))
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
        .with_state(auth_state.clone());

    let admin = admin_config
        .merge(admin_users)
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
    let ws_routes = Router::new()
        .route("/ws", get(ws_handler))
        .with_state((state.clone(), ws_broadcast, auth_state));

    let cors = build_cors_layer();

    let app = Router::new()
        .merge(public)
        .merge(login)
        .merge(logout)
        .merge(probes)
        .merge(protected_api)
        .merge(metrics_route)
        .merge(admin)
        .merge(admin_graph_deploy)
        .merge(ws_routes)
        .layer(cors);

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
/// balance snapshots the engine last published from each connector
/// in the bundle (primary, hedge, SOR extras).
async fn inventory_venues_symbol(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
) -> Json<Vec<VenueBalanceSnapshot>> {
    Json(state.venue_balances(&symbol))
}

/// All per-venue inventory snapshots keyed by symbol. Used by the
/// overview panel to render the cross-symbol picture in one shot.
async fn inventory_venues_all(
    State(state): State<DashboardState>,
) -> Json<std::collections::HashMap<String, Vec<VenueBalanceSnapshot>>> {
    Json(state.all_venue_balances())
}

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

#[derive(serde::Serialize)]
struct SurveillanceScoreRow {
    score: f64,
    alerts_total: u64,
}

#[derive(serde::Serialize)]
struct SurveillanceScoresResponse {
    patterns: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, SurveillanceScoreRow>,
    >,
}

async fn surveillance_scores() -> Json<SurveillanceScoresResponse> {
    use prometheus::proto::MetricType;
    use std::collections::BTreeMap;

    let families = prometheus::gather();
    let mut scores: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut alerts: BTreeMap<(String, String), u64> = BTreeMap::new();

    for fam in &families {
        match fam.get_name() {
            "mm_surveillance_score" if fam.get_field_type() == MetricType::GAUGE => {
                for m in fam.get_metric() {
                    let (Some(pattern), Some(symbol)) = label_pair(m) else {
                        continue;
                    };
                    scores.insert((pattern, symbol), m.get_gauge().get_value());
                }
            }
            "mm_surveillance_alerts_total"
                if fam.get_field_type() == MetricType::COUNTER =>
            {
                for m in fam.get_metric() {
                    let (Some(pattern), Some(symbol)) = label_pair(m) else {
                        continue;
                    };
                    alerts.insert(
                        (pattern, symbol),
                        m.get_counter().get_value().round() as u64,
                    );
                }
            }
            _ => {}
        }
    }

    let mut patterns: BTreeMap<String, BTreeMap<String, SurveillanceScoreRow>> =
        BTreeMap::new();
    for ((pattern, symbol), score) in &scores {
        let alerts_total = alerts.get(&(pattern.clone(), symbol.clone())).copied().unwrap_or(0);
        patterns
            .entry(pattern.clone())
            .or_default()
            .insert(symbol.clone(), SurveillanceScoreRow { score: *score, alerts_total });
    }
    // Union in symbols that have alerts but never reported a score
    // this tick (e.g. detector ran once, hasn't fired since a
    // restart).
    for ((pattern, symbol), alerts_total) in alerts {
        patterns
            .entry(pattern)
            .or_default()
            .entry(symbol)
            .or_insert(SurveillanceScoreRow { score: 0.0, alerts_total });
    }
    Json(SurveillanceScoresResponse { patterns })
}

// ── INT-1: decision ledger readback ─────────────────────

#[derive(serde::Deserialize)]
struct DecisionsQuery {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(serde::Serialize)]
struct DecisionsResponse {
    /// Decisions keyed by symbol, newest-first within each
    /// symbol. When `?symbol=X` is supplied the map has at most
    /// one key; otherwise every registered symbol is included.
    symbols: std::collections::BTreeMap<
        String,
        Vec<mm_risk::decision_ledger::DecisionSnapshot>,
    >,
}

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
                if let Some(existing) = buckets.iter_mut().find(|(boundary, _)| {
                    (boundary - ub).abs() < f64::EPSILON
                }) {
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

/// S1.3 — SOR routing decision log query. `?limit=N` clamps
/// the response size (default 50, max equals the state's
/// `MAX_SOR_DECISIONS` capacity). Newest-first.
#[derive(serde::Deserialize)]
struct SorDecisionsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(serde::Serialize)]
struct SorDecisionsResponse {
    decisions: Vec<crate::state::SorDecisionRecord>,
}

async fn sor_decisions_recent(
    State(state): State<DashboardState>,
    axum::extract::Query(q): axum::extract::Query<SorDecisionsQuery>,
) -> Json<SorDecisionsResponse> {
    let limit = q.limit.unwrap_or(50).min(256);
    Json(SorDecisionsResponse {
        decisions: state.sor_decisions_recent(limit),
    })
}

/// S2.2 — inflight atomic bundle snapshot for the monitor
/// panel. Returns every bundle currently tracked on the
/// shared DashboardState ack map (originator + remote legs),
/// paired up by bundle id.
#[derive(serde::Serialize)]
struct AtomicBundlesResponse {
    bundles: Vec<crate::state::AtomicBundleSnapshot>,
}

async fn atomic_bundles_inflight(
    State(state): State<DashboardState>,
) -> Json<AtomicBundlesResponse> {
    Json(AtomicBundlesResponse {
        bundles: state.atomic_bundles_inflight(),
    })
}

async fn otr_tiered() -> Json<TieredOtrResponse> {
    use prometheus::proto::MetricType;
    let mut out: std::collections::BTreeMap<String, TieredOtrRow> =
        std::collections::BTreeMap::new();
    let families = prometheus::gather();
    for fam in &families {
        if fam.get_name() != "mm_otr_tiered"
            || fam.get_field_type() != MetricType::GAUGE
        {
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
            let (Some(symbol), Some(tier), Some(window)) = (symbol, tier, window)
            else {
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

async fn decisions_recent(
    State(state): State<DashboardState>,
    axum::extract::Query(q): axum::extract::Query<DecisionsQuery>,
) -> Json<DecisionsResponse> {
    let limit = q.limit.unwrap_or(100).min(1_000);
    let symbols = if let Some(sym) = q.symbol.as_deref() {
        let mut m = std::collections::BTreeMap::new();
        if let Some(rows) = state.decisions_recent(sym, limit) {
            m.insert(sym.to_string(), rows);
        }
        m
    } else {
        state.decisions_all_symbols(limit)
    };
    Json(DecisionsResponse { symbols })
}

fn label_pair(m: &prometheus::proto::Metric) -> (Option<String>, Option<String>) {
    let mut pattern = None;
    let mut symbol = None;
    for lbl in m.get_label() {
        match lbl.get_name() {
            "pattern" => pattern = Some(lbl.get_value().to_string()),
            "symbol" => symbol = Some(lbl.get_value().to_string()),
            _ => {}
        }
    }
    (pattern, symbol)
}

// --- Admin: User Management ---

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

// ── Admin config override endpoints ─────────────────────────

/// Apply a config override to a specific symbol.
/// POST /api/admin/config/:symbol
/// Body: `{"field": "Gamma", "value": "0.15"}`
#[derive(serde::Serialize)]
struct ConfigOverrideResponse {
    symbol: String,
    applied: bool,
}

#[derive(serde::Serialize)]
struct ConfigBroadcastResponse {
    engines_updated: usize,
}

async fn admin_config_override(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
    Json(ovr): Json<ConfigOverride>,
) -> Json<ConfigOverrideResponse> {
    let ok = state.send_config_override(&symbol, ovr);
    Json(ConfigOverrideResponse {
        symbol,
        applied: ok,
    })
}

/// Broadcast a config override to ALL running engines.
/// POST /api/admin/config
/// Body: `{"field": "MinSpreadBps", "value": "10"}`
async fn admin_config_broadcast(
    State(state): State<DashboardState>,
    Json(ovr): Json<ConfigOverride>,
) -> Json<ConfigBroadcastResponse> {
    let count = state.broadcast_config_override(ovr);
    Json(ConfigBroadcastResponse {
        engines_updated: count,
    })
}

/// Bulk config override — apply to all symbols matching a
/// substring pattern. POST /api/admin/config/bulk
/// Body: `{"pattern": "USDT", "override": {"field": "MinSpreadBps", "value": "10"}}`
#[derive(serde::Deserialize)]
struct BulkConfigRequest {
    /// Substring pattern to match symbol names against.
    pattern: String,
    /// Config override to apply to all matching symbols.
    #[serde(rename = "override")]
    config_override: ConfigOverride,
}

#[derive(serde::Serialize)]
struct BulkConfigResponse {
    matched_symbols: Vec<String>,
    applied: usize,
}

async fn admin_config_bulk(
    State(state): State<DashboardState>,
    Json(req): Json<BulkConfigRequest>,
) -> Json<BulkConfigResponse> {
    let all_symbols = state.config_symbols();
    let matched: Vec<String> = all_symbols
        .into_iter()
        .filter(|s| s.contains(&req.pattern))
        .collect();
    let mut applied = 0;
    for symbol in &matched {
        if state.send_config_override(symbol, req.config_override.clone()) {
            applied += 1;
        }
    }
    Json(BulkConfigResponse {
        matched_symbols: matched,
        applied,
    })
}

async fn admin_pause_symbol(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
) -> Json<ConfigOverrideResponse> {
    let ok = state.send_config_override(&symbol, ConfigOverride::PauseQuoting);
    Json(ConfigOverrideResponse {
        symbol,
        applied: ok,
    })
}

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

async fn admin_resume_symbol(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
) -> Json<ConfigOverrideResponse> {
    let ok = state.send_config_override(&symbol, ConfigOverride::ResumeQuoting);
    Json(ConfigOverrideResponse {
        symbol,
        applied: ok,
    })
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

// ── Optimization admin endpoints (Epic 6) ───────────────────

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

// ── Epic 33 — hyperopt re-calibrate flow ────────────────────

/// Kick off a hyperopt run against a pre-recorded JSONL. The
/// worker task in the server reads `HyperoptTrigger` payloads
/// from the channel registered at startup, runs the optimiser,
/// and stages the best trial as `PendingCalibration` for
/// operator review.
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

/// List pending calibrations — one per symbol if any hyperopt
/// runs have completed since the last apply / discard.
async fn admin_optimize_pending(
    State(state): State<DashboardState>,
) -> axum::Json<Vec<crate::state::PendingCalibration>> {
    axum::Json(state.all_calibrations())
}

#[derive(serde::Deserialize)]
struct SymbolBody {
    symbol: String,
}

/// Apply the pending calibration for `symbol`: convert each
/// suggested parameter into the matching `ConfigOverride` and
/// dispatch. Clears the pending entry.
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
            // κ and σ are strategy inputs but not hot-reloadable
            // through the existing ConfigOverride surface yet —
            // skip with a note so the operator can patch config
            // manually if they were part of the suggestion.
            _ => {
                skipped.push(key.clone());
                continue;
            }
        };
        if state.send_config_override(&body.symbol, ovr) {
            applied += 1;
        } else {
            skipped.push(format!("{key} (send failed)"));
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

// ── Epic G follow-up — manual headline push ────────────────
//
// Operators with faster eyes than the RSS / Twitter poll
// cycle can inject a headline directly. The endpoint
// broadcasts `ConfigOverride::News(text)` so every engine's
// `NewsRetreatStateMachine` re-evaluates on it — same path
// the automated pipeline uses. The distinction is purely
// provenance: automated pipeline goes via the orchestrator's
// SentimentTick; this endpoint skips straight to the
// retreat-state-machine branch for immediate effect.

#[derive(serde::Deserialize)]
struct HeadlinePayload {
    /// Free text — regex tables in `NewsRetreatConfig` decide
    /// the severity class.
    text: String,
}

async fn admin_sentiment_headline(
    State(state): State<DashboardState>,
    axum::Json(body): axum::Json<HeadlinePayload>,
) -> axum::Json<serde_json::Value> {
    if body.text.trim().is_empty() {
        return axum::Json(serde_json::json!({
            "status": "rejected",
            "reason": "empty text",
        }));
    }
    let recipients = state.broadcast_config_override(
        crate::state::ConfigOverride::News(body.text.clone()),
    );
    tracing::info!(
        chars = body.text.len(),
        recipients,
        "operator-pushed headline broadcast"
    );
    axum::Json(serde_json::json!({
        "status": "broadcast",
        "recipients": recipients,
        "chars": body.text.len(),
    }))
}

// ── Epic H — strategy graph deploy ─────────────────────────
//
// `POST /api/admin/strategy/graph` body: the full graph JSON.
// Flow:
//   1. parse + compile (runs full validation via `Evaluator::build`).
//   2. persist via `GraphStore::save` — atomic tmp+rename + deploy
//      log append + SHA-256 hash.
//   3. broadcast `ConfigOverride::StrategyGraphSwap(json)` to every
//      engine; engines whose scope matches swap in the new graph,
//      engines that don't match silently skip.
//   4. respond with `{ hash, recipients }`.

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
    /// deploy. When the env gate is ON (`MM_RESTRICTED_ALLOW`)
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

async fn admin_deploy_strategy_graph(
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

    // Restricted-kind gate. `MM_RESTRICTED_ALLOW=1` opts in
    // deploying graphs that reference pentest-only nodes; any
    // other value (including the default absent case) refuses the
    // deploy and emits an audit row so regulators can confirm the
    // gate actually fired. Intentional no-config default: prod
    // must be explicit about enabling.
    let allow_restricted = std::env::var("MM_RESTRICTED_ALLOW")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
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
            "restricted nodes without MM_RESTRICTED_ALLOW: {}",
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
                    &format!("restricted deploy awaiting operator ack: {}", offenders.join(",")),
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

    // Scope key string — compact, matches the form the engine uses
    // when routing ConfigOverride messages.
    let scope_key = match &graph.scope {
        mm_strategy_graph::Scope::Symbol(s) => format!("Symbol({s})"),
        mm_strategy_graph::Scope::AssetClass(c) => format!("AssetClass({c})"),
        mm_strategy_graph::Scope::Client(c) => format!("Client({c})"),
        mm_strategy_graph::Scope::Global => "Global".to_string(),
    };

    // Broadcast. ConfigOverride clones are cheap (String body).
    let recipients = state.broadcast_config_override(
        crate::state::ConfigOverride::StrategyGraphSwap(body.clone()),
    );

    // Audit trail — rollback (if flagged by the UI) precedes the
    // deploy row so a grepper who reads top-to-bottom sees intent
    // before result.
    if let Some(audit) = state.audit_log() {
        if let Some(from_hash) = q.rollback_from.as_deref() {
            audit.strategy_graph_rolled_back(&graph.name, from_hash, &hash, &operator);
        }
        audit.strategy_graph_deployed(
            &graph.name,
            &hash,
            &scope_key,
            &operator,
            recipients,
        );
    }

    tracing::info!(
        name = %graph.name,
        hash = %hash,
        recipients,
        operator = %operator,
        rollback_from = ?q.rollback_from,
        "strategy graph deployed"
    );

    (
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "status": "deployed",
            "hash": hash,
            "recipients": recipients,
            "name": graph.name,
        })),
    )
        .into_response()
}

// ── Ops endpoints — manual kill switch control ───────────────
//
// These power the Controls.svelte panel in the dashboard frontend.
// Every endpoint routes through the existing `ConfigOverride`
// mpsc channel so the engine applies the change on its next
// select-loop tick. Admin role only + per-user rate limit, both
// enforced by the admin router's route_layers.

#[derive(serde::Deserialize)]
struct OpsRequest {
    /// Free-form reason string captured in the audit trail and
    /// incident log. Required so every manual escalation has a
    /// human-readable justification for regulator review.
    #[serde(default)]
    reason: String,
}

#[derive(serde::Serialize)]
struct OpsResponse {
    symbol: String,
    level: u8,
    applied: bool,
}

async fn ops_kill_at_level(
    state: &DashboardState,
    symbol: &str,
    level: u8,
    reason: &str,
) -> Json<OpsResponse> {
    let effective_reason = if reason.is_empty() {
        "dashboard operator".to_string()
    } else {
        reason.to_string()
    };
    let ok = state.send_config_override(
        symbol,
        ConfigOverride::ManualKillSwitch {
            level,
            reason: effective_reason,
        },
    );
    Json(OpsResponse {
        symbol: symbol.to_string(),
        level,
        applied: ok,
    })
}

async fn ops_widen(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
    body: Option<Json<OpsRequest>>,
) -> Json<OpsResponse> {
    let reason = body.map(|b| b.reason.clone()).unwrap_or_default();
    ops_kill_at_level(&state, &symbol, 1, &reason).await
}

async fn ops_stop(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
    body: Option<Json<OpsRequest>>,
) -> Json<OpsResponse> {
    let reason = body.map(|b| b.reason.clone()).unwrap_or_default();
    ops_kill_at_level(&state, &symbol, 2, &reason).await
}

async fn ops_cancel_all(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
    body: Option<Json<OpsRequest>>,
) -> Json<OpsResponse> {
    let reason = body.map(|b| b.reason.clone()).unwrap_or_default();
    ops_kill_at_level(&state, &symbol, 3, &reason).await
}

async fn ops_flatten(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
    body: Option<Json<OpsRequest>>,
) -> Json<OpsResponse> {
    let reason = body.map(|b| b.reason.clone()).unwrap_or_default();
    ops_kill_at_level(&state, &symbol, 4, &reason).await
}

async fn ops_disconnect(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
    body: Option<Json<OpsRequest>>,
) -> Json<OpsResponse> {
    let reason = body.map(|b| b.reason.clone()).unwrap_or_default();
    ops_kill_at_level(&state, &symbol, 5, &reason).await
}

async fn ops_kill_reset(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
    body: Option<Json<OpsRequest>>,
) -> Json<OpsResponse> {
    let reason = body
        .map(|b| b.reason.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dashboard operator".to_string());
    let ok = state.send_config_override(
        &symbol,
        ConfigOverride::ManualKillSwitchReset { reason },
    );
    Json(OpsResponse {
        symbol,
        level: 0,
        applied: ok,
    })
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

#[cfg(test)]
mod surveillance_tests {
    use super::*;
    use crate::metrics::{SURVEILLANCE_ALERTS_TOTAL, SURVEILLANCE_SCORE};

    /// Sprint 4 companion — endpoint reflects the live gauge +
    /// counter values for the given (pattern, symbol).
    #[tokio::test]
    async fn surveillance_scores_reflects_metric_state() {
        SURVEILLANCE_SCORE
            .with_label_values(&["spoofing", "UISURV1"])
            .set(0.73);
        SURVEILLANCE_ALERTS_TOTAL
            .with_label_values(&["spoofing", "UISURV1"])
            .inc_by(3);
        let Json(out) = surveillance_scores().await;
        let spoof = out
            .patterns
            .get("spoofing")
            .expect("spoofing bucket present");
        let row = spoof.get("UISURV1").expect("symbol bucket present");
        assert!((row.score - 0.73).abs() < 1e-9, "score round-trips");
        assert_eq!(row.alerts_total, 3, "alerts counter surfaces");
    }
}
