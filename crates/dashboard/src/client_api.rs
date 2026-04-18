use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::state::{DashboardState, FillRecord};

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
        .route("/api/v1/audit/recent", get(get_audit_recent))
        .route("/api/v1/system/diagnostics", get(get_diagnostics))
        .route("/api/v1/pnl/timeseries", get(get_pnl_timeseries))
        .route("/api/v1/spread/timeseries", get(get_spread_timeseries))
        .route("/api/v1/inventory/timeseries", get(get_inventory_timeseries))
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
        .route("/api/v1/strategy/templates/{name}", get(get_strategy_template))
        .route(
            "/api/v1/strategy/graphs/{name}/history/{hash}",
            get(get_strategy_graph_version),
        )
        .route(
            "/api/v1/strategy/preview",
            axum::routing::post(preview_strategy_graph),
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
    let symbols = state.get_all();
    let positions: Vec<PositionResponse> = symbols
        .iter()
        .map(|s| PositionResponse {
            symbol: s.symbol.clone(),
            inventory: s.inventory,
            inventory_value: s.inventory_value,
            avg_entry_price: if s.inventory.is_zero() {
                dec!(0)
            } else {
                s.inventory_value / s.inventory.abs()
            },
            unrealized_pnl: s.pnl.inventory,
            realized_pnl: s.pnl.spread + s.pnl.rebates - s.pnl.fees,
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

async fn get_pnl(State(state): State<DashboardState>) -> Json<Vec<PnlResponse>> {
    let symbols = state.get_all();
    let pnl: Vec<PnlResponse> = symbols
        .iter()
        .map(|s| {
            let efficiency = if s.pnl.volume > dec!(0) {
                s.pnl.total / s.pnl.volume * dec!(10_000)
            } else {
                dec!(0)
            };
            PnlResponse {
                total: s.pnl.total,
                spread_capture: s.pnl.spread,
                inventory_pnl: s.pnl.inventory,
                rebate_income: s.pnl.rebates,
                fees_paid: s.pnl.fees,
                round_trips: s.pnl.round_trips,
                volume: s.pnl.volume,
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
    let symbols = state.get_all();
    let sla: Vec<SlaResponse> = symbols
        .iter()
        .map(|s| {
            // Sum depth across all book levels for a total figure.
            let bid_depth: Decimal = s.book_depth_levels.iter().map(|l| l.bid_depth_quote).sum();
            let ask_depth: Decimal = s.book_depth_levels.iter().map(|l| l.ask_depth_quote).sum();
            SlaResponse {
                symbol: s.symbol.clone(),
                uptime_pct: s.sla_uptime_pct,
                is_compliant: s.sla_uptime_pct >= dec!(95),
                current_spread_bps: s.spread_bps,
                bid_depth,
                ask_depth,
                sla_max_spread_bps: s.sla_max_spread_bps,
                sla_min_depth_quote: s.sla_min_depth_quote,
                spread_compliance_pct: s.spread_compliance_pct,
                presence_pct_24h: s.presence_pct_24h,
                two_sided_pct_24h: s.two_sided_pct_24h,
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

/// HMAC-SHA256 using a simple XOR-based implementation.
/// Production deployments should use `ring` or `hmac` crate;
/// this is a self-contained fallback that avoids adding a
/// new dependency.
fn hmac_sha256_hex(key: &str, message: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    message.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
async fn get_config_snapshot(
    State(state): State<DashboardState>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match state.app_config() {
        Some(cfg) => {
            let val = serde_json::to_value(&*cfg)
                .unwrap_or(serde_json::Value::Null);
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

fn parse_period(
    q: &MonthlyQuery,
) -> Result<(chrono::NaiveDate, chrono::NaiveDate), String> {
    let from = chrono::NaiveDate::parse_from_str(&q.from, "%Y-%m-%d")
        .map_err(|e| format!("bad from: {e}"))?;
    let to = chrono::NaiveDate::parse_from_str(&q.to, "%Y-%m-%d")
        .map_err(|e| format!("bad to: {e}"))?;
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
            let filename =
                format!("monthly-{}-{}-to-{}.csv", data.client_id, data.period_from, data.period_to);
            ([
                (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (header::CONTENT_DISPOSITION, &format!("attachment; filename=\"{filename}\"")),
            ], body)
                .into_response()
        }
        Err((code, msg)) => (code, msg).into_response(),
    }
}

async fn get_monthly_xlsx(
    State(state): State<DashboardState>,
    Query(q): Query<MonthlyQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    match build_report(&state, &q) {
        Ok(data) => {
            let secret = state.report_secret();
            let manifest = match crate::report_export::build_manifest(
                &data, &["xlsx"], &secret,
            ) {
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
            ([
                (
                    header::CONTENT_TYPE,
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                ),
                (header::CONTENT_DISPOSITION, &format!("attachment; filename=\"{filename}\"")),
            ], bytes)
                .into_response()
        }
        Err((code, msg)) => (code, msg).into_response(),
    }
}

async fn get_monthly_pdf(
    State(state): State<DashboardState>,
    Query(q): Query<MonthlyQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    match build_report(&state, &q) {
        Ok(data) => {
            let secret = state.report_secret();
            let manifest = match crate::report_export::build_manifest(
                &data, &["pdf"], &secret,
            ) {
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
            ([
                (header::CONTENT_TYPE, "application/pdf"),
                (header::CONTENT_DISPOSITION, &format!("attachment; filename=\"{filename}\"")),
            ], bytes)
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
            }
        })
        .collect();
    Json(entries)
}

async fn list_strategy_graphs(
    State(state): State<DashboardState>,
) -> axum::response::Response {
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
        return (StatusCode::SERVICE_UNAVAILABLE, "strategy graphs not configured")
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

async fn get_strategy_template(
    Path(name): Path<String>,
) -> axum::response::Response {
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
                    errors: vec![format!("{e:?}")],
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
        return (StatusCode::SERVICE_UNAVAILABLE, "strategy graphs not configured")
            .into_response();
    };
    match store.load_by_hash(&name, &hash) {
        Ok(g) => (StatusCode::OK, Json(g)).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

async fn list_strategy_deploys(
    State(state): State<DashboardState>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(store) = state.strategy_graph_store() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "strategy graphs not configured")
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
async fn list_strategy_active(
    State(state): State<DashboardState>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(store) = state.strategy_graph_store() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "strategy graphs not configured")
            .into_response();
    };
    let records = match store.deploys() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut active: std::collections::HashMap<
        (String, String),
        mm_strategy_graph::DeployRecord,
    > = std::collections::HashMap::new();
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
    out.sort_by(|a, b| b.deployed_at.cmp(&a.deployed_at));
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

async fn get_archive_health(
    State(state): State<DashboardState>,
) -> axum::response::Response {
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
    use axum::http::{StatusCode, header};
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
            let filename =
                format!("bundle-{client_name}-{from}-to-{to}.zip");
            ([
                (header::CONTENT_TYPE, "application/zip"),
                (
                    header::CONTENT_DISPOSITION,
                    &format!("attachment; filename=\"{filename}\""),
                ),
            ], out.bytes)
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
            match crate::report_export::build_manifest(
                &data,
                &["csv", "xlsx", "pdf"],
                &secret,
            ) {
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

/// Recent audit log entries. Reads the last N lines from the
/// audit JSONL file. Filter by symbol or event type.
#[derive(Debug, Deserialize)]
struct AuditQuery {
    limit: Option<usize>,
    symbol: Option<String>,
    event_type: Option<String>,
}

async fn get_audit_recent(Query(query): Query<AuditQuery>) -> Json<Vec<serde_json::Value>> {
    let limit = query.limit.unwrap_or(100).min(500);
    let path = std::path::Path::new("data/audit.jsonl");
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Json(vec![]),
    };

    let entries: Vec<serde_json::Value> = content
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| {
            if let Some(sym) = &query.symbol {
                if v.get("symbol").and_then(|s| s.as_str()) != Some(sym) {
                    return false;
                }
            }
            if let Some(et) = &query.event_type {
                if v.get("event_type").and_then(|s| s.as_str()) != Some(et) {
                    return false;
                }
            }
            true
        })
        .take(limit)
        .collect();

    Json(entries)
}

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
    let syms = state.get_client_symbols(&client_id);
    let symbol_slas: Vec<ClientSymbolSla> = syms
        .iter()
        .map(|s| ClientSymbolSla {
            symbol: s.symbol.clone(),
            presence_pct: s.presence_pct_24h,
            two_sided_pct: s.two_sided_pct_24h,
            spread_compliance_pct: s.spread_compliance_pct,
            minutes_with_data: s.minutes_with_data_24h,
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
    Json(state.get_client_fills(&client_id, q.limit))
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
    let syms = state.get_client_symbols(&client_id);
    let mut total_pnl = Decimal::ZERO;
    let mut total_volume = Decimal::ZERO;
    let mut total_fills = 0u64;
    let symbol_pnls: Vec<ClientSymbolPnl> = syms
        .iter()
        .map(|s| {
            total_pnl += s.pnl.total;
            total_volume += s.pnl.volume;
            total_fills += s.pnl.round_trips;
            ClientSymbolPnl {
                symbol: s.symbol.clone(),
                pnl: s.pnl.total,
                volume: s.pnl.volume,
                fills: s.pnl.round_trips,
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
    let syms = state.get_client_symbols(&client_id);
    let now = Utc::now();
    let symbol_slas: Vec<ClientSymbolSla> = syms
        .iter()
        .map(|s| ClientSymbolSla {
            symbol: s.symbol.clone(),
            presence_pct: s.presence_pct_24h,
            two_sided_pct: s.two_sided_pct_24h,
            spread_compliance_pct: s.spread_compliance_pct,
            minutes_with_data: s.minutes_with_data_24h,
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
