use chrono::{DateTime, Utc};
use mm_common::config::LoanConfig;
use mm_portfolio::{AssetAggregate, CrossVenuePortfolio, PortfolioSnapshot, VenueInventory};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Distributed audit fetcher — an async closure that returns
/// audit events across the whole fleet for a given time range.
/// Set by the server binary at boot when a controller is wired
/// in; `build_monthly_report` calls it in place of the local
/// file reader so MiCA exports include audit events from every
/// agent, not just the controller process's empty audit file.
///
/// The closure is Arc'd so it can be stored on DashboardState
/// and called from any handler thread. The returned future is
/// `Send + 'static` so it can cross task boundaries freely.
pub type AuditRangeFetcher = Arc<
    dyn Fn(
            i64,
            i64,
            usize,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Vec<serde_json::Value>> + Send + 'static>,
        > + Send
        + Sync,
>;

/// Wave B1 — fleet-aware client-metrics fetcher. Returns one
/// JSON row per live deployment in the fleet, shaped as the
/// agent's `client_metrics` details topic emits it (symbol,
/// venue, product, decimal-string PnL fields, SLA scalars,
/// book-depth sums, etc.). Optionally filters by `client_id`
/// — when `Some`, the closure fans out only to deployments
/// whose owning agent's `profile.client_id` matches.
///
/// Installed at server boot when a controller is wired in.
/// `get_positions`, `get_pnl`, `get_sla`, and `/client/{id}/*`
/// handlers call it in place of the controller's local
/// DashboardState (which only receives the scalar slice from
/// `DeploymentStateRow` — the agent's full state is only ever
/// reachable through a details round-trip).
pub type FleetClientMetricsFetcher = Arc<
    dyn Fn(
            Option<String>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Vec<serde_json::Value>> + Send + 'static>,
        > + Send
        + Sync,
>;

/// Wave B5 — fan-out broadcaster for fleet-wide dashboard-state
/// commands. Today the only payload is "register this tenant
/// on every agent's local DashboardState" but the shape is
/// deliberately generic so future hot-onboarding commands
/// (e.g. `AddSymbol`) can ride the same rail without
/// plumbing new closures. Installed by server boot when
/// `mm-controller::AgentRegistry` is available; absent in
/// dashboard-only unit tests.
pub type FleetAddClientBroadcaster = Arc<
    dyn Fn(
            String,
            Vec<String>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = usize> + Send + 'static>>
        + Send
        + Sync,
>;

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
    /// 22W-2 — portfolio-wide VaR throttle. Complements the
    /// per-strategy var_guard with a book-wide PnL VaR gauge.
    /// Values: 1.0 normal, 0.5 VaR_95 breach, 0.0 VaR_99
    /// breach. The engine multiplies its size-multiplier
    /// stack by this value (composes via min()).
    PortfolioVarMult(Decimal),
    /// 22W-3 — register a client-side emulated order
    /// (StopMarket / StopLimit / TrailingStop / OcoLeg /
    /// GtdCancel). Spec serialised as a JSON blob so the
    /// dashboard crate doesn't need to reach into
    /// `mm-risk::order_emulator` for the enum.
    RegisterEmulatedOrder(String),
    /// 22W-3 — cancel a previously-registered emulated order
    /// by id.
    CancelEmulatedOrder(u64),
    /// 22W-4 — start a DCA reduction schedule on the symbol.
    /// Spec JSON string (DcaSpec from mm-risk::dca) parsed by
    /// the engine; dashboard crate passes it through opaquely.
    StartDcaReduction(String),
    /// 22W-4 — cancel an in-flight DCA schedule.
    CancelDcaReduction,
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
    /// Multi-Venue 3.E — a paired maker+hedge bundle that must
    /// either fill both legs within `timeout_ms` or roll the
    /// whole bundle back. Carried as JSON so the enum stays
    /// engine-type-free; the engine decodes into
    /// `mm_strategy_graph::AtomicBundleSpec` on receipt.
    ExternalAtomicBundle(String),
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
    /// I3 (2026-04-21) — timestamp of the last fill we fanned out
    /// to this tenant's webhook dispatcher. The fan-out loop
    /// queries fleet client-fills every tick; anything newer
    /// fires a `WebhookEvent::Fill`, everything older is skipped
    /// so a restart of the loop doesn't replay the ring buffer.
    /// `None` on first launch — first pass initialises to the
    /// newest fill timestamp to avoid a flood of synthetic
    /// notifications for fills that predate the webhook
    /// registration.
    pub webhook_fill_cursor: Option<chrono::DateTime<Utc>>,
}

#[derive(Default)]
struct StateInner {
    // S6.4 — intentionally no derived `Debug`. The inner
    // `dyn ExchangeConnector` and kill-switch closure types do
    // not implement `Debug` and we don't want a format placeholder
    // that can accidentally print credentials off the connector.
    // See the manual `impl Debug` below for what is safe to print.
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
    /// 23-UX-2 — per-leg inventory time-series keyed by
    /// `(venue, symbol)`. Populated alongside
    /// `inventory_timeseries` whenever `publish_inventory` is
    /// called. Lets the frontend draw a stacked-area chart
    /// showing how inventory is distributed across venues +
    /// products over time, not just the aggregate.
    per_leg_inventory_timeseries:
        HashMap<(String, String), std::collections::VecDeque<(i64, Decimal)>>,
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
    /// Distributed audit fetcher — set by `mm-server` at boot
    /// when a controller is present. `build_monthly_report`
    /// prefers this over reading the local `data/audit.jsonl`
    /// so in-process MiCA exports reach across every agent's
    /// disk, not just the (empty) local path. Dashboard-only
    /// tests leave it `None` and the export falls back to
    /// reading the file directly.
    audit_range_fetcher: Option<crate::state::AuditRangeFetcher>,
    /// Wave B1 — fleet-aware client-metrics fan-out. When set,
    /// `/positions`, `/pnl`, `/sla`, `/client/{id}/*` handlers
    /// prefer this over local `symbols` state so the controller
    /// serves fleet-aggregated data instead of the narrow slice
    /// `adapter.rs` projects from `DeploymentStateRow`.
    fleet_client_metrics_fetcher: Option<crate::state::FleetClientMetricsFetcher>,
    /// Wave B5 — broadcaster that pushes `AddClient` to every
    /// accepted agent. `create_client` calls it on success so
    /// per-client report endpoints start working immediately
    /// (no agent restart). Absent in unit tests — handler
    /// degrades to a local-only register.
    fleet_add_client_broadcaster: Option<crate::state::FleetAddClientBroadcaster>,
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
    /// INT-1 — per-symbol decision ledgers. Each engine
    /// publishes its own `Arc<DecisionLedger>` at startup so
    /// the `/api/v1/decisions/recent` handler can read without
    /// threading an engine handle through the request state.
    decision_ledgers: HashMap<String, std::sync::Arc<mm_risk::decision_ledger::DecisionLedger>>,
    /// UI-1 — snapshot of active execution plans per symbol.
    /// Written every tick by engines that hold a plan-bearing
    /// graph; read by the `/api/v1/plans/active` endpoint.
    active_plans: HashMap<String, Vec<PlanSnapshot>>,
    /// INV-4 — dedicated cross-venue portfolio aggregator. Owns
    /// every engine's live `(symbol, venue) → inventory + mark`
    /// snapshot so graph sources, HTTP endpoints, and daily
    /// reports read through one struct instead of a raw map on
    /// `DashboardState`. Engines publish via
    /// [`DashboardState::publish_inventory`] once per tick.
    cross_venue: CrossVenuePortfolio,
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
    /// S2.4 — highest ADL quantile observed across any of
    /// this symbol's positions (0..=4 per PERP-4). Missing
    /// entry = spot engine or no venue-reported data yet.
    per_symbol_adl_quantile: HashMap<String, u8>,
    /// S3.1 — kill-switch L4 flatten waterfall. Each engine
    /// publishes its notional drawdown (|inventory| × mid,
    /// quote-asset units) when it enters L4. Subsequent
    /// entrants read the map, rank themselves among siblings,
    /// and defer their first slice by `rank × slice_stagger_s`
    /// so the venue sees the worst bleeder exit first.
    flatten_priorities: HashMap<String, Decimal>,
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
    /// 23-UX-6 — per-venue kill-switch state. `HashMap<venue, level>`
    /// layered on top of the existing symbol-scoped `KillSwitch`:
    /// when a venue entry is elevated to L3+ the engine short-
    /// circuits order placement + cancels open orders on THAT venue
    /// only, leaving other venues quoting. Operators set via the
    /// admin HTTP endpoint; the engine reads on every
    /// `place`/`cancel` dispatch through the primary/hedge
    /// connectors. Absent venue = L0 (Normal).
    venue_kill_levels: HashMap<String, u8>,
    /// Epic 33 — pending hyperopt calibrations awaiting operator
    /// approval. Keyed by symbol; at most one per symbol at a
    /// time (a new trigger overwrites the previous suggestion).
    pending_calibrations: HashMap<String, PendingCalibration>,
    /// Channel to the server-side hyperopt worker task. Set by
    /// `register_hyperopt_trigger_channel` at startup; the admin
    /// endpoint pushes `HyperoptTrigger` payloads through it.
    /// `None` before registration — endpoint returns HTTP 503.
    hyperopt_trigger_tx: Option<tokio::sync::mpsc::UnboundedSender<HyperoptTrigger>>,
    /// Optional WebSocket broadcaster. When set, state mutators
    /// that operators watch live (venue balance snapshots, etc.)
    /// emit a typed push message so the frontend panel doesn't
    /// need to poll. Left as `None` in headless / test builds.
    ws_broadcast: Option<std::sync::Arc<crate::websocket::WsBroadcast>>,
    /// MV-2 — cross-engine atomic-bundle ack tracker. The
    /// originator registers each bundle's two legs here on
    /// dispatch; every engine publishes into the ack map as
    /// soon as its own live-orders snapshot contains a match.
    /// Cheap to clone (`(bundle_id, venue, symbol)` keys;
    /// booleans as values) and scoped to the lifetime of the
    /// bundle — originator clears on rollback / success.
    atomic_bundle_legs: HashMap<AtomicBundleLegKey, AtomicBundleLeg>,
    /// S1.3 — ring buffer of recent SOR routing decisions.
    /// Every `GreedyRouter::route()` result gets recorded here
    /// by the engine so the operator panel can show "which
    /// venue won, what did it cost, what were the runner-ups"
    /// without scraping Prometheus. Capped at
    /// `MAX_SOR_DECISIONS` most-recent entries per engine.
    sor_decisions: std::collections::VecDeque<SorDecisionRecord>,
    /// S5.1 — cross-venue rebalancer knobs. `None` until the
    /// server boot call to `set_rebalancer_config`. With `None`
    /// the `/api/v1/rebalance/recommendations` endpoint replies
    /// with an empty list, matching the "rebalancer disabled"
    /// baseline.
    rebalancer_config: Option<mm_risk::rebalancer::RebalancerConfig>,
    /// S5.2 — funding-arb driver state per `(pair_key)`. Every
    /// `DriverEvent` the server-side sink sees bumps the matching
    /// counter and replaces `last_event` so the monitor panel
    /// answers "what's the driver doing right now, has it tripped
    /// yet" without tailing logs.
    funding_arb_pairs: HashMap<String, FundingArbPairState>,
    /// Wave C1 — latest order/balance reconciliation outcome per
    /// symbol. Engine's periodic `reconcile()` pushes after every
    /// cycle so operators can see drift without tailing logs.
    reconciliation: HashMap<String, ReconciliationSnapshot>,
    /// Wave D4 — rolling ring of the last `ALERTS_CAP` alerts
    /// emitted on this DashboardState. Agent's
    /// `alerts_recent` details topic reads from here; the
    /// controller fans out across the fleet and dedupes on
    /// `(severity, title_hash)` inside a 60s window so the
    /// UI + future Telegram bridge avoid alert storms.
    alerts_buffer: std::collections::VecDeque<AlertRecord>,
    /// Wave G2/G4 — operator-opened incidents keyed by their
    /// generated id. Fed via `open_incident` when an operator
    /// clicks on a ViolationsPanel row; transitions through
    /// `ack_incident` / `resolve_incident`. In-memory; restart
    /// clears. Distinct from `incidents` above (legacy
    /// auto-logged report entries).
    open_incidents: HashMap<String, OpenIncident>,
    /// S6.4 — per-venue connector handles for the rebalancer
    /// execute endpoint. Populated at boot by the server from
    /// its `ConnectorBundle`; the endpoint looks the sender
    /// venue up here and dispatches `internal_transfer` /
    /// `withdraw` directly. `None` entries mean the dashboard
    /// is headless (tests, paper) — execute POSTs refuse.
    venue_connectors:
        HashMap<String, std::sync::Arc<dyn mm_exchange_core::connector::ExchangeConnector>>,
    /// S6.4 — append-only transfer log. `None` until server
    /// boot; endpoint returns 503 when unset so tests never
    /// accidentally write to a shared path.
    transfer_log: Option<std::sync::Arc<mm_persistence::transfer_log::TransferLogWriter>>,
    /// 23-P1-1 — shared CheckpointManager that the engine flushes
    /// into periodically. `None` until `set_checkpoint_manager`
    /// wires one; that keeps unit tests that build a bare
    /// DashboardState free of disk I/O. When set, the engine
    /// calls `publish_checkpoint_for_symbol` every 30 s with a
    /// full SymbolCheckpoint (inventory + pnl +
    /// strategy_checkpoint_state + engine_checkpoint_state). The
    /// write path flushes every 10 updates per the manager's
    /// flush_every config so we don't hit disk every tick.
    checkpoint_manager:
        Option<std::sync::Arc<std::sync::Mutex<mm_persistence::checkpoint::CheckpointManager>>>,
    /// S5.4 — per-symbol calibration snapshot (currently only
    /// `GlftStrategy` publishes into this map). The engine calls
    /// the active strategy's `recalibrate_if_due` on a
    /// minute-cadence tick and publishes the resulting
    /// `Strategy::calibration_state` afterwards. `None` /
    /// missing entry means the active strategy is stateless
    /// (grid, basis, Avellaneda).
    calibration_snapshots: HashMap<String, CalibrationSnapshot>,
    /// R3.8 — per-symbol on-chain snapshot. Populated by the
    /// server-side `OnchainPoller` task on the operator's
    /// configured refresh cadence. `None` / missing entry
    /// means onchain is disabled or the poller hasn't filled
    /// this symbol yet.
    onchain_snapshots: HashMap<String, OnchainSnapshot>,
}

impl std::fmt::Debug for StateInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Only print bookkeeping counts. Connectors / kill-switch
        // closures / webhook channels are elided both for Debug
        // brevity and because they may transitively reference
        // credentials (connector config).
        f.debug_struct("StateInner")
            .field("clients", &self.clients.len())
            .field("incidents", &self.incidents.len())
            .field("venue_connectors_registered", &self.venue_connectors.len())
            .field(
                "transfer_log",
                &self
                    .transfer_log
                    .as_ref()
                    .map(|w| w.path().display().to_string()),
            )
            .finish()
    }
}

/// S5.4 — mirror of `mm_strategy::trait::CalibrationState` so
/// the dashboard crate stays independent of `mm-strategy`. The
/// server-side caller (engine) builds both, forwards this
/// variant here via `publish_calibration`. Fields kept
/// 1:1 with the trait shape so the UI renders the same thing
/// regardless of transport.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalibrationSnapshot {
    pub symbol: String,
    pub strategy: String,
    pub a: Decimal,
    pub k: Decimal,
    pub samples: usize,
    pub last_recalibrated_ms: Option<i64>,
}

/// S1.3 — one recorded SOR routing decision. Carries the
/// target (side, qty), the legs the router picked (venue +
/// qty + cost), and every runner-up the router considered so
/// operators can see the cost differential between the winner
/// and the next-best venue.
#[derive(Debug, Clone, Serialize)]
pub struct SorDecisionRecord {
    pub ts_ms: i64,
    pub symbol: String,
    pub side: mm_common::types::Side,
    pub target_qty: Decimal,
    pub filled_qty: Decimal,
    pub is_complete: bool,
    pub winners: Vec<SorLegRecord>,
    /// Venues the router evaluated but did NOT pick. Empty
    /// when the router's input set was exhausted by the
    /// winners. Sorted by `cost_bps` ascending — the first
    /// entry is the closest miss.
    pub considered: Vec<SorLegRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SorLegRecord {
    pub venue: String,
    pub qty: Decimal,
    pub is_taker: bool,
    pub cost_bps: Decimal,
}

/// MV-2 — dispatch-time fingerprint of a single leg in an
/// atomic bundle. Every engine's sweep compares its own
/// live-orders snapshot against the set of currently-pending
/// legs so acks propagate across venues via the shared
/// dashboard state instead of a bespoke signalling channel.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct AtomicBundleLegKey {
    pub bundle_id: String,
    pub role: BundleLegRole,
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum BundleLegRole {
    Maker,
    Hedge,
}

#[derive(Debug, Clone)]
pub struct AtomicBundleLeg {
    pub venue: String,
    pub symbol: String,
    pub side: mm_common::types::Side,
    pub price: Decimal,
    pub acked: bool,
}

/// S2.2 — snapshot row for `/api/v1/atomic-bundles/inflight`
/// paired by bundle id. Either leg may be `None` if only one
/// side has been registered (originator mid-dispatch, or
/// ack-map race on read). The panel renders the missing side
/// as "—".
#[derive(Debug, Clone, Serialize)]
pub struct AtomicBundleSnapshot {
    pub bundle_id: String,
    pub maker: Option<AtomicBundleLegSnapshot>,
    pub hedge: Option<AtomicBundleLegSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AtomicBundleLegSnapshot {
    pub venue: String,
    pub symbol: String,
    pub side: mm_common::types::Side,
    pub price: Decimal,
    pub acked: bool,
}

/// S5.2 — per-pair funding-arb driver snapshot. The server-side
/// `DriverEventSink` implementation updates this struct on every
/// event so the monitor panel can render a compact "last event /
/// counts" row without re-playing audit history.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FundingArbPairState {
    pub pair: String,
    pub last_event: String,
    pub last_reason: Option<String>,
    pub last_event_at_ms: Option<i64>,
    pub entered: u64,
    pub exited: u64,
    pub taker_rejected: u64,
    pub pair_break: u64,
    pub pair_break_uncompensated: u64,
    pub hold: u64,
    pub input_unavailable: u64,
}

/// Wave C1 — per-symbol reconciliation outcome. Engine pushes
/// this after every `reconcile()` cycle; operators read from
/// `/api/v1/reconciliation/fleet` which fans out + aggregates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconciliationSnapshot {
    /// Symbol this cycle inspected.
    pub symbol: String,
    /// Monotonically-increasing reconcile cycle counter. Helps
    /// spot agents where the cycle has stalled.
    pub cycle: u64,
    /// Epoch millis when this cycle completed.
    pub last_cycle_ms: i64,
    /// Order IDs tracked locally but absent on venue — they were
    /// removed from our tracker. Empty on clean cycle.
    pub ghost_orders: Vec<String>,
    /// Order IDs live on venue but not tracked locally — they
    /// were recovered into the tracker. Empty on clean cycle.
    pub phantom_orders: Vec<String>,
    /// `(asset, internal_available, exchange_available)` triples
    /// where the pct delta breached the tolerance configured for
    /// this deployment.
    pub balance_mismatches: Vec<BalanceMismatch>,
    /// Count of internal orders at cycle start (pre-reconcile).
    pub internal_orders: u32,
    /// Count of venue orders at cycle start.
    pub venue_orders: u32,
    /// `true` when this cycle's `get_open_orders` call failed —
    /// order-side reconciliation was skipped. Balance side may
    /// still have run.
    pub orders_fetch_failed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BalanceMismatch {
    pub asset: String,
    pub internal: String,
    pub exchange: String,
}

/// Wave D4 — agent-local alert record mirrored from the
/// engine's `AlertManager`. One entry per alert emission
/// (the per-agent manager already dedups on a 5-minute
/// window; the controller adds a second 60-second window
/// across the fleet). Stored in `alerts_buffer` on
/// DashboardState.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRecord {
    pub ts_ms: i64,
    pub severity: String,
    pub title: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

const ALERTS_CAP: usize = 200;
const MAX_SOR_DECISIONS: usize = 256;
const MAX_DAILY_REPORTS: usize = 90;
/// 23-UX-1 — ring buffer size for PnL / spread / inventory
/// time-series. Paired with the 1-second min gap gate in
/// `push_pnl_sample` etc. so 14400 points = 4 hours of history
/// at 1-second resolution. Operators opening the dashboard
/// after a 4-hour session see the full chart from engine boot
/// instead of the previous "warm up from 0 on panel mount" UX.
/// Memory: ~230 KB per metric per symbol.
const MAX_PNL_TIMESERIES: usize = 14400;
/// Minimum milliseconds between successive time-series samples.
/// Engine publishes ~every tick (500 ms); we downsample to 1 s
/// so the ring covers 4 hours instead of 2 hours.
const MIN_TIMESERIES_GAP_MS: i64 = 1000;
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

/// 23-UX-2 — one leg's inventory history for the per-leg
/// stacked-area chart. `venue + symbol` identifies the leg
/// uniquely; `base_asset` is inferred from symbol so the
/// frontend can filter / group across quote-currency variants.
#[derive(Debug, Clone, Serialize)]
pub struct PerLegInventoryHistory {
    pub venue: String,
    pub symbol: String,
    pub base_asset: String,
    pub points: Vec<SeriesPoint>,
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
    /// S2.3 — venue tag the fill hit, in lowercase
    /// (`"binance"`, `"bybit"`, `"hyperliquid"`). Populated
    /// from `exchange_type.to_lowercase()` at fill ingest so
    /// per-venue PnL breakdowns can be computed client-side
    /// without crossing-index against a separate engine map.
    /// `#[serde(default)]` keeps old checkpoints + WS payloads
    /// deserialisable as empty string.
    #[serde(default)]
    pub venue: String,
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

/// Wave G2/G4 — operator-opened incident with full lifecycle
/// (open → acknowledged → resolved). Distinct from the
/// auto-logged `IncidentRecord` above which is a pure
/// append-log entry for the daily report; this one carries
/// ownership + post-mortem fields so on-call actually has a
/// workflow.
///
/// In-memory only in this revision — restart clears the list.
/// Persistent JSONL is a follow-up (same pattern as the audit
/// log) once the operator surface stabilises.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenIncident {
    pub id: String,
    pub opened_at_ms: i64,
    pub opened_by: String,
    /// Which ViolationsPanel row triggered this incident (e.g.
    /// `sla#BTCUSDT`, `manip#eu-01/btc-1`). Used for dedup — a
    /// second "open" on the same key updates the existing open
    /// incident instead of spawning a duplicate row.
    pub violation_key: String,
    pub severity: String,
    pub category: String,
    pub target: String,
    pub metric: String,
    pub detail: String,
    /// Current lifecycle state: `"open"`, `"acked"`, `"resolved"`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at_ms: Option<i64>,
    /// Post-mortem fields — populated on resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_taken: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preventive: Option<String>,
    /// M4-4 GOBS — optional graph deep-link context. Populated
    /// when an incident is filed from a strategy-graph-carrying
    /// deployment so the post-mortem UI can jump straight to
    /// the exact tick in the Live mode canvas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_deployment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_tick_num: Option<u64>,
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
    /// S6.1 — when a strategy graph is currently driving this
    /// symbol, carries the deployed graph's metadata. `None`
    /// means the engine is on the legacy `strategy` slot with no
    /// graph override active. Published on every tick so a swap
    /// becomes visible within one refresh cycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_graph: Option<ActiveGraphSnapshot>,
    /// R2.6 — CEX-side manipulation detector bundle snapshot.
    /// `None` before the first tick; thereafter carries the
    /// four-field view (pump-dump, wash, thin-book, combined)
    /// that drives the AdminPage panel and the
    /// `Surveillance.ManipulationScore` graph source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manipulation_score: Option<ManipulationScoreSnapshot>,
    /// R2.13 — composite rug score aggregating CEX manipulation,
    /// on-chain concentration + inflow, listing age and mcap
    /// proxy signals into one `[0, 1]` number.
    /// `Surveillance.RugScore` graph source reads this; the
    /// AdminPage panel renders per-symbol rows with the
    /// sub-scores expanded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rug_score: Option<RugScoreSnapshot>,
}

/// R2.13 — mirror of `mm_risk::manipulation::RugScoreSnapshot`
/// so the dashboard stays free of mm-risk's internal types.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RugScoreSnapshot {
    pub manipulation: Decimal,
    pub holder_concentration: Decimal,
    pub cex_inflow: Decimal,
    pub listing_age: Decimal,
    pub mcap_ratio: Decimal,
    pub combined: Decimal,
}

/// R2.6 — per-symbol manipulation detector snapshot. Mirror of
/// `mm_risk::manipulation::ManipulationScoreSnapshot` so
/// `mm-dashboard` stays independent of `mm-risk`'s internal
/// types (the engine does the conversion).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManipulationScoreSnapshot {
    pub pump_dump: Decimal,
    pub wash: Decimal,
    pub thin_book: Decimal,
    pub combined: Decimal,
}

/// R3.8 — per-symbol on-chain surveillance snapshot. Mirror of
/// the aggregated output of `mm-onchain` cache + tracker so the
/// dashboard stays free of the `mm-onchain` dependency. The
/// server-side poller fills this on its refresh cadence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnchainSnapshot {
    pub symbol: String,
    pub chain: String,
    /// Top-N holder concentration, clamped to [0, 1]. A RAVE-
    /// style rug sits at 0.8-0.95 on day one.
    pub concentration_pct: Decimal,
    /// Number of top holders the cache fetched.
    pub top_n: u32,
    /// Inflow into known-CEX addresses from operator-listed
    /// suspect wallets, raw token units over the tracker
    /// window.
    pub inflow_total: Decimal,
    /// Number of distinct CEX deposit events in-window.
    pub inflow_events: u32,
    /// When the snapshot was last refreshed on the
    /// poller-task's tick. Exposed to the UI so operators
    /// see whether the numbers are stale after a rate-limit
    /// incident.
    pub computed_at_ms: i64,
}

/// S6.1 — per-symbol active-graph descriptor. Fields mirror the
/// canonical `mm_strategy_graph::Graph` identifiers so operators
/// can cross-reference the /api/admin/strategy/graphs history
/// record by hash if needed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActiveGraphSnapshot {
    /// Graph name as authored in the deployed JSON
    /// (`graph.name`). Human-friendly identifier.
    pub name: String,
    /// Content-hash of the deployed graph body
    /// (`graph.content_hash()`). Stable across byte-identical
    /// re-deploys, flips on any edit.
    pub hash: String,
    /// Graph scope (`"symbol"`, `"pair"`, `"global"`) plus the
    /// scope-value text. Example: `"symbol: BTCUSDT"` or
    /// `"global"`.
    pub scope: String,
    /// Epoch-millis of the last successful `swap_strategy_graph`
    /// / `with_strategy_graph` call on this engine. Used by the
    /// frontend to render "deployed 14:22 UTC".
    pub deployed_at_ms: i64,
    /// Number of nodes in the deployed graph. Stamped once on
    /// swap so the UI can show density without re-parsing.
    pub node_count: usize,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// UI-1 — one entry in `/api/v1/plans/active` response. Serialised
/// shape drives the StrategyPage footer.
#[derive(Debug, Clone, Serialize, Default)]
pub struct PlanSnapshot {
    /// Graph node id (UUID as hex).
    pub node_id: String,
    /// Kind string, e.g. `"Plan.Accumulate"`.
    pub kind: String,
    pub symbol: String,
    pub started_at_ms: Option<i64>,
    pub qty_emitted: Decimal,
    pub aborted: bool,
    pub last_slice_ms: i64,
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
    /// PNL-COUNTER-1 — raw count of individual fills.
    /// `round_trips` only moves when inventory returns to
    /// zero, so tenants / operators looking at "how many
    /// trades have we done" get this value instead.
    #[serde(default)]
    pub fill_count: u64,
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

    /// Wire the distributed audit fetcher. Called once from
    /// `mm-server` boot after both FleetState and AgentRegistry
    /// are available.
    pub fn set_audit_range_fetcher(&self, fetcher: AuditRangeFetcher) {
        self.inner.write().unwrap().audit_range_fetcher = Some(fetcher);
    }

    pub fn audit_range_fetcher(&self) -> Option<AuditRangeFetcher> {
        self.inner.read().unwrap().audit_range_fetcher.clone()
    }

    /// Wave B1 — wire the fleet-aware client-metrics fetcher.
    /// Called once from server boot after both FleetState and
    /// AgentRegistry are available.
    pub fn set_fleet_client_metrics_fetcher(&self, fetcher: FleetClientMetricsFetcher) {
        self.inner.write().unwrap().fleet_client_metrics_fetcher = Some(fetcher);
    }

    pub fn fleet_client_metrics_fetcher(&self) -> Option<FleetClientMetricsFetcher> {
        self.inner
            .read()
            .unwrap()
            .fleet_client_metrics_fetcher
            .clone()
    }

    /// Wave B5 — wire the fleet add-client broadcaster. Called
    /// once from server boot. Admin's `create_client` invokes
    /// it so a newly-registered tenant immediately propagates
    /// to every accepted agent.
    pub fn set_fleet_add_client_broadcaster(&self, broadcaster: FleetAddClientBroadcaster) {
        self.inner.write().unwrap().fleet_add_client_broadcaster = Some(broadcaster);
    }

    pub fn fleet_add_client_broadcaster(&self) -> Option<FleetAddClientBroadcaster> {
        self.inner
            .read()
            .unwrap()
            .fleet_add_client_broadcaster
            .clone()
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
    pub fn sentiment_tick_for(&self, asset: &str) -> Option<mm_sentiment::SentimentTick> {
        self.inner
            .read()
            .unwrap()
            .sentiment_ticks
            .get(asset)
            .cloned()
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
    pub fn strategy_graph_store(&self) -> Option<std::sync::Arc<mm_strategy_graph::GraphStore>> {
        self.inner.read().unwrap().strategy_graph_store.clone()
    }

    /// INT-1 — register an engine's decision ledger. Called
    /// once per engine at boot.
    pub fn register_decision_ledger(
        &self,
        symbol: &str,
        ledger: std::sync::Arc<mm_risk::decision_ledger::DecisionLedger>,
    ) {
        self.inner
            .write()
            .unwrap()
            .decision_ledgers
            .insert(symbol.to_string(), ledger);
    }

    /// INT-1 — snapshot recent decisions for one symbol. `None`
    /// when no ledger has been registered.
    pub fn decisions_recent(
        &self,
        symbol: &str,
        max: usize,
    ) -> Option<Vec<mm_risk::decision_ledger::DecisionSnapshot>> {
        self.inner
            .read()
            .unwrap()
            .decision_ledgers
            .get(symbol)
            .map(|l| l.recent(max))
    }

    /// INT-1 — snapshot recent decisions across every
    /// registered symbol, newest-first within each symbol.
    pub fn decisions_all_symbols(
        &self,
        per_symbol_max: usize,
    ) -> std::collections::BTreeMap<String, Vec<mm_risk::decision_ledger::DecisionSnapshot>> {
        let g = self.inner.read().unwrap();
        let mut out = std::collections::BTreeMap::new();
        for (sym, ledger) in g.decision_ledgers.iter() {
            out.insert(sym.clone(), ledger.recent(per_symbol_max));
        }
        out
    }

    /// INV-4 — publish engine-side inventory (and optional mark
    /// price) for the `(symbol, venue)` tuple. Engines call this
    /// every tick so the cross-venue aggregator reads a fresh
    /// picture without a round-trip through the engine. Pass
    /// `mark = None` while the book is still warming up — the
    /// aggregator will track delta but skip the leg for notional.
    pub fn publish_inventory(
        &self,
        symbol: &str,
        venue: &str,
        inv: Decimal,
        mark: Option<Decimal>,
    ) {
        let mut inner = self.inner.write().unwrap();
        inner.cross_venue.publish(symbol, venue, inv, mark);
        // 23-UX-2 — also push into per-leg time-series so the
        // PerLegInventoryChart can render "Binance spot vs
        // Bybit spot vs Binance perp" over time. Same
        // 1s-gap / 14400-cap rules as `push_pnl_sample`.
        let key = (venue.to_string(), symbol.to_string());
        let ts_ms = chrono::Utc::now().timestamp_millis();
        let series = inner.per_leg_inventory_timeseries.entry(key).or_default();
        let should_push = series
            .back()
            .map(|(last_ts, _)| ts_ms - *last_ts >= MIN_TIMESERIES_GAP_MS)
            .unwrap_or(true);
        if should_push {
            series.push_back((ts_ms, inv));
            while series.len() > MAX_PNL_TIMESERIES {
                series.pop_front();
            }
        }
    }

    /// 23-UX-2 — flat per-leg inventory time-series for
    /// frontend charting. Returns one row per leg with its
    /// full history — the frontend does the stacking. `base`
    /// filter uses `infer_base_asset` to match legs across
    /// quote-variants (BTCUSDT + BTCUSDC both land under
    /// `"BTC"`).
    pub fn per_leg_inventory_timeseries(
        &self,
        base_filter: Option<&str>,
    ) -> Vec<PerLegInventoryHistory> {
        let inner = self.inner.read().unwrap();
        inner
            .per_leg_inventory_timeseries
            .iter()
            .filter(|((_, symbol), _)| {
                // `infer_base_asset("BTCUSDT") == "BTCUSDT"` by
                // design (fallback keeps the full symbol when
                // no separator / digit is found). So we match
                // by prefix instead of equality to group both
                // BTCUSDT and BTCUSDC under "BTC".
                base_filter
                    .map(|b| {
                        let inferred = mm_portfolio::infer_base_asset(symbol);
                        inferred == b || inferred.starts_with(b)
                    })
                    .unwrap_or(true)
            })
            .map(|((venue, symbol), series)| PerLegInventoryHistory {
                venue: venue.clone(),
                symbol: symbol.clone(),
                base_asset: mm_portfolio::infer_base_asset(symbol),
                points: series
                    .iter()
                    .map(|(ts, inv)| SeriesPoint {
                        timestamp_ms: *ts,
                        value: *inv,
                    })
                    .collect(),
            })
            .collect()
    }

    /// INV-4 — net delta in `base_asset` units, summed across
    /// every venue. `"BTC"` matches both `BTCUSDT` on Binance
    /// and `BTCUSDC` on Bybit because the underlying aggregator
    /// infers base asset once at publish time.
    pub fn cross_venue_net_delta(&self, base_asset: &str) -> Decimal {
        self.inner.read().unwrap().cross_venue.net_delta(base_asset)
    }

    /// INV-4 — flat list of every published leg. Readers that
    /// need grouping prefer [`Self::cross_venue_by_asset`].
    pub fn cross_venue_entries(&self) -> Vec<VenueInventory> {
        self.inner.read().unwrap().cross_venue.entries()
    }

    /// INV-4 — per-base-asset grouped view used by the
    /// cross-venue UI panel (`/api/v1/portfolio/cross_venue`).
    /// Legs are sorted `(venue, symbol)` for deterministic
    /// rendering.
    pub fn cross_venue_by_asset(&self) -> Vec<AssetAggregate> {
        self.inner.read().unwrap().cross_venue.by_asset()
    }

    /// MV-2 — register a single atomic-bundle leg on dispatch.
    /// Originator calls this once per leg (maker + hedge). Every
    /// engine's sweep reads back via
    /// [`Self::pending_bundle_leg_matches`] to flip acks as soon
    /// as the leg appears on its local live-orders map.
    pub fn register_atomic_bundle_leg(
        &self,
        bundle_id: &str,
        role: BundleLegRole,
        venue: &str,
        symbol: &str,
        side: mm_common::types::Side,
        price: Decimal,
    ) {
        self.inner.write().unwrap().atomic_bundle_legs.insert(
            AtomicBundleLegKey {
                bundle_id: bundle_id.to_string(),
                role,
            },
            AtomicBundleLeg {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                side,
                price,
                acked: false,
            },
        );
    }

    /// MV-2 — every engine calls this during its ack sweep with
    /// the `(venue, symbol, side, price)` of each of its OWN live
    /// maker orders. Returns the `(bundle_id, role)` pairs that
    /// match the input so the caller can mark the ack below.
    /// `tick_size_tolerance` mirrors the half-tick rounding
    /// slack the engine's own matcher already applies.
    pub fn pending_bundle_leg_matches(
        &self,
        venue: &str,
        symbol: &str,
        side: mm_common::types::Side,
        price: Decimal,
        tick_size_tolerance: Decimal,
    ) -> Vec<(String, BundleLegRole)> {
        let g = self.inner.read().unwrap();
        g.atomic_bundle_legs
            .iter()
            .filter(|(_, leg)| {
                leg.venue == venue
                    && leg.symbol == symbol
                    && leg.side == side
                    && (leg.price - price).abs() <= tick_size_tolerance
                    && !leg.acked
            })
            .map(|(key, _)| (key.bundle_id.clone(), key.role))
            .collect()
    }

    /// MV-2 — flip an ack flag. Safe to call from any engine;
    /// idempotent (repeat calls for a leg already acked are
    /// no-ops).
    pub fn ack_atomic_bundle_leg(&self, bundle_id: &str, role: BundleLegRole) {
        let mut g = self.inner.write().unwrap();
        if let Some(leg) = g.atomic_bundle_legs.get_mut(&AtomicBundleLegKey {
            bundle_id: bundle_id.to_string(),
            role,
        }) {
            leg.acked = true;
        }
    }

    /// MV-2 — originator reads both legs' current ack state.
    /// Returns `(maker_acked, hedge_acked)` — either can be
    /// `false` if the corresponding leg has not landed on a
    /// venue yet.
    pub fn atomic_bundle_ack_state(&self, bundle_id: &str) -> (bool, bool) {
        let g = self.inner.read().unwrap();
        let maker = g
            .atomic_bundle_legs
            .get(&AtomicBundleLegKey {
                bundle_id: bundle_id.to_string(),
                role: BundleLegRole::Maker,
            })
            .map(|l| l.acked)
            .unwrap_or(false);
        let hedge = g
            .atomic_bundle_legs
            .get(&AtomicBundleLegKey {
                bundle_id: bundle_id.to_string(),
                role: BundleLegRole::Hedge,
            })
            .map(|l| l.acked)
            .unwrap_or(false);
        (maker, hedge)
    }

    /// MV-2 — originator removes both legs when the bundle
    /// finishes (success or rollback). Leaving entries around
    /// would match against future live orders and cause
    /// spurious "acks" for retired bundles.
    pub fn clear_atomic_bundle(&self, bundle_id: &str) {
        let mut g = self.inner.write().unwrap();
        g.atomic_bundle_legs.retain(|k, _| k.bundle_id != bundle_id);
    }

    /// S2.2 — full snapshot of every inflight atomic bundle
    /// for the monitor panel. One entry per bundle id with
    /// both legs' venue/symbol/side/price/ack paired up.
    /// Bundles where the originator already cleared the
    /// shared map (success + graduation, or rollback) don't
    /// appear here.
    pub fn atomic_bundles_inflight(&self) -> Vec<AtomicBundleSnapshot> {
        use std::collections::BTreeMap;
        let g = self.inner.read().unwrap();
        let mut by_id: BTreeMap<String, AtomicBundleSnapshot> = BTreeMap::new();
        for (key, leg) in &g.atomic_bundle_legs {
            let entry =
                by_id
                    .entry(key.bundle_id.clone())
                    .or_insert_with(|| AtomicBundleSnapshot {
                        bundle_id: key.bundle_id.clone(),
                        maker: None,
                        hedge: None,
                    });
            let rendered = AtomicBundleLegSnapshot {
                venue: leg.venue.clone(),
                symbol: leg.symbol.clone(),
                side: leg.side,
                price: leg.price,
                acked: leg.acked,
            };
            match key.role {
                BundleLegRole::Maker => entry.maker = Some(rendered),
                BundleLegRole::Hedge => entry.hedge = Some(rendered),
            }
        }
        by_id.into_values().collect()
    }

    /// S1.3 — append a freshly-produced SOR routing decision
    /// to the ring buffer. Capped at `MAX_SOR_DECISIONS`
    /// (oldest entry dropped on overflow).
    pub fn record_sor_decision(&self, rec: SorDecisionRecord) {
        let mut g = self.inner.write().unwrap();
        g.sor_decisions.push_back(rec);
        while g.sor_decisions.len() > MAX_SOR_DECISIONS {
            g.sor_decisions.pop_front();
        }
    }

    /// S1.3 — most-recent `limit` entries, newest-first. The
    /// `/api/v1/sor/decisions/recent` handler consumes this.
    pub fn sor_decisions_recent(&self, limit: usize) -> Vec<SorDecisionRecord> {
        let g = self.inner.read().unwrap();
        g.sor_decisions
            .iter()
            .rev()
            .take(limit.min(MAX_SOR_DECISIONS))
            .cloned()
            .collect()
    }

    /// UI-1 — publish active plans for a symbol. Empty vec
    /// clears previous snapshot.
    pub fn publish_active_plans(&self, symbol: &str, plans: Vec<PlanSnapshot>) {
        self.inner
            .write()
            .unwrap()
            .active_plans
            .insert(symbol.to_string(), plans);
    }

    /// UI-1 — flat list of every active plan across every
    /// symbol, for the `/api/v1/plans/active` endpoint.
    pub fn active_plans_all(&self) -> Vec<PlanSnapshot> {
        let g = self.inner.read().unwrap();
        g.active_plans.values().flatten().cloned().collect()
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

    /// S2.4 — publish the highest ADL quantile observed
    /// across any of `symbol`'s positions. Engine's
    /// `MarginGuard::max_adl_quantile` drives this on every
    /// poll.
    pub fn set_adl_quantile(&self, symbol: &str, quantile: u8) {
        self.inner
            .write()
            .unwrap()
            .per_symbol_adl_quantile
            .insert(symbol.to_string(), quantile);
    }

    pub fn adl_quantile(&self, symbol: &str) -> Option<u8> {
        self.inner
            .read()
            .unwrap()
            .per_symbol_adl_quantile
            .get(symbol)
            .copied()
    }

    /// S3.1 — register this engine's flatten priority when it
    /// enters kill-switch L4. Priority key is the absolute
    /// notional drawdown (`|inventory| × mid` in quote-asset
    /// units at entry time); higher = more urgent.
    pub fn register_flatten_priority(&self, symbol: &str, notional_drawdown: Decimal) {
        self.inner
            .write()
            .unwrap()
            .flatten_priorities
            .insert(symbol.to_string(), notional_drawdown);
    }

    /// S3.1 — drop the registration once the unwind completes
    /// so a repeat kill-switch escalation sees a clean queue.
    pub fn clear_flatten_priority(&self, symbol: &str) {
        self.inner
            .write()
            .unwrap()
            .flatten_priorities
            .remove(symbol);
    }

    /// S3.1 — rank of `symbol` in the waterfall (0-based;
    /// 0 = highest drawdown, first to flatten). Returns
    /// `None` when the symbol isn't registered yet (caller
    /// treats as rank 0 since it's about to register). Ties
    /// are broken by lexicographic symbol order for
    /// deterministic cross-restart behaviour.
    pub fn flatten_priority_rank(&self, symbol: &str) -> Option<usize> {
        let g = self.inner.read().unwrap();
        let self_dd = g.flatten_priorities.get(symbol).copied()?;
        let mut rank = 0usize;
        for (other_sym, other_dd) in g.flatten_priorities.iter() {
            if other_sym == symbol {
                continue;
            }
            if *other_dd > self_dd || (*other_dd == self_dd && other_sym.as_str() < symbol) {
                rank += 1;
            }
        }
        Some(rank)
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
        // R2-REGIME-1 (2026-04-22) — `mm_regime` gauge was
        // defined in metrics.rs but never set. Agent registry
        // reads it via `read_gauge_by_symbol` and decodes into a
        // label (`regime_label`) for DeploymentStateRow. Without
        // this emission, fleet API returned `regime: ""` even
        // though the engine log said `regime=Quiet`. Encoding
        // mirrors the agent's `regime_label` table.
        let regime_code: f64 = match state.regime.as_str() {
            "Quiet" => 0.0,
            "Trending" => 1.0,
            "Volatile" => 2.0,
            "MeanReverting" => 3.0,
            _ => -1.0,
        };
        if regime_code >= 0.0 {
            crate::metrics::REGIME
                .with_label_values(&[&state.symbol])
                .set(regime_code);
        }
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
        inner
            .venue_balances
            .get(symbol)
            .cloned()
            .unwrap_or_default()
    }

    /// Fetch per-venue balance snapshots for every symbol the
    /// dashboard knows about. Used by the drilldown panel's
    /// overview mode.
    pub fn all_venue_balances(&self) -> HashMap<String, Vec<VenueBalanceSnapshot>> {
        let inner = self.inner.read().unwrap();
        inner.venue_balances.clone()
    }

    // ── Venue-scoped kill switch (23-UX-6) ──────────────────

    /// Set the kill-switch level for a single venue. `level` uses
    /// the same 0..=5 scale as the primary `KillSwitch` so
    /// consumers can compare against the same thresholds:
    ///   0 — Normal     (no action)
    ///   1 — WidenQuotes
    ///   2 — StopNewOrders
    ///   3 — CancelAll
    ///   4 — Flatten
    ///   5 — Disconnect
    /// A WS push is emitted so panels update without polling.
    pub fn set_venue_kill_level(&self, venue: &str, level: u8) {
        let mut inner = self.inner.write().unwrap();
        inner.venue_kill_levels.insert(venue.to_string(), level);
        if let Some(bc) = &inner.ws_broadcast {
            if let Ok(payload) = serde_json::to_string(&serde_json::json!({
                "type": "venue_kill_level",
                "venue": venue,
                "level": level,
            })) {
                bc.send(&payload);
            }
        }
    }

    /// Fetch the kill-switch level for a single venue. Returns 0
    /// (Normal) when the venue has never been escalated — absent
    /// entry is treated the same as an explicit Normal write.
    pub fn venue_kill_level(&self, venue: &str) -> u8 {
        let inner = self.inner.read().unwrap();
        inner.venue_kill_levels.get(venue).copied().unwrap_or(0)
    }

    /// Fetch the full venue → kill level map. Used by the
    /// `/api/v1/kill/venues` endpoint and the dashboard panel.
    pub fn all_venue_kill_levels(&self) -> HashMap<String, u8> {
        let inner = self.inner.read().unwrap();
        inner.venue_kill_levels.clone()
    }

    /// S5.2 — record a funding-arb driver event against a
    /// `(pair_key)` bucket. `event_tag` is the lowercase
    /// `DriverEvent` variant name (`"entered"`, `"exited"`,
    /// `"taker_rejected"`, `"pair_break"`, `"hold"`,
    /// `"input_unavailable"`). `uncompensated` is only meaningful
    /// for `"pair_break"`.
    pub fn record_funding_arb_event(
        &self,
        pair_key: &str,
        event_tag: &str,
        reason: Option<&str>,
        uncompensated: bool,
    ) {
        let mut g = self.inner.write().unwrap();
        let entry = g
            .funding_arb_pairs
            .entry(pair_key.to_string())
            .or_insert_with(|| FundingArbPairState {
                pair: pair_key.to_string(),
                ..FundingArbPairState::default()
            });
        entry.last_event = event_tag.to_string();
        entry.last_reason = reason.map(str::to_string);
        entry.last_event_at_ms = Some(Utc::now().timestamp_millis());
        match event_tag {
            "entered" => entry.entered += 1,
            "exited" => entry.exited += 1,
            "taker_rejected" => entry.taker_rejected += 1,
            "pair_break" => {
                entry.pair_break += 1;
                if uncompensated {
                    entry.pair_break_uncompensated += 1;
                }
            }
            "hold" => entry.hold += 1,
            "input_unavailable" => entry.input_unavailable += 1,
            _ => {}
        }
    }

    /// S6.4 — register a connector for `venue` (lowercase).
    /// Boot calls this once per venue in the bundle so the
    /// rebalancer execute endpoint can dispatch transfers
    /// without threading engine handles through request
    /// state.
    pub fn register_venue_connector(
        &self,
        venue: &str,
        connector: std::sync::Arc<dyn mm_exchange_core::connector::ExchangeConnector>,
    ) {
        self.inner
            .write()
            .unwrap()
            .venue_connectors
            .insert(venue.to_lowercase(), connector);
    }

    /// S6.4 — connector handle for `venue`, if registered.
    pub fn venue_connector(
        &self,
        venue: &str,
    ) -> Option<std::sync::Arc<dyn mm_exchange_core::connector::ExchangeConnector>> {
        self.inner
            .read()
            .unwrap()
            .venue_connectors
            .get(&venue.to_lowercase())
            .cloned()
    }

    /// S6.4 — attach the transfer log writer. `None` leaves the
    /// execute endpoint returning 503.
    pub fn set_transfer_log(
        &self,
        log: std::sync::Arc<mm_persistence::transfer_log::TransferLogWriter>,
    ) {
        self.inner.write().unwrap().transfer_log = Some(log);
    }

    pub fn transfer_log(
        &self,
    ) -> Option<std::sync::Arc<mm_persistence::transfer_log::TransferLogWriter>> {
        self.inner.read().unwrap().transfer_log.clone()
    }

    /// 23-P1-1 — attach the shared CheckpointManager so engines
    /// can flush SymbolCheckpoints through the dashboard. Called
    /// once at server boot with the same Arc used by the final
    /// shutdown flush.
    pub fn set_checkpoint_manager(
        &self,
        mgr: std::sync::Arc<std::sync::Mutex<mm_persistence::checkpoint::CheckpointManager>>,
    ) {
        self.inner.write().unwrap().checkpoint_manager = Some(mgr);
    }

    /// 23-P1-1 — engine-facing checkpoint flush. Called from the
    /// engine on its 30 s checkpoint tick with a fully-populated
    /// SymbolCheckpoint (inventory + pnl + strategy_checkpoint_state
    /// + engine_checkpoint_state + inflight_atomic_bundles).
    /// Silently no-ops when no manager is configured (tests, smoke
    /// runs without disk). Failures log at warn.
    pub fn publish_symbol_checkpoint(&self, sc: mm_persistence::checkpoint::SymbolCheckpoint) {
        let mgr_arc = {
            let inner = self.inner.read().unwrap();
            inner.checkpoint_manager.clone()
        };
        let Some(mgr) = mgr_arc else { return };
        let lock_result = mgr.lock();
        match lock_result {
            Ok(mut guard) => {
                guard.update_symbol(sc);
            }
            Err(_) => {
                tracing::warn!("checkpoint manager mutex poisoned — skipping flush");
            }
        }
    }

    /// S6.4 — highest kill-switch level published by any
    /// engine on its per-tick SymbolState. Returns 0 (Normal)
    /// when no engine has published yet.
    pub fn max_kill_level(&self) -> u8 {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .values()
            .flat_map(|c| c.symbols.values())
            .map(|s| s.kill_level)
            .max()
            .unwrap_or(0)
    }

    /// R3.8 — server-side poller publishes the latest on-chain
    /// view for a symbol. Overwrites any prior entry.
    pub fn publish_onchain(&self, snap: OnchainSnapshot) {
        let mut g = self.inner.write().unwrap();
        g.onchain_snapshots.insert(snap.symbol.clone(), snap);
    }

    pub fn onchain_snapshots(&self) -> Vec<OnchainSnapshot> {
        let g = self.inner.read().unwrap();
        let mut out: Vec<_> = g.onchain_snapshots.values().cloned().collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.concentration_pct));
        out
    }

    pub fn onchain_snapshot(&self, symbol: &str) -> Option<OnchainSnapshot> {
        self.inner
            .read()
            .unwrap()
            .onchain_snapshots
            .get(symbol)
            .cloned()
    }

    /// S5.4 — engine-side hook: publish a calibration snapshot
    /// for `symbol`. Overwrites any previous row so the panel
    /// always sees the latest `(a, k, samples)`.
    pub fn publish_calibration(&self, snap: CalibrationSnapshot) {
        let mut g = self.inner.write().unwrap();
        g.calibration_snapshots.insert(snap.symbol.clone(), snap);
    }

    /// S5.4 — flat snapshot list ordered by symbol for the
    /// `/api/v1/calibration/status` handler.
    pub fn calibration_snapshots(&self) -> Vec<CalibrationSnapshot> {
        let g = self.inner.read().unwrap();
        let mut out: Vec<_> = g.calibration_snapshots.values().cloned().collect();
        out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        out
    }

    /// S5.2 — flat list of pair states for the monitor panel.
    pub fn funding_arb_pairs(&self) -> Vec<FundingArbPairState> {
        let g = self.inner.read().unwrap();
        let mut out: Vec<_> = g.funding_arb_pairs.values().cloned().collect();
        out.sort_by(|a, b| a.pair.cmp(&b.pair));
        out
    }

    /// Wave C1 — replace the reconciliation snapshot for a
    /// symbol. Engine calls after every `reconcile()` cycle.
    pub fn push_reconciliation(&self, snap: ReconciliationSnapshot) {
        let mut g = self.inner.write().unwrap();
        g.reconciliation.insert(snap.symbol.clone(), snap);
    }

    /// Wave C1 — read the last reconciliation outcome for a
    /// symbol. Returns `None` if no cycle has completed yet.
    pub fn get_reconciliation(&self, symbol: &str) -> Option<ReconciliationSnapshot> {
        self.inner
            .read()
            .unwrap()
            .reconciliation
            .get(symbol)
            .cloned()
    }

    /// Wave C1 — every reconciliation snapshot, sorted by symbol.
    /// The agent's `reconciliation_snapshot` details topic reads
    /// from here; the controller fans out across the fleet.
    pub fn reconciliation_snapshots(&self) -> Vec<ReconciliationSnapshot> {
        let g = self.inner.read().unwrap();
        let mut out: Vec<_> = g.reconciliation.values().cloned().collect();
        out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        out
    }

    /// Wave D4 — push an alert record onto the agent-local
    /// ring. Engine's `AlertManager` already dedupes on 5-min
    /// windows per agent; the controller applies a second
    /// pass across the fleet.
    pub fn push_alert(&self, record: AlertRecord) {
        let mut g = self.inner.write().unwrap();
        if g.alerts_buffer.len() >= ALERTS_CAP {
            g.alerts_buffer.pop_front();
        }
        g.alerts_buffer.push_back(record);
    }

    /// Wave D4 — newest-first snapshot of the agent's alert
    /// buffer. Capped at `ALERTS_CAP` on the wire.
    pub fn alerts_recent(&self, limit: usize) -> Vec<AlertRecord> {
        let g = self.inner.read().unwrap();
        let cap = limit.min(ALERTS_CAP);
        g.alerts_buffer.iter().rev().take(cap).cloned().collect()
    }

    /// Wave G2 — open a new incident OR refresh an existing
    /// open/acked incident with the same `violation_key`. This
    /// dedup is what lets operators click "Open incident" from
    /// any row without worrying about spamming duplicates — if
    /// the SLA breach keeps firing, we just touch the existing
    /// entry's metric/detail.
    pub fn open_incident(&self, inc: OpenIncident) -> OpenIncident {
        let mut g = self.inner.write().unwrap();
        // Find an existing non-resolved incident on the same key.
        let existing_id = g
            .open_incidents
            .values()
            .find(|i| i.violation_key == inc.violation_key && i.state != "resolved")
            .map(|i| i.id.clone());
        if let Some(id) = existing_id {
            if let Some(rec) = g.open_incidents.get_mut(&id) {
                rec.metric = inc.metric;
                rec.detail = inc.detail;
                rec.severity = inc.severity;
                return rec.clone();
            }
        }
        g.open_incidents.insert(inc.id.clone(), inc.clone());
        inc
    }

    /// Wave G2 — mark an incident acknowledged. Transition
    /// allowed only from `open`. Returns None if the incident
    /// doesn't exist or is already acked / resolved.
    pub fn ack_incident(&self, id: &str, by: &str) -> Option<OpenIncident> {
        let mut g = self.inner.write().unwrap();
        let rec = g.open_incidents.get_mut(id)?;
        if rec.state != "open" {
            return None;
        }
        rec.state = "acked".into();
        rec.acked_by = Some(by.to_string());
        rec.acked_at_ms = Some(chrono::Utc::now().timestamp_millis());
        Some(rec.clone())
    }

    /// Wave G4 — resolve with post-mortem fields. Transition
    /// allowed from open OR acked (operator can skip ack for
    /// obvious cases). The post-mortem text is stamped onto
    /// the record and the state flips to `resolved`.
    pub fn resolve_incident(
        &self,
        id: &str,
        by: &str,
        root_cause: Option<String>,
        action_taken: Option<String>,
        preventive: Option<String>,
    ) -> Option<OpenIncident> {
        let mut g = self.inner.write().unwrap();
        let rec = g.open_incidents.get_mut(id)?;
        if rec.state == "resolved" {
            return None;
        }
        rec.state = "resolved".into();
        rec.resolved_by = Some(by.to_string());
        rec.resolved_at_ms = Some(chrono::Utc::now().timestamp_millis());
        rec.root_cause = root_cause;
        rec.action_taken = action_taken;
        rec.preventive = preventive;
        Some(rec.clone())
    }

    /// List incidents sorted newest-first by `opened_at_ms`.
    pub fn list_incidents(&self) -> Vec<OpenIncident> {
        let g = self.inner.read().unwrap();
        let mut out: Vec<_> = g.open_incidents.values().cloned().collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.opened_at_ms));
        out
    }

    pub fn get_incident(&self, id: &str) -> Option<OpenIncident> {
        self.inner
            .read()
            .ok()
            .and_then(|g| g.open_incidents.get(id).cloned())
    }

    /// S5.1 — register the rebalancer config at server boot.
    /// With `None`, `rebalance_recommendations` short-circuits to
    /// an empty list.
    pub fn set_rebalancer_config(&self, cfg: mm_risk::rebalancer::RebalancerConfig) {
        self.inner.write().unwrap().rebalancer_config = Some(cfg);
    }

    /// S5.1 — run the rebalancer over the dashboard-aggregated
    /// venue balances and return recommendations. Groups all
    /// `VenueBalanceSnapshot` rows across symbols by
    /// `(venue, asset)` first (a single (venue, asset) can be
    /// reported by multiple symbol-scoped engines), sums
    /// `available`, then defers to `Rebalancer::recommend`.
    pub fn rebalance_recommendations(&self) -> Vec<mm_risk::rebalancer::RebalanceRecommendation> {
        let inner = self.inner.read().unwrap();
        let Some(cfg) = inner.rebalancer_config.clone() else {
            return Vec::new();
        };
        let mut by_key: HashMap<(String, String), (Decimal, Decimal)> = HashMap::new();
        for snaps in inner.venue_balances.values() {
            for snap in snaps {
                let key = (snap.venue.clone(), snap.asset.clone());
                let entry = by_key.entry(key).or_insert((Decimal::ZERO, Decimal::ZERO));
                entry.0 += snap.available;
                entry.1 += snap.locked;
            }
        }
        drop(inner);
        let balances: Vec<_> = by_key
            .into_iter()
            .map(
                |((venue, asset), (available, locked))| mm_risk::rebalancer::VenueBalance {
                    venue,
                    asset,
                    available,
                    locked,
                },
            )
            .collect();
        mm_risk::rebalancer::Rebalancer::new(cfg).recommend(&balances)
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

    /// I3 — client_ids that currently have a webhook dispatcher
    /// registered. The fan-out loop walks these every tick so we
    /// don't do work for tenants with no endpoint.
    pub fn client_ids_with_webhooks(&self) -> Vec<String> {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .iter()
            .filter_map(|(id, c)| c.webhook_dispatcher.as_ref().map(|_| id.clone()))
            .collect()
    }

    /// I3 — read the last fill timestamp we've already fanned
    /// out for a given tenant.
    pub fn webhook_fill_cursor(&self, client_id: &str) -> Option<chrono::DateTime<Utc>> {
        let inner = self.inner.read().unwrap();
        inner
            .clients
            .get(client_id)
            .and_then(|c| c.webhook_fill_cursor)
    }

    /// I3 — advance the cursor after a successful fan-out batch.
    pub fn set_webhook_fill_cursor(&self, client_id: &str, ts: chrono::DateTime<Utc>) {
        let mut inner = self.inner.write().unwrap();
        if let Some(c) = inner.clients.get_mut(client_id) {
            c.webhook_fill_cursor = Some(ts);
        }
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
    /// 23-UX-1 — enforces `MIN_TIMESERIES_GAP_MS` between
    /// successive samples so the ring holds ~4 hours of history
    /// regardless of engine tick rate.
    pub fn push_pnl_sample(&self, symbol: &str, timestamp_ms: i64, total_pnl: Decimal) {
        let mut inner = self.inner.write().unwrap();
        let ts = inner.pnl_timeseries.entry(symbol.to_string()).or_default();
        if let Some((last_ts, _)) = ts.back() {
            if timestamp_ms - *last_ts < MIN_TIMESERIES_GAP_MS {
                return;
            }
        }
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

    /// UX-2 / 23-UX-1 — push one (timestamp, value) sample into
    /// the spread-bps rolling history. Same `MIN_TIMESERIES_GAP_MS`
    /// downsample gate + 14400-cap FIFO as `push_pnl_sample`.
    pub fn push_spread_sample(&self, symbol: &str, timestamp_ms: i64, spread_bps: Decimal) {
        let mut inner = self.inner.write().unwrap();
        let ts = inner
            .spread_timeseries
            .entry(symbol.to_string())
            .or_default();
        if let Some((last_ts, _)) = ts.back() {
            if timestamp_ms - *last_ts < MIN_TIMESERIES_GAP_MS {
                return;
            }
        }
        ts.push_back((timestamp_ms, spread_bps));
        while ts.len() > MAX_PNL_TIMESERIES {
            ts.pop_front();
        }
    }

    /// UX-2 / 23-UX-1 — push one inventory sample into the
    /// rolling history.
    pub fn push_inventory_sample(&self, symbol: &str, timestamp_ms: i64, inventory: Decimal) {
        let mut inner = self.inner.write().unwrap();
        let ts = inner
            .inventory_timeseries
            .entry(symbol.to_string())
            .or_default();
        if let Some((last_ts, _)) = ts.back() {
            if timestamp_ms - *last_ts < MIN_TIMESERIES_GAP_MS {
                return;
            }
        }
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
mod tests;
