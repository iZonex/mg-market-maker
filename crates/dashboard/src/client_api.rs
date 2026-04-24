use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::state::{DashboardState, FillRecord};

/// Wave B2 — decoded view of the agent's `client_metrics`
/// topic reply. One row per running deployment in the fleet.
/// Decimals are strings on the wire (rust_decimal precision
/// preservation); we parse lazily via `dec()` below.
fn dec(v: &serde_json::Value, k: &str) -> Decimal {
    v.get(k)
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO)
}

fn u64_field(v: &serde_json::Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn str_field(v: &serde_json::Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Wave B2 — read every running deployment's `client_metrics`
/// slice across the fleet. Falls back to a synthesised view
/// off the local DashboardState (single-engine mode / tests)
/// when the controller hasn't installed a fetcher.
async fn fleet_client_metrics(
    state: &DashboardState,
    client_filter: Option<&str>,
) -> Vec<serde_json::Value> {
    if let Some(fetcher) = state.fleet_client_metrics_fetcher() {
        return fetcher(client_filter.map(|s| s.to_string())).await;
    }
    // Local-state fallback: project each symbol state into the
    // same shape the agent emits over the wire. Legacy callers
    // (unit tests, single-engine deploys) keep working.
    let mut out = Vec::new();
    let syms = match client_filter {
        Some(cid) => state.get_client_symbols(cid),
        None => state.get_all(),
    };
    for s in syms {
        let bid_depth: Decimal = s.book_depth_levels.iter().map(|l| l.bid_depth_quote).sum();
        let ask_depth: Decimal = s.book_depth_levels.iter().map(|l| l.ask_depth_quote).sum();
        out.push(serde_json::json!({
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
        }));
    }
    out
}

/// Client-facing API — what clients and exchanges expect to see.
///
/// Endpoints:
///   GET /api/v1/positions          — current positions per symbol
///   GET /api/v1/pnl                — PnL summary (spread/inventory/rebates)
///   GET /api/v1/sla                — SLA compliance report
///   GET /api/v1/fills/recent       — recent fills
///   GET /api/v1/report/daily       — daily performance report (JSON)
///   GET /api/v1/portfolio          — unified multi-currency portfolio snapshot
pub fn client_routes() -> Router<DashboardState> {
    Router::new()
        .route("/api/v1/positions", get(get_positions))
        .route("/api/v1/pnl", get(get_pnl))
        .route("/api/v1/pnl/per_leg", get(get_pnl_per_leg))
        .route("/api/v1/sla", get(get_sla))
        .route("/api/v1/sla/certificate", get(get_sla_certificate))
        .route("/api/v1/sla/hourly", get(get_sla_hourly))
        .route("/api/v1/fills/recent", get(get_recent_fills))
        .route("/api/v1/fills/slippage", get(get_slippage_report))
        .route("/api/v1/report/daily", get(get_daily_report))
        .route("/api/v1/report/daily/csv", get(get_daily_report_csv))
        .route("/api/v1/market-impact", get(get_market_impact))
        .route("/api/v1/performance", get(get_performance))
        .route("/api/v1/report/history", get(get_report_history))
        .route("/api/v1/report/history/{date}", get(get_historical_report))
        .route("/api/v1/book/analytics", get(get_book_analytics))
        // `/api/v1/audit/recent` removed in the 2026-04
        // stabilization pass — it read a local data/audit.jsonl
        // which doesn't exist in distributed mode (agents write
        // audit to their own disks). AuditStream.svelte now
        // fans out per-deployment via the details endpoint
        // (topic `audit_tail`) and merges client-side.
        .route("/api/v1/system/diagnostics", get(get_diagnostics))
        .route("/api/v1/pnl/timeseries", get(get_pnl_timeseries))
        .route("/api/v1/spread/timeseries", get(get_spread_timeseries))
        .route(
            "/api/v1/inventory/timeseries",
            get(get_inventory_timeseries),
        )
        .route("/api/v1/risk/summary", get(get_risk_summary))
        .route("/api/v1/trade-flow", get(get_trade_flow))
        .route("/api/v1/portfolio", get(get_portfolio))
        .route("/api/v1/system/preflight", get(get_system_preflight))
        .route("/api/v1/config/snapshot", get(get_config_snapshot))
        .route("/api/v1/report/monthly.json", get(get_monthly_json))
        .route("/api/v1/report/monthly.csv", get(get_monthly_csv))
        .route("/api/v1/report/monthly.xlsx", get(get_monthly_xlsx))
        .route("/api/v1/report/monthly.pdf", get(get_monthly_pdf))
        .route("/api/v1/report/monthly.manifest", get(get_monthly_manifest))
        // Wave D2 — signed arbitrary-range audit export. Body
        // carries `from_ms`, `until_ms`, optional `client_id`.
        // Uses the fleet audit fetcher to collect events across
        // every accepted agent and produces a tamper-proof
        // bundle: the raw events + an HMAC manifest (same
        // signing secret as the monthly bundles). Consumers
        // verify by recomputing the HMAC of the event body.
        .route(
            "/api/v1/audit/export",
            axum::routing::post(post_audit_export),
        )
        .route("/api/v1/export/bundle", get(get_export_bundle))
        .route("/api/v1/archive/health", get(get_archive_health))
        .route("/api/v1/sentiment/snapshot", get(get_sentiment_snapshot))
        .route("/api/v1/sentiment/history", get(get_sentiment_history))
        .route("/api/v1/strategy/catalog", get(get_strategy_catalog))
        .route("/api/v1/strategy/graphs", get(list_strategy_graphs))
        .route("/api/v1/strategy/graphs/{name}", get(get_strategy_graph))
        .route("/api/v1/strategy/deploys", get(list_strategy_deploys))
        .route("/api/v1/strategy/active", get(list_strategy_active))
        .route("/api/v1/strategy/templates", get(list_strategy_templates))
        .route(
            "/api/v1/strategy/templates/{name}",
            get(get_strategy_template),
        )
        .route(
            "/api/v1/strategy/graphs/{name}/history/{hash}",
            get(get_strategy_graph_version),
        )
        .route(
            "/api/v1/strategy/preview",
            axum::routing::post(preview_strategy_graph),
        )
        .route(
            "/api/v1/strategy/validate",
            axum::routing::post(validate_strategy_graph),
        )
        .route(
            "/api/v1/strategy/custom_templates",
            get(list_custom_templates).post(save_custom_template),
        )
        .route(
            "/api/v1/strategy/custom_templates/{name}",
            get(get_custom_template).delete(delete_custom_template),
        )
        .route(
            "/api/v1/strategy/custom_templates/{name}/versions/{hash}",
            get(get_custom_template_version),
        )
        .route("/api/v1/loans", get(get_loans))
        .route("/api/v1/loans/{symbol}", get(get_loan_by_symbol))
        .route(
            "/api/v1/portfolio/correlation",
            get(get_portfolio_correlation),
        )
        .route("/api/v1/portfolio/risk", get(get_portfolio_risk))
        .route("/api/v1/client/{id}/sla", get(get_client_sla))
        .route(
            "/api/v1/client/{id}/sla/certificate",
            get(get_client_sla_certificate),
        )
        .route("/api/v1/client/{id}/fills", get(get_client_fills))
        .route("/api/v1/client/{id}/pnl", get(get_client_pnl))
        // Wave E3 — self-scope aliases for ClientReader. Each
        // resolves the caller's client_id from TokenClaims and
        // delegates to the /{id} handler. Requires a tenant-
        // tagged token; untenanted access returns 401.
        .route("/api/v1/client/self/sla", get(get_self_sla))
        .route(
            "/api/v1/client/self/sla/certificate",
            get(get_self_sla_certificate),
        )
        .route("/api/v1/client/self/fills", get(get_self_fills))
        .route("/api/v1/client/self/pnl", get(get_self_pnl))
        .route(
            "/api/v1/client/self/webhook-deliveries",
            get(get_self_webhook_deliveries),
        )
        // Wave I1 — tenant-self webhook CRUD. Tenants register,
        // list, remove, and test-fire their own delivery URLs
        // without admin involvement. Dispatcher is auto-created
        // on first add so clients onboarded without a webhook
        // can opt in any time.
        .route(
            "/api/v1/client/self/webhooks",
            get(list_self_webhooks)
                .post(add_self_webhook)
                .delete(remove_self_webhook),
        )
        .route(
            "/api/v1/client/self/webhooks/test",
            axum::routing::post(test_self_webhook),
        )
        // Wave G2/G4 — incident lifecycle.
        .route(
            "/api/v1/incidents",
            get(list_incidents_handler).post(post_incident_handler),
        )
        .route(
            "/api/v1/incidents/{id}/ack",
            axum::routing::post(ack_incident_handler),
        )
        .route(
            "/api/v1/incidents/{id}/resolve",
            axum::routing::post(resolve_incident_handler),
        )
}

/// Unified multi-currency portfolio snapshot in the reporting
/// currency. Returns `null` when `mm-portfolio` is not wired
/// (operator did not call `MarketMakerEngine::with_portfolio`)
/// or before the first summary tick has pushed a snapshot.
async fn get_portfolio(
    State(state): State<DashboardState>,
) -> Json<Option<mm_portfolio::PortfolioSnapshot>> {
    Json(state.get_portfolio())
}

/// Position per symbol.
#[derive(Debug, Serialize)]
struct PositionResponse {
    symbol: String,
    inventory: Decimal,
    inventory_value: Decimal,
    avg_entry_price: Decimal,
    unrealized_pnl: Decimal,
    realized_pnl: Decimal,
}

async fn get_positions(State(state): State<DashboardState>) -> Json<Vec<PositionResponse>> {
    let rows = fleet_client_metrics(&state, None).await;
    let positions: Vec<PositionResponse> = rows
        .iter()
        .map(|r| {
            let inventory = dec(r, "inventory");
            let inventory_value = dec(r, "inventory_value");
            let pnl_spread = dec(r, "pnl_spread");
            let pnl_rebates = dec(r, "pnl_rebates");
            let pnl_fees = dec(r, "pnl_fees");
            PositionResponse {
                symbol: str_field(r, "symbol"),
                inventory,
                inventory_value,
                avg_entry_price: if inventory.is_zero() {
                    dec!(0)
                } else {
                    inventory_value / inventory.abs()
                },
                unrealized_pnl: dec(r, "pnl_inventory"),
                realized_pnl: pnl_spread + pnl_rebates - pnl_fees,
            }
        })
        .collect();
    Json(positions)
}

/// PnL breakdown.
#[derive(Debug, Serialize)]
struct PnlResponse {
    total: Decimal,
    spread_capture: Decimal,
    inventory_pnl: Decimal,
    rebate_income: Decimal,
    fees_paid: Decimal,
    round_trips: u64,
    volume: Decimal,
    efficiency_bps: Decimal,
}

/// 23-UX-3 — per-leg PnL attribution. One row per
/// (venue, symbol, product) combining the leg's SymbolState
/// with its PnL breakdown. Answers "when total PnL moved -$100,
/// which leg caused it?" without the operator having to scrape
/// multiple endpoints.
#[derive(Debug, Serialize)]
struct PnlPerLegRow {
    venue: String,
    symbol: String,
    product: String,
    mode: String,
    strategy: String,
    total: Decimal,
    spread_capture: Decimal,
    inventory_pnl: Decimal,
    rebate_income: Decimal,
    fees_paid: Decimal,
    volume: Decimal,
    round_trips: u64,
    fills: u64,
    /// Effective PnL per $ of quote-currency volume (bps).
    /// Helps spot which leg is underperforming on capital
    /// efficiency.
    efficiency_bps: Decimal,
}

async fn get_pnl_per_leg(State(state): State<DashboardState>) -> Json<Vec<PnlPerLegRow>> {
    let metrics = fleet_client_metrics(&state, None).await;
    let rows: Vec<PnlPerLegRow> = metrics
        .iter()
        .map(|r| {
            let total = dec(r, "pnl_total");
            let volume = dec(r, "pnl_volume");
            let efficiency = if volume > dec!(0) {
                total / volume * dec!(10_000)
            } else {
                dec!(0)
            };
            PnlPerLegRow {
                venue: str_field(r, "venue"),
                symbol: str_field(r, "symbol"),
                product: str_field(r, "product"),
                mode: str_field(r, "mode"),
                strategy: str_field(r, "strategy"),
                total,
                spread_capture: dec(r, "pnl_spread"),
                inventory_pnl: dec(r, "pnl_inventory"),
                rebate_income: dec(r, "pnl_rebates"),
                fees_paid: dec(r, "pnl_fees"),
                volume,
                round_trips: u64_field(r, "pnl_round_trips"),
                fills: u64_field(r, "total_fills"),
                efficiency_bps: efficiency,
            }
        })
        .collect();
    Json(rows)
}

async fn get_pnl(State(state): State<DashboardState>) -> Json<Vec<PnlResponse>> {
    let metrics = fleet_client_metrics(&state, None).await;
    let pnl: Vec<PnlResponse> = metrics
        .iter()
        .map(|r| {
            let total = dec(r, "pnl_total");
            let volume = dec(r, "pnl_volume");
            let efficiency = if volume > dec!(0) {
                total / volume * dec!(10_000)
            } else {
                dec!(0)
            };
            PnlResponse {
                total,
                spread_capture: dec(r, "pnl_spread"),
                inventory_pnl: dec(r, "pnl_inventory"),
                rebate_income: dec(r, "pnl_rebates"),
                fees_paid: dec(r, "pnl_fees"),
                round_trips: u64_field(r, "pnl_round_trips"),
                volume,
                efficiency_bps: efficiency,
            }
        })
        .collect();
    Json(pnl)
}

/// SLA compliance.
#[derive(Debug, Serialize)]
struct SlaResponse {
    symbol: String,
    uptime_pct: Decimal,
    is_compliant: bool,
    current_spread_bps: Decimal,
    bid_depth: Decimal,
    ask_depth: Decimal,
    sla_max_spread_bps: Decimal,
    sla_min_depth_quote: Decimal,
    spread_compliance_pct: Decimal,
    presence_pct_24h: Decimal,
    two_sided_pct_24h: Decimal,
}

async fn get_sla(State(state): State<DashboardState>) -> Json<Vec<SlaResponse>> {
    let metrics = fleet_client_metrics(&state, None).await;
    let sla: Vec<SlaResponse> = metrics
        .iter()
        .map(|r| {
            let uptime = dec(r, "sla_uptime_pct");
            SlaResponse {
                symbol: str_field(r, "symbol"),
                uptime_pct: uptime,
                is_compliant: uptime >= dec!(95),
                current_spread_bps: dec(r, "spread_bps"),
                bid_depth: dec(r, "bid_depth_quote"),
                ask_depth: dec(r, "ask_depth_quote"),
                sla_max_spread_bps: dec(r, "sla_max_spread_bps"),
                sla_min_depth_quote: dec(r, "sla_min_depth_quote"),
                spread_compliance_pct: dec(r, "spread_compliance_pct"),
                presence_pct_24h: dec(r, "presence_pct_24h"),
                two_sided_pct_24h: dec(r, "two_sided_pct_24h"),
            }
        })
        .collect();
    Json(sla)
}

/// Daily performance report.
#[derive(Debug, Serialize)]
struct DailyReport {
    date: String,
    symbols: Vec<SymbolDailyReport>,
    total_pnl: Decimal,
    total_volume: Decimal,
    total_fills: u64,
}

#[derive(Debug, Serialize)]
struct SymbolDailyReport {
    symbol: String,
    pnl: Decimal,
    volume: Decimal,
    fills: u64,
    avg_spread_bps: Decimal,
    uptime_pct: Decimal,
    max_inventory: Decimal,
    /// P2.2 — per-pair daily presence rolled up from the 1440
    /// per-minute SLA buckets. Refreshed on every dashboard
    /// state push and reset at UTC midnight inside the engine's
    /// `SlaTracker`.
    presence_pct_24h: Decimal,
    /// Two-sided-only daily presence percentage (some MM
    /// rebate agreements pay against this independently of the
    /// spread floor).
    two_sided_pct_24h: Decimal,
    /// Minutes today with any samples — distinguishes a fresh
    /// engine ("100 % over 0 minutes") from a steady-state
    /// one. Audit teams consume this alongside `presence_pct`.
    minutes_with_data_24h: u32,
}

async fn get_daily_report(State(state): State<DashboardState>) -> Json<DailyReport> {
    let symbols = state.get_all();
    let mut total_pnl = dec!(0);
    let mut total_volume = dec!(0);
    let mut total_fills = 0u64;

    let sym_reports: Vec<SymbolDailyReport> = symbols
        .iter()
        .map(|s| {
            total_pnl += s.pnl.total;
            total_volume += s.pnl.volume;
            total_fills += s.pnl.round_trips;
            SymbolDailyReport {
                symbol: s.symbol.clone(),
                pnl: s.pnl.total,
                volume: s.pnl.volume,
                fills: s.pnl.round_trips,
                avg_spread_bps: s.spread_bps,
                uptime_pct: s.sla_uptime_pct,
                max_inventory: s.inventory.abs(),
                presence_pct_24h: s.presence_pct_24h,
                two_sided_pct_24h: s.two_sided_pct_24h,
                minutes_with_data_24h: s.minutes_with_data_24h,
            }
        })
        .collect();

    Json(DailyReport {
        date: Utc::now().format("%Y-%m-%d").to_string(),
        symbols: sym_reports,
        total_pnl,
        total_volume,
        total_fills,
    })
}

/// Performance metrics per symbol (Sharpe, Sortino, drawdown, etc.).
#[derive(Debug, Serialize)]
struct PerformanceResponse {
    symbol: String,
    #[serde(flatten)]
    metrics: mm_risk::performance::PerformanceMetrics,
}

async fn get_performance(State(state): State<DashboardState>) -> Json<Vec<PerformanceResponse>> {
    let symbols = state.get_all();
    let reports: Vec<PerformanceResponse> = symbols
        .iter()
        .filter_map(|s| {
            s.performance.as_ref().map(|m| PerformanceResponse {
                symbol: s.symbol.clone(),
                metrics: m.clone(),
            })
        })
        .collect();
    Json(reports)
}

/// Market impact report per symbol.
#[derive(Debug, Serialize)]
struct MarketImpactResponse {
    symbol: String,
    #[serde(flatten)]
    report: mm_risk::market_impact::MarketImpactReport,
}

async fn get_market_impact(State(state): State<DashboardState>) -> Json<Vec<MarketImpactResponse>> {
    let symbols = state.get_all();
    let reports: Vec<MarketImpactResponse> = symbols
        .iter()
        .filter_map(|s| {
            s.market_impact.as_ref().map(|r| MarketImpactResponse {
                symbol: s.symbol.clone(),
                report: r.clone(),
            })
        })
        .collect();
    Json(reports)
}

/// Per-hour SLA breakdown for time-of-day analysis.
#[derive(Debug, Serialize)]
struct HourlySlaReport {
    symbol: String,
    hours: Vec<mm_risk::sla::HourlyPresenceSummary>,
}

async fn get_sla_hourly(State(state): State<DashboardState>) -> Json<Vec<HourlySlaReport>> {
    let symbols = state.get_all();
    let reports: Vec<HourlySlaReport> = symbols
        .iter()
        .map(|s| HourlySlaReport {
            symbol: s.symbol.clone(),
            hours: s.hourly_presence.clone(),
        })
        .collect();
    Json(reports)
}

/// Query params for fills endpoint.
#[derive(Debug, Deserialize)]
struct FillsQuery {
    symbol: Option<String>,
    limit: Option<usize>,
}

/// Recent fills with NBBO snapshot and slippage.
async fn get_recent_fills(
    State(state): State<DashboardState>,
    Query(query): Query<FillsQuery>,
) -> Json<Vec<FillRecord>> {
    let limit = query.limit.unwrap_or(100).min(1000);
    let fills = state.get_recent_fills(query.symbol.as_deref(), limit);
    Json(fills)
}

/// Execution quality / slippage summary across all recent fills.
#[derive(Debug, Serialize)]
struct SlippageReport {
    total_fills: usize,
    maker_fills: usize,
    taker_fills: usize,
    avg_slippage_bps: Decimal,
    p50_slippage_bps: Decimal,
    p95_slippage_bps: Decimal,
    p99_slippage_bps: Decimal,
    total_fees: Decimal,
    total_rebates: Decimal,
    /// Average fill price improvement vs mid (negative = we
    /// beat the mid; positive = we filled worse).
    avg_price_improvement_bps: Decimal,
}

async fn get_slippage_report(
    State(state): State<DashboardState>,
    Query(query): Query<FillsQuery>,
) -> Json<SlippageReport> {
    let fills = state.get_recent_fills(query.symbol.as_deref(), 1000);
    if fills.is_empty() {
        return Json(SlippageReport {
            total_fills: 0,
            maker_fills: 0,
            taker_fills: 0,
            avg_slippage_bps: dec!(0),
            p50_slippage_bps: dec!(0),
            p95_slippage_bps: dec!(0),
            p99_slippage_bps: dec!(0),
            total_fees: dec!(0),
            total_rebates: dec!(0),
            avg_price_improvement_bps: dec!(0),
        });
    }

    let total = fills.len();
    let maker = fills.iter().filter(|f| f.is_maker).count();
    let taker = total - maker;
    let total_fees: Decimal = fills.iter().filter(|f| !f.is_maker).map(|f| f.fee).sum();
    let total_rebates: Decimal = fills.iter().filter(|f| f.is_maker).map(|f| f.fee).sum();

    let mut slippages: Vec<Decimal> = fills.iter().map(|f| f.slippage_bps).collect();
    slippages.sort();
    let avg = slippages.iter().sum::<Decimal>() / Decimal::from(total as u64);
    let p50 = slippages[total / 2];
    let p95 = slippages[(total as f64 * 0.95) as usize];
    let p99 = slippages[((total as f64 * 0.99) as usize).min(total - 1)];

    Json(SlippageReport {
        total_fills: total,
        maker_fills: maker,
        taker_fills: taker,
        avg_slippage_bps: avg,
        p50_slippage_bps: p50,
        p95_slippage_bps: p95,
        p99_slippage_bps: p99,
        total_fees,
        total_rebates,
        avg_price_improvement_bps: -avg, // negative slippage = improvement
    })
}

/// SLA compliance certificate — structured proof of MM
/// performance for token projects and exchange audit teams.
#[derive(Debug, Serialize)]
struct SlaCertificate {
    /// ISO 8601 generation timestamp.
    generated_at: String,
    /// Period covered (always "current session" for v1; v2
    /// will support arbitrary date ranges from persisted data).
    period: String,
    symbols: Vec<SymbolSlaCertificate>,
    /// Overall compliance verdict.
    overall_compliant: bool,
    /// HMAC-SHA256 signature of the certificate body for
    /// tamper detection. Key is `MM_AUTH_SECRET` env var.
    signature: String,
}

#[derive(Debug, Serialize)]
struct SymbolSlaCertificate {
    symbol: String,
    /// Presence % (full SLA compliance: two-sided, spread
    /// within limit, depth within limit).
    presence_pct: Decimal,
    /// Two-sided presence % (independent metric).
    two_sided_pct: Decimal,
    /// Spread compliance % (ignoring depth requirement).
    spread_compliance_pct: Decimal,
    /// Minutes with at least one observation.
    minutes_observed: u32,
    /// Configured SLA max spread (bps).
    sla_max_spread_bps: Decimal,
    /// Configured SLA min depth (quote asset).
    sla_min_depth_quote: Decimal,
    /// Total trading volume (quote asset).
    volume: Decimal,
    /// Total fills count.
    fills: u64,
    /// Whether this symbol individually meets the SLA.
    is_compliant: bool,
}

async fn get_sla_certificate(State(state): State<DashboardState>) -> Json<SlaCertificate> {
    let symbols = state.get_all();
    let now = Utc::now();

    let sym_certs: Vec<SymbolSlaCertificate> = symbols
        .iter()
        .map(|s| {
            let is_compliant = s.presence_pct_24h >= dec!(95);
            SymbolSlaCertificate {
                symbol: s.symbol.clone(),
                presence_pct: s.presence_pct_24h,
                two_sided_pct: s.two_sided_pct_24h,
                spread_compliance_pct: s.spread_compliance_pct,
                minutes_observed: s.minutes_with_data_24h,
                sla_max_spread_bps: s.sla_max_spread_bps,
                sla_min_depth_quote: s.sla_min_depth_quote,
                volume: s.pnl.volume,
                fills: s.pnl.round_trips,
                is_compliant,
            }
        })
        .collect();

    let overall_compliant = sym_certs.iter().all(|s| s.is_compliant);

    // Generate signature from certificate body.
    let body = format!(
        "{}:{}:{}",
        now.to_rfc3339(),
        overall_compliant,
        sym_certs.len()
    );
    let secret = std::env::var("MM_AUTH_SECRET").unwrap_or_else(|_| "default".to_string());
    let signature = hmac_sha256_hex(&secret, &body);

    Json(SlaCertificate {
        generated_at: now.to_rfc3339(),
        period: format!("{} (current session)", now.format("%Y-%m-%d")),
        symbols: sym_certs,
        overall_compliant,
        signature,
    })
}

/// Fix #1 — honest HMAC-SHA256 over the message with the
/// configured signing secret as key. Previous implementation
/// was `DefaultHasher` (SipHash13) keyed by hashing the key
/// and message sequentially — not HMAC, not SHA-256, not
/// cryptographically sound. Regulators / auditors verifying
/// a signed manifest could not reproduce the signature;
/// `monthly_report.rs` already used real HMAC, the rest of
/// the compliance surface now matches. Hex-encoded 64 chars.
fn hmac_sha256_hex(key: &str, message: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC-SHA256 accepts any key length");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Wave D2 — signed arbitrary-range audit export body.
#[derive(Debug, Deserialize)]
struct AuditExportRequest {
    from_ms: i64,
    until_ms: i64,
    #[serde(default)]
    client_id: Option<String>,
    /// Event cap. Default 10_000, hard cap 100_000 to stop a
    /// single request nuking the controller's memory.
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct AuditExportManifest {
    generated_at: String,
    from_ms: i64,
    until_ms: i64,
    client_id: Option<String>,
    event_count: usize,
    /// Hex-encoded HMAC of the events array serialised as a
    /// canonical JSON string. Recompute + compare to detect
    /// tampering after the bundle leaves the controller.
    signature: String,
    signature_algo: String,
    /// Byte length of the events array (as JSON) — helps the
    /// consumer sanity-check the transfer before verifying.
    body_bytes: usize,
}

#[derive(Debug, Serialize)]
struct AuditExportBundle {
    manifest: AuditExportManifest,
    events: Vec<serde_json::Value>,
}

async fn post_audit_export(
    State(state): State<DashboardState>,
    Json(req): Json<AuditExportRequest>,
) -> Result<Json<AuditExportBundle>, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    if req.from_ms > req.until_ms {
        return Err((
            StatusCode::BAD_REQUEST,
            "from_ms must be <= until_ms".into(),
        ));
    }
    let limit = req.limit.unwrap_or(10_000).min(100_000);

    // Fleet-aware path: reuse the existing audit-range fetcher
    // installed at server boot. Falls back to the local file
    // reader when the controller has no fleet wired (tests).
    let events: Vec<serde_json::Value> = if let Some(fetcher) = state.audit_range_fetcher() {
        fetcher(req.from_ms, req.until_ms, limit).await
    } else if let Some(path) = state.audit_log_path() {
        let from_dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(req.from_ms)
            .ok_or((StatusCode::BAD_REQUEST, "from_ms out of range".into()))?;
        let until_dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(req.until_ms)
            .ok_or((StatusCode::BAD_REQUEST, "until_ms out of range".into()))?;
        mm_risk::audit_reader::read_audit_range(&path, from_dt, until_dt)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .into_iter()
            .filter_map(|ev| serde_json::to_value(ev).ok())
            .collect()
    } else {
        Vec::new()
    };

    // Filter by client_id when requested. Audit events carry
    // `client_id` as an optional field — empty when the event
    // is tenant-less (shared infra) so we skip those on
    // client-scoped exports.
    let filtered: Vec<serde_json::Value> = match req.client_id.as_deref() {
        Some(cid) if !cid.is_empty() => events
            .into_iter()
            .filter(|ev| {
                ev.get("client_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s == cid)
                    .unwrap_or(false)
            })
            .collect(),
        _ => events,
    };

    let body = serde_json::to_string(&filtered)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let secret_bytes = state.report_secret();
    let secret = String::from_utf8(secret_bytes).unwrap_or_default();
    let signature = hmac_sha256_hex(&secret, &body);

    Ok(Json(AuditExportBundle {
        manifest: AuditExportManifest {
            generated_at: Utc::now().to_rfc3339(),
            from_ms: req.from_ms,
            until_ms: req.until_ms,
            client_id: req.client_id,
            event_count: filtered.len(),
            signature,
            signature_algo: "hmac-sha256-hex".into(),
            body_bytes: body.len(),
        },
        events: filtered,
    }))
}

/// Unified risk summary — all risk metrics in one endpoint.
/// Token projects and risk committees consume this for a
/// single-pane view of MM health.
#[derive(Debug, Serialize)]
struct RiskSummary {
    symbol: String,
    // Position risk.
    inventory: Decimal,
    inventory_value: Decimal,
    max_inventory_pct: Decimal,
    // Kill switch.
    kill_level: u8,
    // Spread risk.
    spread_bps: Decimal,
    spread_compliance_pct: Decimal,
    // Toxicity.
    vpin: Decimal,
    kyle_lambda: Decimal,
    adverse_bps: Decimal,
    // Market quality.
    market_resilience: Decimal,
    order_to_trade_ratio: Decimal,
    // Performance.
    total_pnl: Decimal,
    sharpe_ratio: Option<Decimal>,
    max_drawdown: Option<Decimal>,
    // Impact.
    mean_impact_bps: Option<Decimal>,
    adverse_fill_pct: Option<Decimal>,
}

async fn get_risk_summary(State(state): State<DashboardState>) -> Json<Vec<RiskSummary>> {
    let symbols = state.get_all();
    Json(
        symbols
            .iter()
            .map(|s| RiskSummary {
                symbol: s.symbol.clone(),
                inventory: s.inventory,
                inventory_value: s.inventory_value,
                max_inventory_pct: if s.mid_price > Decimal::ZERO {
                    s.inventory.abs() * s.mid_price / s.inventory_value.abs().max(dec!(1))
                        * dec!(100)
                } else {
                    Decimal::ZERO
                },
                kill_level: s.kill_level,
                spread_bps: s.spread_bps,
                spread_compliance_pct: s.spread_compliance_pct,
                vpin: s.vpin,
                kyle_lambda: s.kyle_lambda,
                adverse_bps: s.adverse_bps,
                market_resilience: s.market_resilience,
                order_to_trade_ratio: s.order_to_trade_ratio,
                total_pnl: s.pnl.total,
                sharpe_ratio: s.performance.as_ref().map(|p| p.sharpe_ratio),
                max_drawdown: s.performance.as_ref().map(|p| p.max_drawdown_pct),
                mean_impact_bps: s.market_impact.as_ref().map(|m| m.mean_impact_bps),
                adverse_fill_pct: s.market_impact.as_ref().map(|m| m.adverse_fill_pct),
            })
            .collect(),
    )
}

/// Trade flow analysis — buy/sell pressure across symbols.
#[derive(Debug, Serialize)]
struct TradeFlowSummary {
    symbol: String,
    total_fills: u64,
    total_volume: Decimal,
    /// Net buy/sell ratio from fills. >0.5 = more buys, <0.5 = more sells.
    net_flow_ratio: Decimal,
    /// Spread PnL (positive = capturing spread effectively).
    spread_pnl: Decimal,
    /// Inventory PnL (positive = inventory moved in our favor).
    inventory_pnl: Decimal,
    /// Fee income (rebates - fees).
    net_fee_income: Decimal,
    /// Round trips completed (full buy-sell cycles).
    round_trips: u64,
    /// Volume per round trip.
    volume_per_trip: Decimal,
}

async fn get_trade_flow(State(state): State<DashboardState>) -> Json<Vec<TradeFlowSummary>> {
    let symbols = state.get_all();
    Json(
        symbols
            .iter()
            .map(|s| {
                let vol_per_trip = if s.pnl.round_trips > 0 {
                    s.pnl.volume / Decimal::from(s.pnl.round_trips)
                } else {
                    Decimal::ZERO
                };
                TradeFlowSummary {
                    symbol: s.symbol.clone(),
                    total_fills: s.total_fills,
                    total_volume: s.pnl.volume,
                    net_flow_ratio: dec!(0.5), // Would need per-side tracking
                    spread_pnl: s.pnl.spread,
                    inventory_pnl: s.pnl.inventory,
                    net_fee_income: s.pnl.rebates - s.pnl.fees,
                    round_trips: s.pnl.round_trips,
                    volume_per_trip: vol_per_trip,
                }
            })
            .collect(),
    )
}

/// System diagnostics — version, uptime, symbol count.
#[derive(Debug, Serialize)]
struct DiagnosticsResponse {
    version: String,
    uptime_secs: i64,
    started_at: String,
    active_symbols: usize,
    total_fills: u64,
    total_volume: Decimal,
    config_channels: usize,
    webhook_urls: usize,
    alert_rules: usize,
}

async fn get_diagnostics(State(state): State<DashboardState>) -> Json<DiagnosticsResponse> {
    let symbols = state.get_all();
    let total_fills: u64 = symbols.iter().map(|s| s.pnl.round_trips).sum();
    let total_volume: Decimal = symbols.iter().map(|s| s.pnl.volume).sum();
    let started = state.started_at();
    let uptime = (Utc::now() - started).num_seconds();
    let wh_urls = state
        .webhook_dispatcher()
        .map(|w| w.url_count())
        .unwrap_or(0);

    Json(DiagnosticsResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: uptime,
        started_at: started.to_rfc3339(),
        active_symbols: symbols.len(),
        total_fills,
        total_volume,
        config_channels: state.config_symbols().len(),
        webhook_urls: wh_urls,
        alert_rules: state.get_alert_rules().len(),
    })
}

/// UX-5 — read-only snapshot of the effective `AppConfig` the
/// server booted with. The response is the config struct
/// serialised verbatim; secrets never land in the struct (they
/// come from env), so this is safe to expose to the operator UI.
async fn get_config_snapshot(State(state): State<DashboardState>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match state.app_config() {
        Some(cfg) => {
            let val = serde_json::to_value(&*cfg).unwrap_or(serde_json::Value::Null);
            (StatusCode::OK, Json(val)).into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "config snapshot not registered yet",
            })),
        )
            .into_response(),
    }
}

// ── A1 — MiCA monthly report on-demand ─────────────────────
//
// Query params: `from=YYYY-MM-DD` (required), `to=YYYY-MM-DD`
// (required), `client_id=<id>` (optional; when omitted the
// bundle contains every symbol the dashboard knows about).
//
// The four body endpoints all share one aggregation step and
// differ only in how the body is rendered (JSON / CSV / XLSX /
// PDF). The `.manifest` endpoint returns the HMAC-signed
// manifest separately so auditors can verify counts + signature
// without downloading the bulk body first.

#[derive(Debug, Deserialize)]
struct MonthlyQuery {
    from: String,
    to: String,
    #[serde(default)]
    client_id: Option<String>,
}

fn parse_period(q: &MonthlyQuery) -> Result<(chrono::NaiveDate, chrono::NaiveDate), String> {
    let from = chrono::NaiveDate::parse_from_str(&q.from, "%Y-%m-%d")
        .map_err(|e| format!("bad from: {e}"))?;
    let to =
        chrono::NaiveDate::parse_from_str(&q.to, "%Y-%m-%d").map_err(|e| format!("bad to: {e}"))?;
    Ok((from, to))
}

fn build_report(
    state: &DashboardState,
    q: &MonthlyQuery,
) -> Result<crate::report_export::MonthlyReportData, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    let (from, to) = parse_period(q).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let audit_path = state.audit_log_path();
    let client_name = q.client_id.as_deref().unwrap_or("all");
    crate::monthly_report::build_monthly_report(
        state,
        q.client_id.as_deref(),
        client_name,
        from,
        to,
        audit_path.as_deref(),
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

async fn get_monthly_json(
    State(state): State<DashboardState>,
    Query(q): Query<MonthlyQuery>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match build_report(&state, &q) {
        Ok(data) => (StatusCode::OK, Json(data)).into_response(),
        Err((code, msg)) => (code, msg).into_response(),
    }
}

async fn get_monthly_csv(
    State(state): State<DashboardState>,
    Query(q): Query<MonthlyQuery>,
) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    match build_report(&state, &q) {
        Ok(data) => {
            let body = crate::report_export::render_csv(&data);
            let filename = format!(
                "monthly-{}-{}-to-{}.csv",
                data.client_id, data.period_from, data.period_to
            );
            (
                [
                    (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                    (
                        header::CONTENT_DISPOSITION,
                        &format!("attachment; filename=\"{filename}\""),
                    ),
                ],
                body,
            )
                .into_response()
        }
        Err((code, msg)) => (code, msg).into_response(),
    }
}

async fn get_monthly_xlsx(
    State(state): State<DashboardState>,
    Query(q): Query<MonthlyQuery>,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    match build_report(&state, &q) {
        Ok(data) => {
            let secret = state.report_secret();
            let manifest = match crate::report_export::build_manifest(&data, &["xlsx"], &secret) {
                Ok(m) => m,
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                }
            };
            let bytes = match crate::report_export::render_xlsx(&data, &manifest) {
                Ok(b) => b,
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                }
            };
            let filename = format!(
                "monthly-{}-{}-to-{}.xlsx",
                data.client_id, data.period_from, data.period_to
            );
            (
                [
                    (
                        header::CONTENT_TYPE,
                        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    ),
                    (
                        header::CONTENT_DISPOSITION,
                        &format!("attachment; filename=\"{filename}\""),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err((code, msg)) => (code, msg).into_response(),
    }
}

async fn get_monthly_pdf(
    State(state): State<DashboardState>,
    Query(q): Query<MonthlyQuery>,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    match build_report(&state, &q) {
        Ok(data) => {
            let secret = state.report_secret();
            let manifest = match crate::report_export::build_manifest(&data, &["pdf"], &secret) {
                Ok(m) => m,
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                }
            };
            let bytes = match crate::pdf_report::render_pdf(&data, &manifest) {
                Ok(b) => b,
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                }
            };
            let filename = format!(
                "monthly-{}-{}-to-{}.pdf",
                data.client_id, data.period_from, data.period_to
            );
            (
                [
                    (header::CONTENT_TYPE, "application/pdf"),
                    (
                        header::CONTENT_DISPOSITION,
                        &format!("attachment; filename=\"{filename}\""),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err((code, msg)) => (code, msg).into_response(),
    }
}

// ── Epic H — strategy graph read endpoints ─────────────────

/// Node catalog snapshot — the UI palette reads this to render draggable
/// nodes with their typed port declarations.
#[derive(Serialize)]
struct CatalogEntry {
    kind: String,
    label: String,
    summary: String,
    group: String,
    inputs: Vec<CatalogPort>,
    outputs: Vec<CatalogPort>,
    restricted: bool,
    /// Schema-driven config form — the frontend renders one input per
    /// entry in this vec automatically. Empty for nodes with no
    /// parameters.
    config_schema: Vec<mm_strategy_graph::ConfigField>,
}
#[derive(Serialize)]
struct CatalogPort {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

async fn get_strategy_catalog() -> Json<Vec<CatalogEntry>> {
    let entries = mm_strategy_graph::catalog::kinds()
        .into_iter()
        .map(|(kind, shape)| {
            let m = mm_strategy_graph::catalog::meta(kind);
            // Build the node with a null config just to read its
            // schema — every built-in kind accepts `null` here
            // (configless nodes ignore it, configurable ones fall
            // back on their Default).
            let schema = mm_strategy_graph::catalog::build(kind, &serde_json::Value::Null)
                .map(|n| n.config_schema())
                .unwrap_or_default();
            CatalogEntry {
                kind: kind.to_string(),
                label: m.label.to_string(),
                summary: m.summary.to_string(),
                group: m.group.to_string(),
                inputs: shape
                    .inputs
                    .into_iter()
                    .map(|(name, ty)| CatalogPort {
                        name,
                        ty: format!("{ty:?}"),
                    })
                    .collect(),
                outputs: shape
                    .outputs
                    .into_iter()
                    .map(|(name, ty)| CatalogPort {
                        name,
                        ty: format!("{ty:?}"),
                    })
                    .collect(),
                restricted: shape.restricted,
                config_schema: schema,
            }
        })
        .collect();
    Json(entries)
}

async fn list_strategy_graphs(State(state): State<DashboardState>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(store) = state.strategy_graph_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "strategy graphs not configured" })),
        )
            .into_response();
    };
    match store.list() {
        Ok(names) => (StatusCode::OK, Json(names)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_strategy_graph(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(store) = state.strategy_graph_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "strategy graphs not configured",
        )
            .into_response();
    };
    match store.load(&name) {
        Ok(g) => (StatusCode::OK, Json(g)).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

#[derive(Serialize)]
struct TemplateMeta {
    name: String,
    description: String,
}

async fn list_strategy_templates() -> Json<Vec<TemplateMeta>> {
    let list = mm_strategy_graph::templates::list()
        .into_iter()
        .map(|t| TemplateMeta {
            name: t.name.to_string(),
            description: t.description.to_string(),
        })
        .collect();
    Json(list)
}

async fn get_strategy_template(Path(name): Path<String>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match mm_strategy_graph::templates::load(&name) {
        Some(Ok(g)) => (StatusCode::OK, Json(g)).into_response(),
        Some(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        None => (StatusCode::NOT_FOUND, "unknown template").into_response(),
    }
}

#[derive(Deserialize)]
struct PreviewRequest {
    graph: mm_strategy_graph::Graph,
    /// Optional per-(node, port) source overrides. Keys look like
    /// `"NODE_ID:PORT_NAME"`. Values are simple strings parsed as
    /// `Decimal` for Number ports, `"true" / "false"` for Bool, or
    /// passed through as strings otherwise.
    #[serde(default)]
    source_inputs: std::collections::HashMap<String, String>,
}

#[derive(Serialize)]
struct PreviewResponse {
    /// Per-edge values: key = `"NODE_ID:PORT_NAME"`, value =
    /// formatted value representation (`"7.5"`, `"true"`, `"—"`).
    edges: std::collections::HashMap<String, String>,
    /// Sinks that would fire. Operator reads this to confirm the
    /// graph behaves as expected before clicking Deploy.
    sinks: Vec<serde_json::Value>,
    /// Validation / evaluation errors, if any. Empty when OK.
    errors: Vec<String>,
}

// ─── Epic H Phase 4 — validate + user templates ─────────────

#[derive(Serialize)]
struct ValidateResponse {
    /// Canvas-wide "this graph will deploy" flag. Every front-end
    /// check the operator sees (the green Ready pill, the Deploy
    /// button enable state) is derived from here — server is the
    /// single source of truth so client-side rules can't drift.
    valid: bool,
    /// Human-readable breakdown of whatever prevents the deploy.
    /// Stays empty when `valid == true`. Never contains duplicates.
    issues: Vec<String>,
    /// Quick stats the UI turns into a status pill.
    node_count: usize,
    edge_count: usize,
    sink_count: usize,
    /// M3-GOBS — source kinds this graph actually references. The
    /// UI fades palette entries that are *not* in this list so
    /// operators see at a glance which detectors are dormant.
    /// Always emitted — never dropped on empty — so downstream
    /// `Array.isArray()` checks don't flake on a dense graph.
    required_sources: Vec<String>,
    /// M3-GOBS — nodes that have no path to any sink. Authoring
    /// error — deploy still proceeds but the UI shows a red
    /// dashed border on these and lists them as warnings.
    dead_nodes: Vec<mm_strategy_graph::NodeId>,
    /// M3-GOBS — output ports produced by a node but never
    /// consumed by any edge. Orange informational warning.
    unconsumed_outputs: Vec<(mm_strategy_graph::NodeId, String)>,
}

#[derive(Deserialize)]
struct ValidateRequest {
    graph: mm_strategy_graph::Graph,
}

async fn validate_strategy_graph(
    axum::Json(req): axum::Json<ValidateRequest>,
) -> Json<ValidateResponse> {
    let mut issues: Vec<String> = Vec::new();

    // Shape-level: compile via Evaluator::build. That covers unknown
    // kinds, port type mismatches, cycles, and the SpreadMult-sink
    // requirement in one call. We also hang onto the built evaluator
    // so the topology analysis can run off the same validated DAG —
    // the UI gets dead-node / unconsumed-output / required-source
    // diagnostics for free when the graph compiles cleanly.
    let evaluator = match mm_strategy_graph::Evaluator::build(&req.graph) {
        Ok(ev) => Some(ev),
        Err(e) => {
            // thiserror's Display form is the human-facing message
            // (`"graph contains no reachable Out.SpreadMult sink"`),
            // not the enum variant name. Keep it that way here so the
            // validation strip in the UI shows operator-readable text.
            issues.push(e.to_string());
            None
        }
    };

    // Dangling-edges audit. Evaluator::build tolerates missing inputs
    // (they propagate as Missing) — but an edge whose `from`/`to`
    // references a node that was deleted is a graph-state bug worth
    // surfacing before deploy, so a partial delete doesn't persist.
    let node_ids: std::collections::HashSet<_> = req.graph.nodes.iter().map(|n| n.id).collect();
    for e in &req.graph.edges {
        if !node_ids.contains(&e.from.node) {
            issues.push(format!("edge references missing node {}", e.from.node));
        }
        if !node_ids.contains(&e.to.node) {
            issues.push(format!("edge references missing node {}", e.to.node));
        }
    }

    let sink_count = req
        .graph
        .nodes
        .iter()
        .filter(|n| n.kind.starts_with("Out."))
        .count();

    // M3-GOBS — topology analysis when the graph compiles. Returns
    // empty on failure; the UI already shows the `issues` list in
    // that case and doesn't try to render these.
    let analysis = evaluator
        .as_ref()
        .map(|ev| ev.analyze(req.graph.content_hash()));

    let required_sources = analysis
        .as_ref()
        .map(|a| a.required_sources.clone())
        .unwrap_or_default();
    let dead_nodes = analysis
        .as_ref()
        .map(|a| a.dead_nodes.clone())
        .unwrap_or_default();
    let unconsumed_outputs = analysis
        .as_ref()
        .map(|a| a.unconsumed_outputs.clone())
        .unwrap_or_default();

    Json(ValidateResponse {
        valid: issues.is_empty(),
        issues,
        node_count: req.graph.nodes.len(),
        edge_count: req.graph.edges.len(),
        sink_count,
        required_sources,
        dead_nodes,
        unconsumed_outputs,
    })
}

// ─── User-authored templates (Phase 4 follow-up) ────────────

fn user_templates_dir(state: &DashboardState) -> Option<std::path::PathBuf> {
    let store = state.strategy_graph_store()?;
    Some(store.root().join("user_templates"))
}

#[derive(Serialize, Deserialize)]
struct CustomTemplateRecord {
    name: String,
    description: String,
    /// Canonical graph JSON. Stored alongside the record so a
    /// template is a single self-contained file on disk.
    graph: mm_strategy_graph::Graph,
    #[serde(default)]
    saved_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    saved_by: Option<String>,
}

#[derive(Serialize)]
struct CustomTemplateSummary {
    name: String,
    description: String,
    saved_at: Option<chrono::DateTime<chrono::Utc>>,
    saved_by: Option<String>,
    /// M-SAVE GOBS — latest graph content hash; helps the UI
    /// distinguish identical rename from actual edit, and
    /// prevents double-save of the same bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_hash: Option<String>,
    /// How many versions sit behind the latest. `0` means just
    /// the first save; useful for a "v{n+1}" label.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    version_count: usize,
}

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

/// M-SAVE GOBS — one line per version in `user_templates/<name>/history.jsonl`.
/// The canonical graph JSON for each version lives in
/// `<name>/<hash>.json`. Latest line of the jsonl is the
/// "current" version; `graph` is ALWAYS read from the hash file.
#[derive(Serialize, Deserialize, Clone)]
struct CustomTemplateVersion {
    hash: String,
    saved_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_by: Option<String>,
    #[serde(default)]
    description: String,
}

/// Full response for `GET /custom_templates/:name` — carries the
/// flat graph that the canvas needs (latest version) plus the
/// full history so the UI can render a version selector and
/// fetch specific versions without a second round-trip listing.
#[derive(Serialize)]
struct CustomTemplateFull {
    name: String,
    description: String,
    graph: mm_strategy_graph::Graph,
    saved_at: Option<chrono::DateTime<chrono::Utc>>,
    saved_by: Option<String>,
    /// Newest-first — `history[0].hash` matches the `graph`
    /// returned above. Legacy single-file templates surface as
    /// a one-entry history so the UI can treat everything
    /// uniformly.
    history: Vec<CustomTemplateVersion>,
}

/// Walk `user_templates/` and return one summary per template
/// name, honouring both layouts: the legacy flat-file
/// `<name>.json` and the versioned `<name>/history.jsonl`
/// directory. The newest version of each template feeds the
/// summary; the flat-file form surfaces as a 1-version
/// history so the UI never has to special-case it.
async fn list_custom_templates(
    State(state): State<DashboardState>,
) -> Json<Vec<CustomTemplateSummary>> {
    let Some(dir) = user_templates_dir(&state) else {
        return Json(vec![]);
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Json(vec![]);
    };
    let mut out: Vec<CustomTemplateSummary> = Vec::new();
    for e in entries.filter_map(|x| x.ok()) {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            // Legacy single-file template.
            if let Some(rec) = read_legacy_record(&p) {
                out.push(CustomTemplateSummary {
                    name: rec.name,
                    description: rec.description,
                    saved_at: rec.saved_at,
                    saved_by: rec.saved_by,
                    latest_hash: Some(rec.graph.content_hash()),
                    version_count: 0,
                });
            }
            continue;
        }
        if p.is_dir() {
            // Versioned layout: read history.jsonl, pick newest line.
            let name = match p.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let versions = read_history(&p);
            if let Some(latest) = versions.first() {
                out.push(CustomTemplateSummary {
                    name,
                    description: latest.description.clone(),
                    saved_at: Some(latest.saved_at),
                    saved_by: latest.saved_by.clone(),
                    latest_hash: Some(latest.hash.clone()),
                    version_count: versions.len().saturating_sub(1),
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Json(out)
}

/// Read `history.jsonl` for a template directory, newest-first.
/// One line per save. Silently skips malformed lines — a
/// corrupt middle line shouldn't brick the template's history.
fn read_history(template_dir: &std::path::Path) -> Vec<CustomTemplateVersion> {
    let hist_path = template_dir.join("history.jsonl");
    let Ok(raw) = std::fs::read_to_string(&hist_path) else {
        return Vec::new();
    };
    let mut versions: Vec<CustomTemplateVersion> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<CustomTemplateVersion>(l).ok())
        .collect();
    // Newest-first: history.jsonl is written append-only so
    // the last line on disk is the latest save.
    versions.reverse();
    versions
}

/// Read a legacy flat-file template if present. Returns None
/// when the file is absent, malformed, or missing fields.
fn read_legacy_record(path: &std::path::Path) -> Option<CustomTemplateRecord> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<CustomTemplateRecord>(&raw).ok()
}

#[derive(Deserialize)]
struct SaveCustomTemplate {
    name: String,
    #[serde(default)]
    description: String,
    graph: mm_strategy_graph::Graph,
}

async fn save_custom_template(
    State(state): State<DashboardState>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<SaveCustomTemplate>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(root) = user_templates_dir(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "strategy graphs not configured",
        )
            .into_response();
    };
    if !req
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (StatusCode::BAD_REQUEST, "name must be [A-Za-z0-9_-]+").into_response();
    }
    if req.name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    if let Err(e) = mm_strategy_graph::Evaluator::build(&req.graph) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "validate", "detail": e.to_string() })),
        )
            .into_response();
    }

    let template_dir = root.join(&req.name);
    let legacy_path = root.join(format!("{}.json", req.name));
    if let Err(e) = std::fs::create_dir_all(&template_dir) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {e}")).into_response();
    }

    // Lazy legacy migration: if the old flat file exists and the
    // versioned history hasn't been started yet, seed the history
    // with the legacy record as version 1. Keeps operator-saved
    // templates from silently losing their first version when the
    // new write path takes over.
    let history_path = template_dir.join("history.jsonl");
    if legacy_path.exists() && !history_path.exists() {
        if let Some(legacy) = read_legacy_record(&legacy_path) {
            let legacy_hash = legacy.graph.content_hash();
            let legacy_entry = CustomTemplateVersion {
                hash: legacy_hash.clone(),
                saved_at: legacy.saved_at.unwrap_or_else(chrono::Utc::now),
                saved_by: legacy.saved_by.clone(),
                description: legacy.description.clone(),
            };
            // Write the legacy graph under its hash + append the
            // history line, then drop the flat file to avoid a
            // double-read on next list.
            let legacy_graph_body = serde_json::to_string_pretty(&legacy.graph).unwrap_or_default();
            let _ = std::fs::write(
                template_dir.join(format!("{legacy_hash}.json")),
                legacy_graph_body,
            );
            if let Ok(line) = serde_json::to_string(&legacy_entry) {
                let _ = append_line(&history_path, &line);
            }
            let _ = std::fs::remove_file(&legacy_path);
        }
    }

    let saved_by = headers
        .get("X-MM-User")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let hash = req.graph.content_hash();
    let graph_path = template_dir.join(format!("{hash}.json"));
    // Dedup: if this exact graph hash was saved before, skip
    // rewriting the graph file but still append a history entry
    // (description / saved_by may have changed).
    if !graph_path.exists() {
        let body = match serde_json::to_string_pretty(&req.graph) {
            Ok(b) => b,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        if let Err(e) = std::fs::write(&graph_path, body) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("write graph: {e}"),
            )
                .into_response();
        }
    }
    let entry = CustomTemplateVersion {
        hash: hash.clone(),
        saved_at: chrono::Utc::now(),
        saved_by,
        description: req.description,
    };
    let line = match serde_json::to_string(&entry) {
        Ok(l) => l,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if let Err(e) = append_line(&history_path, &line) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("append: {e}")).into_response();
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "saved",
            "hash": hash,
            "version": read_history(&template_dir).len(),
        })),
    )
        .into_response()
}

fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{line}")
}

async fn get_custom_template(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let Some(root) = user_templates_dir(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "strategy graphs not configured",
        )
            .into_response();
    };
    let template_dir = root.join(&name);
    let legacy_path = root.join(format!("{name}.json"));

    // Prefer the versioned layout; fall back to legacy flat file
    // so pre-M-SAVE installations still serve their templates
    // without a forced migration.
    if template_dir.is_dir() {
        let history = read_history(&template_dir);
        let Some(latest) = history.first().cloned() else {
            return (StatusCode::NOT_FOUND, "no versions recorded").into_response();
        };
        let graph_path = template_dir.join(format!("{}.json", latest.hash));
        let graph: mm_strategy_graph::Graph = match std::fs::read_to_string(&graph_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
        {
            Some(g) => g,
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("latest version {} missing on disk", latest.hash),
                )
                    .into_response()
            }
        };
        return (
            StatusCode::OK,
            Json(CustomTemplateFull {
                name,
                description: latest.description.clone(),
                graph,
                saved_at: Some(latest.saved_at),
                saved_by: latest.saved_by.clone(),
                history,
            }),
        )
            .into_response();
    }

    match std::fs::read_to_string(&legacy_path) {
        Ok(raw) => match serde_json::from_str::<CustomTemplateRecord>(&raw) {
            Ok(rec) => {
                let hash = rec.graph.content_hash();
                let entry = CustomTemplateVersion {
                    hash: hash.clone(),
                    saved_at: rec.saved_at.unwrap_or_else(chrono::Utc::now),
                    saved_by: rec.saved_by.clone(),
                    description: rec.description.clone(),
                };
                (
                    StatusCode::OK,
                    Json(CustomTemplateFull {
                        name: rec.name,
                        description: rec.description,
                        graph: rec.graph,
                        saved_at: rec.saved_at,
                        saved_by: rec.saved_by,
                        history: vec![entry],
                    }),
                )
                    .into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, "not found").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// M-SAVE GOBS — fetch a specific historical version by hash.
/// Returns just the graph JSON (no history wrapper). Used by
/// the version selector in StrategyPage to roll back to an
/// older template revision without affecting other templates.
async fn get_custom_template_version(
    State(state): State<DashboardState>,
    Path((name, hash)): Path<(String, String)>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    if !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
        return (StatusCode::BAD_REQUEST, "bad hash").into_response();
    }
    let Some(root) = user_templates_dir(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "strategy graphs not configured",
        )
            .into_response();
    };
    let path = root.join(&name).join(format!("{hash}.json"));
    match std::fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<mm_strategy_graph::Graph>(&raw) {
            Ok(g) => (StatusCode::OK, Json(g)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, "version not found").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn delete_custom_template(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let Some(root) = user_templates_dir(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "strategy graphs not configured",
        )
            .into_response();
    };
    let template_dir = root.join(&name);
    let legacy_path = root.join(format!("{name}.json"));
    // Delete the whole versioned directory if present + the
    // legacy flat file if it lingers.
    let mut removed = false;
    if template_dir.is_dir() {
        if let Err(e) = std::fs::remove_dir_all(&template_dir) {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("rmdir: {e}")).into_response();
        }
        removed = true;
    }
    if legacy_path.exists() {
        let _ = std::fs::remove_file(&legacy_path);
        removed = true;
    }
    if removed {
        (StatusCode::OK, "deleted").into_response()
    } else {
        (StatusCode::NOT_FOUND, "not found").into_response()
    }
}

async fn preview_strategy_graph(
    axum::Json(req): axum::Json<PreviewRequest>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use mm_strategy_graph::{EvalCtx, Evaluator, NodeId, Value};
    use std::collections::HashMap;
    use std::str::FromStr;

    let mut ev = match Evaluator::build(&req.graph) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::OK,
                Json(PreviewResponse {
                    edges: HashMap::new(),
                    sinks: vec![],
                    errors: vec![e.to_string()],
                }),
            )
                .into_response();
        }
    };

    // Parse source_inputs overrides: `"node_id:port" → "raw"` into
    // `(NodeId, port) → Value`. Try Number first, then Bool, then
    // String as the pragmatic fallback.
    let mut inputs: HashMap<(NodeId, String), Value> = HashMap::new();
    for (key, raw) in &req.source_inputs {
        let Some((id_str, port)) = key.split_once(':') else {
            continue;
        };
        let Ok(node_id) = NodeId::parse(id_str) else {
            continue;
        };
        let v = if let Ok(d) = rust_decimal::Decimal::from_str(raw) {
            Value::Number(d)
        } else if raw == "true" {
            Value::Bool(true)
        } else if raw == "false" {
            Value::Bool(false)
        } else {
            Value::String(raw.clone())
        };
        inputs.insert((node_id, port.to_string()), v);
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let ctx = EvalCtx { now_ms };
    let (sinks, trace) = match ev.tick_with_trace(&ctx, &inputs) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::OK,
                Json(PreviewResponse {
                    edges: HashMap::new(),
                    sinks: vec![],
                    errors: vec![e.to_string()],
                }),
            )
                .into_response();
        }
    };

    let edges: HashMap<String, String> = trace
        .into_iter()
        .map(|((id, port), v)| (format!("{id}:{port}"), render_value(&v)))
        .collect();
    let sinks: Vec<serde_json::Value> = sinks
        .into_iter()
        .map(|s| serde_json::to_value(format!("{s:?}")).unwrap_or(serde_json::Value::Null))
        .collect();

    (
        StatusCode::OK,
        Json(PreviewResponse {
            edges,
            sinks,
            errors: vec![],
        }),
    )
        .into_response()
}

fn render_value(v: &mm_strategy_graph::Value) -> String {
    use mm_strategy_graph::Value;
    match v {
        Value::Number(n) => n.normalize().to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Unit => "()".to_string(),
        Value::String(s) => s.clone(),
        Value::KillLevel(l) => format!("L{l}"),
        Value::StrategyKind(s) | Value::PairClass(s) => s.clone(),
        Value::Quotes(qs) => format!("{} quotes", qs.len()),
        Value::VenueQuotes(qs) => format!("{} venue quotes", qs.len()),
        Value::Missing => "—".to_string(),
    }
}

async fn get_strategy_graph_version(
    State(state): State<DashboardState>,
    Path((name, hash)): Path<(String, String)>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(store) = state.strategy_graph_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "strategy graphs not configured",
        )
            .into_response();
    };
    match store.load_by_hash(&name, &hash) {
        Ok(g) => (StatusCode::OK, Json(g)).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

async fn list_strategy_deploys(State(state): State<DashboardState>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(store) = state.strategy_graph_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "strategy graphs not configured",
        )
            .into_response();
    };
    match store.deploys() {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `/api/v1/strategy/active` — a compliance-flavoured view of
/// [`list_strategy_deploys`] that folds the log down to **one row
/// per (name, scope)** showing the latest hash. Regulators and the
/// Settings UI care about "what's live *right now*" — full history
/// is already at `/deploys`. Folding happens server-side so the UI
/// never misrepresents the ground truth.
async fn list_strategy_active(State(state): State<DashboardState>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(store) = state.strategy_graph_store() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "strategy graphs not configured",
        )
            .into_response();
    };
    let records = match store.deploys() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut active: std::collections::HashMap<(String, String), mm_strategy_graph::DeployRecord> =
        std::collections::HashMap::new();
    for rec in records {
        let key = (rec.name.clone(), rec.scope.clone());
        active
            .entry(key)
            .and_modify(|cur| {
                if rec.deployed_at > cur.deployed_at {
                    *cur = rec.clone();
                }
            })
            .or_insert(rec);
    }
    let mut out: Vec<_> = active.into_values().collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.deployed_at));
    (StatusCode::OK, Json(out)).into_response()
}

// ── Epic G — sentiment snapshot for UI ─────────────────────

async fn get_sentiment_snapshot(
    State(state): State<DashboardState>,
) -> Json<Vec<mm_sentiment::SentimentTick>> {
    let mut snap = state.get_sentiment_snapshot();
    snap.sort_by(|a, b| a.asset.cmp(&b.asset));
    Json(snap)
}

#[derive(Deserialize)]
struct SentimentHistoryQuery {
    asset: String,
    #[serde(default = "default_sentiment_history_limit")]
    limit: usize,
}
fn default_sentiment_history_limit() -> usize {
    240
}

async fn get_sentiment_history(
    State(state): State<DashboardState>,
    Query(q): Query<SentimentHistoryQuery>,
) -> Json<Vec<mm_sentiment::SentimentTick>> {
    Json(state.get_sentiment_history(&q.asset, q.limit.min(1440)))
}

// ── Block D — archive health probe ─────────────────────────

async fn get_archive_health(State(state): State<DashboardState>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match state.archive_client() {
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "ok": false,
                "reason": "archive not configured",
            })),
        )
            .into_response(),
        Some(client) => match client.health_check().await {
            Ok(()) => {
                let cfg = client.config();
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "ok": true,
                        "bucket": cfg.s3_bucket,
                        "region": cfg.s3_region,
                        "endpoint": cfg.s3_endpoint_url,
                        "prefix": cfg.s3_prefix,
                        "retention_days": cfg.retention_days,
                    })),
                )
                    .into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "ok": false,
                    "reason": e.to_string(),
                })),
            )
                .into_response(),
        },
    }
}

// ── Block C — compliance bundle export ─────────────────────

#[derive(Debug, Deserialize)]
struct BundleQuery {
    from: String,
    to: String,
    #[serde(default)]
    client_id: Option<String>,
}

async fn get_export_bundle(
    State(state): State<DashboardState>,
    Query(q): Query<BundleQuery>,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    let from = match chrono::NaiveDate::parse_from_str(&q.from, "%Y-%m-%d") {
        Ok(d) => d,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad from: {e}")).into_response(),
    };
    let to = match chrono::NaiveDate::parse_from_str(&q.to, "%Y-%m-%d") {
        Ok(d) => d,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad to: {e}")).into_response(),
    };
    let client_name = q.client_id.as_deref().unwrap_or("all");
    let req = crate::archive::bundle::BundleRequest {
        state: &state,
        client_id: q.client_id.as_deref(),
        client_name,
        from,
        to,
    };
    match crate::archive::bundle::build_zip(req) {
        Ok(out) => {
            let filename = format!("bundle-{client_name}-{from}-to-{to}.zip");
            (
                [
                    (header::CONTENT_TYPE, "application/zip"),
                    (
                        header::CONTENT_DISPOSITION,
                        &format!("attachment; filename=\"{filename}\""),
                    ),
                ],
                out.bytes,
            )
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_monthly_manifest(
    State(state): State<DashboardState>,
    Query(q): Query<MonthlyQuery>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match build_report(&state, &q) {
        Ok(data) => {
            let secret = state.report_secret();
            match crate::report_export::build_manifest(&data, &["csv", "xlsx", "pdf"], &secret) {
                Ok(m) => (StatusCode::OK, Json(m)).into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }
        Err((code, msg)) => (code, msg).into_response(),
    }
}

/// PnL time-series for charting.
#[derive(Debug, Deserialize)]
struct PnlTimeseriesQuery {
    symbol: String,
}

async fn get_pnl_timeseries(
    State(state): State<DashboardState>,
    Query(query): Query<PnlTimeseriesQuery>,
) -> Json<Vec<crate::state::PnlTimePoint>> {
    Json(state.get_pnl_timeseries(&query.symbol))
}

/// UX-2 — spread-bps rolling history endpoint. Mirrors the
/// PnL path so the frontend can backfill both charts on
/// symbol change.
async fn get_spread_timeseries(
    State(state): State<DashboardState>,
    Query(query): Query<PnlTimeseriesQuery>,
) -> Json<Vec<crate::state::SeriesPoint>> {
    Json(state.get_spread_timeseries(&query.symbol))
}

/// UX-2 — inventory rolling history endpoint.
async fn get_inventory_timeseries(
    State(state): State<DashboardState>,
    Query(query): Query<PnlTimeseriesQuery>,
) -> Json<Vec<crate::state::SeriesPoint>> {
    Json(state.get_inventory_timeseries(&query.symbol))
}

/// Order book analytics — depth, imbalance, liquidity score.
#[derive(Debug, Serialize)]
struct BookAnalytics {
    symbol: String,
    mid_price: Decimal,
    spread_bps: Decimal,
    /// Depth levels from the dashboard state.
    depth_levels: Vec<crate::state::BookDepthLevel>,
    /// Top-of-book imbalance: (bid_depth - ask_depth) / total.
    /// Range [-1, +1]. Positive = more bids.
    top_imbalance: Decimal,
    /// Total depth within 1% of mid (both sides, quote asset).
    total_depth_1pct: Decimal,
    /// Liquidity score: depth × (1/spread). Higher = better
    /// liquidity provision. Unitless relative metric.
    liquidity_score: Decimal,
    /// Value locked in open orders (quote asset).
    locked_in_orders: Decimal,
}

async fn get_book_analytics(State(state): State<DashboardState>) -> Json<Vec<BookAnalytics>> {
    let symbols = state.get_all();
    let analytics: Vec<BookAnalytics> = symbols
        .iter()
        .map(|s| {
            let bid_depth: Decimal = s.book_depth_levels.iter().map(|l| l.bid_depth_quote).sum();
            let ask_depth: Decimal = s.book_depth_levels.iter().map(|l| l.ask_depth_quote).sum();
            let total = bid_depth + ask_depth;
            let top_imbalance = if total > Decimal::ZERO {
                (bid_depth - ask_depth) / total
            } else {
                Decimal::ZERO
            };
            // Depth within 1% — find the 1% level.
            let depth_1pct = s
                .book_depth_levels
                .iter()
                .find(|l| l.pct_from_mid == dec!(1))
                .map(|l| l.bid_depth_quote + l.ask_depth_quote)
                .unwrap_or(Decimal::ZERO);
            let liquidity_score = if s.spread_bps > Decimal::ZERO {
                depth_1pct / s.spread_bps
            } else {
                Decimal::ZERO
            };
            BookAnalytics {
                symbol: s.symbol.clone(),
                mid_price: s.mid_price,
                spread_bps: s.spread_bps,
                depth_levels: s.book_depth_levels.clone(),
                top_imbalance,
                total_depth_1pct: depth_1pct,
                liquidity_score,
                locked_in_orders: s.locked_in_orders_quote,
            }
        })
        .collect();
    Json(analytics)
}

// `get_audit_recent` removed — distributed AuditStream fans
// out per-deployment via the details endpoint (see
// `AuditStream.svelte` + agent's `audit_tail` topic handler in
// `crates/agent/src/lib.rs`). No single process in the
// distributed deployment owns an audit.jsonl file.

/// List available historical report dates.
async fn get_report_history(State(state): State<DashboardState>) -> Json<Vec<String>> {
    Json(state.available_report_dates())
}

/// Get a historical daily report by date.
async fn get_historical_report(
    State(state): State<DashboardState>,
    Path(date): Path<String>,
) -> Json<Option<crate::state::DailyReportSnapshot>> {
    Json(state.get_daily_report(&date))
}

/// Daily report in CSV format for auditors/clients.
async fn get_daily_report_csv(State(state): State<DashboardState>) -> String {
    let symbols = state.get_all();
    let mut csv = String::from(
        "symbol,pnl,volume,fills,avg_spread_bps,uptime_pct,max_inventory,presence_24h,two_sided_24h\n",
    );
    for s in &symbols {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            s.symbol,
            s.pnl.total,
            s.pnl.volume,
            s.pnl.round_trips,
            s.spread_bps,
            s.sla_uptime_pct,
            s.inventory.abs(),
            s.presence_pct_24h,
            s.two_sided_pct_24h,
        ));
    }
    csv
}

// ── Per-client API endpoints (Epic 1) ────────────────────────

/// Per-client SLA aggregate.
#[derive(Debug, Serialize)]
struct ClientSlaSummary {
    client_id: String,
    symbols: Vec<ClientSymbolSla>,
    /// Average presence across all client's symbols.
    avg_presence_pct: Decimal,
    /// Average two-sided presence across all client's symbols.
    avg_two_sided_pct: Decimal,
    /// Worst-case presence among the client's symbols.
    min_presence_pct: Decimal,
    /// Overall compliance: all symbols above 95%.
    is_compliant: bool,
}

#[derive(Debug, Serialize)]
struct ClientSymbolSla {
    symbol: String,
    presence_pct: Decimal,
    two_sided_pct: Decimal,
    spread_compliance_pct: Decimal,
    minutes_with_data: u32,
}

async fn get_client_sla(
    State(state): State<DashboardState>,
    Path(client_id): Path<String>,
) -> Json<ClientSlaSummary> {
    let metrics = fleet_client_metrics(&state, Some(&client_id)).await;
    let symbol_slas: Vec<ClientSymbolSla> = metrics
        .iter()
        .map(|r| ClientSymbolSla {
            symbol: str_field(r, "symbol"),
            presence_pct: dec(r, "presence_pct_24h"),
            two_sided_pct: dec(r, "two_sided_pct_24h"),
            spread_compliance_pct: dec(r, "spread_compliance_pct"),
            minutes_with_data: r
                .get("minutes_with_data_24h")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        })
        .collect();

    let count = Decimal::from(symbol_slas.len().max(1));
    let avg_presence = symbol_slas.iter().map(|s| s.presence_pct).sum::<Decimal>() / count;
    let avg_two_sided = symbol_slas.iter().map(|s| s.two_sided_pct).sum::<Decimal>() / count;
    let min_presence = symbol_slas
        .iter()
        .map(|s| s.presence_pct)
        .min()
        .unwrap_or(dec!(100));
    let is_compliant = symbol_slas.iter().all(|s| s.presence_pct >= dec!(95));

    Json(ClientSlaSummary {
        client_id,
        symbols: symbol_slas,
        avg_presence_pct: avg_presence,
        avg_two_sided_pct: avg_two_sided,
        min_presence_pct: min_presence,
        is_compliant,
    })
}

/// Per-client fill query params.
#[derive(Debug, Deserialize)]
struct ClientFillsQuery {
    #[serde(default = "default_fill_limit")]
    limit: usize,
}

fn default_fill_limit() -> usize {
    100
}

async fn get_client_fills(
    State(state): State<DashboardState>,
    Path(client_id): Path<String>,
    Query(q): Query<ClientFillsQuery>,
) -> Json<Vec<FillRecord>> {
    // 2026-04-21 journey smoke — the legacy implementation read
    // `DashboardState.clients[cid].recent_fills`, but in the
    // distributed model fills happen on agent-local dashboards,
    // never on the controller's. Tenants opened the portal and
    // saw an empty "Recent fills" card even while paper quotes
    // were actively filling. Fan out to each agent via the
    // `client_metrics` topic (which now carries a `recent_fills`
    // array per deployment), flatten, sort newest-first, cap to
    // the requested limit. Falls back to the local fill buffer
    // when no fetcher is installed (unit tests).
    let mut fills = collect_fleet_client_fills(&state, &client_id).await;
    if fills.is_empty() {
        fills = state.get_client_fills(&client_id, q.limit);
    }
    fills.sort_by_key(|f| std::cmp::Reverse(f.timestamp));
    fills.truncate(q.limit);
    Json(fills)
}

async fn collect_fleet_client_fills(state: &DashboardState, client_id: &str) -> Vec<FillRecord> {
    let metrics = fleet_client_metrics(state, Some(client_id)).await;
    let mut out = Vec::new();
    for row in metrics {
        if let Some(arr) = row.get("recent_fills").and_then(|v| v.as_array()) {
            for raw in arr {
                if let Ok(fill) = serde_json::from_value::<FillRecord>(raw.clone()) {
                    out.push(fill);
                }
            }
        }
    }
    out
}

/// Per-client PnL aggregate.
#[derive(Debug, Serialize)]
struct ClientPnlSummary {
    client_id: String,
    total_pnl: Decimal,
    total_volume: Decimal,
    total_fills: u64,
    symbols: Vec<ClientSymbolPnl>,
}

#[derive(Debug, Serialize)]
struct ClientSymbolPnl {
    symbol: String,
    pnl: Decimal,
    volume: Decimal,
    fills: u64,
}

async fn get_client_pnl(
    State(state): State<DashboardState>,
    Path(client_id): Path<String>,
) -> Json<ClientPnlSummary> {
    let metrics = fleet_client_metrics(&state, Some(&client_id)).await;
    let mut total_pnl = Decimal::ZERO;
    let mut total_volume = Decimal::ZERO;
    let mut total_fills = 0u64;
    let symbol_pnls: Vec<ClientSymbolPnl> = metrics
        .iter()
        .map(|r| {
            let pnl = dec(r, "pnl_total");
            let volume = dec(r, "pnl_volume");
            // PNL-COUNTER-1 — tenants expect "fills" to mean
            // raw trade count, not round-trips. New agents emit
            // `pnl_fill_count`; falls back to `pnl_round_trips`
            // for older replies during rolling upgrades.
            let fills = u64_field(r, "pnl_fill_count");
            let fills = if fills > 0 {
                fills
            } else {
                u64_field(r, "pnl_round_trips")
            };
            total_pnl += pnl;
            total_volume += volume;
            total_fills += fills;
            ClientSymbolPnl {
                symbol: str_field(r, "symbol"),
                pnl,
                volume,
                fills,
            }
        })
        .collect();
    Json(ClientPnlSummary {
        client_id,
        total_pnl,
        total_volume,
        total_fills,
        symbols: symbol_pnls,
    })
}

/// Per-client SLA compliance certificate (Epic 1 item 1.5).
#[derive(Debug, Serialize)]
struct ClientSlaCertificate {
    client_id: String,
    generated_at: String,
    overall_compliant: bool,
    symbols: Vec<ClientSymbolSla>,
    avg_presence_pct: Decimal,
    min_presence_pct: Decimal,
    signature: String,
}

async fn get_client_sla_certificate(
    State(state): State<DashboardState>,
    Path(client_id): Path<String>,
) -> Json<ClientSlaCertificate> {
    let metrics = fleet_client_metrics(&state, Some(&client_id)).await;
    let now = Utc::now();
    let symbol_slas: Vec<ClientSymbolSla> = metrics
        .iter()
        .map(|r| ClientSymbolSla {
            symbol: str_field(r, "symbol"),
            presence_pct: dec(r, "presence_pct_24h"),
            two_sided_pct: dec(r, "two_sided_pct_24h"),
            spread_compliance_pct: dec(r, "spread_compliance_pct"),
            minutes_with_data: r
                .get("minutes_with_data_24h")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        })
        .collect();

    let count = Decimal::from(symbol_slas.len().max(1));
    let avg_presence = symbol_slas.iter().map(|s| s.presence_pct).sum::<Decimal>() / count;
    let min_presence = symbol_slas
        .iter()
        .map(|s| s.presence_pct)
        .min()
        .unwrap_or(dec!(100));
    let overall_compliant = symbol_slas.iter().all(|s| s.presence_pct >= dec!(95));

    let body = format!(
        "{}:{}:{}:{}",
        client_id,
        now.to_rfc3339(),
        overall_compliant,
        symbol_slas.len()
    );
    let secret = std::env::var("MM_AUTH_SECRET").unwrap_or_else(|_| "default".to_string());
    let signature = hmac_sha256_hex(&secret, &body);

    Json(ClientSlaCertificate {
        client_id,
        generated_at: now.to_rfc3339(),
        overall_compliant,
        symbols: symbol_slas,
        avg_presence_pct: avg_presence,
        min_presence_pct: min_presence,
        signature,
    })
}

// ── Wave E3 — self-scope handlers for ClientReader ────────────
//
// Each resolves `token.client_id` from the request extensions
// (populated by auth_middleware) and delegates to the existing
// `/{id}` handler. The tenant_scope_middleware already admits
// `/api/v1/client/self/*` for tokens carrying a client_id; here
// we simply wire the id through to the body handler.

use crate::auth::TokenClaims;

/// Pull the caller's client_id from TokenClaims. Returns a 401
/// body for tokens without a client_id (admin/operator who
/// accidentally hit /self/ — they should be hitting /{id}).
fn self_client_id(
    claims: Option<&TokenClaims>,
) -> Result<String, (axum::http::StatusCode, String)> {
    let claims = claims.ok_or((
        axum::http::StatusCode::UNAUTHORIZED,
        "self endpoint requires an authenticated token".into(),
    ))?;
    claims.client_id.clone().ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        "this endpoint is for tenant-scoped tokens; use /api/v1/client/{id}/* with an explicit id"
            .into(),
    ))
}

async fn get_self_sla(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
) -> Result<Json<ClientSlaSummary>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    Ok(get_client_sla(State(state), Path(id)).await)
}

async fn get_self_sla_certificate(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
) -> Result<Json<ClientSlaCertificate>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    Ok(get_client_sla_certificate(State(state), Path(id)).await)
}

async fn get_self_fills(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
    Query(q): Query<ClientFillsQuery>,
) -> Result<Json<Vec<FillRecord>>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    Ok(get_client_fills(State(state), Path(id), Query(q)).await)
}

async fn get_self_pnl(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
) -> Result<Json<ClientPnlSummary>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    Ok(get_client_pnl(State(state), Path(id)).await)
}

/// Self-scoped webhook delivery log — ClientReader checks that
/// their own webhooks fired successfully without needing admin
/// access.
async fn get_self_webhook_deliveries(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    let dispatcher = state.get_client_webhook_dispatcher(&id);
    let records = dispatcher
        .map(|d| d.recent_deliveries())
        .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "client_id": id,
        "count": records.len(),
        "deliveries": records,
    })))
}

/// Wave I1 — self-service webhook registration. Tenant lists,
/// adds, removes, and test-fires their own webhook URLs without
/// needing admin involvement. Auto-creates the dispatcher on
/// first add so existing clients who were onboarded without a
/// webhook can opt in at any time.

#[derive(Debug, Deserialize)]
struct SelfWebhookBody {
    url: String,
}

/// Reject obviously wrong URLs before they land in the
/// dispatcher state. Full SSRF hardening is out of scope here;
/// this is a shape check so tenants get a clear 400 instead of
/// silent retries on a typo. Admin-added URLs go through the
/// same backend, which already trusts operator-curated config.
fn validate_webhook_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("url must not be empty".into());
    }
    if trimmed.len() > 2048 {
        return Err("url too long (> 2048 chars)".into());
    }
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err("url must be http:// or https://".into());
    }
    Ok(trimmed.to_string())
}

async fn list_self_webhooks(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    let urls = state
        .get_client_webhook_dispatcher(&id)
        .map(|d| d.list_urls())
        .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "client_id": id,
        "urls": urls,
    })))
}

async fn add_self_webhook(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
    Json(body): Json<SelfWebhookBody>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    let url =
        validate_webhook_url(&body.url).map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e))?;
    let dispatcher = match state.get_client_webhook_dispatcher(&id) {
        Some(d) => d,
        None => {
            // First-time opt-in: mint a dispatcher and register
            // it against the tenant. Subsequent adds hit the
            // existing instance.
            let d = crate::webhooks::WebhookDispatcher::new();
            state.set_client_webhook_dispatcher(&id, d.clone());
            d
        }
    };
    dispatcher.add_url(url.clone());
    Ok(Json(serde_json::json!({
        "client_id": id,
        "url": url,
        "urls": dispatcher.list_urls(),
    })))
}

async fn remove_self_webhook(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
    Json(body): Json<SelfWebhookBody>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    let Some(dispatcher) = state.get_client_webhook_dispatcher(&id) else {
        return Ok(Json(serde_json::json!({
            "client_id": id,
            "removed": false,
            "urls": [],
        })));
    };
    let url = body.url.trim().to_string();
    dispatcher.remove_url(&url);
    Ok(Json(serde_json::json!({
        "client_id": id,
        "removed": true,
        "urls": dispatcher.list_urls(),
    })))
}

async fn test_self_webhook(
    axum::Extension(claims): axum::Extension<TokenClaims>,
    State(state): State<DashboardState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let id = self_client_id(Some(&claims))?;
    let Some(dispatcher) = state.get_client_webhook_dispatcher(&id) else {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            "no webhooks registered — add one first".into(),
        ));
    };
    let results = dispatcher.test_dispatch().await;
    Ok(Json(serde_json::json!({
        "client_id": id,
        "attempted": results.len(),
        "succeeded": results.iter().filter(|r| r.ok).count(),
        "results": results,
    })))
}

// ── Wave G2/G4 — incident lifecycle endpoints ──────────────

#[derive(Debug, Deserialize)]
struct OpenIncidentRequest {
    violation_key: String,
    severity: String,
    category: String,
    target: String,
    metric: String,
    detail: String,
    // M4-4 GOBS — optional graph deep-link triple. Accepted when
    // the incident was filed from a deployment drilldown so the
    // Incidents page can surface "Open graph at incident".
    #[serde(default)]
    graph_agent_id: Option<String>,
    #[serde(default)]
    graph_deployment_id: Option<String>,
    #[serde(default)]
    graph_tick_num: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct AckIncidentRequest {
    #[serde(default)]
    by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResolveIncidentRequest {
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    root_cause: Option<String>,
    #[serde(default)]
    action_taken: Option<String>,
    #[serde(default)]
    preventive: Option<String>,
}

async fn list_incidents_handler(
    State(state): State<DashboardState>,
) -> Json<Vec<crate::state::OpenIncident>> {
    Json(state.list_incidents())
}

async fn post_incident_handler(
    axum::Extension(claims): axum::Extension<crate::auth::TokenClaims>,
    State(state): State<DashboardState>,
    axum::Json(body): axum::Json<OpenIncidentRequest>,
) -> Json<crate::state::OpenIncident> {
    let now_ms = Utc::now().timestamp_millis();
    let inc = crate::state::OpenIncident {
        id: uuid::Uuid::new_v4().to_string(),
        opened_at_ms: now_ms,
        opened_by: claims.user_id.clone(),
        violation_key: body.violation_key,
        severity: body.severity,
        category: body.category,
        target: body.target,
        metric: body.metric,
        detail: body.detail,
        state: "open".into(),
        acked_by: None,
        acked_at_ms: None,
        resolved_by: None,
        resolved_at_ms: None,
        root_cause: None,
        action_taken: None,
        preventive: None,
        graph_agent_id: body.graph_agent_id,
        graph_deployment_id: body.graph_deployment_id,
        graph_tick_num: body.graph_tick_num,
    };
    Json(state.open_incident(inc))
}

async fn ack_incident_handler(
    axum::Extension(claims): axum::Extension<crate::auth::TokenClaims>,
    State(state): State<DashboardState>,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<AckIncidentRequest>,
) -> Result<Json<crate::state::OpenIncident>, (axum::http::StatusCode, String)> {
    let actor = body.by.unwrap_or_else(|| claims.user_id.clone());
    state
        .ack_incident(&id, &actor)
        .map(Json)
        .ok_or((axum::http::StatusCode::CONFLICT, "incident not open".into()))
}

async fn resolve_incident_handler(
    axum::Extension(claims): axum::Extension<crate::auth::TokenClaims>,
    State(state): State<DashboardState>,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<ResolveIncidentRequest>,
) -> Result<Json<crate::state::OpenIncident>, (axum::http::StatusCode, String)> {
    let actor = body.by.unwrap_or_else(|| claims.user_id.clone());
    state
        .resolve_incident(
            &id,
            &actor,
            body.root_cause,
            body.action_taken,
            body.preventive,
        )
        .map(Json)
        .ok_or((
            axum::http::StatusCode::NOT_FOUND,
            "incident not found or already resolved".into(),
        ))
}

// ── System preflight health (Pre-Flight Toolkit) ────────────

#[derive(Debug, Serialize)]
struct PreflightHealth {
    overall: String,
    symbols_active: usize,
    any_kill_switch_active: bool,
    max_kill_level: u8,
    uptime_secs: i64,
    total_pnl: Decimal,
}

async fn get_system_preflight(State(state): State<DashboardState>) -> Json<PreflightHealth> {
    let symbols = state.get_all();
    let max_kill = symbols.iter().map(|s| s.kill_level).max().unwrap_or(0);
    let any_kill = max_kill > 0;
    let total_pnl: Decimal = symbols.iter().map(|s| s.pnl.total).sum();
    let uptime = (Utc::now() - state.started_at()).num_seconds();

    let overall = if any_kill {
        "DEGRADED"
    } else if symbols.is_empty() {
        "NO_DATA"
    } else {
        "HEALTHY"
    };

    Json(PreflightHealth {
        overall: overall.into(),
        symbols_active: symbols.len(),
        any_kill_switch_active: any_kill,
        max_kill_level: max_kill,
        uptime_secs: uptime,
        total_pnl,
    })
}

// ── Loan endpoints (Epic 2) ──────────────────────────────────

async fn get_loans(
    State(state): State<DashboardState>,
) -> Json<Vec<mm_persistence::loan::LoanAgreement>> {
    Json(state.get_all_loan_agreements())
}

async fn get_loan_by_symbol(
    State(state): State<DashboardState>,
    Path(symbol): Path<String>,
) -> Json<Option<mm_persistence::loan::LoanAgreement>> {
    Json(state.get_loan_agreement_by_symbol(&symbol))
}

// ── Portfolio risk endpoints (Epic 3) ────────────────────────

/// Cross-symbol correlation matrix.
#[derive(Debug, Serialize)]
struct CorrelationEntry {
    factor_a: String,
    factor_b: String,
    correlation: Decimal,
}

async fn get_portfolio_correlation(
    State(state): State<DashboardState>,
) -> Json<Vec<CorrelationEntry>> {
    let matrix = state.get_correlation_matrix();
    Json(
        matrix
            .into_iter()
            .map(|(a, b, c)| CorrelationEntry {
                factor_a: a,
                factor_b: b,
                correlation: c,
            })
            .collect(),
    )
}

async fn get_portfolio_risk(
    State(state): State<DashboardState>,
) -> Json<Option<mm_risk::portfolio_risk::PortfolioRiskSummary>> {
    Json(state.get_portfolio_risk_summary())
}

#[cfg(test)]
mod tests;
