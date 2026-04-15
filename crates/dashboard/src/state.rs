use chrono::{DateTime, Utc};
use mm_common::config::LoanConfig;
use mm_portfolio::PortfolioSnapshot;
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Shared state for the dashboard — updated by engines, read by HTTP handlers.
#[derive(Debug, Clone, Default)]
pub struct DashboardState {
    inner: Arc<RwLock<StateInner>>,
}

#[derive(Debug, Default)]
struct StateInner {
    symbols: HashMap<String, SymbolState>,
    loans: HashMap<String, LoanConfig>,
    incidents: Vec<IncidentRecord>,
    /// Last-seen portfolio snapshot, aggregated across all
    /// symbols/engines. `None` when the operator runs without
    /// `mm-portfolio` wired — in that case the dashboard still
    /// shows per-symbol PnL from `PnlTracker` but the unified
    /// multi-currency view is unavailable.
    portfolio: Option<PortfolioSnapshot>,
}

/// A recorded incident for the daily report.
#[derive(Debug, Clone, Serialize)]
pub struct IncidentRecord {
    pub timestamp: DateTime<Utc>,
    pub severity: String,
    pub description: String,
    pub duration_secs: u64,
    pub resolved: bool,
}

/// Per-symbol state snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolState {
    pub symbol: String,
    pub mid_price: Decimal,
    pub spread_bps: Decimal,
    pub inventory: Decimal,
    pub inventory_value: Decimal,
    pub live_orders: usize,
    pub total_fills: u64,
    pub pnl: PnlSnapshot,
    pub volatility: Decimal,
    pub vpin: Decimal,
    pub kyle_lambda: Decimal,
    pub adverse_bps: Decimal,
    /// Latest Market Resilience score in `[0, 1]`. `1.0` is
    /// "fully recovered / steady state", anything lower means
    /// the book has just been hit by a shock that hasn't fully
    /// cleared.
    pub market_resilience: Decimal,
    /// Regulatory Order-to-Trade Ratio. High values indicate
    /// spoofing / layering; MiCA compliance requires venues
    /// and market makers to monitor this.
    pub order_to_trade_ratio: Decimal,
    /// Latest Hull Moving Average on mid-price. `None` before
    /// the HMA is warmed up, `Some(value)` once it has enough
    /// samples.
    pub hma_value: Option<Decimal>,
    pub kill_level: u8,
    pub sla_uptime_pct: Decimal,
    pub regime: String,
    /// Spread-only compliance (% of ticks where spread was within SLA limit).
    pub spread_compliance_pct: Decimal,
    /// Book depth at various percentages from mid (pct, bid_quote, ask_quote).
    pub book_depth_levels: Vec<BookDepthLevel>,
    /// Total value locked in open orders (quote asset).
    pub locked_in_orders_quote: Decimal,
    /// SLA max spread from config.
    pub sla_max_spread_bps: Decimal,
    /// SLA min depth from config.
    pub sla_min_depth_quote: Decimal,
    /// Per-pair daily presence percentage rolled up from the
    /// `SlaTracker`'s 1440 per-minute buckets (P2.2). Counts
    /// observation seconds, not minute buckets, so a minute
    /// with 60 samples and 30 compliant outweighs a minute
    /// with 30 samples and 30 compliant.
    pub presence_pct_24h: Decimal,
    /// Per-pair daily two-sided percentage — separate from
    /// `presence_pct_24h` because some MM rebate agreements
    /// pay against two-sided uptime independently of the
    /// spread floor.
    pub two_sided_pct_24h: Decimal,
    /// Number of distinct minutes today that recorded any
    /// samples. Useful to distinguish a fresh start
    /// ("100 % over 0 minutes") from a steady-state day.
    pub minutes_with_data_24h: u32,
}

/// Depth at a specific percentage from mid price.
#[derive(Debug, Clone, Serialize)]
pub struct BookDepthLevel {
    pub pct_from_mid: Decimal,
    pub bid_depth_quote: Decimal,
    pub ask_depth_quote: Decimal,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PnlSnapshot {
    pub total: Decimal,
    pub spread: Decimal,
    pub inventory: Decimal,
    pub rebates: Decimal,
    pub fees: Decimal,
    pub round_trips: u64,
    pub volume: Decimal,
}

impl DashboardState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update state for a symbol.
    pub fn update(&self, state: SymbolState) {
        let mut inner = self.inner.write().unwrap();
        // Update prometheus metrics.
        crate::metrics::MID_PRICE
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.mid_price));
        crate::metrics::SPREAD_BPS
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.spread_bps));
        crate::metrics::INVENTORY
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.inventory));
        crate::metrics::INVENTORY_VALUE
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.inventory_value));
        crate::metrics::LIVE_ORDERS
            .with_label_values(&[&state.symbol])
            .set(state.live_orders as f64);
        crate::metrics::PNL_TOTAL
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.pnl.total));
        crate::metrics::PNL_SPREAD
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.pnl.spread));
        crate::metrics::PNL_INVENTORY
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.pnl.inventory));
        crate::metrics::PNL_REBATES
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.pnl.rebates));
        crate::metrics::VOLATILITY
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.volatility));
        crate::metrics::VPIN
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.vpin));
        crate::metrics::KYLE_LAMBDA
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.kyle_lambda));
        crate::metrics::ADVERSE_BPS
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.adverse_bps));
        crate::metrics::MARKET_RESILIENCE
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.market_resilience));
        crate::metrics::ORDER_TO_TRADE_RATIO
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.order_to_trade_ratio));
        if let Some(hma) = state.hma_value {
            crate::metrics::HMA_VALUE
                .with_label_values(&[&state.symbol])
                .set(decimal_to_f64(hma));
        }
        crate::metrics::KILL_SWITCH_LEVEL
            .with_label_values(&[&state.symbol])
            .set(state.kill_level as f64);
        crate::metrics::SLA_UPTIME
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.sla_uptime_pct));
        crate::metrics::SLA_PRESENCE_PCT_24H
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.presence_pct_24h));

        inner.symbols.insert(state.symbol.clone(), state);
    }

    /// Get all symbol states for the HTTP JSON endpoint.
    pub fn get_all(&self) -> Vec<SymbolState> {
        let inner = self.inner.read().unwrap();
        inner.symbols.values().cloned().collect()
    }

    /// Get state for a single symbol.
    pub fn get_symbol(&self, symbol: &str) -> Option<SymbolState> {
        let inner = self.inner.read().unwrap();
        inner.symbols.get(symbol).cloned()
    }

    /// Set loan configs (from AppConfig).
    pub fn set_loans(&self, loans: HashMap<String, LoanConfig>) {
        let mut inner = self.inner.write().unwrap();
        inner.loans = loans;
    }

    /// Get loan config for a symbol.
    pub fn get_loan(&self, symbol: &str) -> Option<LoanConfig> {
        let inner = self.inner.read().unwrap();
        inner.loans.get(symbol).cloned()
    }

    /// Record an incident.
    pub fn add_incident(&self, incident: IncidentRecord) {
        let mut inner = self.inner.write().unwrap();
        inner.incidents.push(incident);
    }

    /// Get all incidents (for daily report).
    pub fn get_incidents(&self) -> Vec<IncidentRecord> {
        let inner = self.inner.read().unwrap();
        inner.incidents.clone()
    }

    /// Publish a portfolio snapshot + its Prometheus gauges.
    /// The engine calls this every summary tick with a snapshot
    /// taken under the shared portfolio mutex.
    pub fn update_portfolio(&self, snap: PortfolioSnapshot) {
        crate::metrics::PORTFOLIO_TOTAL_EQUITY
            .with_label_values(&[&snap.reporting_currency])
            .set(decimal_to_f64(snap.total_equity));
        crate::metrics::PORTFOLIO_REALISED_PNL
            .with_label_values(&[&snap.reporting_currency])
            .set(decimal_to_f64(snap.total_realised_pnl));
        crate::metrics::PORTFOLIO_UNREALISED_PNL
            .with_label_values(&[&snap.reporting_currency])
            .set(decimal_to_f64(snap.total_unrealised_pnl));
        for (symbol, asset) in &snap.per_asset {
            crate::metrics::PORTFOLIO_ASSET_QTY
                .with_label_values(&[symbol])
                .set(decimal_to_f64(asset.qty));
            crate::metrics::PORTFOLIO_ASSET_UNREALISED
                .with_label_values(&[symbol])
                .set(decimal_to_f64(asset.unrealised_pnl_reporting));
        }
        self.inner.write().unwrap().portfolio = Some(snap);
    }

    /// Read the last-published portfolio snapshot.
    pub fn get_portfolio(&self) -> Option<PortfolioSnapshot> {
        self.inner.read().unwrap().portfolio.clone()
    }
}

fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}
