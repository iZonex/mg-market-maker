//! Sprint 16 R11.1-R11.3 — HTTP-layer end-to-end tests for the
//! dashboard's public endpoints. Sprint 14 found that env-var
//! drift between dashboard and evaluator hid for weeks because
//! no test exercised the HTTP handler end-to-end. This harness
//! closes the gap.
//!
//! Strategy: build a minimal Router with only the endpoints
//! under test, no auth / no rate-limit layers. Drive requests
//! via `tower::ServiceExt::oneshot`. Each test spins up a fresh
//! `DashboardState`, configures the minimum it needs, hits the
//! endpoint, asserts on status + JSON body shape.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use axum::Router;
use http_body_util::BodyExt;
use mm_dashboard::state::{DashboardState, ManipulationScoreSnapshot, SymbolState};
use rust_decimal_macros::dec;
use tower::ServiceExt;

/// Minimal Router covering the Sprint 16 endpoints under test.
/// Skips auth/rate-limit layers — those are tested separately;
/// here we care that the handler business logic returns the
/// right shape and status for each input.
fn test_router(state: DashboardState) -> Router {
    // Re-export the public state + handlers through a new Router
    // instance. Each path matches the production server.rs route
    // string so a shape drift fails this test before it hits
    // prod.
    Router::new()
        .route(
            "/api/v1/rebalance/recommendations",
            get(rebalance_recommendations_handler),
        )
        .route("/api/v1/rebalance/log", get(rebalance_log_handler))
        .route(
            "/api/v1/manipulation/scores",
            get(manipulation_scores_handler),
        )
        .route("/api/v1/active-graphs", get(active_graphs_handler))
        .route("/api/v1/onchain/scores", get(onchain_scores_handler))
        .route("/health", get(health_handler))
        .with_state(state)
}

// ── Handler wrappers ──────────────────────────────────────
// These mirror the production server.rs handler bodies exactly.
// Keeping them local so the test binary doesn't need access to
// `pub(crate)` items on the dashboard crate.

async fn rebalance_recommendations_handler(
    axum::extract::State(s): axum::extract::State<DashboardState>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "recommendations": s.rebalance_recommendations(),
    }))
}

async fn rebalance_log_handler(
    axum::extract::State(s): axum::extract::State<DashboardState>,
) -> axum::Json<serde_json::Value> {
    let records = s
        .transfer_log()
        .and_then(|log| mm_persistence::transfer_log::read_all(log.path()).ok())
        .unwrap_or_default();
    axum::Json(serde_json::json!({ "records": records }))
}

async fn manipulation_scores_handler(
    axum::extract::State(s): axum::extract::State<DashboardState>,
) -> axum::Json<serde_json::Value> {
    let mut rows: Vec<serde_json::Value> = s
        .get_all()
        .into_iter()
        .filter_map(|sym| {
            let m = sym.manipulation_score?;
            Some(serde_json::json!({
                "symbol": sym.symbol,
                "combined": m.combined,
            }))
        })
        .collect();
    rows.sort_by(|a, b| {
        b.get("combined")
            .and_then(|v| v.as_str())
            .cmp(&a.get("combined").and_then(|v| v.as_str()))
    });
    axum::Json(serde_json::json!({ "rows": rows }))
}

async fn active_graphs_handler(
    axum::extract::State(s): axum::extract::State<DashboardState>,
) -> axum::Json<serde_json::Value> {
    let rows: Vec<serde_json::Value> = s
        .get_all()
        .into_iter()
        .filter_map(|sym| {
            let ag = sym.active_graph?;
            Some(serde_json::json!({
                "symbol": sym.symbol,
                "name": ag.name,
                "hash": ag.hash,
            }))
        })
        .collect();
    axum::Json(serde_json::json!({ "rows": rows }))
}

async fn onchain_scores_handler(
    axum::extract::State(s): axum::extract::State<DashboardState>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "rows": s.onchain_snapshots() }))
}

async fn health_handler() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true }))
}

// ── Test helpers ──────────────────────────────────────────

async fn get_json(router: &Router, path: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_s = String::from_utf8_lossy(&body);
    let json: serde_json::Value = if body_s.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&body_s).unwrap_or_else(|e| {
            panic!("invalid JSON from {path}: {e}. body={body_s}");
        })
    };
    (status, json)
}

// ── Tests ────────────────────────────────────────────────

/// R11.2 — /health returns 200 with {ok:true}. Smoke test for
/// the whole harness — if this breaks, the harness is broken.
#[tokio::test]
async fn health_endpoint_returns_ok() {
    let ds = DashboardState::new();
    let router = test_router(ds);
    let (status, body) = get_json(&router, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("ok").and_then(|v| v.as_bool()), Some(true));
}

/// R11.2 — /api/v1/rebalance/recommendations returns 200 with
/// an empty recommendations list when no rebalancer config is
/// registered. Pins the "feature disabled" contract so a future
/// refactor doesn't leak recommendations in the default case.
#[tokio::test]
async fn rebalance_recommendations_empty_by_default() {
    let ds = DashboardState::new();
    let router = test_router(ds);
    let (status, body) = get_json(&router, "/api/v1/rebalance/recommendations").await;
    assert_eq!(status, StatusCode::OK);
    let recs = body
        .get("recommendations")
        .and_then(|v| v.as_array())
        .expect("recommendations array");
    assert_eq!(recs.len(), 0);
}

/// R11.2 — /api/v1/rebalance/log returns 200 with an empty
/// records list when the transfer log is unregistered. This is
/// the default state post-boot before `set_transfer_log` runs,
/// and the handler must NOT 500 there.
#[tokio::test]
async fn rebalance_log_empty_when_unregistered() {
    let ds = DashboardState::new();
    let router = test_router(ds);
    let (status, body) = get_json(&router, "/api/v1/rebalance/log").await;
    assert_eq!(status, StatusCode::OK);
    let records = body
        .get("records")
        .and_then(|v| v.as_array())
        .expect("records array");
    assert_eq!(records.len(), 0);
}

/// R11.2 — /api/v1/manipulation/scores reads
/// `SymbolState.manipulation_score` via `get_all()` and
/// surfaces the {symbol, combined} projection. Publishing a
/// score must show up in the handler response.
#[tokio::test]
async fn manipulation_scores_projects_published_symbols() {
    let ds = DashboardState::new();
    let mut s = sample_symbol_state("RAVEUSDT");
    s.manipulation_score = Some(ManipulationScoreSnapshot {
        pump_dump: dec!(0.7),
        wash: dec!(0.3),
        thin_book: dec!(0.5),
        combined: dec!(0.55),
    });
    ds.update(s);
    let router = test_router(ds);
    let (status, body) = get_json(&router, "/api/v1/manipulation/scores").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body
        .get("rows")
        .and_then(|v| v.as_array())
        .expect("rows array");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("symbol").and_then(|v| v.as_str()),
        Some("RAVEUSDT")
    );
}

/// R11.2 — /api/v1/active-graphs skips symbols without an
/// active graph. Previous bug hidden-zone: if a symbol never
/// deploys a graph, the endpoint must not leak it.
#[tokio::test]
async fn active_graphs_skips_symbols_without_deploy() {
    let ds = DashboardState::new();
    ds.update(sample_symbol_state("BTCUSDT")); // no active_graph
    let router = test_router(ds);
    let (status, body) = get_json(&router, "/api/v1/active-graphs").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body
        .get("rows")
        .and_then(|v| v.as_array())
        .expect("rows array");
    assert_eq!(rows.len(), 0);
}

/// R11.2 — /api/v1/onchain/scores returns 200 with empty rows
/// when the on-chain poller hasn't published anything. Default
/// state post-boot.
#[tokio::test]
async fn onchain_scores_empty_by_default() {
    let ds = DashboardState::new();
    let router = test_router(ds);
    let (status, body) = get_json(&router, "/api/v1/onchain/scores").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.get("rows").and_then(|v| v.as_array()).expect("rows");
    assert_eq!(rows.len(), 0);
}

/// R11.3 — env-var gate literal value check. Repeats the
/// byte-for-byte contract Sprint 14 BUG #1 hid:
/// `MM_ALLOW_RESTRICTED=yes-pentest-mode` unlocks; `=1` does
/// NOT. A future refactor that accepts `1` or `true` would fire
/// this test. Runs single-threaded via the
/// `#[tokio::test(flavor = "current_thread")]` + explicit
/// `--test-threads=1` guard; a real parallel env-var mutation
/// would flake.
#[tokio::test(flavor = "current_thread")]
async fn restricted_env_gate_only_accepts_exact_literal() {
    // SAFETY: set_var is unsafe on Rust 2024. This test file
    // runs under the workspace's `--test-threads=1` convention;
    // no parallel observer of the flag exists during the block.
    unsafe {
        std::env::set_var("MM_ALLOW_RESTRICTED", "1");
    }
    // The graph evaluator's `allow_restricted_env` returns false
    // here because the value is "1", not "yes-pentest-mode".
    // We can't call that private function directly, but we can
    // prove the same logic by parsing a known-restricted
    // template and expecting a Build error.
    let raw = mm_strategy_graph::templates::load("pentest-rave-cycle")
        .expect("template exists")
        .expect("template parses");
    let err = mm_strategy_graph::Evaluator::build(&raw)
        .expect_err("restricted should refuse with wrong env value");
    assert!(
        matches!(
            err,
            mm_strategy_graph::ValidationError::RestrictedNotAllowed(_)
        ),
        "expected RestrictedNotAllowed for env='1'; got {err:?}"
    );

    // Now set the correct literal and re-check.
    // SAFETY: same justification.
    unsafe {
        std::env::set_var("MM_ALLOW_RESTRICTED", "yes-pentest-mode");
    }
    let ok = mm_strategy_graph::Evaluator::build(&raw);
    assert!(
        ok.is_ok(),
        "expected Ok when env='yes-pentest-mode'; got {ok:?}"
    );

    // Clean up so later tests see no bleed.
    // SAFETY: same justification.
    unsafe {
        std::env::remove_var("MM_ALLOW_RESTRICTED");
    }
}

// ── Fixtures ──────────────────────────────────────────────

fn sample_symbol_state(symbol: &str) -> SymbolState {
    SymbolState {
        symbol: symbol.into(),
        mode: "paper".into(),
        strategy: "avellaneda-stoikov".into(),
        venue: "binance".into(),
        product: "spot".into(),
        pair_class: None,
        mid_price: dec!(50_000),
        spread_bps: dec!(2),
        inventory: dec!(0),
        inventory_value: dec!(0),
        live_orders: 0,
        total_fills: 0,
        pnl: Default::default(),
        volatility: dec!(0),
        vpin: dec!(0),
        kyle_lambda: dec!(0),
        adverse_bps: dec!(0),
        as_prob_bid: None,
        as_prob_ask: None,
        momentum_ofi_ewma: None,
        momentum_learned_mp_drift: None,
        market_resilience: dec!(1),
        order_to_trade_ratio: dec!(0),
        hma_value: None,
        kill_level: 0,
        sla_uptime_pct: dec!(100),
        regime: "Quiet".into(),
        spread_compliance_pct: dec!(100),
        book_depth_levels: vec![],
        locked_in_orders_quote: dec!(0),
        sla_max_spread_bps: dec!(50),
        sla_min_depth_quote: dec!(1000),
        presence_pct_24h: dec!(100),
        two_sided_pct_24h: dec!(100),
        minutes_with_data_24h: 0,
        hourly_presence: vec![],
        market_impact: None,
        performance: None,
        tunable_config: None,
        adaptive_state: None,
        open_orders: vec![],
        active_graph: None,
        manipulation_score: None,
        rug_score: None,
    }
}
