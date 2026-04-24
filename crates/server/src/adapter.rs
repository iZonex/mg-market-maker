//! Fleet → DashboardState adapter.
//!
//! Periodic task: every `interval` reads a snapshot of the
//! `FleetState` (agents + their deployment telemetry) and
//! projects it into the legacy `DashboardState` that the
//! dashboard HTTP endpoints read from. Lets the Svelte UI
//! continue to work unchanged against the old endpoint shape
//! while the backing architecture is now distributed across
//! remote agents.
//!
//! Fields populated today from `DeploymentStateRow`:
//! - `symbol`, `mode`, `venue`, `product`, `strategy` (template)
//! - `inventory`, `pnl.total` + `pnl.inventory`
//! - `live_orders`, `kill_level`, `regime`
//! - `manipulation_score` — four-field snapshot the engine now
//!   emits via Prometheus + agent scrapes
//! - `adaptive_state` — gamma_factor + last_reason (pair_class
//!   derives from the row's `pair_class` variable when set)
//! - `tunable_config` — read out of the deployment's
//!   `variables` map so the UI's tunable tab shows the LIVE
//!   value the strategy is running with
//!
//! Fields still defaulted because the data isn't on
//! DeploymentStateRow yet: VPIN, Kyle's lambda, adverse-select
//! probabilities, OTR ratio, market-resilience, HMA, book depth
//! levels, SLA uptime, spread compliance, hourly presence.
//! These need engine-side Prometheus emission first, then agent
//! scrape, then row extension. Tracked in TODO.md stabilization
//! section.

use std::time::Duration;

use mm_controller::FleetState;
use mm_dashboard::state::{
    AdaptiveStateSnapshot, CalibrationSnapshot, DashboardState, ManipulationScoreSnapshot,
    PnlSnapshot, SymbolState, TunableConfigSnapshot,
};
use rust_decimal::Decimal;
use std::str::FromStr;

pub fn spawn_fleet_to_dashboard_adapter(
    fleet: FleetState,
    dashboard: DashboardState,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            project_fleet_into_dashboard(&fleet, &dashboard);
        }
    })
}

fn project_fleet_into_dashboard(fleet: &FleetState, dashboard: &DashboardState) {
    for agent in fleet.snapshot() {
        for dep in &agent.deployments {
            let inventory = dep.inventory.parse::<Decimal>().unwrap_or(Decimal::ZERO);
            let unrealized = dep
                .unrealized_pnl_quote
                .parse::<Decimal>()
                .unwrap_or(Decimal::ZERO);
            // 2026-04 stabilization — feed the per-leg inventory
            // ring buffer the `PerLegInventoryChart` reads from.
            // Pre-distributed the local engine called this each
            // tick; in the distributed deploy this is the only
            // writer for the buffer. `venue` defaults to "agent"
            // when the row hasn't populated it yet so the chart
            // still gets a track rather than silently dropping
            // the data.
            let venue = if dep.venue.is_empty() {
                "unknown".to_string()
            } else {
                dep.venue.clone()
            };
            dashboard.publish_inventory(&dep.symbol, &venue, inventory, None);
            // Calibration snapshot — agent already emits the
            // three fields (a, k, samples) via its Prometheus
            // scrape of engine-side gauges. Push into the
            // dashboard's snapshots map so the legacy
            // `/api/v1/calibration/status` endpoint serves live
            // data in distributed mode.
            if !dep.calibration_a.is_empty() || dep.calibration_samples > 0 {
                dashboard.publish_calibration(CalibrationSnapshot {
                    symbol: dep.symbol.clone(),
                    strategy: dep.template.clone(),
                    a: parse_decimal(&dep.calibration_a),
                    k: parse_decimal(&dep.calibration_k),
                    samples: dep.calibration_samples as usize,
                    // Row doesn't carry the recalibration
                    // timestamp yet; telemetry `sampled_at_ms`
                    // is the closest proxy for "when was this
                    // state observed" so the UI staleness chip
                    // still works.
                    last_recalibrated_ms: Some(dep.sampled_at_ms),
                });
            }
            let state = build_symbol_state(dep, inventory, unrealized);
            dashboard.update(state);
        }
    }
}

fn build_symbol_state(
    dep: &mm_control::DeploymentStateRow,
    inventory: Decimal,
    unrealized: Decimal,
) -> SymbolState {
    let mut pnl = PnlSnapshot::default();
    // Inventory-PnL slot carries the mark-to-market line; the
    // UI `total` field is summed across the PnL components.
    pnl.inventory = unrealized;
    pnl.total = unrealized;
    let mut state = default_symbol_state(dep.symbol.clone(), inventory, pnl, dep.running);
    // Fill the fields the agent now carries in telemetry. Older
    // agents that don't populate them leave strings empty and
    // the UI falls back to "—". Decimal parsing is best-effort;
    // a bogus string keeps the default.
    if !dep.venue.is_empty() {
        state.venue = dep.venue.clone();
    }
    if !dep.product.is_empty() {
        state.product = dep.product.clone();
    }
    if !dep.mode.is_empty() {
        state.mode = dep.mode.clone();
    }
    if !dep.regime.is_empty() {
        state.regime = dep.regime.clone();
    }
    if dep.kill_level > 0 {
        state.kill_level = dep.kill_level;
    }
    if dep.live_orders > 0 {
        state.live_orders = dep.live_orders as usize;
    }
    if !dep.template.is_empty() {
        state.strategy = dep.template.clone();
    }

    // ── Book + toxicity + SLA scalars (2026-04-21 port).
    // Agent reads its shared DashboardState each snapshot tick
    // and forwards these as decimal strings — empty = "no
    // sample yet" so the adapter leaves the default in place
    // (UI renders "—" / 0).
    if !dep.mid_price.is_empty() {
        state.mid_price = parse_decimal(&dep.mid_price);
    }
    if !dep.spread_bps.is_empty() {
        state.spread_bps = parse_decimal(&dep.spread_bps);
    }
    if !dep.volatility.is_empty() {
        state.volatility = parse_decimal(&dep.volatility);
    }
    if !dep.vpin.is_empty() {
        state.vpin = parse_decimal(&dep.vpin);
    }
    if !dep.kyle_lambda.is_empty() {
        state.kyle_lambda = parse_decimal(&dep.kyle_lambda);
    }
    if !dep.adverse_bps.is_empty() {
        state.adverse_bps = parse_decimal(&dep.adverse_bps);
    }
    if !dep.sla_uptime_pct.is_empty() {
        state.sla_uptime_pct = parse_decimal(&dep.sla_uptime_pct);
    }
    if !dep.presence_pct_24h.is_empty() {
        state.presence_pct_24h = parse_decimal(&dep.presence_pct_24h);
    }
    if !dep.two_sided_pct_24h.is_empty() {
        state.two_sided_pct_24h = parse_decimal(&dep.two_sided_pct_24h);
    }

    // ── Richer nested structures. Each is opaque JSON on the
    // wire; the adapter deserialises back into the dashboard's
    // concrete type. Deserialisation failures leave the default
    // in place so a schema mismatch never blanks operator view.
    if !dep.open_orders.is_empty() {
        state.open_orders = dep
            .open_orders
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
    }
    if !dep.hourly_presence.is_empty() {
        state.hourly_presence = dep
            .hourly_presence
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
    }
    if dep.minutes_with_data_24h > 0 {
        state.minutes_with_data_24h = dep.minutes_with_data_24h;
    }
    if let Some(v) = dep.market_impact.as_ref() {
        if let Ok(parsed) = serde_json::from_value(v.clone()) {
            state.market_impact = Some(parsed);
        }
    }
    if let Some(v) = dep.performance.as_ref() {
        if let Ok(parsed) = serde_json::from_value(v.clone()) {
            state.performance = Some(parsed);
        }
    }
    if let Some(v) = dep.active_graph.as_ref() {
        if let Ok(parsed) = serde_json::from_value(v.clone()) {
            state.active_graph = Some(parsed);
        }
    }

    // ── Manipulation detector scores (fleet-wide engine
    // gauges scraped into the row) ────────────────────────────
    // We only populate the snapshot if at least one of the four
    // fields has a numeric value. Empty strings = detector
    // warming up — leave None so UI renders "—" rather than
    // misleading zeros.
    let manip = ManipulationScoreSnapshot {
        pump_dump: parse_decimal(&dep.manipulation_pump_dump),
        wash: parse_decimal(&dep.manipulation_wash),
        thin_book: parse_decimal(&dep.manipulation_thin_book),
        combined: parse_decimal(&dep.manipulation_combined),
    };
    if !dep.manipulation_combined.is_empty()
        || !dep.manipulation_pump_dump.is_empty()
        || !dep.manipulation_wash.is_empty()
        || !dep.manipulation_thin_book.is_empty()
    {
        state.manipulation_score = Some(manip);
    }

    // ── Adaptive state (γ factor + reason) ───────────────────
    if !dep.adaptive_gamma.is_empty() || !dep.adaptive_reason.is_empty() {
        let gamma_factor = parse_decimal(&dep.adaptive_gamma);
        let pair_class = variable_str(dep, "pair_class").unwrap_or_default();
        state.adaptive_state = Some(AdaptiveStateSnapshot {
            pair_class,
            enabled: variable_bool(dep, "adaptive_enabled").unwrap_or(false),
            gamma_factor: if gamma_factor == Decimal::ZERO {
                Decimal::ONE
            } else {
                gamma_factor
            },
            last_reason: dep.adaptive_reason.clone(),
        });
    }

    // ── Tunable config (operator-visible variables snapshot)
    // Reads from the deployment's `variables` map — the operator
    // sees the LIVE values the strategy runs with, not the
    // config.toml defaults. Any missing key falls back to 0 /
    // false (UI treats those as "not set").
    if !dep.variables.is_empty() {
        state.tunable_config = Some(TunableConfigSnapshot {
            gamma: variable_decimal(dep, "gamma").unwrap_or(Decimal::ZERO),
            kappa: variable_decimal(dep, "kappa").unwrap_or(Decimal::ZERO),
            sigma: variable_decimal(dep, "sigma").unwrap_or(Decimal::ZERO),
            order_size: variable_decimal(dep, "order_size").unwrap_or(Decimal::ZERO),
            num_levels: variable_u64(dep, "num_levels").unwrap_or(0) as u32,
            min_spread_bps: variable_decimal(dep, "min_spread_bps").unwrap_or(Decimal::ZERO),
            max_distance_bps: variable_decimal(dep, "max_distance_bps").unwrap_or(Decimal::ZERO),
            max_inventory: variable_decimal(dep, "max_inventory").unwrap_or(Decimal::ZERO),
            momentum_enabled: variable_bool(dep, "momentum_enabled").unwrap_or(false),
            market_resilience_enabled: variable_bool(dep, "market_resilience_enabled")
                .unwrap_or(false),
            amend_enabled: variable_bool(dep, "amend_enabled").unwrap_or(false),
            amend_max_ticks: variable_u64(dep, "amend_max_ticks").unwrap_or(0) as u32,
            otr_enabled: variable_bool(dep, "otr_enabled").unwrap_or(false),
        });
    }

    state
}

/// Parse a decimal-string field from the telemetry row. Empty
/// string / unparsable → `Decimal::ZERO` (the UI treats zero as
/// "not set" for these metrics).
fn parse_decimal(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap_or(Decimal::ZERO)
}

fn variable_str(dep: &mm_control::DeploymentStateRow, key: &str) -> Option<String> {
    dep.variables
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn variable_decimal(dep: &mm_control::DeploymentStateRow, key: &str) -> Option<Decimal> {
    let v = dep.variables.get(key)?;
    if let Some(s) = v.as_str() {
        return Decimal::from_str(s).ok();
    }
    if let Some(f) = v.as_f64() {
        return Decimal::try_from(f).ok();
    }
    if let Some(i) = v.as_i64() {
        return Some(Decimal::from(i));
    }
    None
}

fn variable_u64(dep: &mm_control::DeploymentStateRow, key: &str) -> Option<u64> {
    dep.variables.get(key).and_then(|v| v.as_u64())
}

fn variable_bool(dep: &mm_control::DeploymentStateRow, key: &str) -> Option<bool> {
    dep.variables.get(key).and_then(|v| v.as_bool())
}

fn default_symbol_state(
    symbol: String,
    inventory: Decimal,
    pnl: PnlSnapshot,
    running: bool,
) -> SymbolState {
    SymbolState {
        symbol,
        mode: "paper".into(),
        strategy: String::new(),
        venue: String::new(),
        product: String::new(),
        pair_class: None,
        mid_price: Decimal::ZERO,
        spread_bps: Decimal::ZERO,
        inventory,
        inventory_value: Decimal::ZERO,
        live_orders: if running { 1 } else { 0 },
        total_fills: 0,
        pnl,
        volatility: Decimal::ZERO,
        vpin: Decimal::ZERO,
        kyle_lambda: Decimal::ZERO,
        adverse_bps: Decimal::ZERO,
        as_prob_bid: None,
        as_prob_ask: None,
        momentum_ofi_ewma: None,
        momentum_learned_mp_drift: None,
        market_resilience: Decimal::ZERO,
        order_to_trade_ratio: Decimal::ZERO,
        hma_value: None,
        kill_level: 0,
        sla_uptime_pct: Decimal::ZERO,
        regime: String::new(),
        spread_compliance_pct: Decimal::ZERO,
        book_depth_levels: Vec::new(),
        locked_in_orders_quote: Decimal::ZERO,
        sla_max_spread_bps: Decimal::ZERO,
        sla_min_depth_quote: Decimal::ZERO,
        presence_pct_24h: Decimal::ZERO,
        two_sided_pct_24h: Decimal::ZERO,
        minutes_with_data_24h: 0,
        hourly_presence: Vec::new(),
        market_impact: None,
        performance: None,
        tunable_config: None,
        adaptive_state: None,
        open_orders: Vec::new(),
        active_graph: None,
        manipulation_score: None,
        rug_score: None,
    }
}
