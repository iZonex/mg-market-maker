use std::sync::Arc;

use axum::extract::{Path, State};
use axum::middleware;
use axum::routing::{get, post};
use axum::{Json, Router};
use prometheus::TextEncoder;
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::auth::{auth_middleware, login_handler, ApiUser, AuthState, Role};
use crate::state::{ConfigOverride, DashboardState, SymbolState};
use crate::websocket::{ws_handler, WsBroadcast};

/// Start the dashboard HTTP + WebSocket server with authentication.
///
/// Public (no auth):
///   GET  /health
///   POST /api/auth/login
///
/// Protected (require auth):
///   GET  /api/status
///   GET  /api/v1/*           — operator/client API
///   GET  /api/v1/client/*    — client portal
///   GET  /metrics            — Prometheus (admin/operator only)
///   WS   /ws                 — real-time updates
pub async fn start(
    state: DashboardState,
    ws_broadcast: Arc<WsBroadcast>,
    auth_state: AuthState,
    port: u16,
) -> anyhow::Result<()> {
    crate::metrics::init();

    // Public routes — no auth required.
    let public = Router::new()
        .route("/health", get(health))
        .route("/api/auth/login", post(login_handler))
        .with_state(auth_state.clone());

    // K8s probes (no auth, need DashboardState).
    let probes = Router::new()
        .route("/ready", get(readiness))
        .route("/startup", get(readiness))
        .with_state(state.clone());

    // Protected API routes — require auth.
    let protected_api = Router::new()
        .route("/api/status", get(get_status))
        .route("/metrics", get(prometheus_metrics))
        .merge(crate::client_api::client_routes())
        .merge(crate::client_portal::client_portal_routes())
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

    // Admin config routes — hot-reload config per symbol or
    // broadcast to all.
    let admin_config = Router::new()
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
        .merge(crate::admin_clients::admin_client_routes())
        .with_state(state.clone());

    // WebSocket — auth via query param (?token=...).
    let ws_routes = Router::new()
        .route("/ws", get(ws_handler))
        .with_state((state.clone(), ws_broadcast));

    // Admin routes — user management.
    let admin = Router::new()
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/users", post(create_user))
        .route_layer(middleware::from_fn_with_state(
            auth_state.clone(),
            auth_middleware,
        ))
        .with_state(auth_state);

    let app = Router::new()
        .merge(public)
        .merge(probes)
        .merge(protected_api)
        .merge(ws_routes)
        .merge(admin)
        .merge(admin_config)
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%addr, "dashboard server starting");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
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

async fn prometheus_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder
        .encode_to_string(&metric_families)
        .unwrap_or_default()
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

fn generate_api_key() -> String {
    // Generate a 32-char hex API key.
    let bytes: Vec<u8> = (0..16).map(|_| rand_byte()).collect();
    hex::encode(bytes)
}

fn rand_byte() -> u8 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let c = CTR.fetch_add(1, Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    ((c.wrapping_mul(6364136223846793005).wrapping_add(now)) >> 32) as u8
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
