use chrono::{DateTime, Utc};
use mm_common::config::LoanConfig;
use mm_portfolio::PortfolioSnapshot;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Shared state for the dashboard — updated by engines, read by HTTP handlers.
#[derive(Debug, Clone, Default)]
pub struct DashboardState {
    inner: Arc<RwLock<StateInner>>,
}

/// Hot config override that can be sent to a running engine
/// without restarting. The engine applies the override to its
/// owned `AppConfig` copy on the next select-loop tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "field", content = "value")]
pub enum ConfigOverride {
    /// Risk aversion γ for Avellaneda-Stoikov.
    Gamma(Decimal),
    /// Minimum spread floor (bps).
    MinSpreadBps(Decimal),
    /// Base order size (base asset units).
    OrderSize(Decimal),
    /// Max distance from mid (bps).
    MaxDistanceBps(Decimal),
    /// Number of quote levels per side.
    NumLevels(usize),
    /// Toggle momentum alpha signal.
    MomentumEnabled(bool),
    /// Toggle market resilience widening.
    MarketResilienceEnabled(bool),
    /// Toggle amend-in-place (vs cancel+replace).
    AmendEnabled(bool),
    /// Amend tick budget.
    AmendMaxTicks(u32),
    /// Toggle OTR audit snapshots.
    OtrEnabled(bool),
    /// Max inventory (base asset).
    MaxInventory(Decimal),
    /// Pause quoting for this symbol (lifecycle_paused = true).
    PauseQuoting,
    /// Resume quoting for this symbol (lifecycle_paused = false).
    ResumeQuoting,
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
    /// Recent fills for the client API. Capped at
    /// `MAX_RECENT_FILLS` entries, oldest evicted first.
    recent_fills: std::collections::VecDeque<FillRecord>,
    /// Per-symbol config override senders. Engines register
    /// their receiver at startup; admin endpoints send
    /// overrides through these channels.
    config_overrides: HashMap<String, tokio::sync::mpsc::UnboundedSender<ConfigOverride>>,
    /// Append-only fill log writer for persistence across
    /// restarts. Set via `DashboardState::enable_fill_log`.
    fill_log_writer: Option<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>,
    /// Shared webhook dispatcher for client event delivery.
    webhook_dispatcher: Option<crate::webhooks::WebhookDispatcher>,
}

/// Maximum recent fills retained in dashboard state.
const MAX_RECENT_FILLS: usize = 1000;

/// A fill record for the client-facing `/api/v1/fills/recent`
/// endpoint. Captures the fill details plus the NBBO at the
/// time of execution for quality benchmarking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillRecord {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub side: String,
    pub price: Decimal,
    pub qty: Decimal,
    pub is_maker: bool,
    pub fee: Decimal,
    /// Best bid at the time of the fill (NBBO capture).
    pub nbbo_bid: Decimal,
    /// Best ask at the time of the fill (NBBO capture).
    pub nbbo_ask: Decimal,
    /// Slippage vs mid at fill time, in bps. Positive = adverse
    /// (filled worse than mid). Negative = favorable.
    pub slippage_bps: Decimal,
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
    /// Epic D stage-3 — per-side adverse-selection probabilities
    /// derived from
    /// `AdverseSelectionTracker::adverse_selection_bps_{bid,ask}`
    /// via `cartea_spread::as_prob_from_bps`. Both sit at 0.5
    /// (neutral) until the per-side tracker has ≥5 completed
    /// fills on that side. `None` is published as 0.5 to the
    /// gauge so dashboards see a stable baseline before the
    /// per-side path activates.
    pub as_prob_bid: Option<Decimal>,
    pub as_prob_ask: Option<Decimal>,
    /// Epic D wave-2 — Cont-Kukanov-Stoikov L1 OFI EWMA from
    /// `MomentumSignals`. `None` when the OFI tracker has not
    /// been attached (`momentum_ofi_enabled = false`) or has
    /// not yet seen its first observation.
    pub momentum_ofi_ewma: Option<Decimal>,
    /// Epic D wave-2 — Stoikov 2018 learned-microprice drift
    /// expressed as a fraction of the current mid. `None`
    /// when no learned MP model is attached or the current
    /// (imbalance, spread) bucket is under-sampled.
    pub momentum_learned_mp_drift: Option<Decimal>,
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
    /// Per-hour SLA breakdown for time-of-day analysis. 24
    /// entries, one per UTC hour.
    pub hourly_presence: Vec<mm_risk::sla::HourlyPresenceSummary>,
    /// Market impact report for this symbol.
    pub market_impact: Option<mm_risk::market_impact::MarketImpactReport>,
    /// Performance metrics (Sharpe, Sortino, drawdown, etc.).
    pub performance: Option<mm_risk::performance::PerformanceMetrics>,
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
        // Epic D stage-3 — per-side ρ + wave-2 momentum
        // observability. Per-side gauges baseline at 0.5
        // (neutral) when the per-side tracker has fewer than
        // 5 completed fills on a side; OFI EWMA + learned MP
        // drift gauges baseline at 0.0 when the corresponding
        // optional signal is not attached.
        crate::metrics::AS_PROB_BID
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.as_prob_bid.unwrap_or(dec!(0.5))));
        crate::metrics::AS_PROB_ASK
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.as_prob_ask.unwrap_or(dec!(0.5))));
        crate::metrics::MOMENTUM_OFI_EWMA
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.momentum_ofi_ewma.unwrap_or(dec!(0))));
        crate::metrics::MOMENTUM_LEARNED_MP_DRIFT
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(
                state.momentum_learned_mp_drift.unwrap_or(dec!(0)),
            ));
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
        // P2.3 Epic C sub-component #1: per-factor delta gauges.
        for (factor, delta) in &snap.per_factor {
            crate::metrics::PORTFOLIO_FACTOR_DELTA
                .with_label_values(&[factor])
                .set(decimal_to_f64(*delta));
        }
        // P2.3 Epic C sub-component #2: per-strategy PnL gauges.
        for (strategy, pnl) in &snap.per_strategy {
            crate::metrics::PORTFOLIO_STRATEGY_PNL
                .with_label_values(&[strategy])
                .set(decimal_to_f64(*pnl));
        }
        self.inner.write().unwrap().portfolio = Some(snap);
    }

    /// Read the last-published portfolio snapshot.
    pub fn get_portfolio(&self) -> Option<PortfolioSnapshot> {
        self.inner.read().unwrap().portfolio.clone()
    }

    /// Set the webhook dispatcher (shared across all engines).
    pub fn set_webhook_dispatcher(&self, wh: crate::webhooks::WebhookDispatcher) {
        self.inner.write().unwrap().webhook_dispatcher = Some(wh);
    }

    /// Get the webhook dispatcher for admin endpoints.
    pub fn webhook_dispatcher(&self) -> Option<crate::webhooks::WebhookDispatcher> {
        self.inner.read().unwrap().webhook_dispatcher.clone()
    }

    /// Load fill history from a JSONL file (one FillRecord per
    /// line). Called at startup to restore recent fills from a
    /// previous session. Ignores malformed lines.
    pub fn load_fill_history(&self, path: &std::path::Path) {
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let mut inner = self.inner.write().unwrap();
        let mut loaded = 0usize;
        for line in content.lines().rev().take(MAX_RECENT_FILLS) {
            if let Ok(fill) = serde_json::from_str::<FillRecord>(line) {
                inner.recent_fills.push_front(fill);
                loaded += 1;
            }
        }
        // Trim to cap.
        while inner.recent_fills.len() > MAX_RECENT_FILLS {
            inner.recent_fills.pop_front();
        }
        if loaded > 0 {
            tracing::info!(loaded, "restored fill history from disk");
        }
    }

    /// Enable persistent fill logging to a JSONL file. Each
    /// fill is appended as one JSON line so the file survives
    /// restarts. Call once at startup.
    pub fn enable_fill_log(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            self.inner.write().unwrap().fill_log_writer =
                Some(std::sync::Mutex::new(std::io::BufWriter::new(file)));
        }
    }

    /// Register a config override channel for a symbol. Called
    /// by the engine at startup.
    pub fn register_config_channel(
        &self,
        symbol: &str,
        tx: tokio::sync::mpsc::UnboundedSender<ConfigOverride>,
    ) {
        self.inner
            .write()
            .unwrap()
            .config_overrides
            .insert(symbol.to_string(), tx);
    }

    /// Send a config override to a specific symbol's engine.
    /// Returns `false` if the symbol is not registered or the
    /// channel is closed.
    pub fn send_config_override(&self, symbol: &str, ovr: ConfigOverride) -> bool {
        let inner = self.inner.read().unwrap();
        if let Some(tx) = inner.config_overrides.get(symbol) {
            tx.send(ovr).is_ok()
        } else {
            false
        }
    }

    /// Send a config override to ALL registered symbols.
    /// Returns the number of engines that accepted the override.
    pub fn broadcast_config_override(&self, ovr: ConfigOverride) -> usize {
        let inner = self.inner.read().unwrap();
        inner
            .config_overrides
            .values()
            .filter(|tx| tx.send(ovr.clone()).is_ok())
            .count()
    }

    /// List all symbols that have registered config channels.
    pub fn config_symbols(&self) -> Vec<String> {
        let inner = self.inner.read().unwrap();
        let mut v: Vec<String> = inner.config_overrides.keys().cloned().collect();
        v.sort();
        v
    }

    /// Record a fill with NBBO snapshot for the client API.
    /// Called by the engine on every fill event. Persists to
    /// disk if a fill log path is set.
    pub fn record_fill(&self, fill: FillRecord) {
        // Persist to disk for fill history across restarts.
        if let Ok(inner) = self.inner.read() {
            if let Some(writer) = &inner.fill_log_writer {
                if let Ok(mut w) = writer.lock() {
                    if let Ok(line) = serde_json::to_string(&fill) {
                        use std::io::Write;
                        let _ = writeln!(w, "{}", line);
                        let _ = w.flush();
                    }
                }
            }
        }
        let mut inner = self.inner.write().unwrap();
        inner.recent_fills.push_back(fill);
        while inner.recent_fills.len() > MAX_RECENT_FILLS {
            inner.recent_fills.pop_front();
        }
    }

    /// Get recent fills, optionally filtered by symbol.
    /// Returns newest-first, capped at `limit`.
    pub fn get_recent_fills(&self, symbol: Option<&str>, limit: usize) -> Vec<FillRecord> {
        let inner = self.inner.read().unwrap();
        inner
            .recent_fills
            .iter()
            .rev()
            .filter(|f| symbol.is_none_or(|s| f.symbol == s))
            .take(limit)
            .cloned()
            .collect()
    }
}

fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_state(symbol: &str) -> SymbolState {
        SymbolState {
            symbol: symbol.to_string(),
            mid_price: dec!(50_000),
            spread_bps: dec!(2),
            inventory: dec!(0),
            inventory_value: dec!(0),
            live_orders: 0,
            total_fills: 0,
            pnl: PnlSnapshot::default(),
            volatility: dec!(0.02),
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
            regime: "Quiet".to_string(),
            spread_compliance_pct: dec!(100),
            book_depth_levels: vec![],
            locked_in_orders_quote: dec!(0),
            sla_max_spread_bps: dec!(50),
            sla_min_depth_quote: dec!(0),
            presence_pct_24h: dec!(100),
            two_sided_pct_24h: dec!(100),
            minutes_with_data_24h: 0,
            hourly_presence: vec![],
            market_impact: None,
            performance: None,
        }
    }

    /// Epic D stage-3 — pin that `state.update` accepts the
    /// new wave-2 / per-side fields without regressing the
    /// existing publish path. The actual gauge values are a
    /// side effect of the prometheus crate and are not
    /// trivially observable from a unit test, so we assert
    /// only that a default-`None` `SymbolState` flows through
    /// cleanly and that the per-pair entry is retrievable
    /// post-update.
    #[test]
    fn state_update_accepts_new_wave2_fields() {
        crate::metrics::init();
        let ds = DashboardState::new();
        let mut s = empty_state("BTCUSDT");
        s.as_prob_bid = Some(dec!(0.7));
        s.as_prob_ask = Some(dec!(0.4));
        s.momentum_ofi_ewma = Some(dec!(0.123));
        s.momentum_learned_mp_drift = Some(dec!(0.0001));
        ds.update(s);
        let got = ds.get_symbol("BTCUSDT").unwrap();
        assert_eq!(got.as_prob_bid, Some(dec!(0.7)));
        assert_eq!(got.as_prob_ask, Some(dec!(0.4)));
        assert_eq!(got.momentum_ofi_ewma, Some(dec!(0.123)));
        assert_eq!(got.momentum_learned_mp_drift, Some(dec!(0.0001)));
    }

    /// `None` per-side ρ flows through cleanly — the
    /// dashboard publishes the 0.5 baseline to Prometheus
    /// but the original `Option<None>` is preserved in the
    /// in-memory state for the JSON API.
    #[test]
    fn state_update_preserves_none_in_json_api() {
        crate::metrics::init();
        let ds = DashboardState::new();
        let s = empty_state("ETHUSDT");
        ds.update(s);
        let got = ds.get_symbol("ETHUSDT").unwrap();
        assert_eq!(got.as_prob_bid, None);
        assert_eq!(got.as_prob_ask, None);
        assert_eq!(got.momentum_ofi_ewma, None);
        assert_eq!(got.momentum_learned_mp_drift, None);
    }
}
