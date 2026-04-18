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
    /// Portfolio-level risk spread multiplier (Epic 3). The
    /// engine applies this as an additional factor on the
    /// effective spread, composable with existing kill switch
    /// and market resilience multipliers.
    PortfolioRiskMult(Decimal),
    /// Manually escalate the kill switch to a specific level.
    /// `level` maps onto `mm_risk::KillLevel` (1..=5); `reason`
    /// is recorded to the audit trail. Emitted by the dashboard
    /// `/api/v1/ops/*` endpoints so an operator can pull any of
    /// the kill-switch escalations without touching the process.
    ManualKillSwitch { level: u8, reason: String },
    /// Reset the kill switch back to [`KillLevel::Normal`]. Only
    /// honoured when the audit trail contains a matching manual
    /// escalation — the engine refuses to reset an
    /// automatically-triggered kill switch without operator
    /// intervention.
    ManualKillSwitchReset { reason: String },
    /// Epic F #2 — push a news headline into every engine's
    /// `NewsRetreatStateMachine`. Operators (or an external
    /// headline feeder) POST this through
    /// `/api/admin/config` (broadcast) whenever a headline
    /// worth surfacing to the risk layer arrives; the state
    /// machine handles the Low/High/Critical classification
    /// from its own regex tables. Engines without
    /// `with_news_retreat` ignore the push.
    News(String),
    /// Epic G — push a freshly-computed `SentimentTick` into
    /// every engine's `SocialRiskEngine`. Emitted by the
    /// `mm-sentiment` orchestrator once per poll cycle per
    /// asset it tracks; engines whose symbol doesn't touch
    /// the asset just evaluate against stale state (the
    /// risk engine's staleness guard returns neutral).
    SentimentTick(mm_sentiment::SentimentTick),
    /// Epic H — hot-swap the running strategy graph. Payload is
    /// the graph JSON body; the engine validates + compiles +
    /// swaps. Admin-only, routed through
    /// `POST /api/admin/strategy/graph`. Engines whose scope
    /// doesn't match the graph's scope silently ignore the push
    /// (the broadcast goes to every config channel).
    StrategyGraphSwap(String),
    /// Multi-Venue 3.B — an upstream graph on *another* engine
    /// routed a `VenueQuote` batch at this one. The engine
    /// applies it as its `graph_quotes_override` on the next
    /// `refresh_quotes` tick, which funnels through the same
    /// diff / balance-check / order-manager path as any other
    /// strategy-authored bundle.
    ///
    /// Carries the serialised `Vec<VenueQuote>` as JSON so the
    /// `ConfigOverride` enum stays engine-type-free.
    ExternalVenueQuotes(String),
}

/// Per-client state partition (Epic 1: Multi-Client Isolation).
/// Each client owns a disjoint set of symbols with separate fills,
/// webhooks, and config override channels.
#[derive(Debug, Default)]
pub struct ClientState {
    pub symbols: HashMap<String, SymbolState>,
    pub recent_fills: std::collections::VecDeque<FillRecord>,
    pub webhook_dispatcher: Option<crate::webhooks::WebhookDispatcher>,
    pub config_overrides: HashMap<String, tokio::sync::mpsc::UnboundedSender<ConfigOverride>>,
}

#[derive(Debug, Default)]
struct StateInner {
    /// Per-client state partitions. In legacy mode (no clients
    /// configured), a single `"default"` client owns everything.
    clients: HashMap<String, ClientState>,
    /// Reverse index: symbol → client_id for O(1) routing.
    symbol_to_client: HashMap<String, String>,
    loans: HashMap<String, LoanConfig>,
    incidents: Vec<IncidentRecord>,
    portfolio: Option<PortfolioSnapshot>,
    /// Append-only fill log writer for persistence across
    /// restarts. Set via `DashboardState::enable_fill_log`.
    fill_log_writer: Option<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>,
    /// Historical daily report snapshots. Keyed by date string
    /// (YYYY-MM-DD). Capped at 90 days.
    daily_reports: HashMap<String, DailyReportSnapshot>,
    /// Rolling PnL time-series per symbol. Each entry is a
    /// (timestamp_ms, total_pnl) pair. Capped at 1440 entries
    /// per symbol (24h at 1-minute cadence).
    pnl_timeseries: HashMap<String, std::collections::VecDeque<(i64, Decimal)>>,
    /// UX-2 — spread (bps) rolling history so charts can
    /// render a full window on page load instead of warming
    /// up from live ticks. Same 1440-entry cap as PnL.
    spread_timeseries: HashMap<String, std::collections::VecDeque<(i64, Decimal)>>,
    /// UX-2 — inventory (base asset) rolling history.
    inventory_timeseries: HashMap<String, std::collections::VecDeque<(i64, Decimal)>>,
    /// Process start time for uptime calculation.
    started_at: DateTime<Utc>,
    /// Engine product (Epic 40.10) — `Some` once the server has
    /// registered the active product at startup. Used by the
    /// client-onboarding handler to fail-closed on US-jurisdiction
    /// clients attempting to register on a perp engine.
    engine_product: Option<mm_common::config::ProductType>,
    /// UX-5 — effective `AppConfig` snapshot captured at
    /// startup. Exposed read-only through
    /// `/api/v1/config/snapshot` so operators can see which
    /// features are configured, which are on defaults, and which
    /// optional sections are absent. Secrets live in env, not in
    /// the config struct, so serialising the whole thing is safe.
    app_config: Option<std::sync::Arc<mm_common::config::AppConfig>>,
    /// A1 — filesystem path to the append-only JSONL audit log.
    /// Read by the monthly-report aggregator so the bundle
    /// includes every hash-chained event in the requested
    /// period. `None` until the server has registered the path
    /// at startup (tests / headless callers can skip it).
    audit_log_path: Option<std::path::PathBuf>,
    /// A1 — HMAC-SHA256 secret used when signing monthly-report
    /// manifests served via `/api/v1/report/monthly.*`. Falls
    /// back to the `AppConfig`-derived default when unset; never
    /// persisted to disk, only held in-memory for the process
    /// lifetime.
    report_secret: Option<Vec<u8>>,
    /// Block D — registered archive client (if `[archive]`
    /// configured). Exposed through `/api/v1/archive/health`
    /// so the operator's first smoke test covers S3
    /// creds + endpoint before the shipper ticks.
    archive_client: Option<crate::archive::ArchiveClient>,
    /// Epic G — latest `SentimentTick` per normalised asset.
    /// Updated every orchestrator cycle; drained by
    /// `/api/v1/sentiment/snapshot` for the frontend panel.
    /// Holding the *latest* only is deliberate — history is
    /// the mention counter's job; this map is for
    /// at-a-glance status.
    sentiment_ticks: HashMap<String, mm_sentiment::SentimentTick>,
    /// Epic G — rolling per-asset history for the UI
    /// sparkline + `/api/v1/sentiment/history` endpoint. Each
    /// deque is capped at `MAX_SENTIMENT_HISTORY` entries
    /// (24h at 60-second poll cadence).
    sentiment_history: HashMap<String, std::collections::VecDeque<mm_sentiment::SentimentTick>>,
    /// Epic H — disk-backed graph store. `None` until the
    /// server boot call to `set_strategy_graph_store`; the
    /// HTTP handlers treat `None` as "strategy graphs
    /// disabled on this deployment" and return 503.
    strategy_graph_store: Option<std::sync::Arc<mm_strategy_graph::GraphStore>>,
    /// Epic H Phase 3 — shared audit sink the dashboard uses to
    /// record deploy / rollback / reject events on the same
    /// hash-chained timeline as order-lifecycle + risk rows.
    /// `None` for tests / headless callers; real boot registers
    /// the `Arc<AuditLog>` that `AuthState` and the engines also
    /// share, so all writers append into one file.
    audit_log: Option<std::sync::Arc<mm_risk::audit::AuditLog>>,
    /// Epic Multi-Venue Level 2.A — cross-engine data bus. Every
    /// engine publishes L1/L2/trades/funding/balance here; Level
    /// 2.B parameterised source nodes in the strategy graph read
    /// from this same bus. Cheap-to-clone (Arc internally) so the
    /// dashboard state holds it directly, no Option indirection.
    data_bus: crate::data_bus::DataBus,
    /// Latest per-symbol margin ratio (Epic 40.4). Published by
    /// the engine's `MarginGuard` poll each
    /// `refresh_interval_secs`. Surfaced on the dashboard so
    /// operators can see the guard's view of how close the
    /// account is to a venue liquidation.
    per_symbol_margin_ratio: HashMap<String, Decimal>,
    /// Configurable alert rules.
    alert_rules: Vec<AlertRule>,
    /// Loan agreements (Epic 2). Keyed by loan ID.
    loan_agreements: HashMap<String, mm_persistence::loan::LoanAgreement>,
    /// Optimization state (Epic 6). Tracks hyperopt runs.
    optimization: Option<OptimizationState>,
    /// Cross-symbol correlation matrix (Epic 3). Updated by the
    /// portfolio risk background task. Each entry is
    /// `(factor_a, factor_b, correlation)`.
    correlation_matrix: Vec<(String, String, Decimal)>,
    /// Portfolio risk summary (Epic 3). Updated by the
    /// portfolio risk background task.
    portfolio_risk_summary: Option<mm_risk::portfolio_risk::PortfolioRiskSummary>,
    /// Shared per-client loss circuit (Epic 6). Set at startup so
    /// the `/api/v1/clients/loss-state` endpoint and the ops
    /// reset endpoint can snapshot / mutate the same instance
    /// every engine reports into.
    per_client_circuit: Option<std::sync::Arc<mm_risk::PerClientLossCircuit>>,
    /// Per-symbol per-venue balance snapshots. Populated by the
    /// engine after each `get_balances()` refresh. Each symbol
    /// maps to a Vec of snapshots, one per (venue, wallet, asset)
    /// the bundle of connectors reports.
    venue_balances: HashMap<String, Vec<VenueBalanceSnapshot>>,
    /// Epic 33 — pending hyperopt calibrations awaiting operator
    /// approval. Keyed by symbol; at most one per symbol at a
    /// time (a new trigger overwrites the previous suggestion).
    pending_calibrations: HashMap<String, PendingCalibration>,
    /// Channel to the server-side hyperopt worker task. Set by
    /// `register_hyperopt_trigger_channel` at startup; the admin
    /// endpoint pushes `HyperoptTrigger` payloads through it.
    /// `None` before registration — endpoint returns HTTP 503.
    hyperopt_trigger_tx:
        Option<tokio::sync::mpsc::UnboundedSender<HyperoptTrigger>>,
    /// Optional WebSocket broadcaster. When set, state mutators
    /// that operators watch live (venue balance snapshots, etc.)
    /// emit a typed push message so the frontend panel doesn't
    /// need to poll. Left as `None` in headless / test builds.
    ws_broadcast: Option<std::sync::Arc<crate::websocket::WsBroadcast>>,
}

const MAX_DAILY_REPORTS: usize = 90;
const MAX_PNL_TIMESERIES: usize = 1440;
const MAX_SENTIMENT_HISTORY: usize = 1440;

/// Optimization run state (Epic 6).
#[derive(Debug, Clone, Serialize)]
pub struct OptimizationState {
    /// Current status: "idle", "running", "completed", "failed".
    pub status: String,
    /// Number of trials completed.
    pub trials_completed: u64,
    /// Total trials requested.
    pub trials_total: u64,
    /// Best parameters found (JSON map).
    pub best_params: Option<serde_json::Value>,
    /// Best loss value.
    pub best_loss: Option<Decimal>,
    /// When the run started.
    pub started_at: Option<DateTime<Utc>>,
}

/// Configurable alert rule — fires when a condition is met.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    /// Unique rule ID.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// What to check.
    pub condition: AlertCondition,
    /// Whether this rule is active.
    pub enabled: bool,
}

/// Condition that triggers an alert.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AlertCondition {
    /// PnL drops below threshold (quote asset).
    PnlBelow { threshold: Decimal },
    /// Spread exceeds threshold (bps).
    SpreadAbove { threshold_bps: Decimal },
    /// Inventory exceeds threshold (base asset, absolute).
    InventoryAbove { threshold: Decimal },
    /// Uptime drops below threshold (%).
    UptimeBelow { threshold_pct: Decimal },
    /// Fill rate drops below threshold (fills/minute).
    FillRateBelow { threshold_per_min: Decimal },
}

/// PnL time-series entry for charts.
#[derive(Debug, Clone, Serialize)]
pub struct PnlTimePoint {
    pub timestamp_ms: i64,
    pub total_pnl: Decimal,
}

/// UX-2 — generic (timestamp, value) point for the spread-
/// bps and inventory rolling histories. Separate struct
/// from `PnlTimePoint` so the two endpoints can diverge
/// (per-venue breakdowns, delta bars, etc.) without a
/// breaking change on the PnL schema.
#[derive(Debug, Clone, Serialize)]
pub struct SeriesPoint {
    pub timestamp_ms: i64,
    pub value: Decimal,
}

/// Stored daily report for historical queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyReportSnapshot {
    pub date: String,
    pub total_pnl: Decimal,
    pub total_volume: Decimal,
    pub total_fills: u64,
    pub symbols: Vec<DailySymbolSnapshot>,
}

/// Per-symbol snapshot within a daily report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailySymbolSnapshot {
    pub symbol: String,
    pub pnl: Decimal,
    pub volume: Decimal,
    pub fills: u64,
    pub avg_spread_bps: Decimal,
    pub uptime_pct: Decimal,
    pub presence_pct: Decimal,
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
    /// Owning client ID (Epic 1). `None` in legacy mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
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

/// Per-venue balance snapshot for the inventory drilldown panel.
///
/// Published by the engine whenever it refreshes balances from a
/// connector. When the engine drives a dual-venue strategy (basis,
/// funding arb, XEMM), each connector contributes one or more
/// entries so operators can answer "where does my BTC actually
/// sit?" without trawling individual venue dashboards.
#[derive(Debug, Clone, Serialize)]
pub struct VenueBalanceSnapshot {
    pub venue: String,
    pub product: String,
    pub asset: String,
    pub wallet: String,
    pub total: Decimal,
    pub available: Decimal,
    pub locked: Decimal,
    pub updated_at: DateTime<Utc>,
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
    /// Engine mode: `"live"`, `"paper"`, `"smoke"`. Surfaced to the
    /// dashboard so the operator always sees what they are
    /// connected to without consulting config.
    #[serde(default)]
    pub mode: String,
    /// Active strategy name — whatever the `Strategy::name()`
    /// impl returns (`"avellaneda-stoikov"`, `"glft"`, `"grid"`,
    /// etc.). Keeps the dashboard truthful even after a hot
    /// `/api/admin/config` swap.
    #[serde(default)]
    pub strategy: String,
    /// Exchange venue running this symbol — `"binance"`,
    /// `"bybit"`, `"hyperliquid"`, `"custom"`. `"multi"` when the
    /// symbol is traded cross-venue.
    #[serde(default)]
    pub venue: String,
    /// Venue product type — `"spot"`, `"perp"`, `"futures"`.
    #[serde(default)]
    pub product: String,
    /// Pair-class classification (Epic 30/31) published here too
    /// for convenience — mirrors `adaptive_state.pair_class` but
    /// set even when the online tuner is disabled. `None` before
    /// the first classifier pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_class: Option<String>,
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
    /// Live-tunable config snapshot (Epic 8). UI slider panels
    /// read these to show the current value before dispatching
    /// a `ConfigOverride` via the admin config endpoint. Only
    /// fields that are safe to hot-reload are exposed here —
    /// gamma, kappa, sigma floor, order size, level count,
    /// spread floors, inventory limit. Missing fields mean the
    /// engine has not published a snapshot yet (fresh startup).
    #[serde(default)]
    pub tunable_config: Option<TunableConfigSnapshot>,
    /// Pair-class tag + adaptive tuner state (Epic 30). `None`
    /// until the engine has classified the symbol at startup and
    /// run the first tick loop.
    #[serde(default)]
    pub adaptive_state: Option<AdaptiveStateSnapshot>,
    /// Currently live orders on the venue. Populated from the
    /// `OrderManager`'s live-order tracking every refresh tick
    /// so the frontend's Open Orders panel stays current without
    /// hitting a dedicated REST endpoint.
    #[serde(default)]
    pub open_orders: Vec<OrderSnapshot>,
}

/// Per-symbol adaptive-calibration snapshot published to the
/// dashboard. Enables a UI panel showing the γ multiplier stack
/// and the last adjustment reason without having to scrape logs.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AdaptiveStateSnapshot {
    /// Pair-class tag from `mm_common::classify_symbol`.
    pub pair_class: String,
    /// `true` when the online tuner is enabled for this symbol.
    pub enabled: bool,
    /// Current γ multiplier contributed by the AdaptiveTuner
    /// (1.0 = no adjustment). Multiplied on top of the regime
    /// multiplier from AutoTuner.
    pub gamma_factor: Decimal,
    /// Last recorded adjustment reason, lowercase tag.
    pub last_reason: String,
}

/// Epic 33 — trigger payload. Published by the admin endpoint,
/// consumed by a server-side worker that runs hyperopt against
/// the supplied recording and stages the result as a
/// `PendingCalibration` in `DashboardState`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HyperoptTrigger {
    pub symbol: String,
    /// Path to a JSONL recording produced by `mm-record-live`.
    pub recording_path: String,
    #[serde(default = "default_trials")]
    pub num_trials: u32,
    /// Loss function: "sharpe" | "sortino" | "calmar" | "maxdd".
    /// Defaults to sharpe.
    #[serde(default = "default_loss")]
    pub loss_fn: String,
}

fn default_trials() -> u32 {
    100
}
fn default_loss() -> String {
    "sharpe".to_string()
}

/// Epic 33 — staged hyperopt calibration awaiting operator
/// approval. Produced by `POST /api/admin/optimize/trigger`,
/// consumed by `POST /api/admin/optimize/apply` which converts
/// each entry into a `ConfigOverride` and dispatches.
#[derive(Debug, Clone, Serialize)]
pub struct PendingCalibration {
    pub symbol: String,
    pub created_at: DateTime<Utc>,
    /// Number of trials hyperopt ran.
    pub trials: u32,
    /// Loss function name that produced `best_loss`.
    pub loss_fn: String,
    /// Lowest loss achieved (lower = better; usually −Sharpe).
    pub best_loss: Decimal,
    /// Suggested parameter set. Keys mirror the `ConfigOverride`
    /// variants (`gamma`, `kappa`, `sigma`, `min_spread_bps`,
    /// `order_size`, `num_levels`).
    pub suggested: std::collections::HashMap<String, Decimal>,
    /// Current values at the time the run started, for the UI
    /// to render a diff.
    pub current: std::collections::HashMap<String, Decimal>,
}

/// Snapshot of the hot-reloadable parameters the dashboard shows
/// in the tuning panel. The keys line up 1-to-1 with
/// `ConfigOverride` variants so the UI can post the matching
/// override without a separate mapping table.
#[derive(Debug, Clone, Serialize, Default)]
pub struct TunableConfigSnapshot {
    pub gamma: Decimal,
    pub kappa: Decimal,
    pub sigma: Decimal,
    pub order_size: Decimal,
    pub num_levels: u32,
    pub min_spread_bps: Decimal,
    pub max_distance_bps: Decimal,
    pub max_inventory: Decimal,
    pub momentum_enabled: bool,
    pub market_resilience_enabled: bool,
    pub amend_enabled: bool,
    pub amend_max_ticks: u32,
    pub otr_enabled: bool,
}

/// Depth at a specific percentage from mid price.
#[derive(Debug, Clone, Serialize)]
pub struct BookDepthLevel {
    pub pct_from_mid: Decimal,
    pub bid_depth_quote: Decimal,
    pub ask_depth_quote: Decimal,
}

/// A live order snapshot for the dashboard's Open Orders panel.
/// Populated each refresh tick from the `OrderManager`'s live
/// order book so the UI does not need to poll a separate
/// REST endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct OrderSnapshot {
    /// Client-assigned order ID (UUID stringified).
    pub client_order_id: String,
    /// `"buy"` / `"sell"`.
    pub side: String,
    pub price: Decimal,
    /// Remaining (unfilled) quantity.
    pub qty: Decimal,
    /// `"live"` / `"placing"` / `"cancelling"` / `"filled"` /
    /// `"rejected"`.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PnlSnapshot {
    pub total: Decimal,
    pub spread: Decimal,
    pub inventory: Decimal,
    pub rebates: Decimal,
    pub fees: Decimal,
    /// Epic 40.3 — realised funding PnL, booked at settle.
    /// Included in `total`.
    #[serde(default)]
    pub funding: Decimal,
    /// Epic 40.3 — MTM view of the current funding period.
    /// Display-only; excluded from `total` because it flips
    /// into `funding` at the next settle.
    #[serde(default)]
    pub funding_mtm: Decimal,
    pub round_trips: u64,
    pub volume: Decimal,
}

impl DashboardState {
    pub fn new() -> Self {
        let s = Self::default();
        s.inner.write().unwrap().started_at = Utc::now();
        s
    }

    /// Record the engine's active product type (Epic 40.10) so
    /// ingress handlers can gate on it. Called once at startup.
    pub fn set_engine_product(&self, p: mm_common::config::ProductType) {
        self.inner.write().unwrap().engine_product = Some(p);
    }

    /// Get the engine's active product type if set.
    pub fn engine_product(&self) -> Option<mm_common::config::ProductType> {
        self.inner.read().unwrap().engine_product
    }

    /// UX-5 — publish the startup `AppConfig` so the frontend
    /// config viewer can render a read-only snapshot. Called
    /// once from the server boot path with the post-validation
    /// effective config.
    pub fn set_app_config(&self, cfg: std::sync::Arc<mm_common::config::AppConfig>) {
        self.inner.write().unwrap().app_config = Some(cfg);
    }

    /// UX-5 — fetch the startup config snapshot as a clone of
    /// the shared `Arc`. `None` until the server has registered
    /// the config (pre-boot / unit test callers).
    pub fn app_config(&self) -> Option<std::sync::Arc<mm_common::config::AppConfig>> {
        self.inner.read().unwrap().app_config.clone()
    }

    /// A1 — register the on-disk audit log path. Called once
    /// from the server boot path so the monthly-report
    /// aggregator can pull signed events out of the hash-
    /// chained JSONL. Absent in headless callers.
    pub fn set_audit_log_path(&self, path: std::path::PathBuf) {
        self.inner.write().unwrap().audit_log_path = Some(path);
    }

    /// A1 — resolve the audit log path.
    pub fn audit_log_path(&self) -> Option<std::path::PathBuf> {
        self.inner.read().unwrap().audit_log_path.clone()
    }

    /// A1 — register the HMAC signing secret used on monthly
    /// report manifests. Server boot passes the secret sourced
    /// from env so it never hits disk.
    pub fn set_report_secret(&self, secret: Vec<u8>) {
        self.inner.write().unwrap().report_secret = Some(secret);
    }

    /// A1 — fetch the report signing secret. Falls back to a
    /// process-random 32-byte secret when unset so self-signed
    /// exports remain verifiable within the same process.
    pub fn report_secret(&self) -> Vec<u8> {
        let guard = self.inner.read().unwrap();
        guard
            .report_secret
            .clone()
            .unwrap_or_else(|| b"unsigned-dev-only".to_vec())
    }

    /// Block D — register the process-global archive client.
    /// Called once from server boot when `[archive]` is
    /// configured; `None` in headless / test deployments.
    pub fn set_archive_client(&self, client: crate::archive::ArchiveClient) {
        self.inner.write().unwrap().archive_client = Some(client);
    }

    /// Block D — clone of the registered archive client, if any.
    pub fn archive_client(&self) -> Option<crate::archive::ArchiveClient> {
        self.inner.read().unwrap().archive_client.clone()
    }

    /// Epic G — record the latest sentiment tick for an
    /// asset. Called from the server's sink callback before
    /// the broadcast, so the dashboard snapshot never lags
    /// the engine's view by more than one orchestrator
    /// cycle. Also appended to the rolling per-asset
    /// `sentiment_history` so the UI sparkline has a view
    /// of how the rate / sentiment moved over the last 24h.
    pub fn push_sentiment_tick(&self, tick: mm_sentiment::SentimentTick) {
        let mut inner = self.inner.write().unwrap();
        let hist = inner
            .sentiment_history
            .entry(tick.asset.clone())
            .or_default();
        hist.push_back(tick.clone());
        while hist.len() > MAX_SENTIMENT_HISTORY {
            hist.pop_front();
        }
        inner.sentiment_ticks.insert(tick.asset.clone(), tick);
    }

    /// Epic G — window of the last N ticks for one asset.
    /// Returns newest-last (append order) so callers can
    /// plot directly without re-sorting.
    pub fn get_sentiment_history(
        &self,
        asset: &str,
        limit: usize,
    ) -> Vec<mm_sentiment::SentimentTick> {
        let guard = self.inner.read().unwrap();
        match guard.sentiment_history.get(asset) {
            None => Vec::new(),
            Some(hist) => {
                let start = hist.len().saturating_sub(limit);
                hist.iter().skip(start).cloned().collect()
            }
        }
    }

    /// Epic G / H — latest sentiment tick for a canonical asset,
    /// or `None` if no tick has arrived for it yet. Keyed by the
    /// asset's normalised ticker (`"BTC"`, `"ETH"`, …).
    pub fn sentiment_tick_for(
        &self,
        asset: &str,
    ) -> Option<mm_sentiment::SentimentTick> {
        self.inner.read().unwrap().sentiment_ticks.get(asset).cloned()
    }

    /// Epic G — snapshot of the most-recent tick per asset.
    /// Order-independent; the frontend sorts by asset.
    pub fn get_sentiment_snapshot(&self) -> Vec<mm_sentiment::SentimentTick> {
        self.inner
            .read()
            .unwrap()
            .sentiment_ticks
            .values()
            .cloned()
            .collect()
    }

    /// Epic H — register the disk-backed graph store. Called once
    /// from server boot. HTTP handlers 503 until this is set.
    pub fn set_audit_log(&self, log: std::sync::Arc<mm_risk::audit::AuditLog>) {
        self.inner.write().unwrap().audit_log = Some(log);
    }

    pub fn audit_log(&self) -> Option<std::sync::Arc<mm_risk::audit::AuditLog>> {
        self.inner.read().unwrap().audit_log.clone()
    }

    /// Epic Multi-Venue 2.A — shared DataBus handle. Cheap
    /// (Arc-internal) so engines clone their own copy at boot.
    pub fn data_bus(&self) -> crate::data_bus::DataBus {
        self.inner.read().unwrap().data_bus.clone()
    }

    pub fn set_strategy_graph_store(&self, store: std::sync::Arc<mm_strategy_graph::GraphStore>) {
        self.inner.write().unwrap().strategy_graph_store = Some(store);
    }

    /// Epic H — clone of the graph store handle, if registered.
    pub fn strategy_graph_store(
        &self,
    ) -> Option<std::sync::Arc<mm_strategy_graph::GraphStore>> {
        self.inner.read().unwrap().strategy_graph_store.clone()
    }

    /// Publish the latest margin ratio for `symbol` (Epic 40.4).
    /// Called from the engine's `MarginGuard` poll loop. Spot
    /// engines never call this.
    pub fn set_margin_ratio(&self, symbol: &str, ratio: Decimal) {
        self.inner
            .write()
            .unwrap()
            .per_symbol_margin_ratio
            .insert(symbol.to_string(), ratio);
    }

    /// Read the last-known margin ratio for `symbol`, or
    /// `None` when no margin data has been pushed yet (spot or
    /// pre-first-poll perp).
    pub fn margin_ratio(&self, symbol: &str) -> Option<Decimal> {
        self.inner
            .read()
            .unwrap()
            .per_symbol_margin_ratio
            .get(symbol)
            .copied()
    }

    // ── Client registration ──────────────────────────────────

    /// Register a client with its symbols. Called at startup
    /// from the resolved `effective_clients()` list.
    pub fn register_client(&self, client_id: &str, symbols: &[String]) {
        let mut inner = self.inner.write().unwrap();
        inner.clients.entry(client_id.to_string()).or_default();
        for sym in symbols {
            inner
                .symbol_to_client
                .insert(sym.clone(), client_id.to_string());
        }
    }

    /// Resolve the owning client for a symbol. Public wrapper
    /// around the private reverse-index lookup so out-of-module
    /// aggregators (monthly report) can scope their output.
    /// Returns `"default"` for symbols never explicitly
    /// registered — matches the legacy single-client behaviour.
    pub fn get_client_for_symbol(&self, symbol: &str) -> Option<String> {
        let inner = self.inner.read().unwrap();
        inner.symbol_to_client.get(symbol).cloned()
    }

    /// List registered client IDs.
    pub fn client_ids(&self) -> Vec<String> {
        let inner = self.inner.read().unwrap();
        let mut ids: Vec<String> = inner.clients.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Resolve the owning client for a symbol via the reverse
    /// index. Returns `"default"` if the symbol is unknown
    /// (backward compatibility for unregistered symbols).
    fn client_for_symbol(inner: &StateInner, symbol: &str) -> String {
        inner
            .symbol_to_client
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| "default".to_string())
    }

    // ── Symbol state ─────────────────────────────────────────

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
        crate::metrics::PNL_FUNDING_REALISED
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.pnl.funding));
        crate::metrics::PNL_FUNDING_MTM
            .with_label_values(&[&state.symbol])
            .set(decimal_to_f64(state.pnl.funding_mtm));
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

        let client_id = Self::client_for_symbol(&inner, &state.symbol);
        // Emit a typed WebSocket push so subscribed frontends see
        // per-symbol updates without polling /api/status. The
        // broadcast is best-effort: serialisation failures or
        // missing subscribers never block the engine.
        if let Some(bc) = inner.ws_broadcast.clone() {
            if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                "type": "update",
                "symbol": state.symbol,
                "data": &state,
            })) {
                bc.send(&payload);
            }
        }
        let client = inner.clients.entry(client_id).or_default();
        client.symbols.insert(state.symbol.clone(), state);
    }

    /// Get all symbol states across all clients.
    pub fn get_all(&self) -> Vec<SymbolState> {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .values()
            .flat_map(|c| c.symbols.values().cloned())
            .collect()
    }

    /// Get all symbol states for a specific client.
    pub fn get_client_symbols(&self, client_id: &str) -> Vec<SymbolState> {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .get(client_id)
            .map(|c| c.symbols.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Get state for a single symbol (searches all clients).
    pub fn get_symbol(&self, symbol: &str) -> Option<SymbolState> {
        let inner = self.inner.read().unwrap();
        let client_id = Self::client_for_symbol(&inner, symbol);
        inner
            .clients
            .get(&client_id)
            .and_then(|c| c.symbols.get(symbol).cloned())
    }

    // ── Loans ────────────────────────────────────────────────

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

    // ── Incidents ────────────────────────────────────────────

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

    // ── Per-venue balances ───────────────────────────────────

    /// Attach a WebSocket broadcaster so push-capable state
    /// mutators (venue balances, etc.) can notify subscribed
    /// clients in real time. Safe to call exactly once at
    /// startup from the server wiring.
    pub fn enable_ws_broadcast(&self, broadcast: std::sync::Arc<crate::websocket::WsBroadcast>) {
        let mut inner = self.inner.write().unwrap();
        inner.ws_broadcast = Some(broadcast);
    }

    /// Publish per-venue balance snapshots for a symbol. Replaces
    /// any previously stored snapshots for the same symbol so
    /// the panel always shows the latest view. If a WS broadcast
    /// is attached, a typed `venue_balances` push is emitted so
    /// the frontend panel updates without polling.
    pub fn update_venue_balances(&self, symbol: &str, snaps: Vec<VenueBalanceSnapshot>) {
        let mut inner = self.inner.write().unwrap();
        inner
            .venue_balances
            .insert(symbol.to_string(), snaps.clone());
        if let Some(bc) = &inner.ws_broadcast {
            // Serialisation failure here cannot block the engine —
            // fall back silently; the HTTP endpoint still serves
            // the updated data.
            if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                "type": "venue_balances",
                "symbol": symbol,
                "rows": snaps,
            })) {
                bc.send(&payload);
            }
        }
    }

    /// Fetch per-venue balance snapshots for a symbol. Empty vec
    /// if the symbol has not reported any yet.
    pub fn venue_balances(&self, symbol: &str) -> Vec<VenueBalanceSnapshot> {
        let inner = self.inner.read().unwrap();
        inner.venue_balances.get(symbol).cloned().unwrap_or_default()
    }

    /// Fetch per-venue balance snapshots for every symbol the
    /// dashboard knows about. Used by the drilldown panel's
    /// overview mode.
    pub fn all_venue_balances(&self) -> HashMap<String, Vec<VenueBalanceSnapshot>> {
        let inner = self.inner.read().unwrap();
        inner.venue_balances.clone()
    }

    // ── Pending hyperopt calibrations (Epic 33) ──────────────

    /// Stage a new calibration suggestion. Overwrites any
    /// previous suggestion for the same symbol — only the most
    /// recent hyperopt result is actionable.
    pub fn stage_calibration(&self, calibration: PendingCalibration) {
        let mut inner = self.inner.write().unwrap();
        inner
            .pending_calibrations
            .insert(calibration.symbol.clone(), calibration);
    }

    /// Read pending calibration for a symbol (or all of them
    /// when `symbol` is `None`). Used by the admin GET endpoint.
    pub fn get_calibration(&self, symbol: &str) -> Option<PendingCalibration> {
        let inner = self.inner.read().unwrap();
        inner.pending_calibrations.get(symbol).cloned()
    }

    /// All staged calibrations — used by the dashboard list view.
    pub fn all_calibrations(&self) -> Vec<PendingCalibration> {
        let inner = self.inner.read().unwrap();
        inner.pending_calibrations.values().cloned().collect()
    }

    /// Clear a staged calibration after it's been applied or
    /// discarded by the operator.
    pub fn clear_calibration(&self, symbol: &str) -> Option<PendingCalibration> {
        let mut inner = self.inner.write().unwrap();
        inner.pending_calibrations.remove(symbol)
    }

    /// Attach the hyperopt trigger channel from the server's
    /// startup wiring. The admin endpoint publishes through this
    /// channel; the server consumes it on a background task.
    pub fn register_hyperopt_trigger_channel(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<HyperoptTrigger>,
    ) {
        let mut inner = self.inner.write().unwrap();
        inner.hyperopt_trigger_tx = Some(tx);
    }

    /// Push a hyperopt trigger to the server-side worker.
    /// Returns `false` when no channel is registered (startup
    /// race or the feature is compiled out).
    pub fn send_hyperopt_trigger(&self, trigger: HyperoptTrigger) -> bool {
        let inner = self.inner.read().unwrap();
        match &inner.hyperopt_trigger_tx {
            Some(tx) => tx.send(trigger).is_ok(),
            None => false,
        }
    }

    // ── Portfolio ────────────────────────────────────────────

    /// Publish a portfolio snapshot + its Prometheus gauges.
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
        for (factor, delta) in &snap.per_factor {
            crate::metrics::PORTFOLIO_FACTOR_DELTA
                .with_label_values(&[factor])
                .set(decimal_to_f64(*delta));
        }
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

    // ── Webhooks ─────────────────────────────────────────────

    /// Set the webhook dispatcher for a specific client.
    pub fn set_client_webhook_dispatcher(
        &self,
        client_id: &str,
        wh: crate::webhooks::WebhookDispatcher,
    ) {
        let mut inner = self.inner.write().unwrap();
        inner
            .clients
            .entry(client_id.to_string())
            .or_default()
            .webhook_dispatcher = Some(wh);
    }

    /// Set the webhook dispatcher (legacy — sets on "default" client).
    pub fn set_webhook_dispatcher(&self, wh: crate::webhooks::WebhookDispatcher) {
        self.set_client_webhook_dispatcher("default", wh);
    }

    /// Attach the process-wide per-client loss circuit (Epic 6).
    /// Called once at startup; the dashboard API reads from and
    /// resets through it.
    pub fn set_per_client_circuit(&self, circuit: std::sync::Arc<mm_risk::PerClientLossCircuit>) {
        self.inner.write().unwrap().per_client_circuit = Some(circuit);
    }

    /// Read-only handle to the per-client loss circuit. `None`
    /// when the server did not register one (test harness /
    /// legacy single-client mode that only tracks aggregate on
    /// the dashboard).
    pub fn per_client_circuit(&self) -> Option<std::sync::Arc<mm_risk::PerClientLossCircuit>> {
        self.inner.read().unwrap().per_client_circuit.clone()
    }

    /// Get the webhook dispatcher for a specific client.
    pub fn get_client_webhook_dispatcher(
        &self,
        client_id: &str,
    ) -> Option<crate::webhooks::WebhookDispatcher> {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .get(client_id)
            .and_then(|c| c.webhook_dispatcher.clone())
    }

    /// Get the webhook dispatcher (legacy — returns first found).
    pub fn webhook_dispatcher(&self) -> Option<crate::webhooks::WebhookDispatcher> {
        let inner = self.inner.read().unwrap();
        for client in inner.clients.values() {
            if let Some(wh) = &client.webhook_dispatcher {
                return Some(wh.clone());
            }
        }
        None
    }

    /// Dispatch a webhook event, routing to the correct client
    /// based on the symbol in the event.
    pub fn dispatch_webhook_for_symbol(&self, symbol: &str, event: crate::webhooks::WebhookEvent) {
        let inner = self.inner.read().unwrap();
        let client_id = Self::client_for_symbol(&inner, symbol);
        if let Some(client) = inner.clients.get(&client_id) {
            if let Some(wh) = &client.webhook_dispatcher {
                wh.dispatch(event);
            }
        }
    }

    // ── PnL time-series ──────────────────────────────────────

    /// Push a PnL sample for a symbol's time-series.
    pub fn push_pnl_sample(&self, symbol: &str, timestamp_ms: i64, total_pnl: Decimal) {
        let mut inner = self.inner.write().unwrap();
        let ts = inner.pnl_timeseries.entry(symbol.to_string()).or_default();
        ts.push_back((timestamp_ms, total_pnl));
        while ts.len() > MAX_PNL_TIMESERIES {
            ts.pop_front();
        }
    }

    /// Get PnL time-series for a symbol.
    pub fn get_pnl_timeseries(&self, symbol: &str) -> Vec<PnlTimePoint> {
        let inner = self.inner.read().unwrap();
        inner
            .pnl_timeseries
            .get(symbol)
            .map(|ts| {
                ts.iter()
                    .map(|(t, p)| PnlTimePoint {
                        timestamp_ms: *t,
                        total_pnl: *p,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// UX-2 — push one (timestamp, value) sample into the
    /// spread-bps rolling history. Same 1440-cap FIFO as
    /// `push_pnl_sample`. Engine calls once per tick.
    pub fn push_spread_sample(&self, symbol: &str, timestamp_ms: i64, spread_bps: Decimal) {
        let mut inner = self.inner.write().unwrap();
        let ts = inner
            .spread_timeseries
            .entry(symbol.to_string())
            .or_default();
        ts.push_back((timestamp_ms, spread_bps));
        while ts.len() > MAX_PNL_TIMESERIES {
            ts.pop_front();
        }
    }

    /// UX-2 — push one inventory sample into the rolling
    /// history.
    pub fn push_inventory_sample(&self, symbol: &str, timestamp_ms: i64, inventory: Decimal) {
        let mut inner = self.inner.write().unwrap();
        let ts = inner
            .inventory_timeseries
            .entry(symbol.to_string())
            .or_default();
        ts.push_back((timestamp_ms, inventory));
        while ts.len() > MAX_PNL_TIMESERIES {
            ts.pop_front();
        }
    }

    /// UX-2 — spread-bps time-series read path.
    pub fn get_spread_timeseries(&self, symbol: &str) -> Vec<SeriesPoint> {
        let inner = self.inner.read().unwrap();
        inner
            .spread_timeseries
            .get(symbol)
            .map(|ts| {
                ts.iter()
                    .map(|(t, v)| SeriesPoint {
                        timestamp_ms: *t,
                        value: *v,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// UX-2 — inventory time-series read path.
    pub fn get_inventory_timeseries(&self, symbol: &str) -> Vec<SeriesPoint> {
        let inner = self.inner.read().unwrap();
        inner
            .inventory_timeseries
            .get(symbol)
            .map(|ts| {
                ts.iter()
                    .map(|(t, v)| SeriesPoint {
                        timestamp_ms: *t,
                        value: *v,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // ── Alert rules ──────────────────────────────────────────

    /// Add a configurable alert rule.
    pub fn add_alert_rule(&self, rule: AlertRule) {
        let mut inner = self.inner.write().unwrap();
        inner.alert_rules.retain(|r| r.id != rule.id);
        inner.alert_rules.push(rule);
    }

    /// Remove an alert rule by ID.
    pub fn remove_alert_rule(&self, id: &str) -> bool {
        let mut inner = self.inner.write().unwrap();
        let before = inner.alert_rules.len();
        inner.alert_rules.retain(|r| r.id != id);
        inner.alert_rules.len() < before
    }

    /// List all alert rules.
    pub fn get_alert_rules(&self) -> Vec<AlertRule> {
        self.inner.read().unwrap().alert_rules.clone()
    }

    /// Check all alert rules against current state.
    pub fn check_alert_rules(&self) -> Vec<(String, String)> {
        let inner = self.inner.read().unwrap();
        let mut triggered = Vec::new();
        for rule in &inner.alert_rules {
            if !rule.enabled {
                continue;
            }
            for client in inner.clients.values() {
                for sym in client.symbols.values() {
                    let fires = match &rule.condition {
                        AlertCondition::PnlBelow { threshold } => sym.pnl.total < *threshold,
                        AlertCondition::SpreadAbove { threshold_bps } => {
                            sym.spread_bps > *threshold_bps
                        }
                        AlertCondition::InventoryAbove { threshold } => {
                            sym.inventory.abs() > *threshold
                        }
                        AlertCondition::UptimeBelow { threshold_pct } => {
                            sym.sla_uptime_pct < *threshold_pct
                        }
                        AlertCondition::FillRateBelow { .. } => false,
                    };
                    if fires {
                        triggered.push((
                            rule.id.clone(),
                            format!("{}: {}", sym.symbol, rule.description),
                        ));
                    }
                }
            }
        }
        triggered
    }

    // ── Optimization state (Epic 6) ────────────────────────────

    /// Update optimization state.
    pub fn set_optimization_state(&self, state: OptimizationState) {
        self.inner.write().unwrap().optimization = Some(state);
    }

    /// Get current optimization state.
    pub fn get_optimization_state(&self) -> Option<OptimizationState> {
        self.inner.read().unwrap().optimization.clone()
    }

    // ── Loan agreements (Epic 2) ───────────────────────────────

    /// Store a loan agreement.
    pub fn set_loan_agreement(&self, agreement: mm_persistence::loan::LoanAgreement) {
        let mut inner = self.inner.write().unwrap();
        inner
            .loan_agreements
            .insert(agreement.id.clone(), agreement);
    }

    /// Get a loan agreement by ID.
    pub fn get_loan_agreement(&self, loan_id: &str) -> Option<mm_persistence::loan::LoanAgreement> {
        self.inner
            .read()
            .unwrap()
            .loan_agreements
            .get(loan_id)
            .cloned()
    }

    /// Get loan agreement for a symbol.
    pub fn get_loan_agreement_by_symbol(
        &self,
        symbol: &str,
    ) -> Option<mm_persistence::loan::LoanAgreement> {
        self.inner
            .read()
            .unwrap()
            .loan_agreements
            .values()
            .find(|a| a.symbol == symbol)
            .cloned()
    }

    /// Get all loan agreements.
    pub fn get_all_loan_agreements(&self) -> Vec<mm_persistence::loan::LoanAgreement> {
        self.inner
            .read()
            .unwrap()
            .loan_agreements
            .values()
            .cloned()
            .collect()
    }

    // ── Portfolio risk (Epic 3) ────────────────────────────────

    /// Update the correlation matrix snapshot.
    pub fn set_correlation_matrix(&self, matrix: Vec<(String, String, Decimal)>) {
        self.inner.write().unwrap().correlation_matrix = matrix;
    }

    /// Get the correlation matrix snapshot.
    pub fn get_correlation_matrix(&self) -> Vec<(String, String, Decimal)> {
        self.inner.read().unwrap().correlation_matrix.clone()
    }

    /// Update the portfolio risk summary.
    pub fn set_portfolio_risk_summary(
        &self,
        summary: mm_risk::portfolio_risk::PortfolioRiskSummary,
    ) {
        self.inner.write().unwrap().portfolio_risk_summary = Some(summary);
    }

    /// Get the portfolio risk summary.
    pub fn get_portfolio_risk_summary(
        &self,
    ) -> Option<mm_risk::portfolio_risk::PortfolioRiskSummary> {
        self.inner.read().unwrap().portfolio_risk_summary.clone()
    }

    // ── Misc ─────────────────────────────────────────────────

    /// Process start time.
    pub fn started_at(&self) -> DateTime<Utc> {
        self.inner.read().unwrap().started_at
    }

    /// Auto-snapshot the current state as a daily report.
    pub fn snapshot_daily_report(&self) {
        let symbols = self.get_all();
        if symbols.is_empty() {
            return;
        }
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut total_pnl = Decimal::ZERO;
        let mut total_volume = Decimal::ZERO;
        let mut total_fills = 0u64;
        let sym_snaps: Vec<DailySymbolSnapshot> = symbols
            .iter()
            .map(|s| {
                total_pnl += s.pnl.total;
                total_volume += s.pnl.volume;
                total_fills += s.pnl.round_trips;
                DailySymbolSnapshot {
                    symbol: s.symbol.clone(),
                    pnl: s.pnl.total,
                    volume: s.pnl.volume,
                    fills: s.pnl.round_trips,
                    avg_spread_bps: s.spread_bps,
                    uptime_pct: s.sla_uptime_pct,
                    presence_pct: s.presence_pct_24h,
                }
            })
            .collect();
        self.store_daily_report(DailyReportSnapshot {
            date,
            total_pnl,
            total_volume,
            total_fills,
            symbols: sym_snaps,
        });
    }

    /// Store a daily report snapshot for historical queries.
    pub fn store_daily_report(&self, report: DailyReportSnapshot) {
        let mut inner = self.inner.write().unwrap();
        let date = report.date.clone();
        inner.daily_reports.insert(date, report);
        if inner.daily_reports.len() > MAX_DAILY_REPORTS {
            let mut dates: Vec<String> = inner.daily_reports.keys().cloned().collect();
            dates.sort();
            while inner.daily_reports.len() > MAX_DAILY_REPORTS {
                if let Some(oldest) = dates.first() {
                    inner.daily_reports.remove(oldest);
                    dates.remove(0);
                } else {
                    break;
                }
            }
        }
    }

    /// Get a historical daily report by date (YYYY-MM-DD).
    pub fn get_daily_report(&self, date: &str) -> Option<DailyReportSnapshot> {
        self.inner.read().unwrap().daily_reports.get(date).cloned()
    }

    /// List available historical report dates.
    pub fn available_report_dates(&self) -> Vec<String> {
        let inner = self.inner.read().unwrap();
        let mut dates: Vec<String> = inner.daily_reports.keys().cloned().collect();
        dates.sort();
        dates
    }

    // ── Fill history ─────────────────────────────────────────

    /// Load fill history from a JSONL file. Called at startup to
    /// restore recent fills from a previous session.
    pub fn load_fill_history(&self, path: &std::path::Path) {
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let mut inner = self.inner.write().unwrap();
        let mut loaded = 0usize;
        for line in content.lines().rev().take(MAX_RECENT_FILLS) {
            if let Ok(fill) = serde_json::from_str::<FillRecord>(line) {
                let client_id = Self::client_for_symbol(&inner, &fill.symbol);
                let client = inner.clients.entry(client_id).or_default();
                client.recent_fills.push_front(fill);
                loaded += 1;
            }
        }
        // Trim per-client fill buffers.
        for client in inner.clients.values_mut() {
            while client.recent_fills.len() > MAX_RECENT_FILLS {
                client.recent_fills.pop_front();
            }
        }
        if loaded > 0 {
            tracing::info!(loaded, "restored fill history from disk");
        }
    }

    /// Enable persistent fill logging to a JSONL file.
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

    // ── Config overrides ─────────────────────────────────────

    /// Register a config override channel for a symbol.
    pub fn register_config_channel(
        &self,
        symbol: &str,
        tx: tokio::sync::mpsc::UnboundedSender<ConfigOverride>,
    ) {
        let mut inner = self.inner.write().unwrap();
        let client_id = Self::client_for_symbol(&inner, symbol);
        let client = inner.clients.entry(client_id).or_default();
        client.config_overrides.insert(symbol.to_string(), tx);
    }

    /// Send a config override to a specific symbol's engine.
    pub fn send_config_override(&self, symbol: &str, ovr: ConfigOverride) -> bool {
        let inner = self.inner.read().unwrap();
        let client_id = Self::client_for_symbol(&inner, symbol);
        if let Some(client) = inner.clients.get(&client_id) {
            if let Some(tx) = client.config_overrides.get(symbol) {
                return tx.send(ovr).is_ok();
            }
        }
        false
    }

    /// Send a config override to ALL registered symbols.
    pub fn broadcast_config_override(&self, ovr: ConfigOverride) -> usize {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .values()
            .flat_map(|c| c.config_overrides.values())
            .filter(|tx| tx.send(ovr.clone()).is_ok())
            .count()
    }

    /// List all symbols that have registered config channels.
    pub fn config_symbols(&self) -> Vec<String> {
        let inner = self.inner.read().unwrap();
        let mut v: Vec<String> = inner
            .clients
            .values()
            .flat_map(|c| c.config_overrides.keys().cloned())
            .collect();
        v.sort();
        v
    }

    /// Record a fill with NBBO snapshot for the client API.
    /// Routes to the correct client based on fill symbol.
    pub fn record_fill(&self, fill: FillRecord) {
        // Persist to disk.
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
        // Broadcast to WS subscribers first so clients see the
        // fill even if the write-lock below contends momentarily.
        self.broadcast_fill(&fill);
        let mut inner = self.inner.write().unwrap();
        let client_id = Self::client_for_symbol(&inner, &fill.symbol);
        let client = inner.clients.entry(client_id).or_default();
        client.recent_fills.push_back(fill);
        while client.recent_fills.len() > MAX_RECENT_FILLS {
            client.recent_fills.pop_front();
        }
    }

    /// Broadcast a fill event over the optional WS channel. Pure
    /// side-effect helper — callers should prefer `record_fill`,
    /// which stores the fill *and* pushes the message. Split out
    /// so ad-hoc engine call-sites (tests, paper fillers) can
    /// notify subscribers without duplicating the JSON envelope.
    pub fn broadcast_fill(&self, fill: &FillRecord) {
        let bc = match self.inner.read() {
            Ok(inner) => inner.ws_broadcast.clone(),
            Err(_) => return,
        };
        let Some(bc) = bc else { return };
        if let Ok(payload) = serde_json::to_string(&serde_json::json!({
            "type": "fill",
            "data": fill,
        })) {
            bc.send(&payload);
        }
    }

    /// Get recent fills across all clients, optionally filtered
    /// by symbol. Returns newest-first, capped at `limit`.
    pub fn get_recent_fills(&self, symbol: Option<&str>, limit: usize) -> Vec<FillRecord> {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .values()
            .flat_map(|c| c.recent_fills.iter().rev())
            .filter(|f| symbol.is_none_or(|s| f.symbol == s))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get recent fills for a specific client.
    pub fn get_client_fills(&self, client_id: &str, limit: usize) -> Vec<FillRecord> {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .get(client_id)
            .map(|c| c.recent_fills.iter().rev().take(limit).cloned().collect())
            .unwrap_or_default()
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
            mode: "paper".to_string(),
            strategy: "avellaneda-stoikov".to_string(),
            venue: "binance".to_string(),
            product: "spot".to_string(),
            pair_class: None,
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
            tunable_config: None,
            adaptive_state: None,
            open_orders: vec![],
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

    // ── Multi-client isolation tests ─────────────────────────

    #[test]
    fn register_client_and_get_symbols() {
        crate::metrics::init();
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into(), "ETHUSDT".into()]);
        ds.register_client("bob", &["SOLUSDT".into()]);

        ds.update(empty_state("BTCUSDT"));
        ds.update(empty_state("ETHUSDT"));
        ds.update(empty_state("SOLUSDT"));

        let alice_syms = ds.get_client_symbols("alice");
        assert_eq!(alice_syms.len(), 2);
        let bob_syms = ds.get_client_symbols("bob");
        assert_eq!(bob_syms.len(), 1);
        assert_eq!(bob_syms[0].symbol, "SOLUSDT");

        // get_all returns all across clients
        assert_eq!(ds.get_all().len(), 3);
    }

    #[test]
    fn fill_routes_to_correct_client() {
        crate::metrics::init();
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);
        ds.register_client("bob", &["ETHUSDT".into()]);

        let fill = FillRecord {
            timestamp: Utc::now(),
            symbol: "BTCUSDT".into(),
            client_id: Some("alice".into()),
            side: "buy".into(),
            price: dec!(50000),
            qty: dec!(0.01),
            is_maker: true,
            fee: dec!(0.1),
            nbbo_bid: dec!(49999),
            nbbo_ask: dec!(50001),
            slippage_bps: dec!(0),
        };
        ds.record_fill(fill);

        // Alice has the fill
        let alice_fills = ds.get_client_fills("alice", 10);
        assert_eq!(alice_fills.len(), 1);
        assert_eq!(alice_fills[0].symbol, "BTCUSDT");

        // Bob has no fills
        let bob_fills = ds.get_client_fills("bob", 10);
        assert_eq!(bob_fills.len(), 0);

        // Global view still works
        let all_fills = ds.get_recent_fills(None, 10);
        assert_eq!(all_fills.len(), 1);
    }

    #[test]
    fn config_override_routes_through_client() {
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        ds.register_config_channel("BTCUSDT", tx);

        assert!(ds.send_config_override("BTCUSDT", ConfigOverride::Gamma(dec!(0.5))));
        assert!(!ds.send_config_override("UNKNOWN", ConfigOverride::Gamma(dec!(0.5))));

        let ovr = rx.try_recv().unwrap();
        assert!(matches!(ovr, ConfigOverride::Gamma(_)));
    }

    #[test]
    fn broadcast_reaches_all_clients() {
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);
        ds.register_client("bob", &["ETHUSDT".into()]);

        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        ds.register_config_channel("BTCUSDT", tx1);
        ds.register_config_channel("ETHUSDT", tx2);

        let count = ds.broadcast_config_override(ConfigOverride::PauseQuoting);
        assert_eq!(count, 2);
        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    #[test]
    fn client_ids_returns_registered() {
        let ds = DashboardState::new();
        ds.register_client("bob", &["ETHUSDT".into()]);
        ds.register_client("alice", &["BTCUSDT".into()]);
        let ids = ds.client_ids();
        assert_eq!(ids, vec!["alice", "bob"]);
    }

    #[test]
    fn webhook_dispatcher_per_client() {
        let ds = DashboardState::new();
        ds.register_client("alice", &["BTCUSDT".into()]);
        ds.register_client("bob", &["ETHUSDT".into()]);

        let wh_alice = crate::webhooks::WebhookDispatcher::new();
        wh_alice.add_url("https://alice.com/hook".into());
        ds.set_client_webhook_dispatcher("alice", wh_alice);

        let wh_bob = crate::webhooks::WebhookDispatcher::new();
        wh_bob.add_url("https://bob.com/hook".into());
        ds.set_client_webhook_dispatcher("bob", wh_bob);

        let got_alice = ds.get_client_webhook_dispatcher("alice").unwrap();
        assert_eq!(got_alice.url_count(), 1);
        let got_bob = ds.get_client_webhook_dispatcher("bob").unwrap();
        assert_eq!(got_bob.url_count(), 1);
    }

    #[test]
    fn legacy_mode_works_without_registration() {
        crate::metrics::init();
        let ds = DashboardState::new();
        // No register_client call — legacy mode
        ds.update(empty_state("BTCUSDT"));
        let got = ds.get_symbol("BTCUSDT");
        assert!(got.is_some());
        assert_eq!(ds.get_all().len(), 1);
    }

    #[test]
    fn unknown_client_returns_empty() {
        let ds = DashboardState::new();
        assert!(ds.get_client_symbols("nonexistent").is_empty());
        assert!(ds.get_client_fills("nonexistent", 10).is_empty());
    }

    #[test]
    fn venue_balances_roundtrip() {
        let ds = DashboardState::new();
        assert!(ds.venue_balances("BTCUSDT").is_empty());

        let snap = VenueBalanceSnapshot {
            venue: "binance".into(),
            product: "Spot".into(),
            asset: "BTC".into(),
            wallet: "Spot".into(),
            total: dec!(0.5),
            available: dec!(0.4),
            locked: dec!(0.1),
            updated_at: Utc::now(),
        };
        ds.update_venue_balances("BTCUSDT", vec![snap.clone()]);

        let got = ds.venue_balances("BTCUSDT");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].venue, "binance");
        assert_eq!(got[0].asset, "BTC");
        assert_eq!(got[0].total, dec!(0.5));

        // all_venue_balances includes this symbol.
        let all = ds.all_venue_balances();
        assert_eq!(all.len(), 1);
        assert!(all.contains_key("BTCUSDT"));

        // Second update replaces, not appends.
        ds.update_venue_balances("BTCUSDT", vec![]);
        assert!(ds.venue_balances("BTCUSDT").is_empty());
    }
}
