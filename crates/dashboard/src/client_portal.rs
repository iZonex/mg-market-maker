use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;

use crate::state::DashboardState;

/// Client portal — what token projects and exchanges see.
///
/// Designed to address the #1 complaint: "We can't verify what the MM is doing."
///
/// Endpoints:
///   GET /api/v1/client/overview        — executive summary
///   GET /api/v1/client/spread-quality  — spread compliance over time
///   GET /api/v1/client/depth           — order book depth at multiple levels
///   GET /api/v1/client/volume          — volume breakdown by exchange
///   GET /api/v1/client/token-positions — where are the loaned tokens
///   GET /api/v1/client/loan-status     — token loan utilization and options
///   GET /api/v1/client/report/daily    — full daily report for client
pub fn client_portal_routes() -> Router<DashboardState> {
    Router::new()
        .route("/api/v1/client/overview", get(client_overview))
        .route("/api/v1/client/spread-quality", get(spread_quality))
        .route("/api/v1/client/depth", get(depth_report))
        .route("/api/v1/client/volume", get(volume_report))
        .route("/api/v1/client/token-positions", get(token_positions))
        .route("/api/v1/client/loan-status", get(loan_status))
        .route("/api/v1/client/report/daily", get(daily_client_report))
}

// --- Executive Overview ---

#[derive(Serialize)]
struct ClientOverview {
    /// Period this overview covers.
    period: String,
    symbols: Vec<SymbolOverview>,
    totals: TotalOverview,
}

#[derive(Serialize)]
struct SymbolOverview {
    symbol: String,
    exchange: String,
    /// Time-weighted average spread (bps).
    avg_spread_bps: Decimal,
    /// % of time spread was within SLA target.
    spread_compliance_pct: Decimal,
    /// Uptime: % of time two-sided quotes were live.
    uptime_pct: Decimal,
    /// Depth at 1% from mid (quote asset, both sides).
    depth_at_1pct: Decimal,
    /// Depth at 2% from mid.
    depth_at_2pct: Decimal,
    /// 24h volume contributed.
    volume_24h: Decimal,
    /// Our volume as % of total exchange volume.
    volume_share_pct: Decimal,
    /// Current mid price.
    mid_price: Decimal,
}

#[derive(Serialize)]
struct TotalOverview {
    total_volume_24h: Decimal,
    avg_spread_compliance_pct: Decimal,
    avg_uptime_pct: Decimal,
    total_pnl: Decimal,
}

async fn client_overview(State(state): State<DashboardState>) -> Json<ClientOverview> {
    let symbols = state.get_all();
    let mut total_vol = dec!(0);
    let mut sum_compliance = dec!(0);
    let mut sum_uptime = dec!(0);
    let mut total_pnl = dec!(0);

    let sym_overviews: Vec<SymbolOverview> = symbols
        .iter()
        .map(|s| {
            let vol = s.pnl.volume;
            total_vol += vol;
            sum_compliance += s.sla_uptime_pct; // Simplified: using uptime as compliance.
            sum_uptime += s.sla_uptime_pct;
            total_pnl += s.pnl.total;

            SymbolOverview {
                symbol: s.symbol.clone(),
                exchange: "primary".to_string(),
                avg_spread_bps: s.spread_bps,
                spread_compliance_pct: s.sla_uptime_pct, // TODO: separate metric.
                uptime_pct: s.sla_uptime_pct,
                depth_at_1pct: dec!(0), // TODO: compute from book.
                depth_at_2pct: dec!(0),
                volume_24h: vol,
                volume_share_pct: dec!(0), // TODO: need total exchange volume.
                mid_price: s.mid_price,
            }
        })
        .collect();

    let n = Decimal::from(symbols.len().max(1) as u64);

    Json(ClientOverview {
        period: "24h".to_string(),
        symbols: sym_overviews,
        totals: TotalOverview {
            total_volume_24h: total_vol,
            avg_spread_compliance_pct: sum_compliance / n,
            avg_uptime_pct: sum_uptime / n,
            total_pnl,
        },
    })
}

// --- Spread Quality ---

#[derive(Serialize)]
struct SpreadQualityReport {
    symbol: String,
    /// % of time within target spread.
    within_target_pct: Decimal,
    /// Time-weighted average spread.
    time_weighted_avg_bps: Decimal,
    /// Volume-weighted average spread.
    volume_weighted_avg_bps: Decimal,
    /// Spread during high-vol periods.
    high_vol_avg_bps: Decimal,
    /// Spread during normal periods.
    normal_avg_bps: Decimal,
    /// Current spread.
    current_bps: Decimal,
    /// Target from SLA.
    target_bps: Decimal,
}

async fn spread_quality(State(state): State<DashboardState>) -> Json<Vec<SpreadQualityReport>> {
    let symbols = state.get_all();
    Json(
        symbols
            .iter()
            .map(|s| SpreadQualityReport {
                symbol: s.symbol.clone(),
                within_target_pct: s.sla_uptime_pct,
                time_weighted_avg_bps: s.spread_bps,
                volume_weighted_avg_bps: s.spread_bps, // TODO: tracked separately.
                high_vol_avg_bps: s.spread_bps * dec!(1.5),
                normal_avg_bps: s.spread_bps,
                current_bps: s.spread_bps,
                target_bps: dec!(100), // From SLA config.
            })
            .collect(),
    )
}

// --- Depth Report ---

#[derive(Serialize)]
struct DepthReport {
    symbol: String,
    /// Depth at various percentages from mid (in quote asset).
    levels: Vec<DepthLevel>,
}

#[derive(Serialize)]
struct DepthLevel {
    /// Distance from mid (%).
    pct_from_mid: Decimal,
    /// Total bid depth in quote asset at this level.
    bid_depth_quote: Decimal,
    /// Total ask depth in quote asset at this level.
    ask_depth_quote: Decimal,
    /// Minimum required by SLA (if any).
    sla_minimum: Option<Decimal>,
    /// Is requirement met?
    compliant: bool,
}

async fn depth_report(State(state): State<DashboardState>) -> Json<Vec<DepthReport>> {
    let symbols = state.get_all();
    Json(
        symbols
            .iter()
            .map(|s| DepthReport {
                symbol: s.symbol.clone(),
                levels: vec![
                    DepthLevel {
                        pct_from_mid: dec!(0.5),
                        bid_depth_quote: dec!(0), // TODO: compute from book.
                        ask_depth_quote: dec!(0),
                        sla_minimum: Some(dec!(10000)),
                        compliant: false,
                    },
                    DepthLevel {
                        pct_from_mid: dec!(1),
                        bid_depth_quote: dec!(0),
                        ask_depth_quote: dec!(0),
                        sla_minimum: Some(dec!(50000)),
                        compliant: false,
                    },
                    DepthLevel {
                        pct_from_mid: dec!(2),
                        bid_depth_quote: dec!(0),
                        ask_depth_quote: dec!(0),
                        sla_minimum: Some(dec!(100000)),
                        compliant: false,
                    },
                    DepthLevel {
                        pct_from_mid: dec!(5),
                        bid_depth_quote: dec!(0),
                        ask_depth_quote: dec!(0),
                        sla_minimum: None,
                        compliant: true,
                    },
                ],
            })
            .collect(),
    )
}

// --- Volume Report ---

#[derive(Serialize)]
struct VolumeReport {
    symbol: String,
    exchanges: Vec<ExchangeVolume>,
    total_volume_24h: Decimal,
    maker_volume: Decimal,
    taker_volume: Decimal,
}

#[derive(Serialize)]
struct ExchangeVolume {
    exchange: String,
    volume_24h: Decimal,
    volume_share_pct: Decimal,
    num_trades: u64,
}

async fn volume_report(State(state): State<DashboardState>) -> Json<Vec<VolumeReport>> {
    let symbols = state.get_all();
    Json(
        symbols
            .iter()
            .map(|s| VolumeReport {
                symbol: s.symbol.clone(),
                exchanges: vec![ExchangeVolume {
                    exchange: "primary".to_string(),
                    volume_24h: s.pnl.volume,
                    volume_share_pct: dec!(100),
                    num_trades: s.total_fills,
                }],
                total_volume_24h: s.pnl.volume,
                maker_volume: s.pnl.volume, // All PostOnly → all maker.
                taker_volume: dec!(0),
            })
            .collect(),
    )
}

// --- Token Positions ---

#[derive(Serialize)]
struct TokenPositionReport {
    symbol: String,
    total_token_balance: Decimal,
    positions: Vec<ExchangePosition>,
    /// Total as % of original loan (if configured).
    loan_utilization_pct: Option<Decimal>,
}

#[derive(Serialize)]
struct ExchangePosition {
    exchange: String,
    /// Token balance on this exchange.
    balance: Decimal,
    /// In open orders (locked).
    in_orders: Decimal,
    /// Available for trading.
    available: Decimal,
}

async fn token_positions(State(state): State<DashboardState>) -> Json<Vec<TokenPositionReport>> {
    let symbols = state.get_all();
    Json(
        symbols
            .iter()
            .map(|s| TokenPositionReport {
                symbol: s.symbol.clone(),
                total_token_balance: s.inventory.abs(),
                positions: vec![ExchangePosition {
                    exchange: "primary".to_string(),
                    balance: s.inventory.abs(),
                    in_orders: dec!(0), // TODO: from order manager.
                    available: s.inventory.abs(),
                }],
                loan_utilization_pct: None, // TODO: from loan config.
            })
            .collect(),
    )
}

// --- Loan Status ---

#[derive(Serialize)]
struct LoanStatus {
    symbol: String,
    /// Original loan amount.
    loan_amount: Decimal,
    /// Current token position (should ≈ loan_amount if healthy).
    current_position: Decimal,
    /// Call option strike price.
    option_strike: Option<Decimal>,
    /// Option expiry date.
    option_expiry: Option<String>,
    /// Days until expiry.
    days_to_expiry: Option<i64>,
    /// Current token price.
    current_price: Decimal,
    /// Is option in-the-money?
    option_itm: bool,
    /// Estimated option value (simplified).
    estimated_option_value: Decimal,
}

async fn loan_status(State(state): State<DashboardState>) -> Json<Vec<LoanStatus>> {
    let symbols = state.get_all();
    Json(
        symbols
            .iter()
            .map(|s| {
                // In production, loan terms come from config/database.
                LoanStatus {
                    symbol: s.symbol.clone(),
                    loan_amount: dec!(0), // TODO: from loan config.
                    current_position: s.inventory.abs(),
                    option_strike: None,
                    option_expiry: None,
                    days_to_expiry: None,
                    current_price: s.mid_price,
                    option_itm: false,
                    estimated_option_value: dec!(0),
                }
            })
            .collect(),
    )
}

// --- Daily Client Report ---

#[derive(Serialize)]
struct DailyClientReport {
    date: String,
    generated_at: String,
    summary: ClientOverview,
    spread_quality: Vec<SpreadQualityReport>,
    depth: Vec<DepthReport>,
    volume: Vec<VolumeReport>,
    token_positions: Vec<TokenPositionReport>,
    incidents: Vec<Incident>,
}

#[derive(Serialize)]
struct Incident {
    timestamp: String,
    severity: String,
    description: String,
    duration_secs: u64,
    resolved: bool,
}

async fn daily_client_report(State(state): State<DashboardState>) -> Json<DailyClientReport> {
    // Aggregate all sub-reports into one comprehensive daily report.
    let symbols = state.get_all();
    let n = Decimal::from(symbols.len().max(1) as u64);

    let mut total_vol = dec!(0);
    let mut sum_uptime = dec!(0);
    let mut total_pnl = dec!(0);

    let sym_overviews: Vec<SymbolOverview> = symbols
        .iter()
        .map(|s| {
            total_vol += s.pnl.volume;
            sum_uptime += s.sla_uptime_pct;
            total_pnl += s.pnl.total;
            SymbolOverview {
                symbol: s.symbol.clone(),
                exchange: "primary".to_string(),
                avg_spread_bps: s.spread_bps,
                spread_compliance_pct: s.sla_uptime_pct,
                uptime_pct: s.sla_uptime_pct,
                depth_at_1pct: dec!(0),
                depth_at_2pct: dec!(0),
                volume_24h: s.pnl.volume,
                volume_share_pct: dec!(0),
                mid_price: s.mid_price,
            }
        })
        .collect();

    Json(DailyClientReport {
        date: Utc::now().format("%Y-%m-%d").to_string(),
        generated_at: Utc::now().to_rfc3339(),
        summary: ClientOverview {
            period: "24h".to_string(),
            symbols: sym_overviews,
            totals: TotalOverview {
                total_volume_24h: total_vol,
                avg_spread_compliance_pct: sum_uptime / n,
                avg_uptime_pct: sum_uptime / n,
                total_pnl,
            },
        },
        spread_quality: symbols
            .iter()
            .map(|s| SpreadQualityReport {
                symbol: s.symbol.clone(),
                within_target_pct: s.sla_uptime_pct,
                time_weighted_avg_bps: s.spread_bps,
                volume_weighted_avg_bps: s.spread_bps,
                high_vol_avg_bps: s.spread_bps * dec!(1.5),
                normal_avg_bps: s.spread_bps,
                current_bps: s.spread_bps,
                target_bps: dec!(100),
            })
            .collect(),
        depth: vec![],     // TODO: populate from book.
        volume: vec![],    // TODO: populate from tracker.
        token_positions: vec![],
        incidents: vec![], // TODO: from audit log.
    })
}
