use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;

use crate::state::DashboardState;

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
        .route("/api/v1/report/daily", get(get_daily_report))
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
}

async fn get_sla(State(state): State<DashboardState>) -> Json<Vec<SlaResponse>> {
    let symbols = state.get_all();
    let sla: Vec<SlaResponse> = symbols
        .iter()
        .map(|s| SlaResponse {
            symbol: s.symbol.clone(),
            uptime_pct: s.sla_uptime_pct,
            is_compliant: s.sla_uptime_pct >= dec!(95),
            current_spread_bps: s.spread_bps,
            bid_depth: dec!(0), // Would need depth from state.
            ask_depth: dec!(0),
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
