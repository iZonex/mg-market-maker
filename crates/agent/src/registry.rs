//! Strategy registry — agent-local reconcile state.
//!
//! The controller pushes a `SetDesiredStrategies` command carrying the
//! authoritative slice of what should be running on this agent.
//! The registry diffs that slice against its currently running
//! entries and issues the minimum set of start / stop / restart
//! actions to converge.
//!
//! Three properties make this safe:
//!
//! 1. **Keyed by `deployment_id`** — operators reuse an id when
//!    they iterate config; a restart is cheaper than a full
//!    stop + fresh start.
//! 2. **Config-sensitive** — the registry tracks a cheap
//!    content hash per running entry and restarts when the
//!    incoming descriptor diverges from what that entry was
//!    started with. Protects against the silent "new config was
//!    pushed but old task is still running" class of bugs.
//! 3. **Engine factory is pluggable** — PR-2a ships a mock
//!    factory that spawns `tokio::spawn(sleep+log)` tasks; PR-2c
//!    replaces it with a real engine builder without touching
//!    the reconcile loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use mm_control::messages::{DeploymentStateRow, DesiredStrategy};

/// Factory contract. An implementation knows how to spin up a
/// task for one `DesiredStrategy` and returns a handle the
/// registry can abort when reconciling. PR-2a ships the mock
/// implementation in [`MockEngineFactory`]; PR-2c adds a real
/// engine-backed factory that consumes the local settings file.
#[async_trait]
pub trait EngineFactory: Send + Sync {
    /// Spawn the task for `desired`. Returns the join handle
    /// plus a label for logs / metrics.
    async fn spawn(&self, desired: &DesiredStrategy) -> SpawnedEngine;
}

pub struct SpawnedEngine {
    pub handle: JoinHandle<()>,
    pub label: String,
    /// Hot-reload channel into the engine. When present, the
    /// factory wired a `mpsc::unbounded` pair such that sending
    /// a `ConfigOverride` here reaches the engine's select loop
    /// on its next tick. `None` for mock / subscribe-only
    /// engines that don't implement live reconfig.
    pub config_override_tx:
        Option<tokio::sync::mpsc::UnboundedSender<mm_dashboard::state::ConfigOverride>>,
}

impl SpawnedEngine {
    /// Helper for engine factories that spawn tasks without a
    /// hot-reload channel (mock factories, subscribe-only pumps,
    /// no-op fallbacks on unresolved credentials). These tasks
    /// simply ignore `PATCH …/variables` calls at the agent
    /// registry layer.
    pub fn without_hot_reload(handle: JoinHandle<()>, label: String) -> Self {
        Self {
            handle,
            label,
            config_override_tx: None,
        }
    }
}

/// Cheap content signature used to decide "is the running entry
/// compatible with the new desired descriptor, or must we
/// restart?" PR-2a hashes the whole desired struct via its JSON
/// form — sufficient for the reconcile test, and explicit enough
/// that a future richer hash (include variable bindings, credential
/// id, etc.) slots in without changing call sites.
fn config_signature(desired: &DesiredStrategy) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let json = serde_json::to_string(desired).unwrap_or_default();
    let mut h = DefaultHasher::new();
    json.hash(&mut h);
    h.finish()
}

struct Running {
    signature: u64,
    handle: JoinHandle<()>,
    label: String,
    /// Carried so telemetry snapshots can stamp the `symbol`
    /// field per-deployment without a second roundtrip through
    /// the desired-strategy slice.
    symbol: String,
    /// Original template name from the DesiredStrategy — echoed
    /// back in telemetry so the drilldown UI knows what
    /// strategy this instance is running.
    template: String,
    /// Snapshot of the `variables` map at spawn time. Hot
    /// edits via `PATCH .../variables` will update this so
    /// telemetry reflects the live shape. Wrapped in the
    /// serde_json::Map the wire protocol uses to keep zero
    /// conversion cost on the telemetry hot path.
    variables: serde_json::Map<String, serde_json::Value>,
    /// UI-DEPLOY-1 — credentials allow-list this deployment
    /// was spawned with. Echoed into `DeploymentStateRow.credentials`
    /// so the DeployDialog can rebuild the full DesiredStrategy
    /// slice when adding a new deployment (SetDesiredStrategies
    /// is replace-by-set).
    credentials: Vec<String>,
    /// Hot-reload channel — paired with the receiver held by
    /// the running engine. `patch_variables` translates each
    /// `variables[key]=value` pair into a `ConfigOverride`
    /// variant and ships it through here. `None` for engine
    /// kinds that don't support live reconfig.
    config_override_tx:
        Option<tokio::sync::mpsc::UnboundedSender<mm_dashboard::state::ConfigOverride>>,
}

/// Scan the current Prometheus gauge registry for a metric with
/// the given name and return a `symbol → value` map. Returns an
/// empty map when the metric hasn't been written yet (e.g. a
/// fresh deployment whose engine hasn't produced a first
/// sample). Cheap O(gauges × labels) scan — acceptable at
/// 1 Hz telemetry cadence.
fn read_gauge_by_symbol(metric_name: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for family in prometheus::gather() {
        if family.get_name() != metric_name {
            continue;
        }
        for metric in family.get_metric() {
            let mut symbol: Option<&str> = None;
            for label in metric.get_label() {
                if label.get_name() == "symbol" {
                    symbol = Some(label.get_value());
                    break;
                }
            }
            let Some(sym) = symbol else { continue };
            let value = metric.get_gauge().get_value();
            // Use insert — when the same symbol is hit twice
            // (multi-engine), prefer the non-zero value.
            out.entry(sym.to_string())
                .and_modify(|v| {
                    if *v == 0.0 && value != 0.0 {
                        *v = value;
                    }
                })
                .or_insert(value);
        }
    }
    out
}

/// Scrape a counter that carries a `symbol` + `outcome` label
/// pair (funding-arb transitions). Returns a map keyed by
/// `symbol → outcome → count`. Empty when the metric family
/// hasn't been emitted yet.
fn read_counter_by_symbol_outcome(metric_name: &str) -> HashMap<String, HashMap<String, u64>> {
    let mut out: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for family in prometheus::gather() {
        if family.get_name() != metric_name {
            continue;
        }
        for metric in family.get_metric() {
            let mut symbol: Option<&str> = None;
            let mut outcome: Option<&str> = None;
            for label in metric.get_label() {
                match label.get_name() {
                    "symbol" => symbol = Some(label.get_value()),
                    "outcome" => outcome = Some(label.get_value()),
                    _ => {}
                }
            }
            let (Some(sym), Some(out_label)) = (symbol, outcome) else {
                continue;
            };
            let value = metric.get_counter().get_value();
            if !value.is_finite() || value < 0.0 {
                continue;
            }
            out.entry(sym.to_string())
                .or_default()
                .insert(out_label.to_string(), value as u64);
        }
    }
    out
}

/// Sister of [`read_gauge_by_symbol`] for counter metrics.
/// Prometheus exposes counters as monotonically-increasing f64s;
/// we cast to u64 at the telemetry boundary so the wire row
/// doesn't need to carry negative / non-integer noise from the
/// rare counter-reset edge case.
fn read_counter_by_symbol(metric_name: &str) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for family in prometheus::gather() {
        if family.get_name() != metric_name {
            continue;
        }
        for metric in family.get_metric() {
            let mut symbol: Option<&str> = None;
            for label in metric.get_label() {
                if label.get_name() == "symbol" {
                    symbol = Some(label.get_value());
                    break;
                }
            }
            let Some(sym) = symbol else { continue };
            let value = metric.get_counter().get_value();
            if !value.is_finite() || value < 0.0 {
                continue;
            }
            out.insert(sym.to_string(), value as u64);
        }
    }
    out
}

/// Pull a single Decimal field out of the agent's shared
/// `DashboardState` by symbol and render it as a
/// stable-precision string. Returns `""` when the dashboard
/// isn't attached, the symbol hasn't been published yet, or
/// the extractor returns `None`. Empty string is the wire
/// convention for "no sample yet" — distinct from literal
/// zero values that an active sample might produce.
fn dashboard_field<F>(
    dashboard: &Option<mm_dashboard::state::DashboardState>,
    symbol: &str,
    f: F,
) -> String
where
    F: Fn(&mm_dashboard::state::SymbolState) -> Option<rust_decimal::Decimal>,
{
    let Some(dash) = dashboard.as_ref() else {
        return String::new();
    };
    let Some(state) = dash.get_symbol(symbol) else {
        return String::new();
    };
    match f(&state) {
        Some(v) if v == rust_decimal::Decimal::ZERO => String::new(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// Render a Prometheus gauge f64 as a stable-precision string.
/// Telemetry consumers prefer decimal-representation because
/// JSON numbers in JS have precision quirks on small quantities
/// (0.00005 BTC positions). Cap at 8 fractional digits which is
/// finer than any exchange's lot size.
fn format_gauge(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return String::new();
    }
    format!("{v:.8}")
}

/// Map the engine's numeric regime gauge back to the string
/// label operators see. Matches the encoding in
/// `dashboard::metrics::REGIME`: 0 Quiet, 1 Trending,
/// 2 Volatile, 3 MeanReverting.
fn regime_label(code: i32) -> String {
    match code {
        0 => "Quiet".into(),
        1 => "Trending".into(),
        2 => "Volatile".into(),
        3 => "MeanReverting".into(),
        _ => String::new(),
    }
}

/// Harvest boolean feature flags from the deployment's
/// `variables` map. Convention: keys ending `_enabled` (or the
/// explicit legacy keys `momentum_ofi`, `bvc_classifier`, etc.)
/// are surfaced as a `BTreeMap<String, bool>` the UI renders
/// in the FeatureStatusPanel drilldown. Unknown keys simply
/// don't appear in the map — the UI shows "—".
/// Names of variable keys that combine into a single multi-key
/// `ConfigOverride`. The per-key translator skips these; the
/// `compose_multi_key_override` function consumes them together.
fn multi_key_consumed_names() -> &'static [&'static str] {
    &["kill_level", "kill_reason", "kill_reset_reason"]
}

/// Build a single `ConfigOverride` from a patch when multiple
/// keys combine into one variant. Today:
/// - `kill_level: n` (+ optional `kill_reason`) → `ManualKillSwitch`
/// - `kill_reset_reason: "..."` → `ManualKillSwitchReset`
/// Mutually exclusive — if both appear, kill_level wins.
fn compose_multi_key_override(
    patch: &serde_json::Map<String, serde_json::Value>,
) -> Option<mm_dashboard::state::ConfigOverride> {
    use mm_dashboard::state::ConfigOverride;
    if let Some(level_v) = patch.get("kill_level") {
        let level = level_v.as_u64().map(|n| n as u8)?;
        let reason = patch
            .get("kill_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("dashboard operator")
            .to_string();
        return Some(ConfigOverride::ManualKillSwitch { level, reason });
    }
    if let Some(reset_v) = patch.get("kill_reset_reason") {
        let reason = reset_v.as_str().unwrap_or("dashboard operator").to_string();
        return Some(ConfigOverride::ManualKillSwitchReset { reason });
    }
    None
}

/// Translate one `(variable_key, value)` pair into a
/// [`mm_dashboard::state::ConfigOverride`] the running engine can
/// consume on its next tick. Returns `None` for keys the engine
/// doesn't expose as a hot-reloadable knob — those are still
/// merged into the deployment's `variables` snapshot (so the
/// telemetry tick reflects them) but produce no runtime change
/// until the operator redeploys. Adding a new knob means adding
/// a match arm here AND a matching `ConfigOverride` variant in
/// `mm_dashboard::state` + handler in `MarketMakerEngine::apply_config_override`.
fn translate_variable_override(
    key: &str,
    value: &serde_json::Value,
) -> Option<mm_dashboard::state::ConfigOverride> {
    use mm_dashboard::state::ConfigOverride;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    let as_decimal = || -> Option<Decimal> {
        if let Some(s) = value.as_str() {
            Decimal::from_str(s).ok()
        } else if let Some(f) = value.as_f64() {
            Decimal::try_from(f).ok()
        } else {
            value.as_i64().map(Decimal::from)
        }
    };
    let as_bool = || value.as_bool();
    let as_usize = || value.as_u64().map(|v| v as usize);
    let as_u32 = || value.as_u64().map(|v| v as u32);

    match key {
        "gamma" => as_decimal().map(ConfigOverride::Gamma),
        "min_spread_bps" | "spread_bps" => as_decimal().map(ConfigOverride::MinSpreadBps),
        "order_size" => as_decimal().map(ConfigOverride::OrderSize),
        "max_distance_bps" => as_decimal().map(ConfigOverride::MaxDistanceBps),
        "num_levels" => as_usize().map(ConfigOverride::NumLevels),
        "momentum_enabled" => as_bool().map(ConfigOverride::MomentumEnabled),
        "market_resilience_enabled" => as_bool().map(ConfigOverride::MarketResilienceEnabled),
        "amend_enabled" => as_bool().map(ConfigOverride::AmendEnabled),
        "amend_max_ticks" => as_u32().map(ConfigOverride::AmendMaxTicks),
        "otr_enabled" => as_bool().map(ConfigOverride::OtrEnabled),
        "max_inventory" => as_decimal().map(ConfigOverride::MaxInventory),
        "paused" => match as_bool()? {
            true => Some(ConfigOverride::PauseQuoting),
            false => Some(ConfigOverride::ResumeQuoting),
        },
        // Ops catalogue — maps operator-facing variable keys to
        // the corresponding ConfigOverride variant. Kill switch
        // (kill_level / kill_reason / kill_reset_reason) is
        // handled separately in `compose_multi_key_override`
        // because it needs two keys together.
        "emulator_spec" => value
            .as_str()
            .map(|s| ConfigOverride::RegisterEmulatedOrder(s.to_string())),
        "emulator_cancel_id" => value.as_u64().map(ConfigOverride::CancelEmulatedOrder),
        "dca_spec" => value
            .as_str()
            .map(|s| ConfigOverride::StartDcaReduction(s.to_string())),
        "dca_cancel" => match as_bool()? {
            true => Some(ConfigOverride::CancelDcaReduction),
            false => None,
        },
        "strategy_graph" => value
            .as_str()
            .map(|s| ConfigOverride::StrategyGraphSwap(s.to_string())),
        "news" => value.as_str().map(|s| ConfigOverride::News(s.to_string())),
        _ => None,
    }
}

fn feature_flags_from_variables(
    vars: &serde_json::Map<String, serde_json::Value>,
) -> std::collections::BTreeMap<String, bool> {
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in vars {
        if !(k.ends_with("_enabled")
            || k.ends_with("_on")
            || k == "momentum_ofi"
            || k == "bvc_classifier"
            || k == "sor_inline")
        {
            continue;
        }
        if let Some(b) = v.as_bool() {
            out.insert(k.clone(), b);
        }
    }
    out
}

pub struct StrategyRegistry {
    factory: Arc<dyn EngineFactory>,
    running: HashMap<String, Running>,
    /// Shared in-memory `DashboardState` the agent carries. When
    /// present, `snapshot_rows` fills telemetry fields (mid, spread,
    /// VPIN, Kyle, SLA, ...) from the per-symbol `SymbolState` the
    /// engine already writes there. `None` on mock factories / unit
    /// tests that don't run a real engine.
    dashboard: Option<mm_dashboard::state::DashboardState>,
}

impl StrategyRegistry {
    pub fn new(factory: Arc<dyn EngineFactory>) -> Self {
        Self {
            factory,
            running: HashMap::new(),
            dashboard: None,
        }
    }

    /// Attach the shared dashboard so the per-tick snapshot
    /// telemetry can pull operator-visible fields (mid, spread,
    /// toxicity signals, SLA) from the `SymbolState` the engine
    /// populates every refresh cycle.
    pub fn with_dashboard(mut self, dashboard: mm_dashboard::state::DashboardState) -> Self {
        self.dashboard = Some(dashboard);
        self
    }

    /// Number of strategy tasks the registry believes to be live.
    /// Reflects what reconcile last asked for — does NOT re-check
    /// each JoinHandle, because a self-exited task just means the
    /// engine decided to wind down on its own and next reconcile
    /// will re-spawn if desired still contains it.
    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    /// Lookup the symbol a live deployment is trading. Used by the
    /// details-endpoint handler to map a deployment_id coming over
    /// the control channel into the `symbol`-keyed shared ring
    /// buffers.
    pub fn deployment_symbol(&self, deployment_id: &str) -> Option<String> {
        self.running.get(deployment_id).map(|r| r.symbol.clone())
    }

    /// Bring the registry into line with `desired`. Diff:
    ///   * entry in desired not in running → spawn
    ///   * entry in running not in desired → abort + drop
    ///   * entry in both, signature changed → abort + re-spawn
    ///   * entry in both, signature equal → leave alone
    pub async fn reconcile(&mut self, desired: &[DesiredStrategy]) {
        let desired_by_id: HashMap<&str, (&DesiredStrategy, u64)> = desired
            .iter()
            .map(|d| (d.deployment_id.as_str(), (d, config_signature(d))))
            .collect();

        // Drop entries that are no longer desired or whose config
        // signature has drifted. Collect the ids first so we can
        // mutate the map inside the loop body.
        let stale_ids: Vec<String> = self
            .running
            .iter()
            .filter_map(|(id, running)| {
                let keep = desired_by_id
                    .get(id.as_str())
                    .map(|(_, sig)| *sig == running.signature)
                    .unwrap_or(false);
                if keep {
                    None
                } else {
                    Some(id.clone())
                }
            })
            .collect();
        for id in stale_ids {
            if let Some(entry) = self.running.remove(&id) {
                tracing::info!(deployment = %id, label = %entry.label, "stopping strategy");
                entry.handle.abort();
            }
        }

        // Spawn anything desired that isn't (or is no longer)
        // running. Mistakes here log and continue — one broken
        // strategy descriptor must not poison the rest of the
        // reconcile pass.
        for (id, (desc, sig)) in desired_by_id {
            if self.running.contains_key(id) {
                continue;
            }
            let spawned = self.factory.spawn(desc).await;
            tracing::info!(deployment = %id, label = %spawned.label, "starting strategy");

            // Fix #9 — replay variable-driven overrides that
            // the initial engine spawn didn't bake in. Custom
            // graph deploys ship `variables.strategy_graph`
            // containing the full graph JSON; without this
            // replay the engine would quietly run with the
            // template's default strategy instead. Same for
            // any other variable the translator recognises but
            // AppConfig doesn't know how to consume at boot
            // (news, dca_spec, etc.). Pure "config" variables
            // (gamma, min_spread_bps) already flow through
            // AppConfig so replaying them is harmless but
            // redundant — we still do it for consistency.
            if let Some(tx) = spawned.config_override_tx.as_ref() {
                for (k, v) in desc.variables.iter() {
                    if let Some(ovr) = translate_variable_override(k, v) {
                        if tx.send(ovr).is_err() {
                            tracing::warn!(
                                deployment = %desc.deployment_id,
                                key = %k,
                                "initial variable-override replay failed — channel closed at spawn"
                            );
                            break;
                        }
                    }
                }
            }

            self.running.insert(
                id.to_string(),
                Running {
                    signature: sig,
                    handle: spawned.handle,
                    label: spawned.label,
                    symbol: desc.symbol.clone(),
                    template: desc.template.clone(),
                    variables: desc.variables.clone(),
                    credentials: desc.credentials.clone(),
                    config_override_tx: spawned.config_override_tx,
                },
            );
        }
    }

    /// Snapshot each running deployment as a
    /// [`DeploymentStateRow`]. Inventory + PnL come from the
    /// shared Prometheus gauges the engine already writes
    /// (`mm_inventory`, `mm_pnl_total`), so the agent doesn't
    /// need to thread shared state through the JoinHandle — the
    /// existing engine observability surface doubles as the
    /// telemetry-source-of-truth.
    pub fn snapshot_rows(&self) -> Vec<DeploymentStateRow> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Engine already publishes rich state through Prometheus
        // gauges. We scrape what the drilldown UI needs at
        // telemetry cadence — no engine-side plumbing changes,
        // no Arc<RwLock> dance. Labels key everything on
        // `symbol` today; when two deployments can share a
        // symbol (cross-venue hedge) the gauge label model
        // needs a `deployment_id` dimension, which is a follow-
        // up when the engine gains per-deployment metric labels.
        let inv_by_symbol = read_gauge_by_symbol("mm_inventory");
        let pnl_by_symbol = read_gauge_by_symbol("mm_pnl_total");
        let kill_by_symbol = read_gauge_by_symbol("mm_kill_switch_level");
        let regime_by_symbol = read_gauge_by_symbol("mm_regime");
        let spread_by_symbol = read_gauge_by_symbol("mm_spread_bps");
        let orders_by_symbol = read_gauge_by_symbol("mm_live_orders");
        let _ = spread_by_symbol;
        // Wave 1 R item 3 — per-deployment execution scalars.
        // SOR + atomic-bundle gauges already exist in
        // `mm-dashboard::metrics`; the agent just scrapes and
        // forwards.
        let sor_fill_by_symbol = read_gauge_by_symbol("mm_sor_dispatch_filled_qty");
        let sor_success_by_symbol = read_counter_by_symbol("mm_sor_dispatch_success_total");
        let bundles_inflight_by_symbol = read_gauge_by_symbol("mm_atomic_bundles_inflight");
        let bundles_completed_by_symbol =
            read_counter_by_symbol("mm_atomic_bundles_completed_total");
        // Wave 1 R follow-up — engine-side gauge emission now
        // carries calibration / manipulation / funding-arb.
        let calib_a_by_symbol = read_gauge_by_symbol("mm_calibration_a");
        let calib_k_by_symbol = read_gauge_by_symbol("mm_calibration_k");
        let calib_samples_by_symbol = read_gauge_by_symbol("mm_calibration_samples");
        let manip_pd_by_symbol = read_gauge_by_symbol("mm_manipulation_pump_dump");
        let manip_wash_by_symbol = read_gauge_by_symbol("mm_manipulation_wash");
        let manip_thin_by_symbol = read_gauge_by_symbol("mm_manipulation_thin_book");
        let manip_combined_by_symbol = read_gauge_by_symbol("mm_manipulation_combined");
        let funding_active_by_symbol = read_gauge_by_symbol("mm_funding_arb_active");
        let funding_transitions =
            read_counter_by_symbol_outcome("mm_funding_arb_transitions_total");
        self.running
            .iter()
            .map(|(id, r)| DeploymentStateRow {
                deployment_id: id.clone(),
                symbol: r.symbol.clone(),
                running: !r.handle.is_finished(),
                inventory: inv_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                unrealized_pnl_quote: pnl_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                sampled_at_ms: now_ms,
                template: r.template.clone(),
                // Venue / product / mode — sourced from the
                // `variables` config the operator deployed with,
                // extracted by key convention so operators can
                // rebind templates without touching this code.
                // Fall back to the engine-authoritative shape
                // on the local DashboardState (SymbolState.venue
                // / product) — operators rarely set these in
                // variables, and when they do they can drift
                // from the connector that actually runs.
                venue: {
                    let from_vars = r
                        .variables
                        .get("venue")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if !from_vars.is_empty() {
                        from_vars.to_string()
                    } else {
                        self.dashboard
                            .as_ref()
                            .and_then(|d| d.get_symbol(&r.symbol))
                            .map(|s| s.venue)
                            .unwrap_or_default()
                    }
                },
                product: {
                    let from_vars = r
                        .variables
                        .get("product")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if !from_vars.is_empty() {
                        from_vars.to_string()
                    } else {
                        self.dashboard
                            .as_ref()
                            .and_then(|d| d.get_symbol(&r.symbol))
                            .map(|s| s.product)
                            .unwrap_or_default()
                    }
                },
                mode: r
                    .variables
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("paper")
                    .to_string(),
                regime: regime_by_symbol
                    .get(&r.symbol)
                    .map(|v| regime_label(*v as i32))
                    .unwrap_or_default(),
                kill_level: kill_by_symbol.get(&r.symbol).map(|v| *v as u8).unwrap_or(0),
                adaptive_gamma: read_gauge_by_symbol("mm_adaptive_gamma")
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                adaptive_reason: String::new(),
                features: feature_flags_from_variables(&r.variables),
                variables: r.variables.clone(),
                credentials: r.credentials.clone(),
                live_orders: orders_by_symbol
                    .get(&r.symbol)
                    .map(|v| *v as u32)
                    .unwrap_or(0),
                sor_filled_qty: sor_fill_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                sor_dispatch_success: sor_success_by_symbol.get(&r.symbol).copied().unwrap_or(0),
                atomic_bundles_inflight: bundles_inflight_by_symbol
                    .get(&r.symbol)
                    .map(|v| *v as u32)
                    .unwrap_or(0),
                atomic_bundles_completed: bundles_completed_by_symbol
                    .get(&r.symbol)
                    .copied()
                    .unwrap_or(0),
                calibration_a: calib_a_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                calibration_k: calib_k_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                calibration_samples: calib_samples_by_symbol
                    .get(&r.symbol)
                    .map(|v| *v as u32)
                    .unwrap_or(0),
                manipulation_pump_dump: manip_pd_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                manipulation_wash: manip_wash_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                manipulation_thin_book: manip_thin_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                manipulation_combined: manip_combined_by_symbol
                    .get(&r.symbol)
                    .map(|v| format_gauge(*v))
                    .unwrap_or_default(),
                funding_arb_active: funding_active_by_symbol
                    .get(&r.symbol)
                    .map(|v| *v >= 0.5)
                    .unwrap_or(false),
                funding_arb_entered: funding_transitions
                    .get(&r.symbol)
                    .and_then(|m| m.get("entered"))
                    .copied()
                    .unwrap_or(0),
                funding_arb_exited: funding_transitions
                    .get(&r.symbol)
                    .and_then(|m| m.get("exited"))
                    .copied()
                    .unwrap_or(0),
                funding_arb_taker_rejected: funding_transitions
                    .get(&r.symbol)
                    .and_then(|m| m.get("taker_rejected"))
                    .copied()
                    .unwrap_or(0),
                funding_arb_pair_break: funding_transitions
                    .get(&r.symbol)
                    .and_then(|m| m.get("pair_break"))
                    .copied()
                    .unwrap_or(0),
                funding_arb_pair_break_uncompensated: funding_transitions
                    .get(&r.symbol)
                    .and_then(|m| m.get("pair_break_uncompensated"))
                    .copied()
                    .unwrap_or(0),

                // Book + toxicity + SLA fields — sourced from the
                // shared DashboardState when present. Engine
                // already writes a full SymbolState each tick;
                // we forward the fields the Overview / Admin
                // panels need.
                mid_price: dashboard_field(&self.dashboard, &r.symbol, |s| Some(s.mid_price)),
                spread_bps: dashboard_field(&self.dashboard, &r.symbol, |s| Some(s.spread_bps)),
                volatility: dashboard_field(&self.dashboard, &r.symbol, |s| Some(s.volatility)),
                vpin: dashboard_field(&self.dashboard, &r.symbol, |s| Some(s.vpin)),
                kyle_lambda: dashboard_field(&self.dashboard, &r.symbol, |s| Some(s.kyle_lambda)),
                adverse_bps: dashboard_field(&self.dashboard, &r.symbol, |s| Some(s.adverse_bps)),
                sla_uptime_pct: dashboard_field(&self.dashboard, &r.symbol, |s| {
                    Some(s.sla_uptime_pct)
                }),
                presence_pct_24h: dashboard_field(&self.dashboard, &r.symbol, |s| {
                    Some(s.presence_pct_24h)
                }),
                two_sided_pct_24h: dashboard_field(&self.dashboard, &r.symbol, |s| {
                    Some(s.two_sided_pct_24h)
                }),
                open_orders: self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.get_symbol(&r.symbol))
                    .map(|s| {
                        s.open_orders
                            .iter()
                            .filter_map(|o| serde_json::to_value(o).ok())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                hourly_presence: self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.get_symbol(&r.symbol))
                    .map(|s| {
                        s.hourly_presence
                            .iter()
                            .filter_map(|p| serde_json::to_value(p).ok())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                minutes_with_data_24h: self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.get_symbol(&r.symbol))
                    .map(|s| s.minutes_with_data_24h)
                    .unwrap_or(0),
                market_impact: self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.get_symbol(&r.symbol))
                    .and_then(|s| {
                        s.market_impact
                            .as_ref()
                            .and_then(|mi| serde_json::to_value(mi).ok())
                    }),
                performance: self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.get_symbol(&r.symbol))
                    .and_then(|s| {
                        s.performance
                            .as_ref()
                            .and_then(|p| serde_json::to_value(p).ok())
                    }),
                active_graph: self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.get_symbol(&r.symbol))
                    .and_then(|s| {
                        s.active_graph
                            .as_ref()
                            .and_then(|g| serde_json::to_value(g).ok())
                    }),
            })
            .collect()
    }

    /// Merge operator-supplied patch into the running
    /// deployment's `variables` snapshot and — when the engine
    /// kind supports hot-reload — translate each known tunable
    /// into a `ConfigOverride` and ship it through the engine's
    /// override channel so the running strategy actually reacts
    /// without a restart.
    ///
    /// Returns `true` when the deployment existed + was patched,
    /// `false` when the deployment_id doesn't match anything
    /// currently running (stale UI, deployment was stopped
    /// between PATCH arriving at the controller and hitting the
    /// agent).
    ///
    /// Note: merging into the `variables` snapshot is top-level
    /// overwrite — same key wins, no deep-merge. Keys the
    /// translator doesn't recognise still get merged into the
    /// snapshot (so the next telemetry tick reflects them) but
    /// produce no `ConfigOverride` — templates that read raw
    /// variables off the deployment config will pick them up on
    /// the next full restart.
    pub fn patch_variables(
        &mut self,
        deployment_id: &str,
        patch: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        let Some(r) = self.running.get_mut(deployment_id) else {
            return false;
        };
        for (k, v) in patch {
            r.variables.insert(k.clone(), v.clone());
        }
        // Translate recognised keys into `ConfigOverride` and
        // ship them over the hot-reload channel. Engines without
        // a channel (mock / subscribe-only) swallow the patch at
        // the snapshot level only.
        let Some(tx) = r.config_override_tx.as_ref() else {
            return true;
        };
        // Multi-key ops first — some ConfigOverride variants
        // take more than one field (kill switch carries level +
        // reason). We consume those keys here and let the
        // per-key loop below handle the rest. This keeps the
        // per-key translator simple.
        if let Some(ovr) = compose_multi_key_override(patch) {
            if tx.send(ovr).is_err() {
                tracing::warn!(
                    deployment = %deployment_id,
                    "config override channel closed — engine exited"
                );
                return true;
            }
        }
        let multi_key_names = multi_key_consumed_names();
        for (k, v) in patch {
            if multi_key_names.contains(&k.as_str()) {
                continue;
            }
            if let Some(ovr) = translate_variable_override(k, v) {
                if tx.send(ovr).is_err() {
                    tracing::warn!(
                        deployment = %deployment_id,
                        key = %k,
                        "config override channel closed — engine exited; patch recorded at snapshot level only"
                    );
                    break;
                }
            }
        }
        true
    }

    /// Abort every running entry. Called when the agent is losing
    /// authority and needs to wind everything down before walking
    /// the fail-ladder. In PR-2a this just aborts the mock
    /// tasks; real engines in PR-2c respect their own graceful
    /// shutdown sequences.
    pub async fn abort_all(&mut self) {
        for (id, running) in self.running.drain() {
            tracing::info!(deployment = %id, "abort on authority loss");
            running.handle.abort();
        }
    }
}

/// Test / PR-2a-only factory. Spawns a sleep-log task whose
/// lifetime is observable via the shared `probe` watch channel —
/// the integration test flips the channel on start / stop to
/// assert correct reconcile behaviour.
pub struct MockEngineFactory {
    /// Bumped on each spawn + each abort so tests can synchronise
    /// on registry state changes without polling.
    probe: watch::Sender<u64>,
    /// Interval the mock task logs at; kept long so it doesn't
    /// spam logs during normal test runs but still short enough
    /// to observe the task was actually running.
    tick_ms: u64,
}

impl MockEngineFactory {
    pub fn new(probe: watch::Sender<u64>) -> Self {
        Self {
            probe,
            tick_ms: 500,
        }
    }
}

#[async_trait]
impl EngineFactory for MockEngineFactory {
    async fn spawn(&self, desired: &DesiredStrategy) -> SpawnedEngine {
        let label = format!(
            "{}/{}@{}",
            desired.template, desired.symbol, desired.deployment_id
        );
        let probe = self.probe.clone();
        probe.send_modify(|v| *v = v.wrapping_add(1));
        let probe_on_exit = probe.clone();
        let exit_label = label.clone();
        let tick = self.tick_ms;
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(tick));
            interval.tick().await;
            loop {
                interval.tick().await;
                tracing::trace!(strategy = %exit_label, "mock strategy tick");
            }
        });
        // Best-effort: note the spawn on probe; the abort path on
        // reconcile also bumps so tests see both events.
        let _ = probe_on_exit;
        SpawnedEngine::without_hot_reload(handle, label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(id: &str, sym: &str) -> DesiredStrategy {
        DesiredStrategy {
            deployment_id: id.into(),
            template: "mock-template".into(),
            symbol: sym.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn reconcile_starts_missing() {
        let (tx, _rx) = watch::channel(0u64);
        let mut reg = StrategyRegistry::new(Arc::new(MockEngineFactory::new(tx)));
        reg.reconcile(&[desc("a", "BTC"), desc("b", "ETH")]).await;
        assert_eq!(reg.running_count(), 2);
    }

    #[tokio::test]
    async fn reconcile_stops_removed() {
        let (tx, _rx) = watch::channel(0u64);
        let mut reg = StrategyRegistry::new(Arc::new(MockEngineFactory::new(tx)));
        reg.reconcile(&[desc("a", "BTC"), desc("b", "ETH")]).await;
        reg.reconcile(&[desc("a", "BTC")]).await;
        assert_eq!(reg.running_count(), 1);
        assert!(reg.running.contains_key("a"));
        assert!(!reg.running.contains_key("b"));
    }

    #[tokio::test]
    async fn reconcile_restarts_on_config_change() {
        let (tx, _rx) = watch::channel(0u64);
        let mut reg = StrategyRegistry::new(Arc::new(MockEngineFactory::new(tx)));
        reg.reconcile(&[desc("a", "BTC")]).await;
        let sig_before = reg.running.get("a").unwrap().signature;
        reg.reconcile(&[desc("a", "ETH")]).await;
        let sig_after = reg.running.get("a").unwrap().signature;
        assert_ne!(sig_before, sig_after, "signature captures symbol change");
        assert_eq!(reg.running_count(), 1, "still exactly one running");
    }

    #[tokio::test]
    async fn snapshot_rows_reflects_running_set() {
        let (tx, _rx) = watch::channel(0u64);
        let mut reg = StrategyRegistry::new(Arc::new(MockEngineFactory::new(tx)));
        reg.reconcile(&[desc("a", "BTCUSDT"), desc("b", "ETHUSDT")])
            .await;
        let rows = reg.snapshot_rows();
        assert_eq!(rows.len(), 2);
        let mut ids: Vec<_> = rows.iter().map(|r| r.deployment_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
        let a = rows.iter().find(|r| r.deployment_id == "a").unwrap();
        assert_eq!(a.symbol, "BTCUSDT");
        assert!(a.running, "mock task is alive immediately after spawn");
    }

    #[test]
    fn translate_variable_override_maps_known_keys() {
        use mm_dashboard::state::ConfigOverride;
        // Decimal-valued knobs accept both string + float JSON.
        let g = translate_variable_override("gamma", &serde_json::json!("0.02")).unwrap();
        assert!(matches!(g, ConfigOverride::Gamma(_)));
        let s = translate_variable_override("min_spread_bps", &serde_json::json!(5.5)).unwrap();
        assert!(matches!(s, ConfigOverride::MinSpreadBps(_)));
        // Bool / int / paused lane.
        let m = translate_variable_override("momentum_enabled", &serde_json::json!(true)).unwrap();
        assert!(matches!(m, ConfigOverride::MomentumEnabled(true)));
        let n = translate_variable_override("num_levels", &serde_json::json!(3)).unwrap();
        assert!(matches!(n, ConfigOverride::NumLevels(3)));
        let p = translate_variable_override("paused", &serde_json::json!(true)).unwrap();
        assert!(matches!(p, ConfigOverride::PauseQuoting));
        let r = translate_variable_override("paused", &serde_json::json!(false)).unwrap();
        assert!(matches!(r, ConfigOverride::ResumeQuoting));
        // Unknown key — no translation; caller keeps snapshot-only merge.
        assert!(translate_variable_override("some_future_knob", &serde_json::json!(1)).is_none());
    }

    #[tokio::test]
    async fn patch_variables_ships_override_when_channel_present() {
        use mm_dashboard::state::ConfigOverride;
        let (tx, _rx) = watch::channel(0u64);
        let mut reg = StrategyRegistry::new(Arc::new(MockEngineFactory::new(tx)));
        reg.reconcile(&[desc("d", "BTCUSDT")]).await;
        // Mock factory leaves config_override_tx == None. Swap in
        // a synthetic channel so we can assert dispatch.
        let (override_tx, mut override_rx) =
            tokio::sync::mpsc::unbounded_channel::<ConfigOverride>();
        reg.running.get_mut("d").unwrap().config_override_tx = Some(override_tx);

        let mut patch = serde_json::Map::new();
        patch.insert("gamma".into(), serde_json::json!("0.015"));
        patch.insert("momentum_enabled".into(), serde_json::json!(false));
        assert!(reg.patch_variables("d", &patch));

        // Snapshot-level merge happened.
        let row = &reg.snapshot_rows()[0];
        assert_eq!(
            row.variables.get("gamma").unwrap(),
            &serde_json::json!("0.015")
        );
        // Two recognised keys → two ConfigOverride messages.
        let first = override_rx.try_recv().expect("first override");
        let second = override_rx.try_recv().expect("second override");
        let seen = [first, second];
        assert!(seen.iter().any(|o| matches!(o, ConfigOverride::Gamma(_))));
        assert!(seen
            .iter()
            .any(|o| matches!(o, ConfigOverride::MomentumEnabled(false))));
    }

    #[tokio::test]
    async fn patch_variables_without_channel_is_snapshot_only() {
        let (tx, _rx) = watch::channel(0u64);
        let mut reg = StrategyRegistry::new(Arc::new(MockEngineFactory::new(tx)));
        reg.reconcile(&[desc("d", "BTCUSDT")]).await;
        // Mock factory provides no override channel — the patch
        // must still merge into the snapshot without panicking.
        let mut patch = serde_json::Map::new();
        patch.insert("gamma".into(), serde_json::json!("0.05"));
        assert!(reg.patch_variables("d", &patch));
        let row = &reg.snapshot_rows()[0];
        assert_eq!(
            row.variables.get("gamma").unwrap(),
            &serde_json::json!("0.05")
        );
    }

    #[tokio::test]
    async fn abort_all_empties_registry() {
        let (tx, _rx) = watch::channel(0u64);
        let mut reg = StrategyRegistry::new(Arc::new(MockEngineFactory::new(tx)));
        reg.reconcile(&[desc("a", "BTC"), desc("b", "ETH")]).await;
        reg.abort_all().await;
        assert_eq!(reg.running_count(), 0);
    }
}
