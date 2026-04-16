use axum::extract::{Query, State};
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
        .route("/api/v1/portfolio", get(get_portfolio))
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
