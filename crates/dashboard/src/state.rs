use chrono::{DateTime, Utc};
use mm_common::config::LoanConfig;
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
        crate::metrics::KILL_SWITCH_LEVEL
            .with_label_values(&[&state.symbol])
            .set(state.kill_level as f64);
        crate::metrics::SLA_UPTIME
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.sla_uptime_pct));

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
}

fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}
