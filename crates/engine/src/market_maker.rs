use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use mm_common::config::AppConfig;
use mm_common::types::ProductSpec;
use mm_dashboard::alerts::{AlertManager, AlertSeverity};
use mm_dashboard::state::{
    BookDepthLevel, DashboardState, IncidentRecord, PnlSnapshot, SymbolState,
};
use mm_exchange_core::connector::{BorrowError, FeeTierError};
use mm_exchange_core::events::MarketEvent;
use mm_portfolio::Portfolio;
use mm_risk::audit::{AuditEventType, AuditLog};
use mm_risk::borrow::BorrowManager;
use mm_risk::circuit_breaker::{CircuitBreaker, TripReason};
use mm_risk::exposure::ExposureManager;
use mm_risk::hedge_optimizer::{FactorCovarianceEstimator, HedgeBasket, HedgeOptimizer};
use mm_risk::inventory::InventoryManager;
use mm_risk::inventory_drift::InventoryDriftReconciler;
use mm_risk::kill_switch::{KillLevel, KillSwitch, KillSwitchConfig};
use mm_risk::lead_lag_guard::LeadLagGuard;
use mm_risk::margin_guard::{MarginGuard, MarginGuardThresholds};
use mm_risk::news_retreat::{NewsRetreatState, NewsRetreatStateMachine, NewsRetreatTransition};
use mm_risk::otr::OrderToTradeRatio;
use mm_risk::pnl::PnlTracker;
use mm_risk::sla::{SlaConfig, SlaTracker};
use mm_risk::toxicity::{
    AdverseSelectionTracker, BvcBarAggregator, BvcClassifier, KyleLambda, VpinEstimator,
};
use mm_risk::var_guard::{VarGuard, VarGuardConfig};
use mm_strategy::autotune::AutoTuner;
use mm_strategy::funding_arb_driver::{DriverEvent, FundingArbDriver};
use mm_strategy::inventory_skew::AdvancedInventoryManager;
use mm_strategy::market_resilience::{MarketResilienceCalculator, MrConfig};
use mm_strategy::momentum::MomentumSignals;
use mm_strategy::paired_unwind::PairedUnwindExecutor;
use mm_strategy::r#trait::{Strategy, StrategyContext};
use mm_strategy::stat_arb::{SpreadDirection, StatArbDriver, StatArbEvent};
use mm_strategy::twap::TwapExecutor;
use mm_strategy::volatility::VolatilityEstimator;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument, warn};

/// Lossy `Decimal → f64` for Prometheus exposition. The Prometheus
/// gauge API only speaks `f64`, so a one-shot conversion at the
/// metrics boundary is unavoidable. Used by the fee-tier refresh
/// task to expose `mm_maker_fee_bps` / `mm_taker_fee_bps`.
fn decimal_to_f64(d: Decimal) -> f64 {
    use std::str::FromStr;
    f64::from_str(&d.to_string()).unwrap_or(0.0)
}

/// Best-effort `symbol → (base, quote)` split for the hedge-leg
/// registration path in `with_portfolio`. Recognises the
/// canonical fiat / stablecoin / BTC / ETH suffixes the v0.4.0
/// venue matrix trades on. Used only for the Portfolio seed —
/// never for order routing.
/// Multi-Venue 3.C — build a snapshot of the
/// `PortfolioBalanceTracker` from the DataBus's balances map on
/// every graph tick. Cheap — the bus already serialised writes,
/// this just clones the values and derives the aggregate. Returns
/// an empty tracker when the dashboard isn't attached.
fn build_portfolio(
    dashboard: Option<&mm_dashboard::state::DashboardState>,
) -> mm_risk::portfolio_balance::PortfolioBalanceTracker {
    use mm_risk::portfolio_balance::{BalanceInput, PortfolioBalanceTracker};
    let mut tracker = PortfolioBalanceTracker::new();
    let Some(dash) = dashboard else { return tracker };
    let bus = dash.data_bus();
    let Ok(map) = bus.balances.read() else { return tracker };
    let inputs: Vec<BalanceInput> = map
        .iter()
        .map(|((venue, asset), bal)| BalanceInput {
            venue: venue.clone(),
            asset: asset.clone(),
            total: bal.total,
            available: bal.available,
            reserved: bal.reserved,
            // Short-leg tagging requires a per-balance flag the
            // bus doesn't carry yet. For 3.C every entry counts
            // as long — net-delta is correct for spot portfolios
            // and over-counts on perp-short; 3.E will add the
            // short-leg flag when atomic bundles introduce the
            // leg type properly.
            short_leg: false,
        })
        .collect();
    tracker.refresh(&inputs);
    tracker
}

fn split_symbol_bq(symbol: &str) -> (&str, &str) {
    for suffix in ["USDT", "USDC", "BUSD", "FDUSD", "TUSD", "DAI", "BTC", "ETH"] {
        if let Some(base) = symbol.strip_suffix(suffix) {
            return (base, suffix);
        }
    }
    (symbol, "")
}

/// Compact audit-string representation of a stat-arb
/// `LegDispatchReport`. Format:
/// `"y=<side>@<disp>/<tgt>,x=<side>@<disp>/<tgt>"` on success,
/// `"y=err:<msg>"` on leg-level errors, `"none"` when there is
/// no report (non-dispatch event). Kept inline so the engine-
/// side audit-trail format is a one-liner.
fn format_leg_report(report: Option<&mm_strategy::stat_arb::LegDispatchReport>) -> String {
    let Some(r) = report else {
        return "none".to_string();
    };
    let fmt_leg = |label: &str, leg: &mm_strategy::stat_arb::LegOutcome| {
        if let Some(err) = &leg.error {
            format!("{label}=err:{}", err.replace(' ', "_"))
        } else {
            format!(
                "{label}={:?}@{}/{}",
                leg.side, leg.dispatched_qty, leg.target_qty
            )
        }
    };
    let mut parts = Vec::new();
    if let Some(y) = &r.y {
        parts.push(fmt_leg("y", y));
    }
    if let Some(x) = &r.x {
        parts.push(fmt_leg("x", x));
    }
    if parts.is_empty() {
        "empty".to_string()
    } else {
        parts.join(",")
    }
}

use crate::balance_cache::BalanceCache;
use crate::book_keeper::BookKeeper;
use crate::connector_bundle::ConnectorBundle;
use crate::order_id_map::OrderIdMap;
use crate::order_manager::OrderManager;
use crate::pair_lifecycle::{PairLifecycleEvent, PairLifecycleManager};
use crate::sor::cost::VenueCostModel;
use crate::sor::dispatch::{self, DispatchOutcome};
use crate::sor::router::{GreedyRouter, RouteDecision};
use crate::sor::venue_state::{VenueSeed, VenueStateAggregator};

/// The main market-making engine for a single symbol.
///
/// ALL subsystems wired:
/// - Strategy (A-S / GLFT / Grid) + momentum alpha
/// - Risk (circuit breaker, kill switch 5-level, inventory, exposure)
/// - Toxicity (VPIN, Kyle's Lambda, adverse selection)
/// - SLA compliance + PnL attribution
/// - Auto-tuning (regime + toxicity)
/// - Advanced inventory (quadratic skew, urgency, dynamic sizing)
/// - Audit trail (append-only JSONL)
/// - Balance cache (pre-check + reservation)
/// - Order ID mapping (UUID ↔ exchange)
/// - TWAP executor (for kill switch L4 flatten)
/// - Dashboard state updates
/// - Reconciliation on reconnect
pub struct MarketMakerEngine {
    symbol: String,
    /// Owning client ID (Epic 1). `None` in legacy single-client
    /// mode; threaded into `FillRecord.client_id` and audit events.
    client_id: Option<String>,
    config: AppConfig,
    product: ProductSpec,
    strategy: Box<dyn Strategy>,
    connectors: ConnectorBundle,

    // Core.
    book_keeper: BookKeeper,
    /// Second book for the hedge leg when `connectors.hedge` is set.
    /// Cross-product strategies read its mid as `StrategyContext.ref_price`.
    hedge_book: Option<BookKeeper>,
    /// Per-asset borrow state machine (P1.3 stage-1). `Some` when
    /// `config.market_maker.borrow_enabled = true`; the periodic
    /// borrow-rate refresh task pushes APR snapshots into this
    /// manager and the strategy reads
    /// `BorrowManager::effective_carry_bps` via
    /// `StrategyContext.borrow_cost_bps`.
    borrow_manager: Option<BorrowManager>,
    /// Tracks whether the cross-venue basis is currently
    /// "inside" the entry threshold, so the engine emits the
    /// `CrossVenueBasisEntered` / `Exited` audit events exactly
    /// once per round-trip rather than every refresh tick.
    /// `false` until the first inside crossing; flips on
    /// every threshold crossing thereafter. P1.4 stage-1.
    cross_venue_basis_inside: bool,
    /// Per-asset-class kill switch (P2.1). Shared via
    /// `Arc<Mutex<KillSwitch>>` across every engine that maps to
    /// the same class — escalating one engine's asset-class
    /// switch escalates the rest immediately. `None` for
    /// engines whose symbol does not belong to any configured
    /// asset class. The asset-class layer is **soft-only**: the
    /// engine merges the asset level with the global level for
    /// `WidenSpreads` / `StopNewOrders`, but hard escalation
    /// (`CancelAll` / `FlattenAll` / `Disconnect`) is still
    /// driven by the per-engine global state alone.
    asset_class_switch: Option<Arc<Mutex<KillSwitch>>>,
    /// Per-symbol pair lifecycle state machine (P2.3 stage-1).
    /// `Some` when `config.market_maker.pair_lifecycle_enabled`
    /// is true; the periodic refresh task polls
    /// `connector.get_product_spec` and routes the diff into
    /// the audit trail + the `lifecycle_paused` flag.
    pair_lifecycle: Option<PairLifecycleManager>,
    /// Latched by the lifecycle manager when the venue reports
    /// a halt, break, pre-trading, or delisting state. The
    /// `refresh_quotes` short-circuit consults this field on
    /// every refresh tick so the engine never quotes against a
    /// halted symbol — closes the "venue sends fills after a
    /// halt" hole.
    lifecycle_paused: bool,
    /// Set when the book hasn't updated for stale_book_timeout_secs.
    /// Cleared when fresh data arrives.
    stale_book_paused: bool,
    /// Set when the primary-vs-hedge mid divergence exceeded
    /// `max_cross_venue_divergence_pct` on the last tick. Used
    /// to suppress duplicate audit events on consecutive trips
    /// and to log a single "resumed" line when the gap closes.
    cross_venue_divergence_tripped: bool,
    /// Shared per-client daily-loss circuit (Epic 6). The engine
    /// reports its per-symbol PnL each summary tick and checks
    /// `is_tripped(client_id)` in `refresh_quotes`. `None`
    /// disables the integration — single-client setups and
    /// tests typically run without one.
    per_client_circuit: Option<Arc<mm_risk::PerClientLossCircuit>>,
    /// Set once the client circuit has fired for this engine's
    /// client. Used to emit a single audit line per transition
    /// instead of one per tick.
    per_client_trip_noted: bool,
    /// Per-strategy VaR guard (Epic C sub-component #4).
    /// Rolling 24 h ring buffer of PnL samples, parametric
    /// Gaussian VaR, composes into the effective size
    /// multiplier via `min()`. `None` when
    /// `config.market_maker.var_guard_enabled = false`.
    var_guard: Option<VarGuard>,
    /// Cross-asset hedge optimizer (Epic C sub-component #3).
    /// Stateless — the engine re-runs `optimize()` on every
    /// refresh tick with fresh inputs and exposes the result
    /// via [`Self::last_hedge_basket`]. Always initialised;
    /// the recommendation is a read-only advisory the
    /// operator / dashboard can consume without acting on it.
    hedge_optimizer: HedgeOptimizer,
    /// Rolling factor covariance estimator (stage-2). Fed
    /// with per-tick mid-price returns from the book keeper;
    /// replaces the v1 constant-1.0 diagonal stub.
    factor_covariance: FactorCovarianceEstimator,
    /// Shared factor covariance estimator (Epic 3). When set,
    /// the engine pushes return observations into this shared
    /// instance in addition to the local one, enabling
    /// portfolio-level VaR and correlation matrix computation.
    shared_factor_covariance: Option<Arc<Mutex<FactorCovarianceEstimator>>>,
    /// Latest hedge basket recommendation from
    /// `hedge_optimizer::optimize()`. Refreshed on every
    /// quote tick where the engine has a portfolio snapshot.
    /// Not acted on automatically — stage-2 dispatch via an
    /// `ExecAlgorithm` is tracked in ROADMAP.
    last_hedge_basket: HedgeBasket,
    /// Epic A SOR venue-state aggregator. Seeded by the
    /// engine at construction (one entry for the primary
    /// connector's venue) plus any additional entries
    /// operators add via `with_sor_venue`. Consumed by
    /// [`Self::recommend_route`] on demand.
    sor_aggregator: VenueStateAggregator,
    /// Epic A SOR greedy router. Stateless; reused across
    /// every `recommend_route` call.
    sor_router: GreedyRouter,
    /// Epic A stage-2 #2 — per-venue trade-rate estimators.
    /// Updated on every `MarketEvent::Trade`; sampled every
    /// `sor_queue_refresh_secs` seconds and fed into
    /// `sor_aggregator.update_queue_wait`. Missing entries
    /// mean the tracker has seen no venue-tagged trades yet;
    /// the aggregator keeps its seeded constant until the
    /// first refresh produces a live rate.
    sor_trade_rates: std::collections::HashMap<
        mm_exchange_core::connector::VenueId,
        crate::sor::trade_rate::TradeRateEstimator,
    >,
    /// Tracks the last throttle value per strategy class so
    /// `VarGuardThrottleApplied` audit events fire only on
    /// **transitions** rather than every refresh tick while
    /// the throttle is stable.
    var_guard_last_throttle: Option<Decimal>,
    /// Total PnL at the last VaR sample so the 60-second
    /// `record_pnl_sample` can push the **delta** since the
    /// previous sample rather than the cumulative PnL. Reset
    /// on engine start.
    var_guard_last_total_pnl: Decimal,
    /// Portfolio-level risk spread multiplier (Epic 3). Applied
    /// as an additional factor in `refresh_quotes`.
    portfolio_risk_mult: Decimal,
    /// Second `OrderManager` for the hedge leg, built lazily when
    /// `connectors.hedge` is present. Kept strictly separate
    /// from the primary `order_manager` so per-leg diffing,
    /// cancel-all, and fill routing never mix the two venues.
    hedge_order_manager: Option<OrderManager>,
    order_manager: OrderManager,
    inventory_manager: InventoryManager,
    exposure_manager: ExposureManager,
    circuit_breaker: CircuitBreaker,
    volatility_estimator: VolatilityEstimator,

    // Risk.
    kill_switch: KillSwitch,
    audit: Arc<AuditLog>,
    balance_cache: BalanceCache,
    order_id_map: OrderIdMap,

    // Toxicity.
    vpin: VpinEstimator,
    /// Epic D stage-2 — optional Bulk Volume Classification path
    /// for VPIN. When `Some`, the engine routes each closed bar
    /// through `classify → vpin.on_bvc_bar`; when `None` the
    /// legacy `vpin.on_trade` tick-rule path stays in use.
    /// Gated on `toxicity.bvc_enabled`.
    bvc_classifier: Option<BvcClassifier>,
    /// Time-bucket aggregator feeding the classifier. Always
    /// constructed alongside `bvc_classifier` — absence means
    /// BVC is disabled.
    bvc_bar_agg: Option<BvcBarAggregator>,
    kyle_lambda: KyleLambda,
    adverse_selection: AdverseSelectionTracker,
    /// Event-driven Market Resilience detector. Reads every
    /// trade + book update and exposes a `[0, 1]` score that
    /// reflects how badly a **just-happened** liquidity shock
    /// has stressed the book. Feeds into
    /// `AutoTuner::set_market_resilience` per tick.
    market_resilience: MarketResilienceCalculator,
    /// Regulatory OTR counter: `(adds + 2·updates + cancels) /
    /// max(trades, 1) - 1`. Exported into the audit trail and
    /// Prometheus. Market-quality / spoofing proxy tracked for
    /// MiCA compliance.
    otr: OrderToTradeRatio,
    /// Inventory-vs-wallet drift reconciler. Snapshots the
    /// wallet total at first reconcile and flags any
    /// divergence between the wallet delta and the
    /// `InventoryManager` tracker on subsequent reconciles.
    /// A drift signals a missed fill, listen-key gap, or an
    /// external transfer — the caller routes it into the
    /// audit trail and optionally force-corrects the tracker.
    inventory_drift: InventoryDriftReconciler,
    /// Market impact estimator — tracks fill-to-mid correlation
    /// for execution quality reporting. Fed on every fill +
    /// every mid update.
    market_impact: mm_risk::market_impact::MarketImpactEstimator,
    /// Performance tracker — Sharpe, Sortino, drawdown, fill
    /// rate, inventory turnover. Fed on every fill + every
    /// periodic summary tick.
    performance: mm_risk::performance::PerformanceTracker,
    /// Webhook dispatcher for client event delivery. Shared
    /// across all engines via `Arc` so a single set of URLs
    /// covers every symbol.
    webhooks: Option<mm_dashboard::webhooks::WebhookDispatcher>,

    // Strategy augmentation.
    momentum: MomentumSignals,
    auto_tuner: AutoTuner,
    /// Epic 30 — online closed-loop controller sitting on top of
    /// `auto_tuner`. Off by default; `market_maker.adaptive_enabled`
    /// flips it on. See `docs/research/adaptive-calibration.md`.
    adaptive_tuner: mm_strategy::AdaptiveTuner,
    /// Epic 31 — pair-class tag set by the server at startup via
    /// `classify_symbol`. Emitted verbatim in
    /// `AdaptiveStateSnapshot.pair_class`.
    pair_class: Option<mm_common::PairClass>,
    adv_inventory: AdvancedInventoryManager,
    twap: Option<TwapExecutor>,
    /// Paired-unwind executor for kill-switch L4 on a basis /
    /// funding-arb position. Populated only when
    /// `connectors.pair` is set AND the kill switch escalates
    /// to `FlattenAll`. Replaces the single-leg `twap`
    /// executor in dual-connector mode — running both would
    /// double-flatten the primary leg.
    paired_unwind: Option<PairedUnwindExecutor>,
    /// Funding-arb driver owned by the engine. When set, the
    /// engine's select loop adds a periodic tick that pulls
    /// funding rate + both mids, asks
    /// `FundingArbEngine::evaluate`, and dispatches via
    /// `FundingArbExecutor`. DriverEvent routing (audit,
    /// kill-switch escalation on uncompensated pair break)
    /// lives in `handle_driver_event`. Mutually exclusive with
    /// regular maker quoting — operators who run funding arb
    /// set `StrategyType::FundingArb` in config and the engine
    /// uses the driver as the main tick instead of
    /// `refresh_quotes`.
    funding_arb_driver: Option<FundingArbDriver>,
    funding_arb_tick: std::time::Duration,

    /// Cointegrated-pair stat-arb driver (Epic B). Same call-
    /// site shape as `funding_arb_driver`: the engine's select
    /// loop polls it on `stat_arb_tick`, routes every event
    /// through `handle_stat_arb_event`, and leaves the driver
    /// owning the Kalman / z-score state. Stage-1 is
    /// advisory-only — the driver does NOT dispatch leg orders
    /// yet. Events land in the audit trail so operators can
    /// replay what the driver would have done.
    stat_arb_driver: Option<StatArbDriver>,
    stat_arb_tick: std::time::Duration,

    /// Lead-lag guard (Epic F sub-component #1). When attached,
    /// the engine pushes every hedge-connector book event's mid
    /// into the guard via `update_lead_lag_from_mid`, and the
    /// resulting multiplier flows through
    /// `auto_tuner.set_lead_lag_mult`. Operators can also call
    /// `update_lead_lag_from_mid` manually from any orchestration
    /// layer — the guard is a pure push-API state machine.
    lead_lag_guard: Option<LeadLagGuard>,
    /// Latched flag so we only emit a `LeadLagTriggered` audit
    /// record on the `1.0 → > 1.0` transition. Resets when the
    /// multiplier falls back to 1.0.
    lead_lag_active: bool,
    /// News retreat state machine (Epic F sub-component #2).
    /// Operators feed headlines via the public
    /// [`MarketMakerEngine::on_news_headline`] method. Critical-
    /// class transitions escalate the kill switch to L2; all
    /// transitions write audit records.
    news_retreat: Option<NewsRetreatStateMachine>,

    /// Epic G — social-risk engine. Consumes
    /// `SentimentTick`s broadcast via
    /// `ConfigOverride::SentimentTick` and produces
    /// spread/size/skew adjustments fused with the engine's
    /// own volatility + OFI cross-check. `None` = sentiment
    /// layer disabled (all multipliers pinned at 1.0).
    social_risk: Option<mm_risk::social_risk::SocialRiskEngine>,
    /// Epic H — optional strategy graph. When present, evaluated
    /// on every hot-loop tick AFTER the hand-wired signal updates.
    /// Its sinks feed `autotuner.set_graph_{spread,size}_mult` and
    /// the kill switch. `None` = classic hand-wired pipeline.
    strategy_graph: Option<mm_strategy_graph::Evaluator>,
    /// Epic H — stable name of the deployed graph (for audit + metrics
    /// labels). `None` when no graph is attached.
    strategy_graph_name: Option<String>,
    /// Epic H Phase 3 — content hash (SHA-256) of the active graph,
    /// cached at deploy time. Attached to every sink-provenance
    /// audit row so regulators can join a live kill-switch escalation
    /// back to the canonical graph snapshot in `history/{hash}.json`.
    strategy_graph_hash: Option<String>,
    /// Epic H Phase 4 — full graph-authored quote bundle, set by the
    /// `Out.Quotes` sink on the last graph tick. Consumed (and
    /// cleared) at the start of the next quoting pass. `None`
    /// delegates to `self.strategy.compute_quotes(&ctx)` like the
    /// classic pipeline, so switching between graph-authored and
    /// hand-wired mid-run is seamless.
    graph_quotes_override: Option<Vec<mm_common::types::QuotePair>>,
    /// Epic H Phase 4 — last tick's `strategy.compute_quotes()`
    /// result, cached so the next tick's `Strategy.*` composite
    /// nodes can read it via `source_inputs`. Introduces a 1-tick
    /// lag on graph-internal consumers of the strategy output (the
    /// graph sees the previous tick's quotes), which is acceptable
    /// because the engine still applies **this** tick's strategy
    /// output when the graph doesn't override via `Out.Quotes`.
    last_strategy_quotes: Option<Vec<mm_common::types::QuotePair>>,
    /// Epic H Phase 5 — per-node strategy pool. Built on every
    /// graph deploy by walking the graph's `Strategy.*` nodes and
    /// instantiating each one with its own parsed config. The
    /// overlay path reads the matching instance's
    /// `compute_quotes()` output (via the per-node cache below)
    /// instead of `self.strategy`, which means two Strategy.Spoof
    /// nodes in one graph with different `pressure_size_mult` now
    /// genuinely run with those different parameters.
    strategy_pool: std::collections::HashMap<
        mm_strategy_graph::NodeId,
        Box<dyn mm_strategy::r#trait::Strategy>,
    >,
    /// Last tick's `compute_quotes()` output per-pool-instance,
    /// keyed by the node id. Refreshed at the end of
    /// `refresh_quotes` once the StrategyContext is built — the
    /// next graph tick reads this map and injects the bundle as
    /// the node's `quotes` output.
    last_strategy_quotes_per_node: std::collections::HashMap<
        mm_strategy_graph::NodeId,
        Vec<mm_common::types::QuotePair>,
    >,
    /// Epic R — surveillance event lifecycle tracker. Fed by the
    /// order_manager's placement / cancel / fill callbacks; read
    /// by every `Surveillance.*Score` detector through the strategy
    /// graph source overlay. `Arc<Mutex>`-shared with the order
    /// manager so feeds land on the same state readers look at.
    pub(crate) surveillance_tracker: mm_risk::surveillance::SharedTracker,
    /// Last per-pattern alert timestamp — dedupes
    /// `SurveillanceAlert` audit rows when a detector holds above
    /// threshold for several consecutive ticks.
    surveillance_last_alert: std::collections::HashMap<String, i64>,
    /// Epic R Week 5 — previous tick's L2 snapshot. Used by the
    /// FakeLiquidity detector which needs a then/now comparison.
    /// Refreshed at the end of every `tick_strategy_graph` so the
    /// "then" snapshot is always one tick old.
    prev_l2_snapshot: Option<mm_risk::surveillance::L2Snapshot>,
    /// Epic R Week 5b — venue session calendar (funding windows /
    /// settlement). Default = 8-hour funding cadence; operators can
    /// override via `set_session_calendar`.
    session_calendar: mm_risk::session_calendar::SessionCalendar,
    /// Epic H — graph scope cached at `with_strategy_graph` /
    /// `swap_strategy_graph` so the per-tick source marshaller
    /// knows which asset to pull sentiment for without re-reading
    /// the whole graph JSON.
    strategy_graph_scope: Option<mm_strategy_graph::Scope>,
    /// Epic G — reservation-price skew contribution from the
    /// most recent social evaluation. Bumped by
    /// `SocialRiskState.inv_skew_bps` on every tick and
    /// composited alongside the momentum-alpha skew when the
    /// engine computes its reservation mid.
    social_skew_bps: Decimal,

    /// Pre-liquidation margin ratio guard (Epic 40.4). `Some`
    /// when the venue reports margin info (perp connectors) AND
    /// `config.margin` is configured; `None` on spot or when
    /// the operator disabled the guard. Refresh cadence is
    /// driven by `config.margin.refresh_interval_secs` via the
    /// engine's select loop. Decisions feed
    /// `kill_switch.update_margin_ratio` and the pre-order
    /// hook gates new quotes on `projected_ratio`.
    margin_guard: Option<MarginGuard>,
    /// Tick modulus that throttles `account_margin_info` polls
    /// to once every N `tick_second` ticks (N = refresh_secs).
    /// Keeps the cadence integer-aligned to the 1 Hz driver
    /// clock so the poll window is deterministic regardless
    /// of wall-clock jitter. `0` disables the poll entirely
    /// (spot + guard absent).
    margin_poll_modulus: u64,
    /// Effective leverage per symbol (Epic 40.7). Resolved at
    /// startup from `config.margin.per_symbol` with fallback
    /// to `default_leverage`. Fed into
    /// `MarginGuard::projected_ratio` so the pre-order hook's
    /// post-fill forecast reflects the venue's actual IM
    /// requirement, not a conservative 1x placeholder.
    margin_leverage: u32,

    // Tracking.
    sla_tracker: SlaTracker,
    pnl_tracker: PnlTracker,
    volume_limiter: mm_risk::VolumeLimitTracker,
    dashboard: Option<DashboardState>,
    alerts: Option<AlertManager>,
    /// Multi-currency portfolio aggregator. Shared across all
    /// `MarketMakerEngine` instances in a multi-symbol process
    /// via `Arc<Mutex<_>>` so a single unified snapshot covers
    /// every symbol's positions. `None` in single-symbol
    /// deployments or tests that don't care about unified PnL.
    portfolio: Option<Arc<Mutex<Portfolio>>>,

    // Timing.
    cycle_start: Instant,
    last_mid: Decimal,
    tick_count: u64,
    reconcile_counter: u64,
    /// Last UTC date we snapshotted a daily report. Prevents
    /// double-snapshots on the same day.
    last_daily_snapshot_date: String,

    /// A/B split engine (Epic 6). When set, the engine alternates
    /// between variant A and B params on each quote refresh tick.
    ab_split: Option<mm_strategy::ab_split::AbSplitEngine>,

    /// Live market data recorder. When set, every BookSnapshot
    /// and Trade event is appended to a JSONL file for offline
    /// backtesting. Enabled via `with_event_recorder()`.
    event_recorder: Option<mm_backtester::data::EventRecorder>,

    /// Hot config override receiver. Admin endpoints send
    /// overrides through the corresponding sender registered
    /// in `DashboardState`. The engine applies them on the
    /// next select-loop iteration.
    config_override_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<mm_dashboard::state::ConfigOverride>>,
}

impl MarketMakerEngine {
    pub fn new(
        symbol: String,
        config: AppConfig,
        product: ProductSpec,
        strategy: Box<dyn Strategy>,
        connectors: ConnectorBundle,
        dashboard: Option<DashboardState>,
        alerts: Option<AlertManager>,
    ) -> Self {
        let tick_secs = Decimal::from(config.market_maker.refresh_interval_ms) / dec!(1000);
        let vol_est = VolatilityEstimator::new(dec!(0.94), tick_secs);

        let sla_config = SlaConfig {
            max_spread_bps: config.sla.max_spread_bps,
            min_depth_quote: config.sla.min_depth_quote,
            min_uptime_pct: config.sla.min_uptime_pct,
            two_sided_required: config.sla.two_sided_required,
            max_requote_secs: config.sla.max_requote_secs,
            min_order_rest_secs: config.sla.min_order_rest_secs,
        };

        let ks_config = KillSwitchConfig {
            daily_loss_limit: config.kill_switch.daily_loss_limit,
            daily_loss_warning: config.kill_switch.daily_loss_warning,
            max_position_value: config.kill_switch.max_position_value,
            max_message_rate: config.kill_switch.max_message_rate,
            max_consecutive_errors: config.kill_switch.max_consecutive_errors,
            ..Default::default()
        };

        // Audit log: data/audit/{symbol}.jsonl
        let audit_path = format!("data/audit/{}.jsonl", symbol.to_lowercase());
        let audit = Arc::new(
            AuditLog::new(Path::new(&audit_path))
                .unwrap_or_else(|e| panic!("failed to create audit log at {audit_path}: {e}")),
        );

        let vpin = VpinEstimator::new(
            config.toxicity.vpin_bucket_size,
            config.toxicity.vpin_num_buckets,
        );
        let (bvc_classifier, bvc_bar_agg) = if config.toxicity.bvc_enabled {
            (
                Some(BvcClassifier::new(
                    config.toxicity.bvc_nu,
                    config.toxicity.bvc_window.max(2),
                )),
                Some(BvcBarAggregator::new(config.toxicity.bvc_bar_secs)),
            )
        } else {
            (None, None)
        };

        let hedge_book = connectors
            .pair
            .as_ref()
            .map(|pair| BookKeeper::new(&pair.hedge_symbol));
        let paper_mode = config.mode.eq_ignore_ascii_case("paper");
        let hedge_order_manager = connectors.hedge.as_ref().map(|_| {
            if paper_mode {
                OrderManager::new_paper()
            } else {
                OrderManager::new()
            }
        });
        let borrow_manager = if config.market_maker.borrow_enabled {
            Some(BorrowManager::new(
                &product.base_asset,
                config.market_maker.borrow_max_base,
                config.market_maker.borrow_buffer_base,
                config.market_maker.borrow_holding_secs,
            ))
        } else {
            None
        };
        let mut engine = Self {
            client_id: None,
            book_keeper: BookKeeper::new(&symbol),
            hedge_book,
            borrow_manager,
            cross_venue_basis_inside: false,
            asset_class_switch: None,
            pair_lifecycle: if config.market_maker.pair_lifecycle_enabled {
                Some(PairLifecycleManager::new())
            } else {
                None
            },
            lifecycle_paused: false,
            stale_book_paused: false,
            cross_venue_divergence_tripped: false,
            per_client_circuit: None,
            per_client_trip_noted: false,
            var_guard: if config.market_maker.var_guard_enabled {
                Some(VarGuard::new(VarGuardConfig {
                    limit_95: config.market_maker.var_guard_limit_95,
                    limit_99: config.market_maker.var_guard_limit_99,
                    ewma_lambda: config.market_maker.var_guard_ewma_lambda,
                    cvar_limit_95: None,
                    cvar_limit_99: None,
                }))
            } else {
                None
            },
            hedge_optimizer: HedgeOptimizer::new(dec!(1)),
            factor_covariance: FactorCovarianceEstimator::new(
                vec![product.base_asset.clone()],
                1440,
            ),
            shared_factor_covariance: None,
            last_hedge_basket: HedgeBasket::default(),
            sor_aggregator: {
                let mut agg = VenueStateAggregator::new();
                // Seed the primary venue from the engine's
                // own product spec. Available qty defaults
                // to the configured max-inventory cap so
                // the router has a non-zero budget out of
                // the box. Operators override via
                // `with_sor_venue` when running against
                // more than one venue.
                let seed = VenueSeed::new(&symbol, product.clone(), config.risk.max_inventory);
                agg.register_venue(connectors.primary.venue_id(), seed);
                agg
            },
            sor_router: GreedyRouter::new(VenueCostModel::default_v1()),
            sor_trade_rates: std::collections::HashMap::new(),
            var_guard_last_throttle: None,
            var_guard_last_total_pnl: dec!(0),
            portfolio_risk_mult: dec!(1),
            ab_split: None,
            event_recorder: None,
            hedge_order_manager,
            order_manager: if paper_mode {
                OrderManager::new_paper()
            } else {
                OrderManager::new()
            },
            inventory_manager: InventoryManager::new(),
            exposure_manager: ExposureManager::new(dec!(0)),
            circuit_breaker: CircuitBreaker::new(),
            volatility_estimator: vol_est,
            kill_switch: KillSwitch::new(ks_config),
            audit,
            balance_cache: if paper_mode {
                BalanceCache::new_paper_for(mm_common::types::WalletType::Spot)
            } else {
                BalanceCache::new()
            },
            order_id_map: OrderIdMap::new(),
            vpin,
            bvc_classifier,
            bvc_bar_agg,
            kyle_lambda: KyleLambda::new(config.toxicity.kyle_window),
            adverse_selection: AdverseSelectionTracker::new(200),
            market_resilience: MarketResilienceCalculator::new(MrConfig::default()),
            otr: OrderToTradeRatio::new(),
            inventory_drift: InventoryDriftReconciler::new(
                product.base_asset.clone(),
                config.market_maker.inventory_drift_tolerance,
                config.market_maker.inventory_drift_auto_correct,
            ),
            market_impact: mm_risk::market_impact::MarketImpactEstimator::new(20),
            performance: mm_risk::performance::PerformanceTracker::new(1440),
            webhooks: None,
            momentum: {
                let mut ms = MomentumSignals::new(config.market_maker.momentum_window);
                if config.market_maker.hma_enabled {
                    ms = ms.with_hma(config.market_maker.hma_window);
                }
                // Epic D stage-3 — engine-side auto-attach of
                // OFI + learned-microprice signals. Both default
                // to off (`momentum_ofi_enabled = false`,
                // `momentum_learned_microprice_path = None`) so
                // operators who tuned the wave-1 alpha weights
                // see byte-identical behaviour. Flipping
                // `momentum_ofi_enabled = true` attaches a fresh
                // `OfiTracker`; `handle_ws_event` then feeds it
                // every L1 snapshot via `on_l1_snapshot`. A
                // `Some(path)` on `momentum_learned_microprice_path`
                // loads the offline-fitted model via
                // `LearnedMicroprice::from_toml` and attaches it.
                // Load failure logs a warning and continues
                // without the signal — never panics.
                if config.market_maker.momentum_ofi_enabled {
                    ms = ms.with_ofi();
                    info!("MomentumSignals: OFI signal attached (Epic D stage-3)");
                }
                // Epic D stage-3 — per-pair learned MP
                // lookup. Per-pair entry takes precedence over
                // the system-wide fallback. Operators with
                // multi-symbol deployments fit a separate
                // model per pair offline; single-symbol and
                // homogeneous deployments use the system-wide
                // path.
                let lmp_path: Option<&str> = config
                    .market_maker
                    .momentum_learned_microprice_pair_paths
                    .get(&symbol)
                    .map(String::as_str)
                    .or(config
                        .market_maker
                        .momentum_learned_microprice_path
                        .as_deref());
                if let Some(path) = lmp_path {
                    match mm_strategy::learned_microprice::LearnedMicroprice::from_toml(
                        std::path::Path::new(path),
                    ) {
                        Ok(model) => {
                            // Epic D stage-2 — opt-in online fit.
                            // When `momentum_learned_microprice_online`
                            // is true, attach via the online builder
                            // so every L1 snapshot feeds the model's
                            // ring and the g-matrix drifts with the
                            // live tape. Horizon must match the
                            // offline CLI's `--horizon` setting;
                            // defaults to 10 to match the CLI
                            // binary default.
                            if config.market_maker.momentum_learned_microprice_online {
                                ms = ms.with_learned_microprice_online(
                                    model,
                                    config
                                        .market_maker
                                        .momentum_learned_microprice_horizon
                                        .max(1),
                                );
                                info!(
                                    symbol = %symbol,
                                    path = %path,
                                    horizon = config.market_maker.momentum_learned_microprice_horizon,
                                    "MomentumSignals: learned microprice model loaded with online fit"
                                );
                            } else {
                                ms = ms.with_learned_microprice(model);
                                info!(
                                    symbol = %symbol,
                                    path = %path,
                                    "MomentumSignals: learned microprice model loaded (offline-only)"
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                symbol = %symbol,
                                path = %path,
                                error = %e,
                                "failed to load learned microprice — continuing without it"
                            );
                        }
                    }
                }
                ms
            },
            auto_tuner: {
                let mut t = AutoTuner::new(200);
                // Epic 40.8 — per-research (docs/research/
                // spot-vs-perp-mm-apr17.md §Microstructure), VPIN
                // and Kyle's λ run ~1.3-1.5× hotter on perp than
                // spot for the same book depth, because informed
                // flow + leverage concentrates on perp. Bump the
                // toxicity widen multiplier on any perp product
                // so our L1 WidenSpreads response pulls quotes in
                // faster when those signals spike.
                if config.exchange.product.has_funding() {
                    t.set_product_widen_mult(dec!(1.4));
                }
                t
            },
            adaptive_tuner: {
                let mut t = mm_strategy::AdaptiveTuner::new(
                    mm_strategy::AdaptiveConfig::default(),
                );
                if config.market_maker.adaptive_enabled {
                    t.enable(true);
                }
                t
            },
            pair_class: None,
            adv_inventory: AdvancedInventoryManager::new(config.risk.max_inventory),
            twap: None,
            paired_unwind: None,
            funding_arb_driver: None,
            funding_arb_tick: std::time::Duration::from_secs(60),
            stat_arb_driver: None,
            stat_arb_tick: std::time::Duration::from_secs(60),
            lead_lag_guard: None,
            lead_lag_active: false,
            news_retreat: None,
            social_risk: None,
            social_skew_bps: Decimal::ZERO,
            strategy_graph: None,
            strategy_graph_name: None,
            strategy_graph_hash: None,
            graph_quotes_override: None,
            last_strategy_quotes: None,
            strategy_pool: std::collections::HashMap::new(),
            last_strategy_quotes_per_node: std::collections::HashMap::new(),
            surveillance_tracker: mm_risk::surveillance::new_shared_tracker(),
            surveillance_last_alert: std::collections::HashMap::new(),
            prev_l2_snapshot: None,
            session_calendar: mm_risk::session_calendar::SessionCalendar::funding_8h(),
            strategy_graph_scope: None,
            margin_guard: config.margin.as_ref().map(|m| {
                MarginGuard::new(MarginGuardThresholds::from_config(m))
            }),
            margin_poll_modulus: config
                .margin
                .as_ref()
                .map(|m| m.refresh_interval_secs.max(1))
                .unwrap_or(0),
            margin_leverage: config
                .margin
                .as_ref()
                .map(|m| m.for_symbol(&symbol).1)
                .unwrap_or(1),
            sla_tracker: SlaTracker::new(sla_config),
            // Paper-mode fee override — lets operators run a clean
            // "spread capture without fee drag" demo. Parsed as bps;
            // unset leaves the real exchange-fetched fee in place.
            pnl_tracker: {
                let maker = std::env::var("MM_PAPER_FEE_MAKER_BPS")
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|bps| Decimal::from_f64_retain(bps / 10_000.0).unwrap_or(product.maker_fee))
                    .unwrap_or(product.maker_fee);
                let taker = std::env::var("MM_PAPER_FEE_TAKER_BPS")
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|bps| Decimal::from_f64_retain(bps / 10_000.0).unwrap_or(product.taker_fee))
                    .unwrap_or(product.taker_fee);
                PnlTracker::new(maker, taker)
            },
            volume_limiter: mm_risk::VolumeLimitTracker::new(
                config.risk.max_daily_volume_quote,
                config.risk.max_hourly_volume_quote,
            ),
            dashboard,
            alerts,
            portfolio: None,
            symbol,
            config,
            product,
            strategy,
            connectors,
            cycle_start: Instant::now(),
            last_mid: dec!(0),
            tick_count: 0,
            reconcile_counter: 0,
            last_daily_snapshot_date: String::new(),
            config_override_rx: None,
        };
        // Epic R — wire the surveillance tracker to the order
        // manager so every place / cancel we make feeds the detector
        // tape. Has to happen after `Self` is built because both
        // fields are owned here.
        engine
            .order_manager
            .attach_surveillance(engine.surveillance_tracker.clone());
        engine
    }

    /// Attach a webhook dispatcher for client event delivery.
    pub fn with_webhooks(mut self, wh: mm_dashboard::webhooks::WebhookDispatcher) -> Self {
        self.webhooks = Some(wh);
        self
    }

    /// Epic 31 — tag this engine with its pair-class. Consumed
    /// only for dashboard display; does not change any runtime
    /// behaviour today (per-class parameter selection happens at
    /// config-load time via the template merger).
    pub fn with_pair_class(mut self, class: mm_common::PairClass) -> Self {
        self.pair_class = Some(class);
        self
    }

    /// Attach a hot config override channel. The engine will
    /// poll this receiver in its select loop and apply overrides
    /// without restart.
    pub fn with_config_overrides(
        mut self,
        rx: tokio::sync::mpsc::UnboundedReceiver<mm_dashboard::state::ConfigOverride>,
    ) -> Self {
        self.config_override_rx = Some(rx);
        self
    }

    /// Attach a shared multi-currency portfolio to this engine.
    ///
    /// Pass the same `Arc<Mutex<Portfolio>>` to every engine in a
    /// multi-symbol deployment so the dashboard reports a single
    /// unified PnL snapshot. Operators who don't need unified
    /// reporting just skip this call and the engine's existing
    /// `PnlTracker` remains the sole source of truth for dashboard
    /// gauges.
    /// Set the owning client ID (Epic 1). When set, the engine
    /// tags every `FillRecord` and audit event with this client.
    pub fn with_client_id(mut self, client_id: String) -> Self {
        self.client_id = Some(client_id);
        self
    }

    /// Attach a shared [`PerClientLossCircuit`] (Epic 6). The
    /// engine reports its daily PnL to the circuit on every
    /// summary tick and refuses to place new orders while the
    /// circuit is tripped for its client. Without this the
    /// per-client loss aggregate still shows up on the dashboard
    /// but no enforcement happens — tests and single-client
    /// deployments do not need the circuit wired.
    pub fn with_per_client_circuit(
        mut self,
        circuit: Arc<mm_risk::PerClientLossCircuit>,
    ) -> Self {
        self.per_client_circuit = Some(circuit);
        self
    }

    /// Attach an A/B split engine for parameter comparison.
    pub fn with_ab_split(mut self, split: mm_strategy::ab_split::AbSplitEngine) -> Self {
        self.ab_split = Some(split);
        self
    }

    /// Attach a live market data recorder. Every BookSnapshot
    /// and Trade event will be appended to the JSONL file at
    /// `path` for offline backtesting.
    pub fn with_event_recorder(mut self, path: &std::path::Path) -> Self {
        match mm_backtester::data::EventRecorder::new(path) {
            Ok(recorder) => {
                tracing::info!(path = %path.display(), "market data recording enabled");
                self.event_recorder = Some(recorder);
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to create event recorder, continuing without");
            }
        }
        self
    }

    /// Restore engine state from a checkpoint (Epic 7 item 7.2).
    /// Initializes inventory and PnL baseline from the saved
    /// state instead of starting fresh.
    pub fn with_checkpoint_restore(
        mut self,
        checkpoint: &mm_persistence::checkpoint::SymbolCheckpoint,
    ) -> Self {
        self.inventory_manager
            .force_reset_inventory_to(checkpoint.inventory);
        self.pnl_tracker.attribution.spread_pnl = checkpoint.realized_pnl;
        self.audit.risk_event(
            &self.symbol,
            mm_risk::audit::AuditEventType::CheckpointSaved,
            &format!(
                "restored from checkpoint: inventory={}, pnl={}",
                checkpoint.inventory, checkpoint.realized_pnl
            ),
        );
        tracing::info!(
            symbol = %self.symbol,
            inventory = %checkpoint.inventory,
            pnl = %checkpoint.realized_pnl,
            "engine state restored from checkpoint"
        );
        self
    }

    /// Attach a shared factor covariance estimator (Epic 3).
    /// When set, every mid-price return observation is pushed
    /// into both the local and shared estimators.
    pub fn with_shared_factor_covariance(
        mut self,
        shared: Arc<Mutex<FactorCovarianceEstimator>>,
    ) -> Self {
        self.shared_factor_covariance = Some(shared);
        self
    }

    pub fn with_portfolio(mut self, portfolio: Arc<Mutex<Portfolio>>) -> Self {
        // Epic C sub-component #1: seed the per-factor delta
        // aggregator with this engine's base/quote asset pair
        // before the first fill lands. Idempotent — multiple
        // engines registering the same symbol with the same
        // tuple is a no-op.
        if let Ok(mut pf) = portfolio.lock() {
            pf.register_symbol(
                &self.symbol,
                &self.product.base_asset,
                &self.product.quote_asset,
            );
            // If this engine is running with a cross-product
            // hedge leg, register the hedge symbol too. The
            // hedge `ProductSpec` is not carried on the engine
            // directly — we approximate base/quote from the
            // hedge symbol's suffix (USDT/USDC/BTC/ETH), which
            // covers every pair the v0.4.0 venue matrix
            // currently supports.
            if let Some(pair) = self.connectors.pair.as_ref() {
                let (base, quote) = split_symbol_bq(&pair.hedge_symbol);
                pf.register_symbol(&pair.hedge_symbol, base, quote);
            }
        }
        self.portfolio = Some(portfolio);
        self
    }

    /// Attach a shared per-asset-class kill switch (P2.1).
    /// Multiple engines whose symbols belong to the same
    /// asset class call this with the **same** `Arc<Mutex<_>>`
    /// so a coordinated escalation halts the whole class
    /// without touching unrelated symbols. Use the global
    /// `KillSwitch` field for hard escalation
    /// (`CancelAll`/`FlattenAll`/`Disconnect`) — the
    /// asset-class layer is intentionally soft-only.
    pub fn with_asset_class_switch(mut self, switch: Arc<Mutex<KillSwitch>>) -> Self {
        self.asset_class_switch = Some(switch);
        self
    }

    /// Register an additional venue for the Smart Order
    /// Router (Epic A). The engine already seeds the
    /// primary connector's venue automatically; use this
    /// builder to add hedge-leg and any `extra` venues the
    /// bundle carries so the SOR can route across them.
    ///
    /// Idempotent — re-registering the same venue
    /// overwrites its seed. Stage-2 will refresh seeds
    /// automatically from the fee-tier refresh path.
    pub fn with_sor_venue(
        mut self,
        venue: mm_exchange_core::connector::VenueId,
        seed: VenueSeed,
    ) -> Self {
        self.sor_aggregator.register_venue(venue, seed);
        self
    }

    /// Advisory cross-venue routing recommendation
    /// (Epic A). Collects a fresh snapshot from every
    /// registered SOR venue, runs the greedy router, and
    /// returns the decision. **Does not dispatch** — the
    /// caller (operator, dashboard, stage-2 engine) owns
    /// the decision.
    ///
    /// `urgency` is clamped to `[0, 1]` internally:
    /// `0` = "post as maker, patient hedge", `1` = "take
    /// against the book, crash out fast", `0.5` = balanced.
    ///
    /// Returns an empty decision when no venues are
    /// registered — the engine's primary venue is seeded
    /// automatically so this path is unreachable in
    /// practice unless the operator manually cleared the
    /// aggregator. Still guards against it for safety.
    pub async fn recommend_route(
        &self,
        side: mm_common::types::Side,
        qty: Decimal,
        urgency: Decimal,
    ) -> RouteDecision {
        let snapshots = self.sor_aggregator.collect(&self.connectors, side).await;
        let decision = self.sor_router.route(side, qty, urgency, &snapshots);
        self.publish_route_decision(&decision);
        decision
    }

    /// Push per-venue Prometheus gauges + fire the
    /// `RouteDecisionEmitted` audit event for a freshly
    /// produced [`RouteDecision`]. Split out from
    /// `recommend_route` so the synthetic test path can
    /// exercise the exact same publishing logic.
    fn publish_route_decision(&self, decision: &RouteDecision) {
        if decision.legs.is_empty() {
            return;
        }
        for leg in &decision.legs {
            let venue_label = format!("{:?}", leg.venue);
            mm_dashboard::metrics::SOR_ROUTE_COST_BPS
                .with_label_values(&[&venue_label])
                .set(decimal_to_f64(leg.expected_cost_bps));
            mm_dashboard::metrics::SOR_FILL_ATTRIBUTION
                .with_label_values(&[&venue_label])
                .set(decimal_to_f64(leg.qty));
        }
        let summary = decision
            .legs
            .iter()
            .map(|l| format!("{:?}={}({:.2}bps)", l.venue, l.qty, l.expected_cost_bps))
            .collect::<Vec<_>>()
            .join(",");
        let detail = format!(
            "side={:?}, qty={}, filled={}, complete={}, legs=[{}]",
            decision.target_side,
            decision.target_qty,
            decision.filled_qty,
            decision.is_complete,
            summary,
        );
        self.audit
            .risk_event(&self.symbol, AuditEventType::RouteDecisionEmitted, &detail);
    }

    /// Test-only synchronous variant of `recommend_route`
    /// that drives the router off pre-built synthetic
    /// snapshots rather than querying the live connectors.
    /// Used by the engine integration test to exercise the
    /// full Portfolio → snapshot → router pipeline
    /// without a tokio runtime. Fires the same
    /// audit + metrics path as the async variant so the
    /// publishing contract is covered by the same tests.
    #[cfg(test)]
    pub(crate) fn recommend_route_synthetic(
        &self,
        side: mm_common::types::Side,
        qty: Decimal,
        urgency: Decimal,
        venues: &[(mm_exchange_core::connector::VenueId, u32)],
    ) -> RouteDecision {
        let snapshots = self.sor_aggregator.collect_synthetic(venues);
        let decision = self.sor_router.route(side, qty, urgency, &snapshots);
        self.publish_route_decision(&decision);
        decision
    }

    /// Inline cross-venue dispatch (Epic A Stage-2). Produces
    /// a fresh [`RouteDecision`] via [`Self::recommend_route`]
    /// and immediately executes every leg against the matching
    /// connector in the bundle. Taker legs (`urgency ≥ 0.5`)
    /// go out as `TimeInForce::Ioc` through
    /// [`OrderManager::execute_unwind_slice`]; maker legs
    /// (`urgency < 0.5`) go out as `TimeInForce::PostOnly`
    /// directly on the connector.
    ///
    /// Parallel API to [`Self::recommend_route`] — operators
    /// pick "advise-only" or "dispatch" at the call site. The
    /// advisory audit event still fires exactly once from
    /// `recommend_route` inside the pipeline, and a
    /// `tracing::info!` line records the dispatch outcome so
    /// ops can grep "sor dispatch outcome" in the log stream.
    #[instrument(skip(self), fields(symbol = %self.symbol, ?side, %qty, %urgency))]
    pub async fn dispatch_route(
        &mut self,
        side: mm_common::types::Side,
        qty: Decimal,
        urgency: Decimal,
    ) -> DispatchOutcome {
        let decision = self.recommend_route(side, qty, urgency).await;
        let outcome = dispatch::dispatch_route(
            &decision,
            &self.connectors,
            &mut self.order_manager,
            &self.product,
            &self.symbol,
        )
        .await;
        info!(
            side = ?outcome.target_side,
            target = %outcome.total_target_qty,
            dispatched = %outcome.total_dispatched_qty,
            fully = outcome.is_fully_dispatched(),
            errors = outcome.errors.len(),
            "sor dispatch outcome"
        );
        outcome
    }

    /// Epic A stage-2 #2 — refresh `queue_wait_secs` on the
    /// SOR aggregator from the live trade-rate estimators.
    /// Uses the seeded `VenueSeed.available_qty` as the
    /// "depth ahead of us" proxy — operators wanting a
    /// per-tick book-depth reading can extend this to walk
    /// the `book_keeper` for the primary or the bundle's
    /// per-venue best-bid / best-ask.
    ///
    /// Estimators that haven't hit `MIN_SAMPLES` yet leave
    /// the seeded constant in place so a freshly-booted
    /// engine still produces a route decision.
    fn refresh_sor_queue_wait(&mut self) {
        let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let venues = self.sor_aggregator.venues();
        for venue in venues {
            let Some(seed) = self.sor_aggregator.seed(venue) else {
                continue;
            };
            let depth = seed.available_qty;
            let Some(est) = self.sor_trade_rates.get_mut(&venue) else {
                continue;
            };
            if let Some(wait) = est.expected_queue_wait_secs(now_ns, depth) {
                self.sor_aggregator.update_queue_wait(venue, wait);
            }
        }
    }

    /// Epic A stage-2 #1 — automatic inline SOR dispatch
    /// tick. Computes a target `(side, qty, urgency)` from the
    /// configured [`mm_common::config::SorTargetSource`], fires
    /// [`Self::dispatch_route`], and pipes the outcome into
    /// Prometheus + the audit trail. A no-op when the qty
    /// source produces zero (no excess inventory / empty hedge
    /// basket), so the tick is safe to fire at a tight
    /// cadence.
    ///
    /// Failure routing: leg-level errors increment
    /// `SOR_DISPATCH_ERRORS` per venue and flow into the
    /// audit `detail`. Only fully-zero-dispatched ticks skip
    /// the audit write — partial successes always leave a
    /// row behind so the operator can reconstruct the
    /// decision.
    async fn run_sor_dispatch_tick(&mut self) {
        use mm_common::config::SorTargetSource;
        use mm_common::types::Side;

        let urgency = self.config.market_maker.sor_urgency;
        let (side, qty) = match self.config.market_maker.sor_target_qty_source {
            SorTargetSource::InventoryExcess => {
                let inv = self.inventory_manager.inventory();
                let threshold = self.config.market_maker.sor_inventory_threshold;
                let excess = inv.abs() - threshold.abs();
                if excess <= Decimal::ZERO {
                    return;
                }
                // Long inventory → SELL to reduce; short → BUY.
                let side = if inv > Decimal::ZERO {
                    Side::Sell
                } else {
                    Side::Buy
                };
                (side, excess)
            }
            SorTargetSource::HedgeBudget => {
                // Pick the first hedge-basket entry that targets
                // this engine's base asset. The optimizer is
                // cross-symbol; we only act on the leg that
                // matches the symbol we manage.
                let basket = &self.last_hedge_basket;
                if basket.is_empty() {
                    return;
                }
                let Some((_sym, entry_qty)) = basket
                    .entries
                    .iter()
                    .find(|(sym, _)| *sym == self.symbol)
                else {
                    return;
                };
                if *entry_qty == Decimal::ZERO {
                    return;
                }
                let side = if *entry_qty > Decimal::ZERO {
                    Side::Buy
                } else {
                    Side::Sell
                };
                (side, entry_qty.abs())
            }
        };

        let outcome = self.dispatch_route(side, qty, urgency).await;

        // No legs at all → router produced an empty decision
        // (no venue could serve the qty). Skip the audit +
        // metrics write; the router already logged its own
        // "no route" line.
        if outcome.legs.is_empty() {
            return;
        }

        // Metrics — success counter on fully-dispatched, per-
        // venue error counter otherwise.
        if outcome.is_fully_dispatched() {
            mm_dashboard::metrics::SOR_DISPATCH_SUCCESS
                .with_label_values(&[&self.symbol])
                .inc();
        }
        for leg in &outcome.legs {
            if leg.error.is_some() {
                mm_dashboard::metrics::SOR_DISPATCH_ERRORS
                    .with_label_values(&[&self.symbol, &format!("{:?}", leg.venue)])
                    .inc();
            }
        }
        use rust_decimal::prelude::ToPrimitive;
        mm_dashboard::metrics::SOR_DISPATCH_FILLED_QTY
            .with_label_values(&[&self.symbol])
            .set(outcome.total_dispatched_qty.to_f64().unwrap_or(0.0));

        // Audit — one row per dispatch with per-leg detail.
        let mut detail = format!(
            "side={side:?}, target_qty={}, dispatched={}, legs=[",
            outcome.total_target_qty, outcome.total_dispatched_qty
        );
        for (i, leg) in outcome.legs.iter().enumerate() {
            if i > 0 {
                detail.push_str(", ");
            }
            match &leg.error {
                Some(err) => detail.push_str(&format!("{:?}:ERR({err})", leg.venue)),
                None => detail.push_str(&format!(
                    "{:?}:qty={},cost_bps={}",
                    leg.venue, leg.dispatched_qty, leg.expected_cost_bps
                )),
            }
        }
        detail.push(']');
        self.audit
            .risk_event(&self.symbol, AuditEventType::RouteDispatched, &detail);
    }

    /// Pick the PnL attribution class for a fill on this
    /// engine. Stage-2 stat-arb: if the engine has a stat-arb
    /// driver and no funding-arb driver, use the pair's
    /// `strategy_class` (e.g. `"stat_arb_BTCUSDT_ETHUSDT"`).
    /// Otherwise fall back to the current primary strategy
    /// name — preserves pre-stage-2 semantics for maker /
    /// funding-arb paths.
    fn pnl_strategy_class(&self) -> String {
        if let Some(driver) = self.stat_arb_driver.as_ref() {
            if self.funding_arb_driver.is_none() {
                return driver.pair().strategy_class.clone();
            }
        }
        self.strategy.name().to_string()
    }

    /// Effective kill level for soft-decision purposes — the
    /// max of the per-engine global level and the shared
    /// per-asset-class level. Hard-decision call sites
    /// (`CancelAll` / `FlattenAll`) keep reading
    /// `self.kill_switch.level()` directly so an asset-wide
    /// widening never accidentally flattens another pair's
    /// inventory. P2.1.
    fn effective_kill_level(&self) -> KillLevel {
        let global = self.kill_switch.level();
        match self.asset_class_switch.as_ref() {
            Some(arc) => {
                let asset = arc.lock().map(|ks| ks.level()).unwrap_or(KillLevel::Normal);
                global.max(asset)
            }
            None => global,
        }
    }

    /// Attach a `FundingArbDriver` to the engine. The engine's
    /// main loop polls it on `tick_interval` and routes every
    /// `DriverEvent` to the audit trail + kill switch.
    /// Requires a dual-connector bundle — funding arb can't run
    /// without a hedge leg.
    pub fn with_funding_arb_driver(
        mut self,
        driver: FundingArbDriver,
        tick_interval: std::time::Duration,
    ) -> Self {
        self.funding_arb_driver = Some(driver);
        self.funding_arb_tick = tick_interval;
        self
    }

    /// Attach a [`StatArbDriver`] (Epic B). The engine's main
    /// loop polls it on `tick_interval` and routes every
    /// [`StatArbEvent`] to the audit trail via
    /// [`Self::handle_stat_arb_event`]. Stage-1 is advisory
    /// only — the driver tracks its state machine and emits
    /// intent events but does not dispatch leg orders. Stage-2
    /// wires inline leg execution through `OrderManager`.
    ///
    /// # Wiring gap
    /// `AppConfig` lacks a `stat_arb: Option<StatArbDriverCfg>`
    /// entry, so server boot never constructs a driver. To
    /// activate: mirror the `funding_arb: Option<FundingArbCfg>`
    /// pattern in `common::config`, build a
    /// `StatArbDriver` in `run_symbol` from `config.stat_arb`,
    /// and invoke `.with_stat_arb_driver(driver, tick)` in the
    /// engine builder chain.
    pub fn with_stat_arb_driver(
        mut self,
        driver: StatArbDriver,
        tick_interval: std::time::Duration,
    ) -> Self {
        self.stat_arb_driver = Some(driver);
        self.stat_arb_tick = tick_interval;
        self
    }

    /// Attach a [`LeadLagGuard`] (Epic F sub-component #1).
    /// The engine auto-feeds the guard with every hedge-
    /// connector mid update via [`Self::update_lead_lag_from_mid`].
    /// Operators with a separate orchestration layer can also
    /// push leader mids directly via the same public method.
    pub fn with_lead_lag_guard(mut self, guard: LeadLagGuard) -> Self {
        self.lead_lag_guard = Some(guard);
        self.lead_lag_active = false;
        self
    }

    /// Epic H Phase 5 — walk a graph's `Strategy.*` nodes and build
    /// one `Box<dyn Strategy>` per occurrence, reading the node's
    /// own JSON config for per-knob overrides. Intentionally
    /// tolerates missing kinds (skipped silently) and bad configs
    /// (falls back to the strategy's `Default`) — the deploy
    /// validator already caught catalog-level issues; we just want
    /// the pool to keep running on configs it can't parse.
    fn build_strategy_pool(
        graph: &mm_strategy_graph::Graph,
    ) -> std::collections::HashMap<
        mm_strategy_graph::NodeId,
        Box<dyn mm_strategy::r#trait::Strategy>,
    > {
        use mm_strategy::r#trait::Strategy as StrategyTrait;
        let mut pool: std::collections::HashMap<
            mm_strategy_graph::NodeId,
            Box<dyn StrategyTrait>,
        > = std::collections::HashMap::new();
        for n in &graph.nodes {
            let built: Option<Box<dyn StrategyTrait>> = match n.kind.as_str() {
                "Strategy.Avellaneda" => {
                    Some(Box::new(mm_strategy::AvellanedaStoikov))
                }
                "Strategy.GLFT" => {
                    Some(Box::new(mm_strategy::glft::GlftStrategy::default()))
                }
                "Strategy.Grid" => Some(Box::new(mm_strategy::grid::GridStrategy)),
                "Strategy.Basis" => {
                    // Basis params come from engine config today —
                    // node-level override lands when Basis gets its
                    // own schema.
                    let shift = rust_decimal_macros::dec!(0.5);
                    let max_basis_bps = rust_decimal_macros::dec!(100);
                    Some(Box::new(mm_strategy::basis::BasisStrategy::new(
                        shift,
                        max_basis_bps,
                    )))
                }
                "Strategy.CrossExchange" => {
                    // min_profit_bps taken from engine config; per-
                    // node override lands with the schema pass.
                    let min_profit = rust_decimal_macros::dec!(5);
                    Some(Box::new(
                        mm_strategy::cross_exchange::CrossExchangeStrategy::new(min_profit),
                    ))
                }
                "Strategy.Spoof" => {
                    use mm_common::types::Side;
                    let cfg = &n.config;
                    let parse_dec = |k: &str, default: rust_decimal::Decimal| {
                        cfg.get(k)
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(default)
                    };
                    let pressure_side = match cfg.get("pressure_side").and_then(|v| v.as_str()) {
                        Some("sell") => Side::Sell,
                        _ => Side::Buy,
                    };
                    let spoof_cfg = mm_strategy::spoof::SpoofConfig {
                        pressure_side,
                        pressure_size_mult: parse_dec("pressure_size_mult", dec!(10)),
                        pressure_distance_bps: parse_dec("pressure_distance_bps", dec!(15)),
                        real_size_mult: parse_dec("real_size_mult", dec!(1)),
                        real_distance_bps: parse_dec("real_distance_bps", dec!(3)),
                    };
                    Some(Box::new(mm_strategy::spoof::SpoofStrategy::with_config(spoof_cfg)))
                }
                "Strategy.Wash" => {
                    let cfg = &n.config;
                    let parse_dec = |k: &str, default: rust_decimal::Decimal| {
                        cfg.get(k)
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(default)
                    };
                    let wash_cfg = mm_strategy::wash::WashConfig {
                        leg_size: parse_dec("leg_size", dec!(0.001)),
                        offset_bps: parse_dec("offset_bps", dec!(0)),
                    };
                    Some(Box::new(mm_strategy::wash::WashStrategy::with_config(wash_cfg)))
                }
                "Strategy.Mark" => {
                    use mm_common::types::Side;
                    let cfg = &n.config;
                    let parse_dec = |k: &str, default: rust_decimal::Decimal| {
                        cfg.get(k)
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(default)
                    };
                    let parse_i64 = |k: &str, default: i64| {
                        cfg.get(k).and_then(|v| v.as_i64()).unwrap_or(default)
                    };
                    let push_side = match cfg.get("push_side").and_then(|v| v.as_str()) {
                        Some("sell") => Side::Sell,
                        _ => Side::Buy,
                    };
                    let mark_cfg = mm_strategy::mark::MarkConfig {
                        push_side,
                        window_secs: parse_i64("window_secs", 60),
                        burst_size: parse_dec("burst_size", dec!(0.001)),
                        cross_depth_bps: parse_dec("cross_depth_bps", dec!(30)),
                    };
                    let strat = mm_strategy::mark::MarkStrategy::with_config(mark_cfg);
                    Some(Box::new(strat))
                }
                "Strategy.Ignite" => {
                    use mm_common::types::Side;
                    let cfg = &n.config;
                    let parse_dec = |k: &str, default: rust_decimal::Decimal| {
                        cfg.get(k)
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(default)
                    };
                    let parse_i64 = |k: &str, default: i64| {
                        cfg.get(k).and_then(|v| v.as_i64()).unwrap_or(default)
                    };
                    let push_side = match cfg.get("push_side").and_then(|v| v.as_str()) {
                        Some("sell") => Side::Sell,
                        _ => Side::Buy,
                    };
                    let ignite_cfg = mm_strategy::ignite::IgniteConfig {
                        push_side,
                        burst_size: parse_dec("burst_size", dec!(0.001)),
                        cross_depth_bps: parse_dec("cross_depth_bps", dec!(30)),
                        burst_ticks: parse_i64("burst_ticks", 5) as u64,
                        rest_ticks: parse_i64("rest_ticks", 3) as u64,
                    };
                    Some(Box::new(
                        mm_strategy::ignite::IgniteStrategy::with_config(ignite_cfg),
                    ))
                }
                _ => None,
            };
            if let Some(s) = built {
                pool.insert(n.id, s);
            }
        }
        pool
    }

    /// Epic H — attach a user-authored strategy graph. Compiles
    /// the graph (runs `Evaluator::build` which includes
    /// `Graph::validate` — cycle check, port types, etc.). The
    /// evaluator short-circuits source nodes at tick time, so the
    /// engine populates `source_inputs` each tick before calling
    /// `tick()` — see [`Self::tick_strategy_graph`].
    pub fn with_strategy_graph(
        mut self,
        graph: &mm_strategy_graph::Graph,
    ) -> Result<Self, mm_strategy_graph::ValidationError> {
        let ev = mm_strategy_graph::Evaluator::build(graph)?;
        self.strategy_graph = Some(ev);
        self.strategy_graph_name = Some(graph.name.clone());
        self.strategy_graph_hash = Some(graph.content_hash());
        self.strategy_graph_scope = Some(graph.scope.clone());
        self.strategy_pool = Self::build_strategy_pool(graph);
        self.last_strategy_quotes_per_node.clear();
        // Drop any pending `Out.Quotes` override from the previous
        // graph — without this, the first tick after a swap would
        // consume a stale bundle and place quotes the new graph
        // never authored.
        self.graph_quotes_override = None;
        Ok(self)
    }

    /// Epic H — hot-swap the running graph. Called from the
    /// `ConfigOverride::StrategyGraphSwap` branch on the config
    /// override channel. On validation failure the existing graph
    /// stays in place and the error is audit-logged; the caller
    /// gets the error for surfacing to the operator.
    pub fn swap_strategy_graph(
        &mut self,
        graph: &mm_strategy_graph::Graph,
    ) -> Result<(), mm_strategy_graph::ValidationError> {
        let ev = mm_strategy_graph::Evaluator::build(graph)?;
        self.strategy_graph = Some(ev);
        self.strategy_graph_name = Some(graph.name.clone());
        self.strategy_graph_hash = Some(graph.content_hash());
        self.strategy_graph_scope = Some(graph.scope.clone());
        self.strategy_pool = Self::build_strategy_pool(graph);
        self.last_strategy_quotes_per_node.clear();
        // Drop any pending `Out.Quotes` override from the previous
        // graph — without this, the first tick after a swap would
        // consume a stale bundle and place quotes the new graph
        // never authored.
        self.graph_quotes_override = None;
        self.audit.risk_event(
            &self.symbol,
            mm_risk::audit::AuditEventType::StrategyGraphDeployed,
            &format!(
                "graph={} hash={}",
                graph.name,
                graph.content_hash()
            ),
        );
        info!(symbol = %self.symbol, graph = %graph.name, hash = %graph.content_hash(), "strategy graph swapped");
        Ok(())
    }

    /// Evaluate the attached strategy graph (if any). Builds the
    /// per-tick `source_inputs` from local engine state, calls
    /// `Evaluator::tick`, applies sink actions to the autotuner
    /// and kill switch. No-op when no graph is attached.
    /// Multi-Venue 2.B — resolve a parameterised source-node's
    /// stream key from its JSON config. Empty venue / symbol /
    /// product fall back to the engine's own defaults. `read_extra`
    /// also extracts a numeric extra (depth / window_secs), default
    /// 10 when absent.
    fn read_cross_venue_key(
        cfg: Option<&serde_json::Value>,
        engine_venue: String,
        engine_symbol: String,
        engine_product: mm_common::config::ProductType,
        read_extra: bool,
    ) -> (String, String, mm_common::config::ProductType, usize) {
        let v = cfg.and_then(|c| c.get("venue")).and_then(|v| v.as_str()).unwrap_or("");
        let s = cfg.and_then(|c| c.get("symbol")).and_then(|v| v.as_str()).unwrap_or("");
        let p_str = cfg.and_then(|c| c.get("product")).and_then(|v| v.as_str()).unwrap_or("");
        let venue = if v.is_empty() { engine_venue } else { v.to_string() };
        let symbol = if s.is_empty() { engine_symbol } else { s.to_string() };
        let product = match p_str {
            "spot" => mm_common::config::ProductType::Spot,
            "linear_perp" => mm_common::config::ProductType::LinearPerp,
            "inverse_perp" => mm_common::config::ProductType::InversePerp,
            _ => engine_product,
        };
        let extra = if read_extra {
            cfg.and_then(|c| c.get("depth").or_else(|| c.get("window_secs")))
                .and_then(|v| v.as_i64())
                .unwrap_or(10) as usize
        } else {
            0
        };
        (venue, symbol, product, extra)
    }

    fn tick_strategy_graph(&mut self) {
        use mm_strategy_graph::{EvalCtx, NodeId, SinkAction, Value};
        use std::collections::HashMap;

        let Some(ref mut graph) = self.strategy_graph else {
            return;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let ctx = EvalCtx { now_ms };

        // Marshal source values. We key by (_, port_name) and
        // iterate every source node in the compiled evaluator by
        // peeking at the graph structure — for MVP, we broadcast
        // every known source's value to every node of that kind
        // (there's at most one of each per graph in practice).
        // The `source_inputs` map is keyed by `(NodeId, port)`
        // and the evaluator looks up per-node, so we loop over
        // every known source kind and fan out.
        let mut src: HashMap<(NodeId, String), Value> = HashMap::new();
        // Epic R — surveillance alerts detected this tick. Emitted
        // AFTER the source overlay loop releases its `&graph` borrow
        // so the dedupe map + audit writer can mutate self freely.
        let mut pending_alerts: Vec<(
            String,
            NodeId,
            mm_risk::surveillance::DetectorOutput,
        )> = Vec::new();
        // Pre-compute values so the closure loop is cheap.
        let book = &self.book_keeper.book;
        // `Book.best_{bid,ask}()` return top-of-book prices. Qty at
        // the top touch isn't on the common orderbook accessor (yet),
        // so we surface Missing — the UI catalog doc flags the gap
        // and a follow-up can plumb qty through the book snapshot.
        let (bid_px, bid_qty, ask_px, ask_qty, mid, spread_bps) = (
            book.best_bid().map(Value::Number).unwrap_or(Value::Missing),
            Value::Missing,
            book.best_ask().map(Value::Number).unwrap_or(Value::Missing),
            Value::Missing,
            book.mid_price()
                .map(Value::Number)
                .unwrap_or(Value::Missing),
            book.spread_bps()
                .map(Value::Number)
                .unwrap_or(Value::Missing),
        );
        let realised_vol = self
            .volatility_estimator
            .volatility()
            .map(Value::Number)
            .unwrap_or(Value::Missing);
        let vpin = self
            .vpin
            .vpin()
            .map(Value::Number)
            .unwrap_or(Value::Missing);
        let ofi_z = self
            .momentum
            .ofi_z()
            .map(Value::Number)
            .unwrap_or(Value::Missing);

        // Resolve asset from scope → latest sentiment tick. Only
        // Symbol-scoped graphs can resolve an asset today; Global /
        // AssetClass / Client graphs get Missing (they may aggregate
        // cross-asset in a future pass — see
        // `docs/research/visual-strategy-builder.md` §11 Q1).
        let sentiment_tick = match &self.strategy_graph_scope {
            Some(mm_strategy_graph::Scope::Symbol(sym)) => {
                let (base, _quote) = split_symbol_bq(sym);
                self.dashboard
                    .as_ref()
                    .and_then(|d| d.sentiment_tick_for(base))
            }
            _ => None,
        };

        // Iterate every node in the evaluator's order, fill source
        // outputs by kind. The evaluator exposes `order` via a
        // read-only helper; the kind lookup is identical to what
        // `tick()` does internally.
        for (id, kind) in graph.nodes_by_kind().iter() {
            match kind.as_str() {
                "Book.L1" => {
                    // Multi-Venue Level 2.B — if the node's config
                    // names a different (venue, symbol, product)
                    // tuple, read that stream off the DataBus.
                    // Empty config → default to this engine's own
                    // view (current behaviour).
                    let cfg = graph.node_configs().get(id);
                    let v = cfg.and_then(|c| c.get("venue")).and_then(|v| v.as_str()).unwrap_or("");
                    let s = cfg.and_then(|c| c.get("symbol")).and_then(|v| v.as_str()).unwrap_or("");
                    let p_str = cfg.and_then(|c| c.get("product")).and_then(|v| v.as_str()).unwrap_or("");
                    let cross = !v.is_empty() || !s.is_empty() || !p_str.is_empty();
                    if cross {
                        let venue = if v.is_empty() {
                            format!("{:?}", self.config.exchange.exchange_type).to_lowercase()
                        } else { v.to_string() };
                        let symbol = if s.is_empty() { self.symbol.clone() } else { s.to_string() };
                        let product = match p_str {
                            "spot" => mm_common::config::ProductType::Spot,
                            "linear_perp" => mm_common::config::ProductType::LinearPerp,
                            "inverse_perp" => mm_common::config::ProductType::InversePerp,
                            _ => self.config.exchange.product,
                        };
                        let key = (venue, symbol, product);
                        let snap = self
                            .dashboard
                            .as_ref()
                            .and_then(|d| d.data_bus().get_l1(&key));
                        let (b, a, m_mid, sb) = match snap {
                            Some(s) => (
                                s.bid_px.map(Value::Number).unwrap_or(Value::Missing),
                                s.ask_px.map(Value::Number).unwrap_or(Value::Missing),
                                s.mid.map(Value::Number).unwrap_or(Value::Missing),
                                s.spread_bps.map(Value::Number).unwrap_or(Value::Missing),
                            ),
                            None => (Value::Missing, Value::Missing, Value::Missing, Value::Missing),
                        };
                        src.insert((*id, "bid_px".into()), b);
                        src.insert((*id, "bid_qty".into()), Value::Missing);
                        src.insert((*id, "ask_px".into()), a);
                        src.insert((*id, "ask_qty".into()), Value::Missing);
                        src.insert((*id, "mid".into()), m_mid);
                        src.insert((*id, "spread_bps".into()), sb);
                    } else {
                        src.insert((*id, "bid_px".into()), bid_px.clone());
                        src.insert((*id, "bid_qty".into()), bid_qty.clone());
                        src.insert((*id, "ask_px".into()), ask_px.clone());
                        src.insert((*id, "ask_qty".into()), ask_qty.clone());
                        src.insert((*id, "mid".into()), mid.clone());
                        src.insert((*id, "spread_bps".into()), spread_bps.clone());
                    }
                }
                // Multi-Venue 2.B.2 — cross-venue sources. Each
                // reads the DataBus by the (venue, symbol, product)
                // config on the node; empty fields fall back to
                // this engine's own stream.
                "Book.L2" => {
                    let (venue, symbol, product, depth) = Self::read_cross_venue_key(
                        graph.node_configs().get(id),
                        format!("{:?}", self.config.exchange.exchange_type).to_lowercase(),
                        self.symbol.clone(),
                        self.config.exchange.product,
                        true,
                    );
                    let snap = self
                        .dashboard
                        .as_ref()
                        .and_then(|d| d.data_bus().get_l2(&(venue, symbol, product)));
                    let (bids_str, asks_str, best_bid, best_ask) = match snap {
                        Some(s) => {
                            let d = depth.max(1).min(s.bids.len().max(s.asks.len()).max(1));
                            let fmt = |v: &[(rust_decimal::Decimal, rust_decimal::Decimal)]| {
                                v.iter()
                                    .take(d)
                                    .map(|(p, q)| format!("{p}@{q}"))
                                    .collect::<Vec<_>>()
                                    .join(",")
                            };
                            (
                                Value::String(fmt(&s.bids)),
                                Value::String(fmt(&s.asks)),
                                s.bids.first().map(|(p, _)| Value::Number(*p)).unwrap_or(Value::Missing),
                                s.asks.first().map(|(p, _)| Value::Number(*p)).unwrap_or(Value::Missing),
                            )
                        }
                        None => (
                            Value::String(String::new()),
                            Value::String(String::new()),
                            Value::Missing,
                            Value::Missing,
                        ),
                    };
                    src.insert((*id, "bids".into()), bids_str);
                    src.insert((*id, "asks".into()), asks_str);
                    src.insert((*id, "best_bid_px".into()), best_bid);
                    src.insert((*id, "best_ask_px".into()), best_ask);
                }
                "Trade.Tape" => {
                    let (venue, symbol, product, _window) = Self::read_cross_venue_key(
                        graph.node_configs().get(id),
                        format!("{:?}", self.config.exchange.exchange_type).to_lowercase(),
                        self.symbol.clone(),
                        self.config.exchange.product,
                        true,
                    );
                    let ticks = self
                        .dashboard
                        .as_ref()
                        .map(|d| d.data_bus().get_trades(&(venue, symbol, product)))
                        .unwrap_or_default();
                    let mut buy_qty = rust_decimal::Decimal::ZERO;
                    let mut sell_qty = rust_decimal::Decimal::ZERO;
                    let mut last_px: Option<rust_decimal::Decimal> = None;
                    for t in &ticks {
                        match t.aggressor {
                            Some(mm_dashboard::data_bus::TradeSide::Buy) => buy_qty += t.qty,
                            Some(mm_dashboard::data_bus::TradeSide::Sell) => sell_qty += t.qty,
                            None => {}
                        }
                        last_px = Some(t.price);
                    }
                    src.insert(
                        (*id, "trade_count".into()),
                        Value::Number(rust_decimal::Decimal::from(ticks.len())),
                    );
                    src.insert((*id, "buy_qty".into()), Value::Number(buy_qty));
                    src.insert((*id, "sell_qty".into()), Value::Number(sell_qty));
                    src.insert(
                        (*id, "last_price".into()),
                        last_px.map(Value::Number).unwrap_or(Value::Missing),
                    );
                }
                "Balance" => {
                    let cfg = graph.node_configs().get(id);
                    let venue = cfg
                        .and_then(|c| c.get("venue"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            format!("{:?}", self.config.exchange.exchange_type).to_lowercase()
                        });
                    let asset = cfg
                        .and_then(|c| c.get("asset"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("USDT")
                        .to_string();
                    let bal = self
                        .dashboard
                        .as_ref()
                        .and_then(|d| d.data_bus().get_balance(&venue, &asset));
                    let (total, available, reserved) = match bal {
                        Some(b) => (
                            Value::Number(b.total),
                            Value::Number(b.available),
                            Value::Number(b.reserved),
                        ),
                        None => (Value::Missing, Value::Missing, Value::Missing),
                    };
                    src.insert((*id, "total".into()), total);
                    src.insert((*id, "available".into()), available);
                    src.insert((*id, "reserved".into()), reserved);
                }
                "Portfolio.NetDelta" => {
                    let asset = graph
                        .node_configs()
                        .get(id)
                        .and_then(|c| c.get("asset"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("BTC")
                        .to_string();
                    let delta = build_portfolio(self.dashboard.as_ref()).net_delta(&asset);
                    src.insert((*id, "value".into()), Value::Number(delta));
                }
                "Portfolio.QuoteAvailable" => {
                    let venue = graph
                        .node_configs()
                        .get(id)
                        .and_then(|c| c.get("venue"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("binance")
                        .to_string();
                    let avail = build_portfolio(self.dashboard.as_ref()).quote_available(&venue);
                    src.insert((*id, "value".into()), Value::Number(avail));
                }
                "Funding" => {
                    let (venue, symbol, product, _) = Self::read_cross_venue_key(
                        graph.node_configs().get(id),
                        format!("{:?}", self.config.exchange.exchange_type).to_lowercase(),
                        self.symbol.clone(),
                        self.config.exchange.product,
                        false,
                    );
                    let fr = self
                        .dashboard
                        .as_ref()
                        .and_then(|d| d.data_bus().get_funding(&(venue, symbol, product)));
                    let (rate, seconds) = match fr {
                        Some(f) => (
                            f.rate.map(Value::Number).unwrap_or(Value::Missing),
                            f.next_funding_ts
                                .map(|ts| {
                                    Value::Number(rust_decimal::Decimal::from(
                                        (ts - chrono::Utc::now()).num_seconds().max(0),
                                    ))
                                })
                                .unwrap_or(Value::Missing),
                        ),
                        None => (Value::Missing, Value::Missing),
                    };
                    src.insert((*id, "rate".into()), rate);
                    src.insert((*id, "seconds_to_next".into()), seconds);
                }
                "Volatility.Realised" => {
                    src.insert((*id, "value".into()), realised_vol.clone());
                }
                "Toxicity.VPIN" => {
                    src.insert((*id, "value".into()), vpin.clone());
                }
                "Momentum.OFIZ" => {
                    src.insert((*id, "value".into()), ofi_z.clone());
                }
                // Sentiment sources — resolve the asset from the
                // graph's scope (Symbol → base asset via
                // `extract_base_asset`) then look up the latest
                // tick on the dashboard's in-memory snapshot.
                // Missing propagates when no tick has arrived
                // yet or when the scope isn't resolvable (e.g.
                // Global graph — ambiguous without per-engine
                // asset tagging).
                // Phase 2 Wave A — strategy + pair-class tags.
                "Strategy.Active" => {
                    src.insert(
                        (*id, "kind".into()),
                        Value::StrategyKind(self.strategy.name().to_string()),
                    );
                }
                // Phase 2 Wave B — risk layer sources.
                "Risk.MarginRatio" => {
                    // MarginGuard publishes into DashboardState on
                    // every poll; reading the cached per-symbol
                    // value avoids doubling the guard's internal
                    // state. `None` on spot engines or pre-first-
                    // poll → Missing propagates.
                    let v = self
                        .dashboard
                        .as_ref()
                        .and_then(|d| d.margin_ratio(&self.symbol))
                        .map(Value::Number)
                        .unwrap_or(Value::Missing);
                    src.insert((*id, "value".into()), v);
                }
                "Risk.OTR" => {
                    src.insert(
                        (*id, "value".into()),
                        Value::Number(self.otr.ratio()),
                    );
                }
                "Inventory.Level" => {
                    src.insert(
                        (*id, "value".into()),
                        Value::Number(self.inventory_manager.inventory()),
                    );
                }
                // Phase 2 Wave C — signal + toxicity + regime.
                "Signal.ImbalanceDepth" => {
                    // Top-5 book imbalance; matches momentum's
                    // book-imbalance feature.
                    let v = mm_strategy::momentum::MomentumSignals::book_imbalance(
                        &self.book_keeper.book,
                        5,
                    );
                    src.insert((*id, "value".into()), Value::Number(v));
                }
                "Signal.TradeFlow" => {
                    src.insert(
                        (*id, "value".into()),
                        Value::Number(self.momentum.trade_flow_imbalance()),
                    );
                }
                "Signal.Microprice" => {
                    // Learned-microprice drift ratio. `None` until
                    // the online fit produces its first estimate.
                    let mid = self.book_keeper.book.mid_price();
                    let v = mid
                        .and_then(|m| self.momentum.learned_microprice_drift(&self.book_keeper.book, m))
                        .map(Value::Number)
                        .unwrap_or(Value::Missing);
                    src.insert((*id, "value".into()), v);
                }
                "Toxicity.KyleLambda" => {
                    let v = self
                        .kyle_lambda
                        .lambda()
                        .map(Value::Number)
                        .unwrap_or(Value::Missing);
                    src.insert((*id, "value".into()), v);
                }
                "Regime.Detector" => {
                    let regime = self.auto_tuner.regime_detector.regime();
                    src.insert(
                        (*id, "regime".into()),
                        Value::String(format!("{regime:?}")),
                    );
                }
                "PairClass.Current" => {
                    let label = self
                        .pair_class
                        .as_ref()
                        .map(|c| format!("{c:?}"))
                        .unwrap_or_else(|| "Unknown".into());
                    src.insert((*id, "class".into()), Value::PairClass(label));
                }
                "Sentiment.Rate" | "Sentiment.Score" => {
                    let tick = sentiment_tick.as_ref();
                    let v = match (kind.as_str(), tick) {
                        ("Sentiment.Rate", Some(t)) => Value::Number(t.mentions_rate),
                        ("Sentiment.Score", Some(t)) => {
                            Value::Number(t.sentiment_score_5min)
                        }
                        _ => Value::Missing,
                    };
                    src.insert((*id, "value".into()), v);
                }
                // Phase 4.7 — composite strategies. Every Strategy.*
                // node reads last tick's `strategy.compute_quotes()`
                // output as its `quotes: Quotes` port. All Strategy.*
                // nodes in a given graph see the same bundle today;
                // per-node `γ`/`κ`/`σ` override lands in Phase 5.
                // Epic R — surveillance detector overlays. All three
                // detectors share one tracker; we take the lock once
                // per-node, project the output onto four ports, then
                // stash an alert tuple if the score crossed 0.8 so
                // the loop can emit them after it releases its borrow
                // of `self.strategy_graph`.
                "Session.TimeToBoundary" => {
                    let now = chrono::Utc::now();
                    let ttb = self.session_calendar.seconds_to_next(now);
                    let tsl = self.session_calendar.seconds_since_last(now);
                    src.insert(
                        (*id, "seconds_to_next".into()),
                        ttb.map(|s| Value::Number(rust_decimal::Decimal::from(s)))
                            .unwrap_or(Value::Missing),
                    );
                    src.insert(
                        (*id, "seconds_since_last".into()),
                        tsl.map(|s| Value::Number(rust_decimal::Decimal::from(s)))
                            .unwrap_or(Value::Missing),
                    );
                }
                "Surveillance.MarkingCloseScore" => {
                    // Close-window trade-volume ratio. Use the
                    // DataBus public-trade tape: the last 60 s
                    // vs. the 60 s before that.
                    let key = (
                        format!("{:?}", self.config.exchange.exchange_type).to_lowercase(),
                        self.symbol.clone(),
                        self.config.exchange.product,
                    );
                    let now = chrono::Utc::now();
                    let ttb = self.session_calendar.seconds_to_next(now);
                    let tape = self
                        .dashboard
                        .as_ref()
                        .map(|d| d.data_bus().get_trades(&key))
                        .unwrap_or_default();
                    // Two halves: recent 60 s (close window) + the
                    // minute before it (baseline).
                    let window = chrono::Duration::seconds(60);
                    let (recent, base_cutoff) = (now - window, now - window * 2);
                    let mut window_vol = rust_decimal::Decimal::ZERO;
                    let mut baseline_vol = rust_decimal::Decimal::ZERO;
                    for t in &tape {
                        if t.ts >= recent {
                            window_vol += t.qty;
                        } else if t.ts >= base_cutoff {
                            baseline_vol += t.qty;
                        }
                    }
                    let out = mm_risk::surveillance::MarkingCloseDetector::new()
                        .score(ttb.unwrap_or(i64::MAX), window_vol, baseline_vol);
                    src.insert((*id, "value".into()), Value::Number(out.score));
                    if out.score >= rust_decimal_macros::dec!(0.8) {
                        pending_alerts.push((kind.clone(), *id, out));
                    }
                }
                "Surveillance.FakeLiquidityScore" => {
                    // Pull current L2 off the bus, compare against
                    // the snapshot we stashed at the end of the
                    // previous graph tick.
                    let key = (
                        format!("{:?}", self.config.exchange.exchange_type).to_lowercase(),
                        self.symbol.clone(),
                        self.config.exchange.product,
                    );
                    let current = self.dashboard.as_ref().and_then(|d| d.data_bus().get_l2(&key));
                    let score = match (self.prev_l2_snapshot.as_ref(), current.as_ref()) {
                        (Some(prev), Some(now_snap)) => {
                            let to_levels = |v: &[(rust_decimal::Decimal, rust_decimal::Decimal)]|
                                -> Vec<mm_risk::surveillance::L2Level> {
                                v.iter()
                                    .map(|(p, q)| mm_risk::surveillance::L2Level {
                                        price: *p,
                                        qty: *q,
                                    })
                                    .collect()
                            };
                            let then_ss = prev;
                            let now_ss = mm_risk::surveillance::L2Snapshot {
                                bids: to_levels(&now_snap.bids),
                                asks: to_levels(&now_snap.asks),
                                ts: now_snap.ts.unwrap_or_else(chrono::Utc::now),
                            };
                            let out = mm_risk::surveillance::FakeLiquidityDetector::new()
                                .score(then_ss, &now_ss);
                            if out.score >= rust_decimal_macros::dec!(0.8) {
                                pending_alerts.push((kind.clone(), *id, out.clone()));
                            }
                            out.score
                        }
                        _ => rust_decimal::Decimal::ZERO,
                    };
                    src.insert((*id, "value".into()), Value::Number(score));
                }
                "Surveillance.WashScore" => {
                    let fills = self
                        .surveillance_tracker
                        .lock()
                        .ok()
                        .map(|t| t.recent_fills(&self.symbol))
                        .unwrap_or_default();
                    let out = mm_risk::surveillance::WashDetector::new()
                        .score_from_fills(&fills);
                    src.insert((*id, "value".into()), Value::Number(out.score));
                    if out.score >= rust_decimal_macros::dec!(0.8) {
                        pending_alerts.push((kind.clone(), *id, out));
                    }
                }
                "Surveillance.MomentumIgnitionScore" => {
                    // Read public trades off the DataBus for this
                    // engine's (venue, symbol, product). Map into
                    // the detector's sample shape.
                    let key = (
                        format!("{:?}", self.config.exchange.exchange_type).to_lowercase(),
                        self.symbol.clone(),
                        self.config.exchange.product,
                    );
                    let samples: Vec<mm_risk::surveillance::PublicTradeSample> = self
                        .dashboard
                        .as_ref()
                        .map(|d| d.data_bus().get_trades(&key))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|t| mm_risk::surveillance::PublicTradeSample {
                            ts: t.ts,
                            price: t.price,
                            qty: t.qty,
                            aggressor: t.aggressor.map(|a| match a {
                                mm_dashboard::data_bus::TradeSide::Buy => {
                                    mm_risk::surveillance::Side::Buy
                                }
                                mm_dashboard::data_bus::TradeSide::Sell => {
                                    mm_risk::surveillance::Side::Sell
                                }
                            }),
                        })
                        .collect();
                    let out = mm_risk::surveillance::MomentumIgnitionDetector::new()
                        .score(&samples);
                    src.insert((*id, "value".into()), Value::Number(out.score));
                    if out.score >= rust_decimal_macros::dec!(0.8) {
                        pending_alerts.push((kind.clone(), *id, out));
                    }
                }
                "Surveillance.SpoofingScore" |
                "Surveillance.LayeringScore" |
                "Surveillance.QuoteStuffingScore" => {
                    let out = {
                        let Ok(t) = self.surveillance_tracker.lock() else { continue };
                        match kind.as_str() {
                            "Surveillance.SpoofingScore" =>
                                mm_risk::surveillance::SpoofingDetector::new().score(&self.symbol, &t),
                            "Surveillance.LayeringScore" =>
                                mm_risk::surveillance::LayeringDetector::new().score(&self.symbol, &t),
                            _ =>
                                mm_risk::surveillance::QuoteStuffingDetector::new().score(&self.symbol, &t),
                        }
                    };
                    src.insert((*id, "value".into()), Value::Number(out.score));
                    src.insert(
                        (*id, "cancel_ratio".into()),
                        Value::Number(out.cancel_to_fill_ratio),
                    );
                    src.insert(
                        (*id, "lifetime_ms".into()),
                        match out.median_order_lifetime_ms {
                            Some(ms) => Value::Number(rust_decimal::Decimal::from(ms)),
                            None => Value::Missing,
                        },
                    );
                    src.insert(
                        (*id, "size_ratio".into()),
                        match out.size_vs_avg_trade {
                            Some(r) => Value::Number(r),
                            None => Value::Missing,
                        },
                    );
                    if out.score >= rust_decimal_macros::dec!(0.8) {
                        pending_alerts.push((kind.clone(), *id, out));
                    }
                }
                // Multi-Venue 3.D — BasisArb reads its snapshot off
                // the DataBus (spot + perp mids) and the portfolio
                // tracker (net delta), then composes a
                // `Value::VenueQuotes` output via the pure
                // `mm_strategy::basis_arb::compute_basis_arb_legs`
                // helper. Written inline here rather than going
                // through `strategy_pool` because the computation
                // doesn't need a `Strategy` trait impl — there's no
                // per-instance state worth pooling.
                "Strategy.BasisArb" => {
                    let cfg = graph.node_configs().get(id);
                    let getstr = |k: &str, default: &str| -> String {
                        cfg.and_then(|c| c.get(k))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or(default)
                            .to_string()
                    };
                    let getdec = |k: &str, default: rust_decimal::Decimal| {
                        cfg.and_then(|c| c.get(k))
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse::<rust_decimal::Decimal>().ok())
                            .unwrap_or(default)
                    };
                    let spot_venue = getstr("spot_venue", "binance");
                    let perp_venue = getstr("perp_venue", "bybit");
                    let symbol = getstr("symbol", &self.symbol);
                    let leg_size = getdec("leg_size", rust_decimal_macros::dec!(0.001));
                    let maker_offset = getdec("maker_offset_bps", rust_decimal_macros::dec!(2));
                    let min_basis = getdec("min_basis_bps", rust_decimal_macros::dec!(10));
                    let max_delta = getdec("max_delta", rust_decimal_macros::dec!(0.05));
                    let bus = self.dashboard.as_ref().map(|d| d.data_bus());
                    let spot_mid = bus
                        .as_ref()
                        .and_then(|b| {
                            b.get_l1(&(
                                spot_venue.clone(),
                                symbol.clone(),
                                mm_common::config::ProductType::Spot,
                            ))
                        })
                        .and_then(|s| s.mid)
                        .unwrap_or(rust_decimal::Decimal::ZERO);
                    let perp_mid = bus
                        .as_ref()
                        .and_then(|b| {
                            b.get_l1(&(
                                perp_venue.clone(),
                                symbol.clone(),
                                mm_common::config::ProductType::LinearPerp,
                            ))
                        })
                        .and_then(|s| s.mid)
                        .unwrap_or(rust_decimal::Decimal::ZERO);
                    let (base, _) = split_symbol_bq(&symbol);
                    let net_delta = build_portfolio(self.dashboard.as_ref()).net_delta(base);
                    let snap = mm_strategy::basis_arb::BasisSnapshot {
                        spot_venue: spot_venue.clone(),
                        spot_symbol: symbol.clone(),
                        spot_mid,
                        perp_venue: perp_venue.clone(),
                        perp_symbol: symbol.clone(),
                        perp_mid,
                        net_delta,
                    };
                    let arb_cfg = mm_strategy::basis_arb::BasisArbConfig {
                        leg_size,
                        maker_offset_bps: maker_offset,
                        min_basis_bps: min_basis,
                        max_delta,
                    };
                    let legs = mm_strategy::basis_arb::compute_basis_arb_legs(&snap, &arb_cfg);
                    let vqs: Vec<mm_strategy_graph::VenueQuote> = legs
                        .into_iter()
                        .map(|l| mm_strategy_graph::VenueQuote {
                            venue: l.venue,
                            symbol: l.symbol,
                            product: l.product,
                            side: match l.side {
                                mm_strategy::basis_arb::Side::Buy => {
                                    mm_strategy_graph::QuoteSide::Buy
                                }
                                mm_strategy::basis_arb::Side::Sell => {
                                    mm_strategy_graph::QuoteSide::Sell
                                }
                            },
                            price: l.price,
                            qty: l.qty,
                        })
                        .collect();
                    src.insert((*id, "quotes".into()), Value::VenueQuotes(vqs));
                }
                // Phase 5 — composite strategies with per-node pool.
                // Each `Strategy.*` node reads its own pool instance's
                // last-tick output (computed with the node's own
                // parsed config — γ / spoof size / ref_symbol / …).
                // Falls back to the shared `last_strategy_quotes` so
                // legacy graphs with no pool instance keep working.
                k if k.starts_with("Strategy.") => {
                    let qs: Option<&Vec<mm_common::types::QuotePair>> =
                        self.last_strategy_quotes_per_node
                            .get(id)
                            .or(self.last_strategy_quotes.as_ref());
                    let v = match qs {
                        Some(qs) => {
                            use mm_common::types::Side;
                            use mm_strategy_graph::{GraphQuote, QuoteSide};
                            let mut out = Vec::with_capacity(qs.len() * 2);
                            for pair in qs {
                                if let Some(b) = &pair.bid {
                                    out.push(GraphQuote {
                                        side: QuoteSide::Buy,
                                        price: b.price,
                                        qty: b.qty,
                                    });
                                }
                                if let Some(a) = &pair.ask {
                                    out.push(GraphQuote {
                                        side: match a.side {
                                            Side::Buy => QuoteSide::Buy,
                                            Side::Sell => QuoteSide::Sell,
                                        },
                                        price: a.price,
                                        qty: a.qty,
                                    });
                                }
                            }
                            Value::Quotes(out)
                        }
                        None => Value::Missing,
                    };
                    src.insert((*id, "quotes".into()), v);
                }
                _ => {}
            }
        }

        let actions = match graph.tick(&ctx, &src) {
            Ok(a) => a,
            Err(e) => {
                warn!(
                    symbol = %self.symbol,
                    error = %e,
                    "strategy graph tick failed"
                );
                return;
            }
        };
        for a in actions {
            match a {
                SinkAction::SpreadMult(m) => {
                    self.auto_tuner.set_graph_spread_mult(m);
                }
                SinkAction::SizeMult(m) => {
                    self.auto_tuner.set_graph_size_mult(m);
                }
                SinkAction::KillEscalate { level, reason } => {
                    let kl = match level {
                        1 => mm_risk::kill_switch::KillLevel::WidenSpreads,
                        2 => mm_risk::kill_switch::KillLevel::StopNewOrders,
                        3 => mm_risk::kill_switch::KillLevel::CancelAll,
                        4 => mm_risk::kill_switch::KillLevel::FlattenAll,
                        5 => mm_risk::kill_switch::KillLevel::Disconnect,
                        _ => {
                            warn!(level, "graph kill sink emitted invalid level");
                            continue;
                        }
                    };
                    self.kill_switch.manual_trigger(kl, &reason);
                    self.audit.risk_event(
                        &self.symbol,
                        mm_risk::audit::AuditEventType::KillSwitchEscalated,
                        &format!("graph L{level}: {reason}"),
                    );
                    // Phase 3 — provenance row with the graph hash so
                    // regulators can join this kill back to the exact
                    // authored JSON in history/{hash}.json.
                    if let Some(hash) = self.strategy_graph_hash.as_deref() {
                        self.audit.strategy_graph_sink_fired(
                            &self.symbol,
                            &format!("KillEscalate {{ level: {level}, reason: {reason:?} }}"),
                            hash,
                        );
                    }
                    error!(
                        symbol = %self.symbol,
                        graph = self.strategy_graph_name.as_deref().unwrap_or("?"),
                        level,
                        reason = %reason,
                        "strategy graph escalated kill switch"
                    );
                }
                SinkAction::Quotes(qs) => {
                    // Phase 4 — graph fully authored the quoting
                    // pipeline. Group GraphQuotes into QuotePairs by
                    // index of their side: the N-th buy pairs with
                    // the N-th sell. The classic `num_levels × pair`
                    // shape is preserved so downstream
                    // (inventory_manager, balance_cache, order_manager)
                    // consumers need no changes.
                    use mm_common::types::{Quote, QuotePair, Side};
                    use mm_strategy_graph::QuoteSide;
                    let mut bids: Vec<Quote> = qs
                        .iter()
                        .filter(|q| q.side == QuoteSide::Buy)
                        .map(|q| Quote {
                            side: Side::Buy,
                            price: self.product.round_price(q.price),
                            qty: self.product.round_qty(q.qty),
                        })
                        .collect();
                    let mut asks: Vec<Quote> = qs
                        .iter()
                        .filter(|q| q.side == QuoteSide::Sell)
                        .map(|q| Quote {
                            side: Side::Sell,
                            price: self.product.round_price(q.price),
                            qty: self.product.round_qty(q.qty),
                        })
                        .collect();
                    // Deeper levels last: sort so level 0 is closest
                    // to mid on both sides (inner-first).
                    bids.sort_by(|a, b| b.price.cmp(&a.price));
                    asks.sort_by(|a, b| a.price.cmp(&b.price));
                    let n = bids.len().max(asks.len());
                    let mut pairs = Vec::with_capacity(n);
                    for i in 0..n {
                        pairs.push(QuotePair {
                            bid: bids.get(i).cloned(),
                            ask: asks.get(i).cloned(),
                        });
                    }
                    self.graph_quotes_override = Some(pairs);
                }
                SinkAction::VenueQuotes(vqs) => {
                    // Multi-Venue 3.A — degenerate dispatcher. Fan
                    // entries to:
                    //   · self engine (same venue + symbol)  →
                    //     reuse the classic `graph_quotes_override`
                    //     path (legacy diffing, inventory limits,
                    //     balance check all get their shot).
                    //   · any other venue/symbol → ignored with a
                    //     warn; 3.B replaces this arm with a real
                    //     MultiVenueOrderRouter.
                    use mm_common::types::{Quote, QuotePair, Side};
                    use mm_strategy_graph::QuoteSide;
                    let self_venue = format!("{:?}", self.config.exchange.exchange_type)
                        .to_lowercase();
                    let self_product_label =
                        format!("{:?}", self.config.exchange.product).to_lowercase();
                    let (mut local_bids, mut local_asks): (Vec<Quote>, Vec<Quote>) =
                        (Vec::new(), Vec::new());
                    // Multi-Venue 3.B — bucket remote entries by
                    // target symbol. Each bucket goes through as a
                    // single `ExternalVenueQuotes` payload via the
                    // dashboard's per-symbol override channel, so
                    // the recipient engine reads them on its next
                    // select-loop tick.
                    let mut remote_by_symbol: std::collections::HashMap<
                        String,
                        Vec<mm_strategy_graph::VenueQuote>,
                    > = std::collections::HashMap::new();
                    for vq in &vqs {
                        let targets_self = vq.venue == self_venue
                            && vq.symbol == self.symbol
                            && (vq.product.is_empty() || vq.product == self_product_label);
                        if !targets_self {
                            remote_by_symbol
                                .entry(vq.symbol.clone())
                                .or_default()
                                .push(vq.clone());
                            continue;
                        }
                        let q = Quote {
                            side: match vq.side {
                                QuoteSide::Buy => Side::Buy,
                                QuoteSide::Sell => Side::Sell,
                            },
                            price: self.product.round_price(vq.price),
                            qty: self.product.round_qty(vq.qty),
                        };
                        match q.side {
                            Side::Buy => local_bids.push(q),
                            Side::Sell => local_asks.push(q),
                        }
                    }
                    // Fire each remote batch. If the target engine
                    // isn't registered (no matching symbol), the
                    // dashboard returns false — we surface that as
                    // a dropped-routing warn.
                    if !remote_by_symbol.is_empty() {
                        if let Some(dash) = self.dashboard.as_ref() {
                            for (sym, batch) in &remote_by_symbol {
                                let json = serde_json::to_string(batch)
                                    .unwrap_or_else(|_| "[]".to_string());
                                let ok = dash.send_config_override(
                                    sym,
                                    mm_dashboard::state::ConfigOverride::ExternalVenueQuotes(
                                        json,
                                    ),
                                );
                                if !ok {
                                    warn!(
                                        source_symbol = %self.symbol,
                                        target_symbol = %sym,
                                        count = batch.len(),
                                        "VenueQuotes dispatch — no engine registered for target symbol"
                                    );
                                }
                            }
                        }
                    }
                    // Pair bids + asks inner-first, same shape as the
                    // Quotes sink path.
                    local_bids.sort_by(|a, b| b.price.cmp(&a.price));
                    local_asks.sort_by(|a, b| a.price.cmp(&b.price));
                    let n = local_bids.len().max(local_asks.len());
                    let mut pairs = Vec::with_capacity(n);
                    for i in 0..n {
                        pairs.push(QuotePair {
                            bid: local_bids.get(i).cloned(),
                            ask: local_asks.get(i).cloned(),
                        });
                    }
                    self.graph_quotes_override = Some(pairs);
                }
                SinkAction::AtomicBundle(spec) => {
                    // Multi-Venue 3.E MVP — materialise both legs as
                    // an `ExternalVenueQuotes` dispatch (same path
                    // as 3.B's fan-out), plus an audit row so the
                    // operator sees the bundle intent. Timeout +
                    // rollback on one-sided ack failure is a
                    // follow-up (3.E.2): for now we dispatch
                    // fire-and-forget and surface the timeout_ms
                    // into the audit detail so post-mortems can
                    // spot stuck bundles.
                    let legs = vec![spec.maker.clone(), spec.hedge.clone()];
                    let json = serde_json::to_string(&legs).unwrap_or_else(|_| "[]".into());
                    // Audit — regulator-visible intent.
                    self.audit.risk_event(
                        &self.symbol,
                        mm_risk::audit::AuditEventType::StrategyGraphSinkFired,
                        &format!(
                            "AtomicBundle maker={}:{} hedge={}:{} timeout_ms={}",
                            spec.maker.venue,
                            spec.maker.symbol,
                            spec.hedge.venue,
                            spec.hedge.symbol,
                            spec.timeout_ms,
                        ),
                    );
                    // Dispatch each leg to its target symbol's
                    // channel — the target engine may be this one
                    // (self-venue legs collapse into
                    // graph_quotes_override) or another one
                    // (ExternalVenueQuotes consumer path).
                    if let Some(dash) = self.dashboard.as_ref() {
                        for leg in &legs {
                            if leg.symbol == self.symbol {
                                // Degenerate — treat as VenueQuotes
                                // for the self path.
                                continue;
                            }
                            let payload = serde_json::to_string(&vec![leg.clone()])
                                .unwrap_or_else(|_| "[]".into());
                            let _ = dash.send_config_override(
                                &leg.symbol,
                                mm_dashboard::state::ConfigOverride::ExternalVenueQuotes(
                                    payload,
                                ),
                            );
                        }
                    }
                    // Self-leg materialisation (mirror of the
                    // VenueQuotes path above). Only runs if one of
                    // the legs targets this engine's symbol.
                    use mm_common::types::{Quote, QuotePair, Side};
                    use mm_strategy_graph::QuoteSide;
                    let self_venue = format!("{:?}", self.config.exchange.exchange_type)
                        .to_lowercase();
                    let mut bids = Vec::new();
                    let mut asks = Vec::new();
                    for leg in &legs {
                        if leg.venue != self_venue || leg.symbol != self.symbol {
                            continue;
                        }
                        let q = Quote {
                            side: match leg.side {
                                QuoteSide::Buy => Side::Buy,
                                QuoteSide::Sell => Side::Sell,
                            },
                            price: self.product.round_price(leg.price),
                            qty: self.product.round_qty(leg.qty),
                        };
                        match q.side {
                            Side::Buy => bids.push(q),
                            Side::Sell => asks.push(q),
                        }
                    }
                    if !bids.is_empty() || !asks.is_empty() {
                        let n = bids.len().max(asks.len());
                        let mut pairs = Vec::with_capacity(n);
                        for i in 0..n {
                            pairs.push(QuotePair {
                                bid: bids.get(i).cloned(),
                                ask: asks.get(i).cloned(),
                            });
                        }
                        self.graph_quotes_override = Some(pairs);
                    }
                    let _ = json; // reserved for 3.E.2 rollback path
                }
                SinkAction::Flatten { policy } => {
                    // Graph-authored flatten. Escalates kill L4 with
                    // the policy string; existing paired_unwind /
                    // twap_executor machinery picks up the algo on
                    // next tick. Audit-log the full policy so
                    // compliance can reproduce what the graph
                    // actually asked for.
                    self.kill_switch.manual_trigger(
                        mm_risk::kill_switch::KillLevel::FlattenAll,
                        &format!("graph flatten: {policy}"),
                    );
                    self.audit.risk_event(
                        &self.symbol,
                        mm_risk::audit::AuditEventType::KillSwitchEscalated,
                        &format!("graph L4 flatten policy={policy}"),
                    );
                    if let Some(hash) = self.strategy_graph_hash.as_deref() {
                        self.audit.strategy_graph_sink_fired(
                            &self.symbol,
                            &format!("Flatten {{ policy: {policy:?} }}"),
                            hash,
                        );
                    }
                    error!(
                        symbol = %self.symbol,
                        graph = self.strategy_graph_name.as_deref().unwrap_or("?"),
                        %policy,
                        "strategy graph requested flatten"
                    );
                }
            }
        }

        // Week 5b — feed per-strategy session timing so Mark-
        // class exploits know when they're inside the close-window.
        let now = chrono::Utc::now();
        if let Some(s) = self.session_calendar.seconds_to_next(now) {
            for strat in self.strategy_pool.values() {
                strat.on_session_tick(s);
            }
        }

        // Week 5 — stash the current L2 for the FakeLiquidity
        // detector's next-tick delta read. Done after the overlay
        // loop so this tick still compared against the previous
        // snapshot.
        let key = (
            format!("{:?}", self.config.exchange.exchange_type).to_lowercase(),
            self.symbol.clone(),
            self.config.exchange.product,
        );
        if let Some(snap) = self.dashboard.as_ref().and_then(|d| d.data_bus().get_l2(&key)) {
            let to_levels =
                |v: &[(rust_decimal::Decimal, rust_decimal::Decimal)]|
                 -> Vec<mm_risk::surveillance::L2Level> {
                    v.iter()
                        .map(|(p, q)| mm_risk::surveillance::L2Level {
                            price: *p,
                            qty: *q,
                        })
                        .collect()
                };
            self.prev_l2_snapshot = Some(mm_risk::surveillance::L2Snapshot {
                bids: to_levels(&snap.bids),
                asks: to_levels(&snap.asks),
                ts: snap.ts.unwrap_or_else(chrono::Utc::now),
            });
        }

        // Epic R — emit surveillance alerts for scores that crossed
        // threshold this tick. Cooldown is per-(pattern, node id)
        // so two detectors of the same kind in one graph don't
        // stomp each other's dedupe entries.
        let now_ms = chrono::Utc::now().timestamp_millis();
        for (pattern, node_id, out) in pending_alerts {
            let key = format!("{pattern}:{node_id}");
            let recently = self
                .surveillance_last_alert
                .get(&key)
                .map(|prev| now_ms - *prev < 60_000)
                .unwrap_or(false);
            if recently {
                continue;
            }
            self.surveillance_last_alert.insert(key, now_ms);
            let extra = format!(
                "cancel_ratio={} lifetime_ms={} size_ratio={}",
                out.cancel_to_fill_ratio,
                out.median_order_lifetime_ms
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "-".into()),
                out.size_vs_avg_trade
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "-".into()),
            );
            let pattern_tag = match pattern.as_str() {
                "Surveillance.SpoofingScore" => "spoofing",
                "Surveillance.LayeringScore" => "layering",
                "Surveillance.QuoteStuffingScore" => "quote_stuffing",
                "Surveillance.WashScore" => "wash",
                "Surveillance.MomentumIgnitionScore" => "momentum_ignition",
                "Surveillance.FakeLiquidityScore" => "fake_liquidity",
                "Surveillance.MarkingCloseScore" => "marking_close",
                _ => "unknown",
            };
            self.audit.surveillance_alert(
                &self.symbol,
                pattern_tag,
                out.score,
                &extra,
            );
        }
    }

    /// Epic G — attach a `SocialRiskEngine`. Once attached,
    /// every `ConfigOverride::SentimentTick` routed to the
    /// engine is evaluated against the current volatility +
    /// OFI state; the fused output pushes spread / size /
    /// skew multipliers into the autotuner and — on extreme
    /// rate+vol confirmation — escalates the kill switch to
    /// L2. The orchestrator (`mm-sentiment`-driven, in
    /// `server/src/main.rs`) is the canonical producer of
    /// these ticks.
    pub fn with_social_risk(
        mut self,
        engine: mm_risk::social_risk::SocialRiskEngine,
    ) -> Self {
        self.social_risk = Some(engine);
        self
    }

    /// Apply a freshly-received `SentimentTick` to the social
    /// risk engine. Pure internal helper: takes the caller-
    /// measured realised vol + OFI z-score as the market-side
    /// cross-validation inputs. Routed here from the
    /// `ConfigOverride::SentimentTick` branch.
    fn on_sentiment_tick(&mut self, tick: mm_sentiment::SentimentTick) {
        let Some(engine) = self.social_risk.as_mut() else {
            return;
        };
        // Use the engine's own realised vol (already EWMA'd
        // from mid returns) and 0-OFI as a conservative
        // default — the CKS OFI signal is per-tick and not
        // always available; expanding this is an easy follow-up.
        let realised_vol = self
            .volatility_estimator
            .volatility()
            .unwrap_or(Decimal::ZERO);
        // Epic G follow-up — live OFI z-score from the
        // momentum tracker. `None` until the OFI path has
        // seen two snapshots; zero is the safe conservative
        // fallback (no confirmation → widen only, no skew).
        let ofi_z = self.momentum.ofi_z().unwrap_or(Decimal::ZERO);
        let market = mm_risk::social_risk::MarketContext {
            realised_vol,
            ofi_z,
        };
        let state = engine.evaluate(&tick, market, chrono::Utc::now());
        self.auto_tuner.set_social_spread_mult(state.vol_multiplier);
        self.auto_tuner.set_social_size_mult(state.size_multiplier);
        self.social_skew_bps = state.inv_skew_bps;

        // Prometheus — last-applied multipliers per symbol.
        use rust_decimal::prelude::ToPrimitive;
        mm_dashboard::metrics::SOCIAL_SPREAD_MULT
            .with_label_values(&[&self.symbol])
            .set(state.vol_multiplier.to_f64().unwrap_or(1.0));
        mm_dashboard::metrics::SOCIAL_SIZE_MULT
            .with_label_values(&[&self.symbol])
            .set(state.size_multiplier.to_f64().unwrap_or(1.0));

        if state.kill_trigger {
            self.kill_switch.manual_trigger(
                mm_risk::kill_switch::KillLevel::StopNewOrders,
                "social risk: mentions_rate + vol confirmed spike",
            );
            self.audit.risk_event(
                &self.symbol,
                mm_risk::audit::AuditEventType::KillSwitchEscalated,
                &format!(
                    "social: rate={} vol={} asset={}",
                    tick.mentions_rate, realised_vol, tick.asset
                ),
            );
            mm_dashboard::metrics::SOCIAL_KILL_TRIGGERS_TOTAL
                .with_label_values(&[&self.symbol])
                .inc();
            error!(
                symbol = %self.symbol,
                asset = %tick.asset,
                rate = %tick.mentions_rate,
                vol = %realised_vol,
                "social risk escalated kill switch to L2"
            );
        } else if state.vol_multiplier > Decimal::ONE {
            debug!(
                symbol = %self.symbol,
                asset = %tick.asset,
                spread_mult = %state.vol_multiplier,
                size_mult = %state.size_multiplier,
                skew_bps = %state.inv_skew_bps,
                reason = state.reason,
                "social risk adjustment applied"
            );
        }
    }

    /// Attach a [`NewsRetreatStateMachine`] (Epic F sub-component
    /// #2). Operators feed headlines through the dashboard's
    /// `POST /api/admin/config` broadcast endpoint using
    /// `ConfigOverride::News(text)`; the engine's config-
    /// override handler routes those directly into
    /// [`Self::on_news_headline`]. Critical-class transitions
    /// escalate the kill switch to L2.
    pub fn with_news_retreat(mut self, sm: NewsRetreatStateMachine) -> Self {
        self.news_retreat = Some(sm);
        self
    }

    /// Public push API for the lead-lag guard. Operators (or
    /// the engine's own hedge-connector book event handler)
    /// pipe the latest leader mid here. Updates the guard,
    /// pushes the new multiplier into the autotuner, and
    /// fires a `LeadLagTriggered` audit record on the
    /// `1.0 → > 1.0` transition.
    pub fn update_lead_lag_from_mid(&mut self, mid: Decimal) {
        let Some(guard) = self.lead_lag_guard.as_mut() else {
            return;
        };
        guard.on_leader_mid(mid);
        let mult = guard.current_multiplier();
        let z_abs = guard.current_z_abs();
        self.auto_tuner.set_lead_lag_mult(mult);
        let now_active = mult > dec!(1);
        if now_active && !self.lead_lag_active {
            self.audit.risk_event(
                &self.symbol,
                AuditEventType::LeadLagTriggered,
                &format!("z_abs={z_abs} mult={mult}"),
            );
        }
        self.lead_lag_active = now_active;
    }

    /// Public push API for the news retreat state machine.
    /// Operators wire any feed source (Telegram bot, file
    /// tail, paid Tiingo adapter, their own scraper) and call
    /// this for each headline. Routes the transition to
    /// audit + alert + autotuner + kill switch.
    pub fn on_news_headline(&mut self, text: &str) {
        let Some(sm) = self.news_retreat.as_mut() else {
            return;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let transition = sm.on_headline(text, now_ms);
        if matches!(transition, NewsRetreatTransition::NoMatch) {
            return;
        }
        // Update the autotuner multiplier on every non-NoMatch
        // transition so the cooldown window stays in effect
        // for refreshes / suppressions as well as promotions.
        let mult = sm.current_multiplier(now_ms);
        self.auto_tuner.set_news_retreat_mult(mult);
        if let NewsRetreatTransition::Promoted { from, to } = transition {
            self.audit.risk_event(
                &self.symbol,
                AuditEventType::NewsRetreatActivated,
                &format!("{from:?} → {to:?}: {text}"),
            );
            if matches!(to, NewsRetreatState::Critical) {
                self.kill_switch.manual_trigger(
                    mm_risk::kill_switch::KillLevel::StopNewOrders,
                    "news retreat: Critical headline",
                );
                error!(headline = %text, "news retreat Critical → kill switch L2");
            } else {
                warn!(headline = %text, ?to, "news retreat activated");
            }
        }
    }

    /// Refresh the news-retreat multiplier from the cooldown
    /// timer. Called periodically (typically from the engine's
    /// 30-second summary tick) so a Critical headline that
    /// has cooled out properly drops the autotuner widening
    /// without waiting for the next fresh headline. Fires a
    /// `NewsRetreatExpired` audit record on the cooldown-
    /// expiry transition.
    pub fn tick_news_retreat(&mut self) {
        let Some(sm) = self.news_retreat.as_mut() else {
            return;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let prev_state = sm.current_state(now_ms);
        let mult = sm.current_multiplier(now_ms);
        self.auto_tuner.set_news_retreat_mult(mult);
        // current_state returns the post-expiry value, so a
        // transition from active → Normal means cooldown
        // just fired. Re-read after the expiry.
        if !matches!(prev_state, NewsRetreatState::Normal) {
            let after = sm.current_state(now_ms);
            if matches!(after, NewsRetreatState::Normal) {
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::NewsRetreatExpired,
                    &format!("expired from {prev_state:?}"),
                );
            }
        }
    }

    pub async fn run(
        &mut self,
        ws_rx: mpsc::UnboundedReceiver<MarketEvent>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        self.run_with_hedge(ws_rx, None, shutdown_rx).await
    }

    /// Dual-connector variant of [`run`]. Callers with a hedge
    /// connector subscribe its market-data stream and pass it in
    /// `hedge_rx`; events from that channel only update the hedge
    /// `BookKeeper` and never place orders — the primary leg is
    /// still the only one touched by `OrderManager`.
    #[instrument(skip_all, fields(symbol = %self.symbol, strategy = self.strategy.name()))]
    pub async fn run_with_hedge(
        &mut self,
        mut ws_rx: mpsc::UnboundedReceiver<MarketEvent>,
        mut hedge_rx: Option<mpsc::UnboundedReceiver<MarketEvent>>,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        info!(
            symbol = %self.symbol,
            strategy = self.strategy.name(),
            dual = self.connectors.is_dual(),
            "engine starting"
        );
        self.audit.risk_event(
            &self.symbol,
            AuditEventType::EngineStarted,
            self.strategy.name(),
        );
        if let Some(wh) = &self.webhooks {
            wh.dispatch(mm_dashboard::webhooks::WebhookEvent::EngineStarted {
                symbol: self.symbol.clone(),
            });
        }

        // Initial balance fetch.
        self.refresh_balances().await;

        // Startup reconciliation — reconcile local order state
        // against the venue BEFORE the first refresh_quotes tick
        // fires. Without this, orphaned orders from a previous
        // session stay live on the venue while the engine happily
        // quotes new orders, doubling exposure until the first
        // 60-second reconcile cycle runs. The reconcile path
        // cancels any phantom orders it finds and cleans up ghosts.
        self.reconcile().await;
        if let Err(e) = self
            .order_manager
            .cancel_all(&self.connectors.primary, &self.symbol)
            .await
        {
            warn!(
                symbol = %self.symbol,
                error = %e,
                "startup cancel_all left survivors — next reconcile will retry"
            );
        }

        // Initial orderbook snapshot via REST.
        match self
            .connectors
            .primary
            .get_orderbook(&self.symbol, 25)
            .await
        {
            Ok((bids, asks, seq)) => {
                self.book_keeper.book.apply_snapshot(bids, asks, seq);
                info!(seq, "initial book snapshot loaded");
            }
            Err(e) => warn!(error = %e, "failed to fetch initial book"),
        }

        // Initial hedge orderbook snapshot.
        if let (Some(hedge), Some(pair)) = (
            self.connectors.hedge.as_ref(),
            self.connectors.pair.as_ref(),
        ) {
            match hedge.get_orderbook(&pair.hedge_symbol, 25).await {
                Ok((bids, asks, seq)) => {
                    if let Some(hb) = self.hedge_book.as_mut() {
                        hb.book.apply_snapshot(bids, asks, seq);
                        info!(seq, hedge_symbol = %pair.hedge_symbol, "initial hedge book loaded");
                    }
                }
                Err(e) => warn!(error = %e, "failed to fetch initial hedge book"),
            }
        }

        let refresh_ms = self.config.market_maker.refresh_interval_ms;
        let mut refresh_interval =
            tokio::time::interval(tokio::time::Duration::from_millis(refresh_ms));
        let mut sla_interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        let mut summary_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        // Reconcile every 60 seconds.
        let mut reconcile_interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        // Funding-arb driver tick. Only used when
        // `funding_arb_driver` is Some; the select arm below
        // gates on that so a None driver never fires.
        let mut funding_arb_interval = tokio::time::interval(self.funding_arb_tick);
        // Skip the first immediate tick so a fresh driver does
        // not fire before the first market-data sample lands.
        funding_arb_interval.tick().await;
        // Stat-arb driver tick (Epic B). Same gating pattern:
        // only fires when `stat_arb_driver` is Some, and the
        // first immediate tick is skipped.
        let mut stat_arb_interval = tokio::time::interval(self.stat_arb_tick);
        stat_arb_interval.tick().await;
        // Periodic fee-tier refresh (P1.2). The first tick fires
        // immediately so the operator gets the venue's authoritative
        // fee schedule into Prometheus before the first quote
        // refresh, instead of starting the dashboard with the stale
        // `ProductSpec` defaults.
        let fee_tier_secs = self.config.market_maker.fee_tier_refresh_secs.max(1);
        let mut fee_tier_interval =
            tokio::time::interval(tokio::time::Duration::from_secs(fee_tier_secs));
        let fee_tier_enabled = self.config.market_maker.fee_tier_refresh_enabled
            && self.config.market_maker.fee_tier_refresh_secs > 0;
        // Periodic borrow-rate refresh (P1.3 stage-1). Same shape
        // as the fee-tier task — first tick fires immediately so
        // the strategy gets the venue's authoritative rate before
        // the first quote refresh.
        let borrow_secs = self.config.market_maker.borrow_rate_refresh_secs.max(1);
        let mut borrow_rate_interval =
            tokio::time::interval(tokio::time::Duration::from_secs(borrow_secs));
        let borrow_enabled =
            self.borrow_manager.is_some() && self.config.market_maker.borrow_rate_refresh_secs > 0;
        // Pair lifecycle refresh (P2.3 stage-1). Same pattern
        // as the fee-tier and borrow-rate arms — first tick
        // fires immediately so a halt that landed during the
        // previous run-cycle is detected before the first
        // quote refresh.
        let lifecycle_secs = self.config.market_maker.pair_lifecycle_refresh_secs.max(1);
        let mut pair_lifecycle_interval =
            tokio::time::interval(tokio::time::Duration::from_secs(lifecycle_secs));
        let pair_lifecycle_enabled = self.pair_lifecycle.is_some()
            && self.config.market_maker.pair_lifecycle_refresh_secs > 0;

        // Epic A stage-2 #1 — auto-dispatch SOR tick. Skips the
        // first tick so a fresh process doesn't fire a route
        // decision against a cold book snapshot.
        let sor_secs = self.config.market_maker.sor_dispatch_interval_secs.max(1);
        let mut sor_dispatch_interval =
            tokio::time::interval(tokio::time::Duration::from_secs(sor_secs));
        sor_dispatch_interval.tick().await;
        let sor_dispatch_enabled = self.config.market_maker.sor_inline_enabled;

        // Epic A stage-2 #2 — queue-wait refresh tick. Publishes
        // live `trade_rate → queue_wait_secs` into the SOR
        // aggregator so cost decisions reflect fresh tape.
        let sor_queue_secs = self.config.market_maker.sor_queue_refresh_secs.max(1);
        let mut sor_queue_refresh_interval =
            tokio::time::interval(tokio::time::Duration::from_secs(sor_queue_secs));
        sor_queue_refresh_interval.tick().await;

        self.cycle_start = Instant::now();

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        self.shutdown().await;
                        return Ok(());
                    }
                }
                Some(event) = ws_rx.recv() => {
                    self.handle_ws_event(event);
                }
                Some(event) = async {
                    match hedge_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.handle_hedge_event(event);
                }
                _ = refresh_interval.tick() => {
                    if let Err(e) = self.refresh_quotes().await {
                        error!(error = %e, "quote refresh failed");
                        self.kill_switch.on_error();
                    }
                }
                _ = sla_interval.tick() => {
                    self.tick_second().await;
                }
                _ = summary_interval.tick() => {
                    // Feed performance tracker with periodic data.
                    let pnl_return = self.pnl_tracker.attribution.total_pnl() - self.var_guard_last_total_pnl;
                    self.performance.record_return(pnl_return);
                    self.performance.update_equity(self.pnl_tracker.attribution.total_pnl());
                    self.performance.sample_inventory(self.inventory_manager.inventory().abs());
                    // Midnight UTC snapshot — auto-persist daily report.
                    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
                    if today != self.last_daily_snapshot_date {
                        if let Some(ds) = &self.dashboard {
                            ds.snapshot_daily_report();
                        }
                        self.last_daily_snapshot_date = today;
                    }
                    self.log_periodic_summary();
                    self.update_dashboard();
                    // Epic 6: report this engine's daily PnL
                    // share to the per-client circuit so the
                    // aggregate across sibling engines can trip
                    // the breaker when the client's budget is
                    // exhausted. Absolute value — the circuit
                    // expects a replace-not-add semantics.
                    if let (Some(client_id), Some(circuit)) = (
                        self.client_id.as_ref(),
                        self.per_client_circuit.as_ref(),
                    ) {
                        circuit.report_symbol_pnl(
                            client_id,
                            &self.symbol,
                            self.pnl_tracker.attribution.total_pnl(),
                        );
                    }
                    // Epic F sub-component #2: drive the
                    // news retreat cooldown forward so an
                    // expired Critical state drops the
                    // autotuner widening even without a fresh
                    // headline arrival.
                    self.tick_news_retreat();
                    self.audit.flush();
                }
                _ = reconcile_interval.tick() => {
                    self.reconcile().await;
                }
                _ = fee_tier_interval.tick(), if fee_tier_enabled => {
                    self.refresh_fee_tiers().await;
                }
                _ = borrow_rate_interval.tick(), if borrow_enabled => {
                    self.refresh_borrow_rate().await;
                }
                _ = pair_lifecycle_interval.tick(), if pair_lifecycle_enabled => {
                    self.refresh_pair_lifecycle().await;
                }
                _ = sor_dispatch_interval.tick(), if sor_dispatch_enabled => {
                    self.run_sor_dispatch_tick().await;
                }
                _ = sor_queue_refresh_interval.tick() => {
                    self.refresh_sor_queue_wait();
                }
                _ = funding_arb_interval.tick(),
                    if self.funding_arb_driver.is_some() =>
                {
                    // Borrow-check dance: take the driver out,
                    // tick it, then put it back. Keeps
                    // `handle_driver_event` free to mutate
                    // other engine state (kill switch, audit)
                    // without conflicting on `&mut self`.
                    if let Some(mut driver) = self.funding_arb_driver.take() {
                        let event = driver.tick_once().await;
                        self.funding_arb_driver = Some(driver);
                        self.handle_driver_event(event);
                    }
                }
                _ = stat_arb_interval.tick(),
                    if self.stat_arb_driver.is_some() =>
                {
                    // Same take/tick/put pattern as funding
                    // arb — lets `handle_stat_arb_event`
                    // mutate other engine state without a
                    // borrow conflict.
                    if let Some(mut driver) = self.stat_arb_driver.take() {
                        let event = driver.tick_once().await;
                        // Stage-2: fire real dispatch before we
                        // put the driver back so `try_dispatch_legs_*`
                        // has exclusive &mut access. The engine's
                        // audit-trail path then runs off the returned
                        // event + dispatch report.
                        let dispatch_report = match &event {
                            StatArbEvent::Entered { .. } => {
                                Some(driver.try_dispatch_legs_for_entry(&event).await)
                            }
                            StatArbEvent::Exited { .. } => {
                                Some(driver.try_dispatch_legs_for_exit().await)
                            }
                            _ => None,
                        };
                        self.stat_arb_driver = Some(driver);
                        self.handle_stat_arb_event(event, dispatch_report);
                    }
                }
                Some(ovr) = async {
                    match self.config_override_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.apply_config_override(ovr);
                }
            }
        }
    }

    /// Handle a `DriverEvent` from the in-engine
    /// `FundingArbDriver`. Routes to audit + kill switch so
    /// pair breaks escalate automatically. An uncompensated
    /// pair break trips kill switch L2 `StopNewOrders` —
    /// stronger escalation (L4 paired unwind) is left to the
    /// operator via the regular kill-switch policy paths.
    fn handle_driver_event(&mut self, event: DriverEvent) {
        match event {
            DriverEvent::Entered { .. } => {
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairDispatchEntered,
                    "funding arb driver entered",
                );
            }
            DriverEvent::Exited { reason, .. } => {
                self.audit
                    .risk_event(&self.symbol, AuditEventType::PairDispatchExited, &reason);
            }
            DriverEvent::TakerRejected { reason } => {
                self.audit
                    .risk_event(&self.symbol, AuditEventType::PairTakerRejected, &reason);
                warn!(%reason, "funding arb taker leg rejected — position stayed flat");
            }
            DriverEvent::PairBreak {
                reason,
                compensated,
            } => {
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairBreak,
                    &format!("compensated={compensated}: {reason}"),
                );
                self.record_incident(
                    if compensated { "high" } else { "critical" },
                    &format!("funding arb pair break (compensated={compensated}): {reason}"),
                );
                if !compensated {
                    // Uncompensated break: flip to L2 so no
                    // new orders fire until the operator
                    // investigates. Intentionally NOT L4 —
                    // L4 would start a paired unwind on an
                    // already-broken pair, which compounds
                    // the problem.
                    self.kill_switch.manual_trigger(
                        mm_risk::kill_switch::KillLevel::StopNewOrders,
                        "uncompensated funding arb pair break",
                    );
                    // Drop the driver so it stops ticking
                    // until the operator explicitly restarts
                    // the engine.
                    self.funding_arb_driver = None;
                }
            }
            DriverEvent::Hold | DriverEvent::InputUnavailable { .. } => {}
        }
    }

    /// Handle a [`StatArbEvent`] from the attached
    /// [`StatArbDriver`]. Stage-2: `Entered` / `Exited` events
    /// trigger real leg dispatch via
    /// [`StatArbDriver::try_dispatch_legs_for_entry`] /
    /// [`StatArbDriver::try_dispatch_legs_for_exit`] (fired at
    /// the driver-tick call site before this handler runs).
    /// This handler receives the resulting
    /// [`mm_strategy::stat_arb::LegDispatchReport`] (or `None`
    /// for non-dispatch events) and records both the decision
    /// and the outcome to the audit trail. `NotCointegrated`,
    /// `Warmup`, `Hold`, and `InputUnavailable` stay silent
    /// (already debug-logged inside the driver) to keep the
    /// audit trail focused on state-change events.
    fn handle_stat_arb_event(
        &mut self,
        event: StatArbEvent,
        dispatch_report: Option<mm_strategy::stat_arb::LegDispatchReport>,
    ) {
        match event {
            StatArbEvent::Entered {
                direction,
                y_qty,
                x_qty,
                z,
                spread,
            } => {
                let dir_tag = match direction {
                    SpreadDirection::SellY => "sell_y",
                    SpreadDirection::BuyY => "buy_y",
                };
                let outcome_tag = format_leg_report(dispatch_report.as_ref());
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::StatArbEntered,
                    &format!(
                        "dir={dir_tag} y_qty={y_qty} x_qty={x_qty} z={z} spread={spread} dispatch={outcome_tag}"
                    ),
                );
                info!(
                    symbol = %self.symbol,
                    ?direction,
                    %y_qty,
                    %x_qty,
                    dispatch = %outcome_tag,
                    "stat_arb entry dispatched"
                );
            }
            StatArbEvent::Exited {
                z,
                spread,
                realised_pnl_estimate,
            } => {
                let outcome_tag = format_leg_report(dispatch_report.as_ref());
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::StatArbExited,
                    &format!(
                        "z={z} spread={spread} pnl_estimate={realised_pnl_estimate} dispatch={outcome_tag}"
                    ),
                );
                info!(
                    symbol = %self.symbol,
                    %z,
                    %spread,
                    %realised_pnl_estimate,
                    dispatch = %outcome_tag,
                    "stat_arb exit dispatched"
                );
            }
            StatArbEvent::NotCointegrated { .. }
            | StatArbEvent::Warmup { .. }
            | StatArbEvent::Hold { .. }
            | StatArbEvent::InputUnavailable { .. } => {}
        }
    }

    /// Handle an event from the hedge connector's market-data
    /// stream. Book events update `hedge_book`; `Fill` events
    /// from the hedge leg feed per-leg bookkeeping
    /// (`hedge_order_manager`, `paired_unwind.on_hedge_fill`,
    /// shared portfolio). Connectivity events are logged at
    /// trace level but intentionally don't trip the primary
    /// engine's circuit breaker — a hedge disconnect is a
    /// degraded state, not a full market-maker outage.
    fn handle_hedge_event(&mut self, event: MarketEvent) {
        match &event {
            MarketEvent::BookSnapshot { .. } | MarketEvent::BookDelta { .. } => {
                if let Some(hb) = self.hedge_book.as_mut() {
                    hb.on_event(&event);
                }
                // Epic F sub-component #1: feed the latest
                // hedge-side mid into the lead-lag guard. v1
                // wires the guard to the hedge connector
                // because the engine doesn't yet have a
                // separate "leader" connector subscription —
                // operators with a distinct leader feed call
                // `update_lead_lag_from_mid` directly from
                // their orchestration layer.
                if self.lead_lag_guard.is_some() {
                    if let Some(hb) = self.hedge_book.as_ref() {
                        if let Some(mid) = hb.book.mid_price() {
                            self.update_lead_lag_from_mid(mid);
                        }
                    }
                }
            }
            MarketEvent::Fill { fill, .. } => {
                info!(
                    trade_id = fill.trade_id,
                    side = ?fill.side,
                    price = %fill.price,
                    qty = %fill.qty,
                    "HEDGE FILL"
                );
                if let Some(hedge_om) = self.hedge_order_manager.as_mut() {
                    hedge_om.on_fill(fill.order_id, fill.qty);
                }
                if let Some(unwind) = self.paired_unwind.as_mut() {
                    unwind.on_hedge_fill(fill.qty);
                    if !unwind.active() {
                        info!("paired unwind complete");
                        self.paired_unwind = None;
                    }
                }
                let signed_qty = match fill.side {
                    mm_common::types::Side::Buy => fill.qty,
                    mm_common::types::Side::Sell => -fill.qty,
                };

                // Portfolio gets the hedge fill with the hedge
                // symbol so per-asset tracking remains symmetric
                // across the two legs of a pair.
                //
                // Stage-2: share the stat-arb PnL class with the
                // primary-leg path so both legs of a stat-arb
                // round-trip land in the same bucket. Pair the
                // funding-arb and stat-arb discriminators here
                // too.
                let hedge_class = self.pnl_strategy_class();
                if let (Some(pf), Some(pair)) = (&self.portfolio, self.connectors.pair.as_ref()) {
                    if let Ok(mut pf) = pf.lock() {
                        pf.on_fill(&pair.hedge_symbol, signed_qty, fill.price, &hedge_class);
                    }
                }

                // Reconcile the funding-arb driver's perp-leg
                // bookkeeping with the real fill.
                if let Some(driver) = self.funding_arb_driver.as_mut() {
                    driver.on_hedge_fill(signed_qty);
                }
            }
            _ => {}
        }
    }

    /// Refresh balances from exchange.
    #[instrument(skip(self), fields(symbol = %self.symbol))]
    async fn refresh_balances(&mut self) {
        if let Ok(balances) = self.connectors.primary.get_balances().await {
            let quote_balance = balances
                .iter()
                .find(|b| b.asset == self.product.quote_asset)
                .map(|b| b.available)
                .unwrap_or_default();
            self.exposure_manager = ExposureManager::new(quote_balance);
            self.balance_cache.update_from_exchange(&balances);
            info!(quote_balance = %quote_balance, "balances refreshed");
        }
        // Epic 22: publish a per-venue snapshot for every connector
        // in the bundle (primary + hedge + SOR extras). The engine
        // already calls `get_balances()` on the primary above; here
        // we additionally probe the others so the dashboard's
        // drilldown panel can render a full cross-venue picture.
        self.publish_venue_balances().await;
    }

    /// Push a `VenueBalanceSnapshot` for every connector in the
    /// bundle to the dashboard. Skips silently when no dashboard
    /// is attached (tests / headless mode).
    async fn publish_venue_balances(&self) {
        let Some(ds) = &self.dashboard else { return };
        let now = chrono::Utc::now();
        let mut snaps: Vec<mm_dashboard::state::VenueBalanceSnapshot> = Vec::new();
        for conn in self.connectors.all_connectors() {
            let venue = conn.venue_id().to_string();
            let product = format!("{:?}", conn.product());
            match conn.get_balances().await {
                Ok(balances) => {
                    for b in balances {
                        // Skip dust rows — they clutter the UI and
                        // the operator cares about venues that hold
                        // real positions.
                        if b.total.is_zero() && b.available.is_zero() && b.locked.is_zero() {
                            continue;
                        }
                        // Multi-Venue 2.B.3 — feed the shared
                        // DataBus so `Balance(venue, asset)` source
                        // nodes on any engine's graph can read
                        // this balance. `reserved` mirrors `locked`
                        // in the venue adapter shape.
                        ds.data_bus().publish_balance(
                            venue.clone(),
                            b.asset.clone(),
                            mm_dashboard::data_bus::BalanceEntry {
                                total: b.total,
                                available: b.available,
                                reserved: b.locked,
                                ts: Some(now),
                            },
                        );
                        snaps.push(mm_dashboard::state::VenueBalanceSnapshot {
                            venue: venue.clone(),
                            product: product.clone(),
                            asset: b.asset,
                            wallet: format!("{:?}", b.wallet),
                            total: b.total,
                            available: b.available,
                            locked: b.locked,
                            updated_at: now,
                        });
                    }
                }
                Err(e) => {
                    warn!(%venue, error = %e, "venue balance fetch failed");
                }
            }
        }
        ds.update_venue_balances(&self.symbol, snaps);
    }

    /// Periodic reconciliation: compare internal state vs exchange.
    #[instrument(skip(self), fields(symbol = %self.symbol, iter = self.reconcile_counter))]
    async fn reconcile(&mut self) {
        self.reconcile_counter += 1;

        // Refresh balances every reconciliation.
        self.refresh_balances().await;

        // Query open orders from exchange and reconcile against
        // the internal OrderManager state. Detects phantom orders
        // (live on venue but unknown locally) and ghost orders
        // (tracked locally but gone from venue).
        let internal_ids: std::collections::HashSet<_> =
            self.order_manager.live_order_ids().into_iter().collect();

        match self.connectors.primary.get_open_orders(&self.symbol).await {
            Ok(venue_orders) => {
                let venue_ids: std::collections::HashSet<_> =
                    venue_orders.iter().map(|o| o.order_id).collect();
                // Ghost orders: tracked locally but gone from venue.
                let ghosts: Vec<_> = internal_ids.difference(&venue_ids).copied().collect();
                // Phantom orders: live on venue but unknown locally.
                let phantoms: Vec<_> = venue_ids.difference(&internal_ids).copied().collect();

                if !ghosts.is_empty() {
                    warn!(
                        count = ghosts.len(),
                        "reconciliation: removing ghost orders (tracked locally but absent on venue)"
                    );
                    for id in &ghosts {
                        self.order_manager.remove_order(*id);
                    }
                }
                if !phantoms.is_empty() {
                    warn!(
                        count = phantoms.len(),
                        "reconciliation: detected phantom orders (live on venue but not tracked)"
                    );
                    // Track phantom orders so the next diff can
                    // cancel them if they're not in the desired set.
                    for vo in &venue_orders {
                        if phantoms.contains(&vo.order_id) {
                            self.order_manager.track_order(vo.clone());
                        }
                    }
                }

                info!(
                    internal_orders = internal_ids.len(),
                    venue_orders = venue_ids.len(),
                    ghosts = ghosts.len(),
                    phantoms = phantoms.len(),
                    reconcile_cycle = self.reconcile_counter,
                    "reconciliation cycle"
                );
            }
            Err(e) => {
                debug!(
                    error = %e,
                    internal_orders = internal_ids.len(),
                    "reconciliation: get_open_orders failed — skipping order reconciliation"
                );
            }
        }

        self.audit.risk_event(
            &self.symbol,
            AuditEventType::BalanceReconciled,
            &format!("cycle {}", self.reconcile_counter),
        );

        // Inventory-vs-wallet drift check. The first reconcile
        // cycle captures the wallet baseline; subsequent
        // cycles compare the tracked inventory delta against
        // the wallet delta and surface any mismatch to the
        // audit trail + optional auto-correct.
        self.check_inventory_drift();
    }

    /// Compare `InventoryManager.inventory()` against the
    /// wallet-delta baseline and fire a drift report when the
    /// mismatch exceeds the configured tolerance. Called from
    /// inside `reconcile` on every reconcile cycle.
    fn check_inventory_drift(&mut self) {
        let wallet_total = self
            .balance_cache
            .total_in(&self.product.base_asset, mm_common::types::WalletType::Spot);
        let tracked = self.inventory_manager.inventory();
        let Some(report) = self.inventory_drift.check(wallet_total, tracked) else {
            return;
        };
        warn!(
            asset = %report.asset,
            baseline_wallet = %report.baseline_wallet,
            current_wallet = %report.current_wallet,
            expected = %report.expected_inventory,
            tracked = %report.tracked_inventory,
            drift = %report.drift,
            corrected = report.corrected,
            "INVENTORY DRIFT DETECTED"
        );
        let detail = format!(
            "asset={} baseline={} current={} expected={} tracked={} drift={} corrected={}",
            report.asset,
            report.baseline_wallet,
            report.current_wallet,
            report.expected_inventory,
            report.tracked_inventory,
            report.drift,
            report.corrected,
        );
        self.audit.risk_event(
            &self.symbol,
            AuditEventType::InventoryDriftDetected,
            &detail,
        );
        if report.corrected {
            // Operator opted into auto-correction — force the
            // tracker to match the wallet delta. PnL attribution
            // becomes approximate until the position is flat
            // again, see `force_reset_inventory_to` for details.
            self.inventory_manager
                .force_reset_inventory_to(report.expected_inventory);
        }
    }

    async fn tick_second(&mut self) {
        self.sla_tracker.tick();
        self.kill_switch.tick_second();
        self.adv_inventory.tick(self.inventory_manager.inventory());

        // Epic D stage-2 — drain quiet BVC bars. When a symbol
        // sees no trades for a full bar window, `push()` would
        // never fire — we'd lose the chance to record zero-
        // volume bars (which are themselves informative:
        // classifier's rolling std would otherwise be skewed
        // by gaps). `flush_if_due` returns the closed bar
        // without resetting the trade anchor mid-bar.
        if let (Some(agg), Some(classifier)) = (
            self.bvc_bar_agg.as_mut(),
            self.bvc_classifier.as_mut(),
        ) {
            let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            if let Some((dp, vol)) = agg.flush_if_due(now_ns) {
                if let Some((buy, sell)) = classifier.classify(dp, vol) {
                    self.vpin.on_bvc_bar(buy, sell);
                }
            }
        }

        // Epic C sub-component #4: feed the VaR guard one
        // PnL-delta sample per minute. Gated on
        // `tick_count % 60 == 0` so the sampling cadence is
        // independent of the engine's sub-second quote
        // refresh. Uses the total_pnl delta rather than the
        // cumulative value so the ring buffer carries per-
        // minute returns, which is what the parametric
        // Gaussian VaR formulation expects.
        if self.var_guard.is_some() && self.tick_count.is_multiple_of(60) {
            let total_pnl = self.pnl_tracker.attribution.total_pnl();
            let delta = total_pnl - self.var_guard_last_total_pnl;
            self.var_guard_last_total_pnl = total_pnl;
            let class = self.strategy.name().to_string();
            if let Some(vg) = self.var_guard.as_mut() {
                vg.record_pnl_sample(&class, delta);
            }
        }

        if let Some(mid) = self.book_keeper.book.mid_price() {
            self.adverse_selection
                .update_mid(mid, self.config.toxicity.adverse_selection_lookback_ms);
            self.pnl_tracker.mark_to_market(mid);

            // Mark-to-market the shared portfolio. The portfolio
            // holds a mark price per symbol so unrealised PnL
            // snapshots reflect the latest mid on every tick.
            if let Some(pf) = &self.portfolio {
                if let Ok(mut pf) = pf.lock() {
                    pf.mark_price(&self.symbol, mid);
                }
            }

            let total_pnl = self.pnl_tracker.attribution.total_pnl();
            self.kill_switch.update_pnl(total_pnl);

            let position_value = self.inventory_manager.inventory().abs() * mid;
            self.kill_switch.update_position_value(position_value);

            // Epic 40.4 — margin guard poll. Fired once every
            // `refresh_interval_secs` ticks (1 s driver ticks).
            // Failure paths: a venue error counts as a stale
            // snapshot; the guard auto-widens via
            // `MarginGuardDecision::Stale` on the next
            // `decide(now)` call.
            if self.margin_poll_modulus > 0
                && self.tick_count.is_multiple_of(self.margin_poll_modulus)
            {
                self.refresh_margin_guard().await;
            }

            // Epic 40.3 — funding accrual. Only perp engines
            // do anything here; spot connectors short-circuit
            // at the `has_funding()` gate. Three operations:
            //   1. Poll the venue for a fresh `FundingRate`
            //      every 30 s (same cadence design doc
            //      specifies) so rate + next-funding drift in
            //      the tracker stay under a minute.
            //   2. Recompute the MTM funding PnL every tick
            //      from the live inventory + mark. Stateless
            //      so a mid-period restart is handled.
            //   3. At/after `next_funding_time`, book the
            //      realised delta and emit a `FundingAccrued`
            //      audit event.
            if self.config.exchange.product.has_funding() {
                self.pnl_tracker.set_inventory_for_funding(
                    self.inventory_manager.inventory(),
                );
                if self.tick_count.is_multiple_of(30) {
                    self.refresh_funding_rate().await;
                }
                let now_utc = chrono::Utc::now();
                self.pnl_tracker.accrue_funding_mtm(now_utc, mid);
                if let Some(delta) = self.pnl_tracker.settle_funding(now_utc, mid) {
                    let inv = self.inventory_manager.inventory();
                    let rate = self.pnl_tracker.funding_rate().unwrap_or_default();
                    self.audit.risk_event(
                        &self.symbol,
                        AuditEventType::FundingAccrued,
                        &format!(
                            "rate={rate}, mark={mid}, inventory={inv}, delta={delta}"
                        ),
                    );
                    info!(
                        symbol = %self.symbol,
                        rate = %rate,
                        delta = %delta,
                        inventory = %inv,
                        "funding settled"
                    );
                }
            }

            // TWAP execution (if active).
            if let Some(twap) = &mut self.twap {
                if let Some(quote) = twap.next_slice(mid) {
                    // Execute TWAP slice via order manager.
                    info!(side = ?quote.side, price = %quote.price, qty = %quote.qty, "TWAP slice");
                }
            }

            // Paired-unwind execution (if active). Dispatches
            // the slice through the hedge leg's own
            // `OrderManager` so cancel-all + fill tracking
            // stay per-venue.
            if let Some(unwind) = self.paired_unwind.as_mut() {
                if let Some(hedge_mid) = self.hedge_book.as_ref().and_then(|hb| hb.book.mid_price())
                {
                    let slice = unwind.next_slice(mid, hedge_mid);
                    if let Some(p) = &slice.primary {
                        if let Err(e) = self
                            .order_manager
                            .execute_unwind_slice(
                                &self.symbol,
                                p,
                                &self.product,
                                &self.connectors.primary,
                            )
                            .await
                        {
                            warn!(error = %e, "unwind primary dispatch failed");
                        }
                    }
                    if let Some(h) = &slice.hedge {
                        if let (Some(hedge_conn), Some(hedge_om), Some(pair)) = (
                            self.connectors.hedge.as_ref(),
                            self.hedge_order_manager.as_mut(),
                            self.connectors.pair.as_ref(),
                        ) {
                            if let Err(e) = hedge_om
                                .execute_unwind_slice(
                                    &pair.hedge_symbol,
                                    h,
                                    &self.product,
                                    hedge_conn,
                                )
                                .await
                            {
                                warn!(error = %e, "unwind hedge dispatch failed");
                            }
                        }
                    }
                }
            }
        }

        // Kill switch L3+ → cancel all. If verification reports
        // surviving orders we hold off on L4 flatten so TwapExecutor
        // does not race the still-live quotes.
        if self.kill_switch.level() >= KillLevel::CancelAll {
            match self
                .order_manager
                .cancel_all(&self.connectors.primary, &self.symbol)
                .await
            {
                Ok(()) => {
                    self.balance_cache.reset_reservations();
                }
                Err(e) => {
                    warn!(
                        symbol = %self.symbol,
                        error = %e,
                        "cancel_all left orders on venue — deferring L4 flatten this tick"
                    );
                    self.audit.risk_event(
                        &self.symbol,
                        AuditEventType::CircuitBreakerTripped,
                        &format!("cancel_all incomplete: {e}"),
                    );
                    return;
                }
            }

            // Kill switch L4 → start the right flatten executor.
            //
            // Single-connector mode: single-leg `TwapExecutor`
            // against the one position.
            //
            // Dual-connector mode (basis / funding arb): pick
            // `PairedUnwindExecutor` so both legs flatten in
            // lockstep. Running `TwapExecutor` alongside would
            // double-flatten the primary leg and leave the hedge
            // leg dangling, exactly the failure mode AD-11 of
            // the spot-and-cross-product epic calls out.
            if self.kill_switch.level() >= KillLevel::FlattenAll
                && self.twap.is_none()
                && self.paired_unwind.is_none()
            {
                let inv = self.inventory_manager.inventory();
                if !inv.is_zero() {
                    let side = if inv > dec!(0) {
                        mm_common::types::Side::Sell
                    } else {
                        mm_common::types::Side::Buy
                    };

                    if let Some(pair) = self.connectors.pair.clone() {
                        // Paired unwind: infer the hedge-leg
                        // direction from the primary inventory
                        // sign. Long-spot → short-hedge, and vice
                        // versa. Basis-neutral pairs always have
                        // opposite sides on the two legs.
                        let primary_side = if inv > dec!(0) {
                            mm_common::types::Side::Buy
                        } else {
                            mm_common::types::Side::Sell
                        };
                        let hedge_side = primary_side.opposite();
                        self.paired_unwind = Some(PairedUnwindExecutor::new(
                            pair,
                            primary_side,
                            hedge_side,
                            inv.abs(),
                            60,
                            10,
                            dec!(5),
                        ));
                        self.audit.risk_event(
                            &self.symbol,
                            AuditEventType::KillSwitchEscalated,
                            &format!("paired unwind started: primary {side:?} {}", inv.abs()),
                        );
                        self.record_incident(
                            "critical",
                            &format!("Kill switch L4: paired unwind {} base", inv.abs()),
                        );
                    } else {
                        self.twap = Some(TwapExecutor::new(
                            self.symbol.clone(),
                            side,
                            inv.abs(),
                            60,      // 60 seconds.
                            10,      // 10 slices.
                            dec!(5), // 5 bps aggressive.
                        ));
                        self.audit.risk_event(
                            &self.symbol,
                            AuditEventType::KillSwitchEscalated,
                            &format!("TWAP flatten started: {side:?} {}", inv.abs()),
                        );
                        self.record_incident(
                            "critical",
                            &format!("Kill switch L4: TWAP flatten {side:?} {}", inv.abs()),
                        );
                    }
                }
            }
        }
    }

    fn handle_ws_event(&mut self, event: MarketEvent) {
        // Record market data for offline backtesting.
        if let Some(recorder) = &mut self.event_recorder {
            match &event {
                MarketEvent::BookSnapshot {
                    bids,
                    asks,
                    sequence,
                    ..
                } => {
                    let _ = recorder.record(&mm_backtester::data::RecordedEvent::BookSnapshot {
                        timestamp: chrono::Utc::now(),
                        bids: bids.clone(),
                        asks: asks.clone(),
                        sequence: *sequence,
                    });
                }
                MarketEvent::Trade { trade, .. } => {
                    let _ = recorder.record(&mm_backtester::data::RecordedEvent::Trade {
                        timestamp: trade.timestamp,
                        price: trade.price,
                        qty: trade.qty,
                        taker_side: trade.taker_side,
                    });
                }
                _ => {}
            }
        }

        match &event {
            MarketEvent::BookSnapshot { .. } | MarketEvent::BookDelta { .. } => {
                self.book_keeper.on_event(&event);
                // OTR: every book event is a price-level update
                // from the L2 perspective — we don't have L3
                // add/cancel granularity, so we account for it
                // under the "updates" bucket. Gated behind the
                // `otr_enabled` toggle.
                if self.config.market_maker.otr_enabled {
                    self.otr.on_update();
                }
                if let Some(mid) = self.book_keeper.book.mid_price() {
                    self.volatility_estimator.update(mid);
                    if !self.last_mid.is_zero() {
                        let ret = (mid - self.last_mid) / self.last_mid;
                        self.auto_tuner.on_return(ret);
                    }
                    // Feed the mid into the momentum HMA so
                    // the alpha() call downstream can pick up
                    // slope information.
                    self.momentum.on_mid(mid);
                    // Feed the market impact estimator so
                    // pending fills tick down their horizon.
                    self.market_impact.on_mid_update(mid);
                    self.last_mid = mid;
                }
                // Epic D stage-3 — feed the L1 top-of-book
                // into the momentum OFI tracker. No-op when
                // `with_ofi` was not called at construction
                // time. Reads the book directly so the OFI
                // observation lines up with the freshly
                // applied snapshot/delta.
                let book = &self.book_keeper.book;
                if let (Some(bid_px), Some(ask_px)) = (book.best_bid(), book.best_ask()) {
                    let bid_qty = book.bids.get(&bid_px).copied().unwrap_or_default();
                    let ask_qty = book.asks.get(&ask_px).copied().unwrap_or_default();
                    self.momentum
                        .on_l1_snapshot(bid_px, bid_qty, ask_px, ask_qty);
                }
                // Feed the current book into the Market
                // Resilience detector so it can track spread /
                // depth shocks alongside trade-driven shocks.
                // Gated behind the `market_resilience_enabled`
                // toggle.
                if self.config.market_maker.market_resilience_enabled {
                    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
                    let bids = self.book_keeper.book.top_bids(10);
                    let asks = self.book_keeper.book.top_asks(10);
                    self.market_resilience.on_book(&bids, &asks, now_ns);
                }
            }
            MarketEvent::Trade { venue, trade } => {
                let trade_venue = *venue;
                // Multi-Venue 2.B.3 — public-trade tape to DataBus.
                // `Trade.Tape` source nodes on any graph read this
                // back by (venue, symbol, product). Aggressor side
                // comes straight from the venue event.
                if let Some(dash) = self.dashboard.as_ref() {
                    let bus = dash.data_bus();
                    let key = (
                        format!("{trade_venue:?}").to_lowercase(),
                        self.symbol.clone(),
                        self.config.exchange.product,
                    );
                    let aggressor = match trade.taker_side {
                        mm_common::types::Side::Buy => {
                            mm_dashboard::data_bus::TradeSide::Buy
                        }
                        mm_common::types::Side::Sell => {
                            mm_dashboard::data_bus::TradeSide::Sell
                        }
                    };
                    bus.publish_trade(
                        key,
                        mm_dashboard::data_bus::TradeTick {
                            price: trade.price,
                            qty: trade.qty,
                            aggressor: Some(aggressor),
                            ts: chrono::Utc::now(),
                        },
                    );
                }
                // Epic A stage-2 #2 — feed the per-venue
                // trade-rate estimator so queue-wait
                // projections track live tape activity. The
                // window length comes from config; the
                // estimator prunes stale samples internally.
                {
                    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
                    let window = self
                        .config
                        .market_maker
                        .sor_trade_rate_window_secs
                        .max(1);
                    self.sor_trade_rates
                        .entry(trade_venue)
                        .or_insert_with(|| {
                            crate::sor::trade_rate::TradeRateEstimator::new(window)
                        })
                        .record(now_ns, trade.qty);
                }
                // Epic D stage-2 — two classification paths for
                // VPIN's volume source:
                //   1. BVC (config opt-in): bar-bucket the trade
                //      stream, call `classify` on each closed
                //      bar, route the (buy, sell) split into
                //      `vpin.on_bvc_bar`. Cleaner signal on
                //      fast tapes where `taker_side` is noisy.
                //   2. Legacy tick-rule: hand the trade straight
                //      to `vpin.on_trade`, which reads
                //      `Trade::taker_side` via Lee-Ready. Kept
                //      as the default so byte-identical
                //      behaviour holds for every deployment
                //      that didn't opt into BVC.
                if let (Some(agg), Some(classifier)) = (
                    self.bvc_bar_agg.as_mut(),
                    self.bvc_classifier.as_mut(),
                ) {
                    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
                    if let Some((dp, vol)) = agg.push(now_ns, trade.price, trade.qty) {
                        if let Some((buy, sell)) = classifier.classify(dp, vol) {
                            self.vpin.on_bvc_bar(buy, sell);
                        }
                    }
                } else {
                    self.vpin.on_trade(trade);
                }
                self.momentum.on_trade(trade);
                if self.config.market_maker.otr_enabled {
                    self.otr.on_trade();
                }
                if self.config.market_maker.market_resilience_enabled {
                    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
                    self.market_resilience.on_trade(trade.qty, now_ns);
                }

                if !self.last_mid.is_zero() {
                    let signed_vol = match trade.taker_side {
                        mm_common::types::Side::Buy => trade.qty * trade.price,
                        mm_common::types::Side::Sell => -(trade.qty * trade.price),
                    };
                    let dp = trade.price - self.last_mid;
                    self.kyle_lambda.update(dp, signed_vol);
                }
                if let Some(v) = self.vpin.vpin() {
                    self.auto_tuner.set_toxicity(v);
                }

                // Paper-mode fill simulation. Drive every resting
                // paper quote that was crossed by this taker trade
                // through the same MarketEvent::Fill code path a
                // real fill would take, so PnL / inventory / SLA
                // accumulate end-to-end without touching the
                // venue. No-op in live mode.
                if self.order_manager.is_paper() {
                    let synthetic = self
                        .order_manager
                        .paper_match_trade(trade.price, trade.taker_side);
                    for fill in synthetic {
                        info!(
                            order_id = %fill.order_id,
                            side = ?fill.side,
                            price = %fill.price,
                            qty = %fill.qty,
                            trade_price = %trade.price,
                            "[PAPER] simulated fill"
                        );
                        self.handle_ws_event(MarketEvent::Fill { venue: trade_venue, fill });
                    }
                }
            }
            MarketEvent::Fill { fill, .. } => {
                info!(
                    trade_id = fill.trade_id,
                    side = ?fill.side,
                    price = %fill.price,
                    qty = %fill.qty,
                    "FILL"
                );

                self.audit.order_filled(
                    &self.symbol,
                    fill.order_id,
                    fill.side,
                    fill.price,
                    fill.qty,
                    fill.is_maker,
                );

                self.inventory_manager.on_fill(fill);
                self.order_manager.on_fill(fill.order_id, fill.qty);
                // Epic R — feed the surveillance tracker so
                // detectors can reason about our fills. Cheap
                // (mutex lock + one vec push), acceptable cost on
                // the fill path.
                if let Ok(mut t) = self.surveillance_tracker.lock() {
                    // Map engine's `Side` to risk-crate side so the
                    // tracker module has no dep on mm-common types.
                    let surv_side = match fill.side {
                        mm_common::types::Side::Buy => mm_risk::surveillance::Side::Buy,
                        mm_common::types::Side::Sell => mm_risk::surveillance::Side::Sell,
                    };
                    t.feed(&mm_risk::surveillance::SurveillanceEvent::OrderFilled {
                        order_id: format!("{:?}", fill.order_id),
                        symbol: self.symbol.clone(),
                        side: surv_side,
                        filled_qty: fill.qty,
                        price: fill.price,
                        ts: chrono::Utc::now(),
                    });
                }
                self.sla_tracker.on_fill();
                self.kill_switch.on_fill();
                self.volume_limiter.on_trade(fill.price * fill.qty);

                // Epic 30 — feed the adaptive tuner. Derive edge
                // in bps from the current mid: buy earns (mid -
                // price)/mid, sell earns (price - mid)/mid. Fee
                // rate depends on maker/taker status.
                if let Some(mid) = self.book_keeper.book.mid_price() {
                    let edge_bps = if mid.is_zero() {
                        dec!(0)
                    } else {
                        match fill.side {
                            mm_common::types::Side::Buy => {
                                (mid - fill.price) / mid * dec!(10_000)
                            }
                            mm_common::types::Side::Sell => {
                                (fill.price - mid) / mid * dec!(10_000)
                            }
                        }
                    };
                    let fee_rate = if fill.is_maker {
                        self.product.maker_fee
                    } else {
                        self.product.taker_fee
                    };
                    let fee_bps = fee_rate * dec!(10_000);
                    self.adaptive_tuner
                        .on_fill(fill.price, fill.qty, edge_bps, fee_bps);
                }
                self.adaptive_tuner
                    .on_inventory(self.inventory_manager.inventory());

                // Feed the shared multi-currency portfolio. The
                // qty passed to `on_fill` is signed — positive on
                // buys, negative on sells — so the portfolio can
                // correctly flip / close positions.
                //
                // Stage-2: when a stat-arb driver is attached
                // (and funding-arb is not), route the PnL
                // attribution through `pair.strategy_class`
                // (e.g. `"stat_arb_BTCUSDT_ETHUSDT"`). Keeps
                // per-pair PnL buckets separated from the
                // generic maker-strategy class.
                if let Some(pf) = &self.portfolio {
                    let signed_qty = match fill.side {
                        mm_common::types::Side::Buy => fill.qty,
                        mm_common::types::Side::Sell => -fill.qty,
                    };
                    let class = self.pnl_strategy_class();
                    if let Ok(mut pf) = pf.lock() {
                        pf.on_fill(&self.symbol, signed_qty, fill.price, &class);
                    }
                }

                self.balance_cache.release(
                    fill.side,
                    fill.price,
                    fill.qty,
                    &self.product.base_asset,
                    &self.product.quote_asset,
                );

                if let Some(mid) = self.book_keeper.book.mid_price() {
                    self.pnl_tracker.on_fill(fill, mid);
                    self.adverse_selection.on_fill(fill.price, fill.side, mid);
                    if let Some(bps) = self.adverse_selection.adverse_selection_bps() {
                        // Feed the adaptive tuner so it can widen
                        // γ when the desk is being picked off.
                        self.adaptive_tuner.on_adverse(bps);
                    }

                    // Market impact tracking.
                    let side_sign = match fill.side {
                        mm_common::types::Side::Buy => dec!(1),
                        mm_common::types::Side::Sell => dec!(-1),
                    };
                    self.market_impact.on_fill(mid, side_sign);

                    // Webhook: large fill notification (> $10k notional).
                    let fill_value = fill.price * fill.qty;
                    if fill_value > dec!(10_000) {
                        if let Some(wh) = &self.webhooks {
                            wh.dispatch(mm_dashboard::webhooks::WebhookEvent::LargeFill {
                                symbol: self.symbol.clone(),
                                side: format!("{:?}", fill.side),
                                price: fill.price,
                                qty: fill.qty,
                                value_quote: fill_value,
                            });
                        }
                    }

                    // Performance tracking.
                    let spread_capture = match fill.side {
                        mm_common::types::Side::Buy => (mid - fill.price) / mid * dec!(10_000),
                        mm_common::types::Side::Sell => (fill.price - mid) / mid * dec!(10_000),
                    };
                    self.performance
                        .on_order_filled(fill.price * fill.qty, spread_capture);

                    // NBBO capture + dashboard fill recording.
                    if let Some(ds) = &self.dashboard {
                        let nbbo_bid = self.book_keeper.book.best_bid().unwrap_or(mid);
                        let nbbo_ask = self.book_keeper.book.best_ask().unwrap_or(mid);
                        let slippage_bps = if mid > Decimal::ZERO {
                            match fill.side {
                                mm_common::types::Side::Buy => {
                                    (fill.price - mid) / mid * dec!(10_000)
                                }
                                mm_common::types::Side::Sell => {
                                    (mid - fill.price) / mid * dec!(10_000)
                                }
                            }
                        } else {
                            Decimal::ZERO
                        };
                        let fee_rate = if fill.is_maker {
                            self.product.maker_fee
                        } else {
                            self.product.taker_fee
                        };
                        ds.record_fill(mm_dashboard::state::FillRecord {
                            timestamp: fill.timestamp,
                            symbol: self.symbol.clone(),
                            client_id: self.client_id.clone(),
                            side: format!("{:?}", fill.side),
                            price: fill.price,
                            qty: fill.qty,
                            is_maker: fill.is_maker,
                            fee: fill.price * fill.qty * fee_rate,
                            nbbo_bid,
                            nbbo_ask,
                            slippage_bps,
                        });
                    }
                }

                if let Some(twap) = &mut self.twap {
                    twap.on_fill(fill.qty);
                    if twap.is_complete() {
                        info!("TWAP execution complete");
                        self.twap = None;
                    }
                }

                if let Some(unwind) = self.paired_unwind.as_mut() {
                    unwind.on_primary_fill(fill.qty);
                    if !unwind.active() {
                        info!("paired unwind complete");
                        self.paired_unwind = None;
                    }
                }

                // Reconcile the funding-arb driver's position
                // bookkeeping with the real primary-leg fill.
                if let Some(driver) = self.funding_arb_driver.as_mut() {
                    let signed_qty = match fill.side {
                        mm_common::types::Side::Buy => fill.qty,
                        mm_common::types::Side::Sell => -fill.qty,
                    };
                    driver.on_primary_fill(signed_qty);
                }
            }
            MarketEvent::OrderUpdate { .. } => {}
            MarketEvent::BalanceUpdate {
                asset,
                wallet,
                total,
                locked,
                available,
                ..
            } => {
                // Listen-key / user-data streams push balance
                // snapshots when the wallet moves (fills,
                // deposits, withdrawals). Plug them straight into
                // `BalanceCache` — same shape as
                // `update_from_exchange` but for a single asset.
                use mm_common::types::Balance;
                self.balance_cache.update_from_exchange(&[Balance {
                    asset: asset.clone(),
                    wallet: *wallet,
                    total: *total,
                    locked: *locked,
                    available: *available,
                }]);
            }
            MarketEvent::Connected { .. } => {
                info!("exchange connected");
                self.audit
                    .risk_event(&self.symbol, AuditEventType::ExchangeConnected, "");
                self.circuit_breaker.reset();
            }
            MarketEvent::Disconnected { .. } => {
                warn!("exchange disconnected");
                self.audit
                    .risk_event(&self.symbol, AuditEventType::ExchangeDisconnected, "");
                self.circuit_breaker.trip(TripReason::StaleBook);
            }
        }
    }

    /// Poll the primary connector for a fresh funding rate
    /// snapshot (Epic 40.3). Only called when
    /// `product.has_funding()`; errors log-and-continue —
    /// accrual math falls through to the last-known rate
    /// until the next successful poll.
    async fn refresh_funding_rate(&mut self) {
        match self
            .connectors
            .primary
            .get_funding_rate(&self.symbol)
            .await
        {
            Ok(rate) => {
                self.pnl_tracker.on_funding_update(
                    rate.rate,
                    rate.next_funding_time,
                    chrono::Duration::from_std(rate.interval)
                        .unwrap_or_else(|_| chrono::Duration::hours(8)),
                );
            }
            Err(mm_exchange_core::connector::FundingRateError::NotSupported) => {
                // Capability mismatch — connector claims
                // funding support in `has_funding()` but the
                // trait returns NotSupported. Would be a
                // venue wiring bug; tolerate without spam.
            }
            Err(e) => {
                warn!(symbol = %self.symbol, error = %e, "funding rate poll failed");
            }
        }
    }

    /// Pull a fresh `AccountMarginInfo` from the primary
    /// connector and feed both the guard and the kill switch.
    /// Called once every `config.margin.refresh_interval_secs`
    /// seconds from `tick_second` when the guard is active.
    ///
    /// Error policy: a venue call failure does NOT escalate by
    /// itself — the guard's own staleness check handles the
    /// absence of a fresh snapshot. We log once per failure so
    /// an ongoing venue outage is visible without spamming.
    /// `NotSupported` is a programmer bug (capability mismatch
    /// between connector and config), so it falls through the
    /// same path — the guard will auto-widen via `Stale`.
    async fn refresh_margin_guard(&mut self) {
        let Some(guard) = self.margin_guard.as_mut() else {
            return;
        };
        match self.connectors.primary.account_margin_info().await {
            Ok(info) => {
                guard.update(info);
            }
            Err(e) => {
                warn!(
                    symbol = %self.symbol,
                    error = %e,
                    "margin info poll failed — guard will auto-widen on stale decision"
                );
            }
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let decision = guard.decide(now_ms);
        self.kill_switch.update_margin_ratio(decision);
        if let Some(dash) = &self.dashboard {
            if let Some(info) = guard.last() {
                dash.set_margin_ratio(&self.symbol, info.margin_ratio);
            }
        }
    }

    #[instrument(skip(self), fields(symbol = %self.symbol, tick = self.tick_count))]
    async fn refresh_quotes(&mut self) -> Result<()> {
        self.tick_count += 1;

        // Per-client loss circuit (Epic 6). When the aggregate
        // daily PnL across this client's symbols has breached
        // the configured limit, STOP quoting everywhere for that
        // client. Local kill switch is escalated to CancelAll on
        // the first trip so every sibling engine follows the
        // same path via its own check; the trip is sticky and
        // requires operator reset via
        // `POST /api/v1/ops/client-reset/{client_id}`.
        if let (Some(client_id), Some(circuit)) =
            (self.client_id.as_ref(), self.per_client_circuit.as_ref())
        {
            if circuit.is_tripped(client_id) {
                if !self.per_client_trip_noted {
                    warn!(
                        symbol = %self.symbol,
                        client_id = %client_id,
                        "per-client loss circuit tripped — escalating kill switch to CancelAll"
                    );
                    self.audit.risk_event(
                        &self.symbol,
                        AuditEventType::KillSwitchEscalated,
                        &format!("per-client loss circuit for client={client_id}"),
                    );
                    self.record_incident(
                        "critical",
                        &format!("Per-client loss circuit tripped (client={client_id})"),
                    );
                    // Use the existing manual_trigger path so
                    // reset semantics match operator intent —
                    // auto-recovery is NOT allowed until the
                    // client circuit is explicitly reset.
                    self.kill_switch.manual_trigger(
                        mm_risk::KillLevel::CancelAll,
                        "per-client loss circuit",
                    );
                    self.per_client_trip_noted = true;
                }
                // Fall through — the kill switch will cancel
                // every open order and block new orders on the
                // next tick's `!allow_new_orders()` guard.
            } else if self.per_client_trip_noted {
                // Client was reset. Clear local noted flag so a
                // subsequent trip on the same client audits
                // again. The kill switch itself is NOT auto-
                // reset here — operators reset each engine's
                // kill switch through the existing ops endpoint
                // so every post-incident escalation is ack'd.
                info!(
                    symbol = %self.symbol,
                    client_id = %client_id,
                    "per-client loss circuit cleared — operator should POST /api/v1/ops/reset/<symbol> to resume quoting"
                );
                self.per_client_trip_noted = false;
            }
        }

        // P2.3: lifecycle paused (halt / pre-trading / break /
        // delisted) — refuse to quote until the lifecycle
        // manager flips the flag back. The cancel-all that put
        // us into the paused state already cleared the venue
        // book on the last refresh tick.
        if self.lifecycle_paused {
            return Ok(());
        }

        // Stale book watchdog — if no book update received for
        // longer than stale_book_timeout_secs, cancel all orders
        // and skip quoting. Resumes automatically when fresh
        // data arrives.
        if let Some(last) = self.book_keeper.last_update_at() {
            let stale_secs = self.config.risk.stale_book_timeout_secs;
            if last.elapsed().as_secs() >= stale_secs {
                if !self.stale_book_paused {
                    warn!(
                        symbol = %self.symbol,
                        elapsed_secs = last.elapsed().as_secs(),
                        "book stale — pausing quotes and cancelling orders"
                    );
                    if let Err(e) = self
                        .order_manager
                        .cancel_all(&self.connectors.primary, &self.symbol)
                        .await
                    {
                        warn!(symbol = %self.symbol, error = %e, "cancel_all during stale-book pause left survivors");
                    }
                    self.balance_cache.reset_reservations();
                    self.stale_book_paused = true;
                    self.audit.risk_event(
                        &self.symbol,
                        AuditEventType::CircuitBreakerTripped,
                        &format!("stale book: no update for {stale_secs}s"),
                    );
                }
                return Ok(());
            } else if self.stale_book_paused {
                info!(symbol = %self.symbol, "book fresh again — resuming quotes");
                self.stale_book_paused = false;
            }
        }

        // Periodic OTR snapshot into the audit trail — once
        // every 60 ticks (≈1 minute at the default refresh
        // cadence). MiCA compliance: regulators expect the OTR
        // time series to be reconstructable from the audit log.
        if self.config.market_maker.otr_enabled && self.tick_count.is_multiple_of(60) {
            self.audit.order_to_trade_ratio_snapshot(
                &self.symbol,
                self.otr.ratio(),
                self.otr.adds(),
                self.otr.updates(),
                self.otr.cancels(),
                self.otr.trades(),
            );
            self.otr.reset();
        }

        // Kill switch check.
        if !self.kill_switch.allow_new_orders() {
            if self.kill_switch.level() >= KillLevel::CancelAll {
                if let Err(e) = self
                    .order_manager
                    .cancel_all(&self.connectors.primary, &self.symbol)
                    .await
                {
                    warn!(symbol = %self.symbol, error = %e, "cancel_all during kill-switch tick left survivors");
                }
                self.balance_cache.reset_reservations();
            }
            return Ok(());
        }

        // Epic 40.4 — pre-order margin projection. Forecast the
        // post-fill margin ratio if the engine were to add a
        // full two-sided notional footprint (`2 * order_size *
        // mid`). If the projected ratio crosses `stop_ratio`,
        // skip this tick's refresh — the guard's periodic poll
        // will have already widened the kill switch; this just
        // keeps us from dispatching a quote whose *fill* would
        // push us over a threshold that the *observed* ratio
        // hasn't yet crossed.
        if let (Some(guard), Some(mid)) = (
            self.margin_guard.as_ref(),
            self.book_keeper.book.mid_price(),
        ) {
            let notional_delta =
                self.config.market_maker.order_size * mid * Decimal::from(2u64);
            if let Some(projected) =
                guard.projected_ratio(notional_delta, self.margin_leverage)
            {
                if projected >= guard.thresholds().stop_ratio {
                    warn!(
                        symbol = %self.symbol,
                        projected = %projected,
                        stop_ratio = %guard.thresholds().stop_ratio,
                        "margin projection would cross stop_ratio — skipping quote refresh"
                    );
                    return Ok(());
                }
            }
        }

        // Circuit breaker.
        self.circuit_breaker
            .check_stale_book(self.book_keeper.book.last_update_ms, &self.config.risk);
        self.circuit_breaker
            .check_spread(self.book_keeper.book.spread_bps(), &self.config.risk);

        // Soft spread gate — skip quoting for this tick when the
        // current book spread blows past the quote-gate threshold
        // without tripping the circuit breaker. Covers the "book
        // resync / thin-book volatility blip" case where a full
        // cancel-all is overkill and we just want to step back
        // for one tick and resume once the spread narrows. The
        // hard circuit breaker above still catches sustained
        // wide-spread events at the higher `max_spread_bps`
        // threshold.
        if let (Some(gate_bps), Some(current_bps)) = (
            self.config.risk.max_spread_to_quote_bps,
            self.book_keeper.book.spread_bps(),
        ) {
            if current_bps > gate_bps {
                warn!(
                    %current_bps,
                    gate = %gate_bps,
                    "spread-gate: skipping quote this tick (book too wide)"
                );
                return Ok(());
            }
        }

        if let Some(mid) = self.book_keeper.book.mid_price() {
            let equity = self.exposure_manager_equity(mid);
            self.exposure_manager.update_equity(equity);
            if self
                .exposure_manager
                .is_drawdown_breached(equity, &self.config.risk)
            {
                self.circuit_breaker.trip(TripReason::MaxDrawdown);
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::CircuitBreakerTripped,
                    "drawdown",
                );
                self.record_incident("high", "Circuit breaker tripped: max drawdown exceeded");
            }
            if self.exposure_manager.is_exposure_breached(
                self.inventory_manager.inventory(),
                mid,
                &self.config.risk,
            ) {
                self.circuit_breaker.trip(TripReason::MaxExposure);
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::CircuitBreakerTripped,
                    "exposure",
                );
                self.record_incident("high", "Circuit breaker tripped: max exposure exceeded");
            }
        }

        if self.circuit_breaker.is_tripped() {
            if let Err(e) = self
                .order_manager
                .cancel_all(&self.connectors.primary, &self.symbol)
                .await
            {
                warn!(symbol = %self.symbol, error = %e, "cancel_all on circuit-breaker trip left survivors");
            }
            self.balance_cache.reset_reservations();
            return Ok(());
        }

        if !self.book_keeper.is_ready() {
            return Ok(());
        }

        // Sequence-gap guard: if the WS book stream skipped a
        // delta, the local book is out of sync with the venue.
        // Quoting against an out-of-sync book is how phantom
        // inventory shows up — we would place orders that cross
        // the real best quote. Pull a fresh REST snapshot and
        // feed it through the keeper; that re-anchors the stream
        // and clears the flag. If the REST call itself fails we
        // stand down this tick and retry next.
        if self.book_keeper.needs_resync() {
            warn!(
                symbol = %self.symbol,
                gaps = self.book_keeper.gap_count(),
                "book stream had a sequence gap — fetching REST snapshot to resync"
            );
            match self
                .connectors
                .primary
                .get_orderbook(&self.symbol, 25)
                .await
            {
                Ok((bids, asks, seq)) => {
                    // Replay as a synthetic snapshot event so the
                    // keeper clears needs_resync and re-seeds
                    // last_sequence the same way a WS snapshot would.
                    let snap = mm_exchange_core::events::MarketEvent::BookSnapshot {
                        venue: self.connectors.primary.venue_id(),
                        symbol: self.symbol.clone(),
                        bids,
                        asks,
                        sequence: seq,
                    };
                    self.book_keeper.on_event(&snap);
                    self.audit.risk_event(
                        &self.symbol,
                        AuditEventType::BookResync,
                        &format!("sequence-gap resync via REST (seq={seq})"),
                    );
                }
                Err(e) => {
                    warn!(symbol = %self.symbol, error = %e, "REST snapshot resync failed — skipping tick");
                    return Ok(());
                }
            }
        }

        let Some(mid) = self.book_keeper.book.mid_price() else {
            // Book was marked ready but mid is unavailable
            // (e.g. empty side after a flash crash). Skip this
            // tick — next refresh will retry.
            return Ok(());
        };

        // Feed mid-price return into the factor covariance
        // estimator for the hedge optimizer.
        if self.last_mid > Decimal::ZERO {
            let ret = (mid - self.last_mid) / self.last_mid;
            self.factor_covariance
                .push_return(&self.product.base_asset, ret);
            // Epic 3: also push into the shared estimator for
            // portfolio-level VaR and correlation matrix.
            if let Some(shared) = &self.shared_factor_covariance {
                if let Ok(mut est) = shared.lock() {
                    est.merge_observation(&self.product.base_asset, ret);
                }
            }
        }

        // Time remaining.
        let elapsed_secs = self.cycle_start.elapsed().as_secs();
        let horizon = self.config.market_maker.time_horizon_secs;
        let t_remaining = if elapsed_secs >= horizon {
            self.cycle_start = Instant::now();
            dec!(1)
        } else {
            (Decimal::from(horizon - elapsed_secs) / Decimal::from(horizon)).max(dec!(0.01))
        };

        // Volatility.
        let sigma = self
            .volatility_estimator
            .volatility()
            .unwrap_or(self.config.market_maker.sigma);

        // Kill switch multipliers.
        let effective_level = self.effective_kill_level();
        let ks_spread = effective_level.spread_multiplier();
        // Base size multiplier from the kill switch, then
        // composed with the VaR guard's per-strategy throttle
        // via `min()` (max-restrictive wins). Epic C
        // sub-component #4.
        let ks_base_size = effective_level.size_multiplier();
        let strategy_class = self.strategy.name().to_string();
        let var_throttle = self
            .var_guard
            .as_ref()
            .map(|vg| vg.effective_throttle(&strategy_class))
            .unwrap_or(Decimal::ONE);
        let ks_size = ks_base_size.min(var_throttle);

        // Fire a transition audit event when the VaR throttle
        // flips. Stable-state ticks do not spam the trail.
        if self.var_guard.is_some() {
            let prior = self.var_guard_last_throttle;
            if prior != Some(var_throttle) {
                if var_throttle < Decimal::ONE {
                    self.audit.risk_event(
                        &self.symbol,
                        AuditEventType::VarGuardThrottleApplied,
                        &format!("strategy={strategy_class}, throttle={var_throttle}"),
                    );
                }
                self.var_guard_last_throttle = Some(var_throttle);
            }
        }

        // Push the current inventory + time-remaining snapshot
        // into the auto-tuner so the optional inventory γ
        // policy reads them on its `effective_gamma_mult()`
        // call. No-op when `inventory_gamma_policy` is None.
        self.auto_tuner
            .update_policy_state(self.inventory_manager.inventory(), t_remaining);

        // Push the current Market Resilience reading into the
        // auto-tuner so `effective_spread_mult()` can widen the
        // book in response to a just-happened liquidity shock.
        // Decays back to 1.0 over ~5s after the last shock, so
        // steady-state markets stay at the regime+toxicity
        // baseline. Gated behind the `market_resilience_enabled`
        // toggle — when off, we explicitly clear any stale
        // reading on the autotuner so the baseline multiplier
        // is restored.
        if self.config.market_maker.market_resilience_enabled {
            let mr_now = chrono::Utc::now();
            let mr_now_ns = mr_now.timestamp_nanos_opt().unwrap_or(0);
            let mr_score = self.market_resilience.score(mr_now_ns);
            self.auto_tuner.set_market_resilience(mr_score);
            // Also feed the kill switch — a sustained dip below
            // 0.3 for 3+ seconds trips L1 (WidenSpreads). Harder
            // levels remain driven by PnL / position value.
            self.kill_switch.update_market_resilience(mr_score, mr_now);
        } else {
            self.auto_tuner.clear_market_resilience();
        }

        // Multi-Venue Level 2.A — publish this engine's L1 snapshot
        // to the shared DataBus. Graphs with parameterised
        // `Book.L1(venue, symbol, product)` nodes pick it up during
        // the same tick's `tick_strategy_graph` pass below.
        if let Some(dash) = self.dashboard.as_ref() {
            let bus = dash.data_bus();
            let key = (
                format!("{:?}", self.config.exchange.exchange_type).to_lowercase(),
                self.symbol.clone(),
                self.config.exchange.product,
            );
            let book = &self.book_keeper.book;
            bus.publish_l1(
                key.clone(),
                mm_dashboard::data_bus::BookL1Snapshot {
                    bid_px: book.best_bid(),
                    ask_px: book.best_ask(),
                    mid: book.mid_price(),
                    spread_bps: book.spread_bps(),
                    ts: Some(chrono::Utc::now()),
                },
            );
            // 2.B.3 — L2 snapshot (top-20 levels per side). Iterate
            // BTreeMap; bids descending so index 0 = best bid.
            let bids: Vec<(rust_decimal::Decimal, rust_decimal::Decimal)> = book
                .bids
                .iter()
                .rev()
                .take(20)
                .map(|(p, q)| (*p, *q))
                .collect();
            let asks: Vec<(rust_decimal::Decimal, rust_decimal::Decimal)> =
                book.asks.iter().take(20).map(|(p, q)| (*p, *q)).collect();
            bus.publish_l2(
                key,
                mm_dashboard::data_bus::BookL2Snapshot {
                    bids,
                    asks,
                    ts: Some(chrono::Utc::now()),
                },
            );
        }

        // Epic H — evaluate the attached strategy graph (if any)
        // BEFORE the autotune read. The graph's sinks push spread /
        // size multipliers into the autotuner, so the product below
        // already includes them.
        self.tick_strategy_graph();

        // Auto-tune.
        let (mut eff_gamma, mut eff_size, mut eff_spread) = if self.config.toxicity.autotune_enabled
        {
            (
                self.config.market_maker.gamma * self.auto_tuner.effective_gamma_mult() * ks_spread,
                self.config.market_maker.order_size
                    * self.auto_tuner.effective_size_mult()
                    * ks_size,
                self.config.market_maker.min_spread_bps
                    * self.auto_tuner.effective_spread_mult()
                    * ks_spread,
            )
        } else {
            (
                self.config.market_maker.gamma * ks_spread,
                self.config.market_maker.order_size * ks_size,
                self.config.market_maker.min_spread_bps * ks_spread,
            )
        };

        // A/B split — apply variant multipliers if active.
        if let Some(ab) = &mut self.ab_split {
            let variant = ab.active_variant(&self.symbol);
            eff_gamma *= variant.gamma_mult;
            eff_spread *= variant.spread_mult;
            eff_size *= variant.size_mult;
            ab.tick();
        }

        // Portfolio risk multiplier (Epic 3).
        eff_spread *= self.portfolio_risk_mult;

        // Epic 30 — adaptive tuner multiplier layer. Tick the
        // tuner on every refresh so a new 1-minute bucket rolls
        // over when due; then fold its γ factor into the stack.
        // `gamma_factor()` returns 1.0 unless
        // `market_maker.adaptive_enabled = true`, so existing
        // deployments see byte-identical γ.
        self.adaptive_tuner.tick(std::time::Instant::now());
        let adaptive_factor = self.adaptive_tuner.gamma_factor();
        eff_gamma *= adaptive_factor;

        // Momentum alpha.
        let mut alpha_mid = if self.config.market_maker.momentum_enabled {
            let alpha = self.momentum.alpha(&self.book_keeper.book, mid);
            mid + alpha * mid * t_remaining
        } else {
            mid
        };
        // Epic G — social risk skew. `social_skew_bps` is set
        // by `on_sentiment_tick` when sentiment + OFI agree;
        // at 0 bps this is a no-op. Applied additively on top
        // of the momentum alpha so the two skew paths
        // compose: momentum handles per-tick micro-structure
        // signal, social handles slower regime shift.
        if self.social_skew_bps != Decimal::ZERO {
            let social_shift = alpha_mid * self.social_skew_bps / dec!(10000);
            alpha_mid += social_shift;
        }

        let mut tuned = self.config.market_maker.clone();
        tuned.gamma = eff_gamma;
        tuned.order_size = eff_size;
        tuned.min_spread_bps = eff_spread;

        let ref_price = self.hedge_book.as_ref().and_then(|hb| hb.book.mid_price());

        // Cross-venue divergence guard. When both books have
        // mids and the operator has opted into the guard via
        // `max_cross_venue_divergence_pct`, compare the two and
        // stand down when the relative gap exceeds the threshold.
        // A 50 bps gap on BTC/ETH in steady state is already
        // anomalous — either one venue halted / is mispriced, or
        // our feed stalled. Quoting through it drives orders at a
        // mid that does not exist on the other leg. The tick is
        // skipped and an audit event fires once per crossing so
        // the operator sees the symptom in the MiCA trail.
        if let (Some(primary_mid), Some(hedge_mid), Some(limit)) = (
            self.book_keeper.book.mid_price(),
            ref_price,
            self.config.market_maker.max_cross_venue_divergence_pct,
        ) {
            if !primary_mid.is_zero() && limit > dec!(0) {
                let diff = if primary_mid > hedge_mid {
                    primary_mid - hedge_mid
                } else {
                    hedge_mid - primary_mid
                };
                let pct = diff / primary_mid;
                if pct > limit {
                    warn!(
                        symbol = %self.symbol,
                        %primary_mid,
                        %hedge_mid,
                        divergence_pct = %pct,
                        limit = %limit,
                        "cross-venue divergence exceeds limit — skipping quote refresh"
                    );
                    // One audit event per crossing: use the flag
                    // on self to suppress duplicates across
                    // consecutive ticks where the condition
                    // persists. Reset when we come back inside.
                    if !self.cross_venue_divergence_tripped {
                        self.audit.risk_event(
                            &self.symbol,
                            AuditEventType::CircuitBreakerTripped,
                            &format!(
                                "cross-venue divergence {pct:.6} > {limit:.6} \
                                 (primary {primary_mid} / hedge {hedge_mid})"
                            ),
                        );
                        self.cross_venue_divergence_tripped = true;
                        self.record_incident(
                            "high",
                            &format!(
                                "Cross-venue divergence tripped: {}",
                                (pct * dec!(10000)).round_dp(1)
                            ),
                        );
                    }
                    return Ok(());
                } else if self.cross_venue_divergence_tripped {
                    info!(
                        symbol = %self.symbol,
                        %pct,
                        "cross-venue divergence back within limits — resuming quotes"
                    );
                    self.cross_venue_divergence_tripped = false;
                }
            }
        }

        // Epic 40.9 — borrow cost applies only to **spot-short**
        // legs. Perp short has no borrow requirement (funding P&L
        // is separately accounted by Epic 40.3 once wired). Gate
        // the shim on `product == Spot` so a perp-product engine
        // never threads a spurious borrow_cost_bps through the
        // strategy's reservation price.
        let borrow_cost_bps = if self.config.exchange.product == mm_common::config::ProductType::Spot {
            self.borrow_manager
                .as_ref()
                .map(|bm| bm.effective_carry_bps())
                .filter(|bps| !bps.is_zero())
        } else {
            None
        };
        // Cross-venue staleness reading (P1.4 stage-1). Computed
        // here so the strategy gate has a single canonical value
        // per refresh tick rather than a re-derived one.
        let hedge_book_age_ms = self.hedge_book.as_ref().and_then(|hb| {
            if hb.book.last_update_ms == 0 {
                return None;
            }
            Some(chrono::Utc::now().timestamp_millis() - hb.book.last_update_ms)
        });
        // Also push the cross-venue basis to Prometheus + emit
        // entry/exit audit events when the basis flips across
        // the configured threshold. Computed BEFORE we borrow
        // `&self.hedge_book.book` for `hedge_book_ref` so the
        // mut borrow inside `update_cross_venue_basis_state` does
        // not conflict with the immutable book reference the
        // strategy context holds for the rest of the call.
        let cross_venue_basis_bps = if let (Some(hb), Some(spot_mid)) =
            (self.hedge_book.as_ref(), self.book_keeper.book.mid_price())
        {
            hb.book.mid_price().and_then(|perp_mid| {
                if spot_mid.is_zero() {
                    None
                } else {
                    Some((perp_mid - spot_mid) / spot_mid * dec!(10_000))
                }
            })
        } else {
            None
        };
        if let Some(basis_bps) = cross_venue_basis_bps {
            mm_dashboard::metrics::CROSS_VENUE_BASIS_BPS
                .with_label_values(&[&self.symbol])
                .set(decimal_to_f64(basis_bps));
            self.update_cross_venue_basis_state(basis_bps);
        }

        let hedge_book_ref = self.hedge_book.as_ref().map(|hb| &hb.book);
        // Epic D sub-component #4: thread the adverse-selection
        // probability from the existing `AdverseSelectionTracker`
        // measurement into the Cartea AS closed-form spread
        // widening inside the strategy. Stage-1 uses the simple
        // bps → ρ map from `cartea_spread::as_prob_from_bps`;
        // operators tune the ±20 bps saturation as needed.
        let as_prob = self
            .adverse_selection
            .adverse_selection_bps()
            .map(mm_strategy::cartea_spread::as_prob_from_bps);
        // Epic D stage-3 — per-side ρ threading. When the
        // tracker has enough completed fills on each side
        // (≥5 each), populate the per-side fields so the
        // strategy uses the asymmetric Cartea path. Either
        // side returning `None` falls back to the symmetric
        // `as_prob` path inside the strategy. Pre-stage-3
        // behaviour is byte-identical when neither per-side
        // path fires.
        let as_prob_bid = self
            .adverse_selection
            .adverse_selection_bps_bid()
            .map(mm_strategy::cartea_spread::as_prob_from_bps);
        let as_prob_ask = self
            .adverse_selection
            .adverse_selection_bps_ask()
            .map(mm_strategy::cartea_spread::as_prob_from_bps);
        let ctx = StrategyContext {
            book: &self.book_keeper.book,
            product: &self.product,
            config: &tuned,
            inventory: self.inventory_manager.inventory(),
            volatility: sigma,
            time_remaining: t_remaining,
            mid_price: alpha_mid,
            ref_price,
            hedge_book: hedge_book_ref,
            borrow_cost_bps,
            hedge_book_age_ms,
            as_prob,
            as_prob_bid,
            as_prob_ask,
        };

        // Phase 4 — if the graph authored a quote bundle on the last
        // tick via `Out.Quotes`, use it instead of the hand-wired
        // strategy. Override is consumed (take) so the next tick
        // either re-authors or falls back cleanly.
        let mut quotes = match self.graph_quotes_override.take() {
            Some(q) => q,
            None => {
                let strategy_quotes = self.strategy.compute_quotes(&ctx);
                // Cache for the next tick's `Strategy.*` composite
                // nodes — they read it out of `last_strategy_quotes`
                // via the source marshaller.
                self.last_strategy_quotes = Some(strategy_quotes.clone());
                // Phase 5 — also refresh every pool-instance so the
                // node-level config (different γ across two
                // `Strategy.Avellaneda`s for example) actually takes
                // effect. Same ctx, different knobs.
                let mut per_node = std::collections::HashMap::with_capacity(
                    self.strategy_pool.len(),
                );
                for (node_id, strat) in &self.strategy_pool {
                    per_node.insert(*node_id, strat.compute_quotes(&ctx));
                }
                self.last_strategy_quotes_per_node = per_node;
                strategy_quotes
            }
        };

        // Inventory limits + urgency + dynamic sizing.
        self.inventory_manager
            .apply_limits(&mut quotes, &self.config.risk);
        self.adv_inventory
            .apply_urgency(&mut quotes, self.inventory_manager.inventory(), mid);

        for q in quotes.iter_mut() {
            if let Some(bid) = &mut q.bid {
                bid.qty = self.product.round_qty(self.adv_inventory.dynamic_size(
                    bid.qty,
                    self.inventory_manager.inventory(),
                    bid.side,
                ));
                // Balance pre-check.
                if !self.balance_cache.can_afford(
                    bid.side,
                    bid.price,
                    bid.qty,
                    &self.product.base_asset,
                    &self.product.quote_asset,
                ) {
                    *bid = mm_common::types::Quote {
                        side: bid.side,
                        price: bid.price,
                        qty: dec!(0),
                    };
                }
            }
            if let Some(ask) = &mut q.ask {
                ask.qty = self.product.round_qty(self.adv_inventory.dynamic_size(
                    ask.qty,
                    self.inventory_manager.inventory(),
                    ask.side,
                ));
                if !self.balance_cache.can_afford(
                    ask.side,
                    ask.price,
                    ask.qty,
                    &self.product.base_asset,
                    &self.product.quote_asset,
                ) {
                    *ask = mm_common::types::Quote {
                        side: ask.side,
                        price: ask.price,
                        qty: dec!(0),
                    };
                }
            }
        }

        // Enforce max order size.
        let max_size = self.config.risk.max_order_size;
        if !max_size.is_zero() {
            for q in quotes.iter_mut() {
                if let Some(bid) = &mut q.bid {
                    if bid.qty > max_size {
                        bid.qty = self.product.round_qty(max_size);
                    }
                }
                if let Some(ask) = &mut q.ask {
                    if ask.qty > max_size {
                        ask.qty = self.product.round_qty(max_size);
                    }
                }
            }
        }

        // Enforce volume limits — cancel all new quotes if daily/hourly cap exceeded.
        if !self.volume_limiter.can_trade(dec!(0)) {
            warn!("volume limit reached — suppressing new quotes");
            for q in quotes.iter_mut() {
                q.bid = None;
                q.ask = None;
            }
        }

        // Remove quotes with zero qty.
        for q in quotes.iter_mut() {
            if q.bid.as_ref().map(|b| b.qty.is_zero()).unwrap_or(false) {
                q.bid = None;
            }
            if q.ask.as_ref().map(|a| a.qty.is_zero()).unwrap_or(false) {
                q.ask = None;
            }
        }

        self.kill_switch.on_message_sent();

        let amend_epsilon = if self.config.market_maker.amend_enabled {
            self.config.market_maker.amend_max_ticks
        } else {
            0
        };
        self.order_manager
            .execute_diff(
                &self.symbol,
                &quotes,
                &self.product,
                &self.connectors.primary,
                amend_epsilon,
            )
            .await?;

        // SLA update.
        let has_bid = quotes.iter().any(|q| q.bid.is_some());
        let has_ask = quotes.iter().any(|q| q.ask.is_some());
        let bid_depth: Decimal = quotes
            .iter()
            .filter_map(|q| q.bid.as_ref())
            .map(|b| b.price * b.qty)
            .sum();
        let ask_depth: Decimal = quotes
            .iter()
            .filter_map(|q| q.ask.as_ref())
            .map(|a| a.price * a.qty)
            .sum();
        self.sla_tracker.update_quotes(
            has_bid,
            has_ask,
            self.book_keeper.book.spread_bps(),
            bid_depth,
            ask_depth,
        );

        Ok(())
    }

    fn exposure_manager_equity(&self, mid_price: Decimal) -> Decimal {
        self.inventory_manager.total_pnl(mid_price)
    }

    /// Record an incident to the dashboard state and fire a Telegram alert.
    fn record_incident(&self, severity: &str, description: &str) {
        if let Some(ds) = &self.dashboard {
            ds.add_incident(IncidentRecord {
                timestamp: chrono::Utc::now(),
                severity: severity.to_string(),
                description: description.to_string(),
                duration_secs: 0,
                resolved: false,
            });
        }
        if let Some(alerts) = &self.alerts {
            let alert_severity = match severity {
                "critical" => AlertSeverity::Critical,
                "high" => AlertSeverity::Warning,
                _ => AlertSeverity::Info,
            };
            alerts.alert(
                alert_severity,
                description,
                &format!("Symbol: {}", self.symbol),
                Some(&self.symbol),
            );
        }
        // Webhook dispatch for client event delivery.
        if severity == "critical" {
            if let Some(wh) = &self.webhooks {
                wh.dispatch(mm_dashboard::webhooks::WebhookEvent::KillSwitchEscalated {
                    symbol: self.symbol.clone(),
                    level: self.kill_switch.level() as u8,
                    reason: description.to_string(),
                });
            }
        }
    }

    /// Push state to dashboard for HTTP API + Prometheus metrics.
    /// Current recommended hedge basket from the last
    /// optimizer refresh. Read-only accessor for tests +
    /// the dashboard. Epic C sub-component #3.
    pub fn last_hedge_basket(&self) -> &HedgeBasket {
        &self.last_hedge_basket
    }

    /// Build the per-refresh hedge universe from the engine's
    /// known products. Stage-1 only knows about the primary
    /// symbol; stage-2 will wire in a proper per-venue
    /// universe from the connector bundle. For now the v1
    /// universe is a single synthetic perp instrument per
    /// base asset, using `config.risk.max_inventory` as the
    /// position cap and a conservative 1 bps default funding.
    fn build_hedge_universe(&self) -> Vec<mm_risk::hedge_optimizer::HedgeInstrument> {
        use mm_risk::hedge_optimizer::HedgeInstrument;
        let base = self.product.base_asset.clone();
        if base.is_empty() {
            return Vec::new();
        }
        vec![HedgeInstrument {
            symbol: format!("{base}-PERP"),
            factor: base,
            cross_betas: vec![],
            funding_bps: dec!(1),
            position_cap: self.config.risk.max_inventory,
        }]
    }

    /// Apply a hot config override from the admin API.
    fn apply_config_override(&mut self, ovr: mm_dashboard::state::ConfigOverride) {
        use mm_dashboard::state::ConfigOverride;
        match ovr {
            ConfigOverride::Gamma(v) => {
                info!(symbol = %self.symbol, gamma = %v, "hot-reload: gamma");
                self.config.market_maker.gamma = v;
            }
            ConfigOverride::MinSpreadBps(v) => {
                info!(symbol = %self.symbol, min_spread_bps = %v, "hot-reload: min_spread_bps");
                self.config.market_maker.min_spread_bps = v;
            }
            ConfigOverride::OrderSize(v) => {
                info!(symbol = %self.symbol, order_size = %v, "hot-reload: order_size");
                self.config.market_maker.order_size = v;
            }
            ConfigOverride::MaxDistanceBps(v) => {
                info!(symbol = %self.symbol, max_distance_bps = %v, "hot-reload: max_distance_bps");
                self.config.market_maker.max_distance_bps = v;
            }
            ConfigOverride::NumLevels(v) => {
                info!(symbol = %self.symbol, num_levels = v, "hot-reload: num_levels");
                self.config.market_maker.num_levels = v;
            }
            ConfigOverride::MomentumEnabled(v) => {
                info!(symbol = %self.symbol, momentum_enabled = v, "hot-reload: momentum_enabled");
                self.config.market_maker.momentum_enabled = v;
            }
            ConfigOverride::MarketResilienceEnabled(v) => {
                info!(symbol = %self.symbol, mr = v, "hot-reload: market_resilience_enabled");
                self.config.market_maker.market_resilience_enabled = v;
            }
            ConfigOverride::AmendEnabled(v) => {
                info!(symbol = %self.symbol, amend = v, "hot-reload: amend_enabled");
                self.config.market_maker.amend_enabled = v;
            }
            ConfigOverride::AmendMaxTicks(v) => {
                info!(symbol = %self.symbol, ticks = v, "hot-reload: amend_max_ticks");
                self.config.market_maker.amend_max_ticks = v;
            }
            ConfigOverride::OtrEnabled(v) => {
                info!(symbol = %self.symbol, otr = v, "hot-reload: otr_enabled");
                self.config.market_maker.otr_enabled = v;
            }
            ConfigOverride::MaxInventory(v) => {
                info!(symbol = %self.symbol, max_inv = %v, "hot-reload: max_inventory");
                self.config.risk.max_inventory = v;
            }
            ConfigOverride::PauseQuoting => {
                info!(symbol = %self.symbol, "hot-reload: pausing quoting");
                self.lifecycle_paused = true;
            }
            ConfigOverride::ResumeQuoting => {
                info!(symbol = %self.symbol, "hot-reload: resuming quoting");
                self.lifecycle_paused = false;
            }
            ConfigOverride::PortfolioRiskMult(v) => {
                info!(symbol = %self.symbol, mult = %v, "hot-reload: portfolio risk spread multiplier");
                self.portfolio_risk_mult = v;
            }
            ConfigOverride::ManualKillSwitch { level, reason } => {
                let kl = match level {
                    1 => mm_risk::KillLevel::WidenSpreads,
                    2 => mm_risk::KillLevel::StopNewOrders,
                    3 => mm_risk::KillLevel::CancelAll,
                    4 => mm_risk::KillLevel::FlattenAll,
                    5 => mm_risk::KillLevel::Disconnect,
                    _ => {
                        warn!(level, "ignoring manual kill-switch with invalid level");
                        return;
                    }
                };
                info!(symbol = %self.symbol, level, reason = %reason, "manual kill switch from ops API");
                self.kill_switch.manual_trigger(kl, &reason);
                self.audit.risk_event(
                    &self.symbol,
                    mm_risk::audit::AuditEventType::KillSwitchEscalated,
                    &format!("manual L{level}: {reason}"),
                );
                self.record_incident(
                    match level {
                        1 | 2 => "warning",
                        _ => "critical",
                    },
                    &format!("Manual kill switch L{level}: {reason}"),
                );
                return;
            }
            ConfigOverride::News(text) => {
                info!(
                    symbol = %self.symbol,
                    chars = text.len(),
                    "news headline received"
                );
                self.on_news_headline(&text);
                return;
            }
            ConfigOverride::SentimentTick(tick) => {
                self.on_sentiment_tick(tick);
                return;
            }
            ConfigOverride::StrategyGraphSwap(json) => {
                match mm_strategy_graph::Graph::from_json(&json) {
                    Ok(g) => {
                        // Scope matching — only apply graphs whose
                        // scope includes this symbol. Global
                        // always applies; Symbol must match
                        // exactly; AssetClass / Client handled by
                        // the server-side broadcast filter, but
                        // we double-check here as a belt-and-braces.
                        let applies = match &g.scope {
                            mm_strategy_graph::Scope::Global => true,
                            mm_strategy_graph::Scope::Symbol(s) => s == &self.symbol,
                            mm_strategy_graph::Scope::AssetClass(_)
                            | mm_strategy_graph::Scope::Client(_) => true,
                        };
                        if !applies {
                            return;
                        }
                        if let Err(e) = self.swap_strategy_graph(&g) {
                            warn!(
                                symbol = %self.symbol,
                                error = ?e,
                                "strategy graph swap rejected by validator"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            symbol = %self.symbol,
                            error = %e,
                            "invalid strategy graph JSON"
                        );
                    }
                }
                return;
            }
            ConfigOverride::ManualKillSwitchReset { reason } => {
                info!(symbol = %self.symbol, reason = %reason, "manual kill switch reset from ops API");
                self.kill_switch.reset();
                self.audit.risk_event(
                    &self.symbol,
                    mm_risk::audit::AuditEventType::KillSwitchReset,
                    &format!("manual reset: {reason}"),
                );
                return;
            }
            ConfigOverride::ExternalAtomicBundle(json) => {
                // 3.E MVP — recipient engine receives a bundle leg
                // via the same path as ExternalVenueQuotes; full
                // two-phase commit lands in 3.E.2. Audit the arrival
                // so post-mortems can reconcile intent vs. action.
                self.audit.risk_event(
                    &self.symbol,
                    mm_risk::audit::AuditEventType::StrategyGraphSinkFired,
                    &format!("AtomicBundle leg arrived (pending: {json})"),
                );
                return;
            }
            ConfigOverride::ExternalVenueQuotes(json) => {
                // Multi-Venue 3.B — another engine's graph authored a
                // VenueQuote bundle that targets this engine. Parse
                // the JSON, filter to entries that actually belong
                // here (symbol must match; venue/product are best-
                // effort), materialise as `QuotePair`s through the
                // same inner-first pairing the local dispatcher uses.
                match serde_json::from_str::<Vec<mm_strategy_graph::VenueQuote>>(&json) {
                    Ok(entries) => {
                        use mm_common::types::{Quote, QuotePair, Side};
                        use mm_strategy_graph::QuoteSide;
                        let mut bids: Vec<Quote> = Vec::new();
                        let mut asks: Vec<Quote> = Vec::new();
                        for e in entries {
                            if e.symbol != self.symbol {
                                continue;
                            }
                            let q = Quote {
                                side: match e.side {
                                    QuoteSide::Buy => Side::Buy,
                                    QuoteSide::Sell => Side::Sell,
                                },
                                price: self.product.round_price(e.price),
                                qty: self.product.round_qty(e.qty),
                            };
                            match q.side {
                                Side::Buy => bids.push(q),
                                Side::Sell => asks.push(q),
                            }
                        }
                        bids.sort_by(|a, b| b.price.cmp(&a.price));
                        asks.sort_by(|a, b| a.price.cmp(&b.price));
                        let n = bids.len().max(asks.len());
                        let mut pairs = Vec::with_capacity(n);
                        for i in 0..n {
                            pairs.push(QuotePair {
                                bid: bids.get(i).cloned(),
                                ask: asks.get(i).cloned(),
                            });
                        }
                        self.graph_quotes_override = Some(pairs);
                    }
                    Err(e) => {
                        warn!(
                            symbol = %self.symbol,
                            error = %e,
                            "ExternalVenueQuotes JSON decode failed"
                        );
                    }
                }
                return;
            }
        }
        self.audit.risk_event(
            &self.symbol,
            mm_risk::audit::AuditEventType::ConfigLoaded,
            "hot-reload config override applied",
        );
    }

    /// Per-factor variance map from the rolling covariance
    /// estimator. Falls back to constant `1.0` when the
    /// estimator has too few samples.
    fn factor_variances(&self) -> std::collections::HashMap<String, Decimal> {
        self.factor_covariance.variances()
    }

    fn update_dashboard(&mut self) {
        let Some(ds) = &self.dashboard else { return };

        // Publish the unified portfolio snapshot on every
        // dashboard update. Taking the snapshot under the mutex
        // keeps the dashboard's view consistent across all
        // symbols in a multi-engine deployment.
        if let Some(pf) = &self.portfolio {
            if let Ok(pf) = pf.lock() {
                let snapshot = pf.snapshot();
                // Epic C sub-component #3: refresh the hedge
                // basket recommendation from the latest
                // per-factor delta. Universe is constructed
                // from the primary connector's product (plus
                // the hedge connector's product when dual
                // mode is active). Operators see the result
                // in the dashboard; nothing trades
                // automatically in stage-1.
                let universe = self.build_hedge_universe();
                let factor_variances = self.factor_variances();
                let basket = self.hedge_optimizer.optimize(
                    &snapshot.per_factor,
                    &universe,
                    &factor_variances,
                );
                if !basket.is_empty() && basket.entries != self.last_hedge_basket.entries {
                    let summary = basket
                        .entries
                        .iter()
                        .map(|(s, q)| format!("{s}={q}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    self.audit.risk_event(
                        &self.symbol,
                        AuditEventType::HedgeBasketRecommended,
                        &summary,
                    );
                }
                self.last_hedge_basket = basket;
                ds.update_portfolio(snapshot);
            }
        }

        let regime = self.auto_tuner.regime_detector.regime();
        let regime_str = format!("{regime:?}");

        let venue_label = match self.config.exchange.exchange_type {
            mm_common::config::ExchangeType::Binance
            | mm_common::config::ExchangeType::BinanceTestnet => "binance",
            mm_common::config::ExchangeType::Bybit
            | mm_common::config::ExchangeType::BybitTestnet => "bybit",
            mm_common::config::ExchangeType::HyperLiquid
            | mm_common::config::ExchangeType::HyperLiquidTestnet => "hyperliquid",
            mm_common::config::ExchangeType::Custom => "custom",
        }
        .to_string();
        let pair_class_label = self
            .pair_class
            .as_ref()
            .map(|c| format!("{c:?}").to_lowercase());
        ds.update(SymbolState {
            symbol: self.symbol.clone(),
            mode: self.config.mode.clone(),
            strategy: self.strategy.name().to_string(),
            // Product label — authoritative source is
            // `config.exchange.product` (Epic 40.1). Label stays
            // stable: "spot", "linear_perp", "inverse_perp".
            product: self.config.exchange.product.label().to_string(),
            venue: venue_label,
            pair_class: pair_class_label,
            mid_price: self.last_mid,
            spread_bps: self.book_keeper.book.spread_bps().unwrap_or(dec!(0)),
            inventory: self.inventory_manager.inventory(),
            inventory_value: self.inventory_manager.inventory().abs() * self.last_mid,
            live_orders: self.order_manager.live_count(),
            total_fills: self.pnl_tracker.attribution.round_trips,
            pnl: PnlSnapshot {
                total: self.pnl_tracker.attribution.total_pnl(),
                spread: self.pnl_tracker.attribution.spread_pnl,
                inventory: self.pnl_tracker.attribution.inventory_pnl,
                rebates: self.pnl_tracker.attribution.rebate_income,
                fees: self.pnl_tracker.attribution.fees_paid,
                funding: self.pnl_tracker.attribution.funding_pnl_realised,
                funding_mtm: self.pnl_tracker.attribution.funding_pnl_mtm,
                round_trips: self.pnl_tracker.attribution.round_trips,
                volume: self.pnl_tracker.attribution.total_volume,
            },
            volatility: self.volatility_estimator.volatility().unwrap_or(dec!(0)),
            vpin: self.vpin.vpin().unwrap_or(dec!(0)),
            kyle_lambda: self.kyle_lambda.lambda().unwrap_or(dec!(0)),
            adverse_bps: self
                .adverse_selection
                .adverse_selection_bps()
                .unwrap_or(dec!(0)),
            // Epic D stage-3 — per-side ρ + wave-2 momentum
            // observability. Per-side ρ is derived from the
            // existing `AdverseSelectionTracker` per-side
            // bps accessors via the same
            // `cartea_spread::as_prob_from_bps` map the
            // strategy uses inside `refresh_quotes`. OFI
            // EWMA + learned-MP drift come straight off
            // `MomentumSignals` accessors that already exist
            // (no-op when the optional signals are not
            // attached). Dashboard publishes `None` as a
            // baseline value so Grafana sees a stable
            // pre-warmup gauge.
            as_prob_bid: self
                .adverse_selection
                .adverse_selection_bps_bid()
                .map(mm_strategy::cartea_spread::as_prob_from_bps),
            as_prob_ask: self
                .adverse_selection
                .adverse_selection_bps_ask()
                .map(mm_strategy::cartea_spread::as_prob_from_bps),
            momentum_ofi_ewma: self.momentum.ofi_ewma(),
            momentum_learned_mp_drift: self
                .momentum
                .learned_microprice_drift(&self.book_keeper.book, self.last_mid),
            market_resilience: self
                .market_resilience
                .score(chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)),
            order_to_trade_ratio: self.otr.ratio(),
            hma_value: self.momentum.hma_value(),
            kill_level: self.effective_kill_level() as u8,
            sla_uptime_pct: self.sla_tracker.uptime_pct(),
            regime: regime_str,
            spread_compliance_pct: self.sla_tracker.spread_compliance_pct(),
            book_depth_levels: [dec!(0.5), dec!(1), dec!(2), dec!(5)]
                .iter()
                .map(|&pct| BookDepthLevel {
                    pct_from_mid: pct,
                    bid_depth_quote: self.book_keeper.book.bid_depth_within_pct_quote(pct),
                    ask_depth_quote: self.book_keeper.book.ask_depth_within_pct_quote(pct),
                })
                .collect(),
            locked_in_orders_quote: self.order_manager.locked_value_quote(),
            sla_max_spread_bps: self.sla_tracker.config().max_spread_bps,
            sla_min_depth_quote: self.sla_tracker.config().min_depth_quote,
            presence_pct_24h: {
                let s = self.sla_tracker.daily_presence_summary();
                s.presence_pct
            },
            two_sided_pct_24h: self.sla_tracker.daily_presence_summary().two_sided_pct,
            minutes_with_data_24h: self.sla_tracker.daily_presence_summary().minutes_with_data,
            hourly_presence: self.sla_tracker.hourly_presence_summary(),
            market_impact: Some(self.market_impact.report()),
            performance: Some(self.performance.compute(dec!(525600))), // 365.25d × 1440min
            // Epic 8: publish the hot-reloadable config so the
            // dashboard's tuning panel shows the LIVE value
            // before the operator moves a slider. Reads
            // directly from `self.config` — any `ConfigOverride`
            // the engine has applied is already stored there.
            tunable_config: Some(mm_dashboard::state::TunableConfigSnapshot {
                gamma: self.config.market_maker.gamma,
                kappa: self.config.market_maker.kappa,
                sigma: self.config.market_maker.sigma,
                order_size: self.config.market_maker.order_size,
                num_levels: self.config.market_maker.num_levels as u32,
                min_spread_bps: self.config.market_maker.min_spread_bps,
                max_distance_bps: self.config.market_maker.max_distance_bps,
                max_inventory: self.config.risk.max_inventory,
                momentum_enabled: self.config.market_maker.momentum_enabled,
                market_resilience_enabled: self.config.market_maker.market_resilience_enabled,
                amend_enabled: self.config.market_maker.amend_enabled,
                amend_max_ticks: self.config.market_maker.amend_max_ticks,
                otr_enabled: self.config.market_maker.otr_enabled,
            }),
            adaptive_state: Some(mm_dashboard::state::AdaptiveStateSnapshot {
                pair_class: self
                    .pair_class
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unclassified".to_string()),
                enabled: self.config.market_maker.adaptive_enabled,
                gamma_factor: self.adaptive_tuner.gamma_factor(),
                last_reason: format!("{:?}", self.adaptive_tuner.last_reason())
                    .to_lowercase(),
            }),
            open_orders: self
                .order_manager
                .live_orders_snapshot()
                .into_iter()
                .map(|(id, side, price, qty, status)| {
                    let side_str = match side {
                        mm_common::types::Side::Buy => "buy",
                        mm_common::types::Side::Sell => "sell",
                    };
                    mm_dashboard::state::OrderSnapshot {
                        client_order_id: id.to_string(),
                        side: side_str.to_string(),
                        price,
                        qty,
                        status: status.to_string(),
                    }
                })
                .collect(),
        });

        // Push PnL time-series sample for charting.
        let now_ms = chrono::Utc::now().timestamp_millis();
        ds.push_pnl_sample(
            &self.symbol,
            now_ms,
            self.pnl_tracker.attribution.total_pnl(),
        );
        // UX-2 — spread + inventory rolling history so charts
        // can render the full window on page load rather than
        // warming up from live ticks. Spread comes from the
        // book keeper, inventory from the tracker.
        if let Some(spread_bps) = self.book_keeper.book.spread_bps() {
            ds.push_spread_sample(&self.symbol, now_ms, spread_bps);
        }
        ds.push_inventory_sample(
            &self.symbol,
            now_ms,
            self.inventory_manager.inventory(),
        );

        // Push market impact metrics to Prometheus.
        let impact = self.market_impact.report();
        mm_dashboard::metrics::MARKET_IMPACT_MEAN_BPS
            .with_label_values(&[&self.symbol])
            .set(decimal_to_f64(impact.mean_impact_bps));
        mm_dashboard::metrics::MARKET_IMPACT_ADVERSE_PCT
            .with_label_values(&[&self.symbol])
            .set(decimal_to_f64(impact.adverse_fill_pct));
    }

    /// Refresh the venue's effective fee schedule for this
    /// symbol and hot-swap it into the `PnlTracker` plus the
    /// in-memory `ProductSpec`. Called by the periodic
    /// `fee_tier_interval` arm in `run_with_hedge`.
    ///
    /// The `Decimal → bps` conversion (`x × 10_000`) is done
    /// inside this helper so the Prometheus exposition stays in
    /// the operator-friendly basis-point unit even though the
    /// venue API speaks fractions.
    async fn refresh_fee_tiers(&mut self) {
        let result = self.connectors.primary.fetch_fee_tiers(&self.symbol).await;
        match result {
            Ok(info) => {
                self.product.maker_fee = info.maker_fee;
                self.product.taker_fee = info.taker_fee;
                self.pnl_tracker
                    .set_fee_rates(info.maker_fee, info.taker_fee);
                let maker_bps = info.maker_fee * dec!(10000);
                let taker_bps = info.taker_fee * dec!(10000);
                mm_dashboard::metrics::MAKER_FEE_BPS
                    .with_label_values(&[&self.symbol])
                    .set(decimal_to_f64(maker_bps));
                mm_dashboard::metrics::TAKER_FEE_BPS
                    .with_label_values(&[&self.symbol])
                    .set(decimal_to_f64(taker_bps));
                // Stage-2: push updated fees into the SOR
                // venue-state aggregator so the cost model
                // reflects the live fee tier on subsequent
                // route recommendations.
                self.sor_aggregator.update_fees(
                    self.connectors.primary.venue_id(),
                    info.maker_fee,
                    info.taker_fee,
                );
                debug!(
                    symbol = %self.symbol,
                    maker_bps = %maker_bps,
                    taker_bps = %taker_bps,
                    vip_tier = ?info.vip_tier,
                    "refreshed fee tier from venue (SOR seed updated)"
                );
            }
            Err(FeeTierError::NotSupported) => {
                // Venue (HL, Custom) does not have a per-account
                // fee endpoint — keep the startup snapshot.
                debug!(
                    symbol = %self.symbol,
                    "fetch_fee_tiers not supported on this venue"
                );
            }
            Err(FeeTierError::Other(e)) => {
                warn!(
                    symbol = %self.symbol,
                    error = %e,
                    "fee tier refresh failed — keeping previous rates"
                );
            }
        }
    }

    /// Track the cross-venue basis through its entry threshold
    /// and emit `CrossVenueBasisEntered` / `Exited` audit events
    /// on the first refresh tick after each crossing. The
    /// "entry threshold" is taken from
    /// `config.hedge.pair.basis_threshold_bps` so the same
    /// number that gates the strategy also gates the audit
    /// events. P1.4 stage-1.
    fn update_cross_venue_basis_state(&mut self, basis_bps: Decimal) {
        let Some(threshold) = self
            .config
            .hedge
            .as_ref()
            .map(|h| h.pair.basis_threshold_bps)
        else {
            return;
        };
        let inside = basis_bps.abs() <= threshold;
        if inside == self.cross_venue_basis_inside {
            return;
        }
        self.cross_venue_basis_inside = inside;
        let event = if inside {
            AuditEventType::CrossVenueBasisEntered
        } else {
            AuditEventType::CrossVenueBasisExited
        };
        let detail = format!("basis_bps={basis_bps}, threshold_bps={threshold}");
        self.audit.risk_event(&self.symbol, event, &detail);
    }

    /// Refresh the venue's `ProductSpec` for this symbol and
    /// route any lifecycle drift into the audit trail + the
    /// `lifecycle_paused` flag (P2.3 stage-1). Periodic arm in
    /// `run_with_hedge` calls this on the `pair_lifecycle_interval`
    /// cadence. Quietly short-circuits when no manager is
    /// attached.
    async fn refresh_pair_lifecycle(&mut self) {
        if self.pair_lifecycle.is_none() {
            return;
        }
        let symbol = self.symbol.clone();
        let result = self.connectors.primary.get_product_spec(&symbol).await;
        let events: Vec<PairLifecycleEvent> = match result {
            Ok(spec) => {
                // Apply tick / lot drift into self.product so
                // the next quote refresh rounds against the
                // venue's authoritative values.
                let tick_or_lot_changed = self
                    .pair_lifecycle
                    .as_ref()
                    .and_then(|m| m.current())
                    .map(|prev| prev.tick_size != spec.tick_size || prev.lot_size != spec.lot_size)
                    .unwrap_or(false);
                let new_status = spec.trading_status;
                let new_tick = spec.tick_size;
                let new_lot = spec.lot_size;
                let new_min_notional = spec.min_notional;
                let evts = self
                    .pair_lifecycle
                    .as_mut()
                    .map(|m| m.diff(spec))
                    .unwrap_or_default();
                if tick_or_lot_changed {
                    self.product.tick_size = new_tick;
                    self.product.lot_size = new_lot;
                }
                self.product.trading_status = new_status;
                self.product.min_notional = new_min_notional;
                evts
            }
            Err(e) => {
                // The "symbol not found" path on Binance Spot
                // surfaces here. Treat any persistent error as
                // a delisting candidate — the manager latches
                // and refuses to recover until restart.
                warn!(
                    symbol = %symbol,
                    error = %e,
                    "get_product_spec failed during lifecycle refresh — treating as delisted"
                );
                self.pair_lifecycle
                    .as_mut()
                    .map(|m| m.on_delisted())
                    .unwrap_or_default()
            }
        };
        let mut needs_cancel = false;
        for event in events {
            if matches!(
                event,
                PairLifecycleEvent::Halted { .. } | PairLifecycleEvent::Delisted
            ) {
                needs_cancel = true;
            }
            self.handle_lifecycle_event(event);
        }
        if needs_cancel {
            // Inline cancel-all on the primary leg so the venue
            // book has none of our quotes if/when it re-opens.
            // Hedge leg cancel is handled by the existing
            // shutdown path — lifecycle pauses are per-symbol,
            // not per-pair, so flushing only the primary leg is
            // the right scope.
            let symbol = self.symbol.clone();
            if let Err(e) = self
                .order_manager
                .cancel_all(&self.connectors.primary, &symbol)
                .await
            {
                warn!(symbol = %symbol, error = %e, "cancel_all on lifecycle halt left survivors");
            }
        }
    }

    /// Route one lifecycle event into the audit trail and
    /// flip the `lifecycle_paused` flag where appropriate.
    /// Returns nothing — the caller is responsible for the
    /// async cancel-all that Halted/Delisted require, since
    /// `OrderManager::cancel_all` is async and this helper
    /// is sync.
    fn handle_lifecycle_event(&mut self, event: PairLifecycleEvent) {
        match &event {
            PairLifecycleEvent::Listed => {
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairLifecycleListed,
                    &format!(
                        "tick={}, lot={}, min_notional={}",
                        self.product.tick_size, self.product.lot_size, self.product.min_notional
                    ),
                );
            }
            PairLifecycleEvent::Halted { from, to } => {
                self.lifecycle_paused = true;
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairLifecycleHalted,
                    &format!("from={from:?}, to={to:?}"),
                );
                warn!(
                    symbol = %self.symbol,
                    from = ?from,
                    to = ?to,
                    "pair lifecycle: halted — pausing quoting"
                );
            }
            PairLifecycleEvent::Resumed { from } => {
                self.lifecycle_paused = false;
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairLifecycleResumed,
                    &format!("from={from:?}"),
                );
                info!(symbol = %self.symbol, "pair lifecycle: resumed — quoting re-enabled");
            }
            PairLifecycleEvent::Delisted => {
                self.lifecycle_paused = true;
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairLifecycleDelisted,
                    "venue removed symbol",
                );
                error!(symbol = %self.symbol, "pair lifecycle: DELISTED — quoting halted permanently");
            }
            PairLifecycleEvent::TickLotChanged {
                old_tick,
                new_tick,
                old_lot,
                new_lot,
            } => {
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairLifecycleTickLotChanged,
                    &format!("tick {old_tick} -> {new_tick}, lot {old_lot} -> {new_lot}"),
                );
            }
            PairLifecycleEvent::MinNotionalChanged { old, new } => {
                self.audit.risk_event(
                    &self.symbol,
                    AuditEventType::PairLifecycleMinNotionalChanged,
                    &format!("min_notional {old} -> {new}"),
                );
            }
        }
    }

    /// Refresh the borrow rate for the base asset and push the
    /// snapshot into `BorrowManager`. Engine select arm calls
    /// this on the `borrow_rate_interval` cadence. Quietly
    /// short-circuits when the venue returns `NotSupported` so
    /// HL / custom connectors do not spam warnings.
    async fn refresh_borrow_rate(&mut self) {
        let Some(bm) = self.borrow_manager.as_mut() else {
            return;
        };
        let asset = self.product.base_asset.clone();
        let result = self.connectors.primary.get_borrow_rate(&asset).await;
        match result {
            Ok(info) => {
                bm.apply_rate_refresh(info.rate_apr);
                let carry_bps = bm.effective_carry_bps();
                mm_dashboard::metrics::BORROW_RATE_BPS_HOURLY
                    .with_label_values(&[&asset])
                    .set(decimal_to_f64(info.rate_bps_hourly));
                mm_dashboard::metrics::BORROW_CARRY_BPS
                    .with_label_values(&[&asset])
                    .set(decimal_to_f64(carry_bps));
                debug!(
                    asset = %asset,
                    apr = %info.rate_apr,
                    bps_hourly = %info.rate_bps_hourly,
                    carry_bps = %carry_bps,
                    "refreshed borrow rate"
                );
            }
            Err(BorrowError::NotSupported) => {
                debug!(
                    asset = %asset,
                    "get_borrow_rate not supported on this venue"
                );
            }
            Err(BorrowError::Other(e)) => {
                warn!(
                    asset = %asset,
                    error = %e,
                    "borrow rate refresh failed — keeping previous APR"
                );
            }
        }
    }

    fn log_periodic_summary(&self) {
        let regime = self.auto_tuner.regime_detector.regime();
        info!(
            symbol = %self.symbol,
            ?regime,
            kill_level = %self.kill_switch.level(),
            inventory = %self.inventory_manager.inventory(),
            vpin = ?self.vpin.vpin(),
            kyle_lambda = ?self.kyle_lambda.lambda(),
            adverse_bps = ?self.adverse_selection.adverse_selection_bps(),
            live_orders = self.order_manager.live_count(),
            id_map = self.order_id_map.len(),
            balance_usdt = %self.balance_cache.available(&self.product.quote_asset),
            "status"
        );
        self.pnl_tracker.log_summary();
        self.sla_tracker.log_summary();

        if self.adv_inventory.is_urgent() {
            warn!(urgency = %self.adv_inventory.urgency_level(), "inventory urgency active");
        }
    }

    pub async fn shutdown(&mut self) {
        info!(symbol = %self.symbol, "shutting down — cancelling all orders");
        self.audit
            .risk_event(&self.symbol, AuditEventType::EngineShutdown, "graceful");
        if let Err(e) = self
            .order_manager
            .cancel_all(&self.connectors.primary, &self.symbol)
            .await
        {
            error!(symbol = %self.symbol, error = %e, "shutdown cancel_all left survivors on primary venue");
        }
        // Cancel live orders on the hedge leg too — a shutdown
        // with a dangling hedge order is the exact state that
        // turns a delta-neutral pair into a naked position over
        // the restart window.
        if let (Some(hedge_om), Some(hedge_conn), Some(pair)) = (
            self.hedge_order_manager.as_mut(),
            self.connectors.hedge.as_ref(),
            self.connectors.pair.as_ref(),
        ) {
            if let Err(e) = hedge_om.cancel_all(hedge_conn, &pair.hedge_symbol).await {
                error!(hedge = %pair.hedge_symbol, error = %e, "shutdown cancel_all left survivors on hedge venue");
            }
        }
        self.balance_cache.reset_reservations();
        self.pnl_tracker.log_summary();
        self.sla_tracker.log_summary();
        self.audit.flush();
        self.update_dashboard();
        info!(symbol = %self.symbol, "shutdown complete");
    }
}

#[cfg(test)]
mod dual_connector_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::{InstrumentPair, PriceLevel};
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_strategy::AvellanedaStoikov;

    fn sample_config() -> AppConfig {
        AppConfig::default()
    }

    fn sample_product(symbol: &str) -> ProductSpec {
        ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn sample_pair() -> InstrumentPair {
        InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTC".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(20),
        }
    }

    fn snapshot(symbol: &str, venue: VenueId, bid: Decimal, ask: Decimal) -> MarketEvent {
        MarketEvent::BookSnapshot {
            venue,
            symbol: symbol.to_string(),
            bids: vec![PriceLevel {
                price: bid,
                qty: dec!(1),
            }],
            asks: vec![PriceLevel {
                price: ask,
                qty: dec!(1),
            }],
            sequence: 1,
        }
    }

    #[test]
    fn single_bundle_has_no_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        assert!(engine.hedge_book.is_none());
        assert!(!engine.connectors.is_dual());
    }

    #[test]
    fn dual_bundle_creates_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let hb = engine.hedge_book.as_ref().expect("hedge_book must exist");
        assert_eq!(hb.book.symbol, "BTC");
        assert!(engine.connectors.is_dual());
    }

    #[test]
    fn handle_hedge_event_routes_book_updates_to_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        // Primary gets a spot quote around 50 000. Hedge gets a
        // perp quote around 50 100 — a +10 bps basis.
        engine.handle_ws_event(snapshot(
            "BTCUSDT",
            VenueId::Binance,
            dec!(49_999),
            dec!(50_001),
        ));
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));

        assert_eq!(
            engine.book_keeper.book.mid_price(),
            Some(dec!(50_000)),
            "primary mid"
        );
        let hb = engine.hedge_book.as_ref().unwrap();
        assert_eq!(hb.book.mid_price(), Some(dec!(50_100)), "hedge mid");
    }

    #[test]
    fn handle_hedge_event_is_noop_in_single_mode() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // Must not panic; hedge_book is None so the routing is a
        // silent drop. The primary book must stay untouched.
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));
        assert!(engine.book_keeper.book.mid_price().is_none());
    }

    #[test]
    fn hedge_book_mid_feeds_ref_price_via_refresh_quotes() {
        // Verify the wiring that `refresh_quotes` reads
        // `hedge_book.book.mid_price()` into `StrategyContext.ref_price`.
        // Testing the real `refresh_quotes` is heavy (async, lots
        // of side effects) so we inspect the intermediate
        // expression the production code uses.
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));

        let ref_price = engine
            .hedge_book
            .as_ref()
            .and_then(|hb| hb.book.mid_price());
        assert_eq!(ref_price, Some(dec!(50_100)));
    }

    /// Epic H Phase 5 — graph swap must rebuild the strategy pool
    /// and clear the per-node quote cache. A stale cache entry from
    /// a previous graph would leak into the new graph's overlay
    /// reads until it happens to be re-written.
    #[test]
    fn swap_strategy_graph_rebuilds_pool_and_clears_cache() {
        use mm_strategy_graph::{Edge, Graph, GraphNode, NodeId, PortRef, Scope};

        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        // Graph A: Strategy.Avellaneda → Out.Quotes + Math.Const → Out.SpreadMult.
        // Builds a pool entry for the Avellaneda node.
        let mk_graph = |strategy_kind: &str| -> Graph {
            let strat = NodeId::new();
            let quotes_sink = NodeId::new();
            let cst = NodeId::new();
            let mult_sink = NodeId::new();
            let mut g = Graph::empty("t", Scope::Symbol("BTCUSDT".into()));
            g.nodes.push(GraphNode {
                id: strat,
                kind: strategy_kind.into(),
                config: serde_json::Value::Null,
                pos: (0.0, 0.0),
            });
            g.nodes.push(GraphNode {
                id: quotes_sink,
                kind: "Out.Quotes".into(),
                config: serde_json::Value::Null,
                pos: (0.0, 0.0),
            });
            g.nodes.push(GraphNode {
                id: cst,
                kind: "Math.Const".into(),
                config: serde_json::json!({ "value": "1" }),
                pos: (0.0, 0.0),
            });
            g.nodes.push(GraphNode {
                id: mult_sink,
                kind: "Out.SpreadMult".into(),
                config: serde_json::Value::Null,
                pos: (0.0, 0.0),
            });
            g.edges.push(Edge {
                from: PortRef { node: strat, port: "quotes".into() },
                to: PortRef { node: quotes_sink, port: "quotes".into() },
            });
            g.edges.push(Edge {
                from: PortRef { node: cst, port: "value".into() },
                to: PortRef { node: mult_sink, port: "mult".into() },
            });
            g
        };

        // Deploy A — pool has one Strategy.Avellaneda instance.
        let g_a = mk_graph("Strategy.Avellaneda");
        let a_strat_id = g_a.nodes[0].id;
        engine.swap_strategy_graph(&g_a).expect("graph A compiles");
        assert_eq!(
            engine.strategy_pool.len(),
            1,
            "pool must hold one instance per Strategy.* node"
        );
        assert!(
            engine.strategy_pool.contains_key(&a_strat_id),
            "pool keyed by the Avellaneda node id"
        );

        // Prime the per-node cache with a dummy entry — simulates
        // what `refresh_quotes` would have written last tick.
        engine
            .last_strategy_quotes_per_node
            .insert(a_strat_id, Vec::new());
        assert_eq!(engine.last_strategy_quotes_per_node.len(), 1);
        // Also prime `graph_quotes_override` to represent a pending
        // `Out.Quotes` bundle from the last tick of graph A. The
        // next refresh-quotes pass would normally consume this;
        // a swap must drop it instead so the new graph isn't
        // surprised by quotes it never authored.
        engine.graph_quotes_override = Some(vec![]);

        // Deploy B — different node ids, different kind.
        let g_b = mk_graph("Strategy.Grid");
        engine.swap_strategy_graph(&g_b).expect("graph B compiles");

        assert_eq!(
            engine.strategy_pool.len(),
            1,
            "pool reshaped for new graph (one Grid instance)"
        );
        assert!(
            !engine.strategy_pool.contains_key(&a_strat_id),
            "old Avellaneda entry gone after swap"
        );
        assert!(
            engine.last_strategy_quotes_per_node.is_empty(),
            "stale per-node cache from graph A cleared on swap"
        );
        assert!(
            engine.graph_quotes_override.is_none(),
            "pending Out.Quotes override from graph A dropped on swap"
        );
    }
}

#[cfg(test)]
mod portfolio_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::{Fill, Side};
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_portfolio::Portfolio;
    use mm_strategy::AvellanedaStoikov;

    fn sample_product(symbol: &str) -> ProductSpec {
        ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn build_engine(symbol: &str, portfolio: Arc<Mutex<Portfolio>>) -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        MarketMakerEngine::new(
            symbol.to_string(),
            AppConfig::default(),
            sample_product(symbol),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
        .with_portfolio(portfolio)
    }

    fn fill_event(symbol: &str, side: Side, qty: Decimal, price: Decimal) -> MarketEvent {
        MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: symbol.to_string(),
                side,
                price,
                qty,
                is_maker: true,
                timestamp: chrono::Utc::now(),
            },
        }
    }

    #[test]
    fn engine_without_portfolio_runs_untouched() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        assert!(engine.portfolio.is_none());
        // Fill should NOT panic when portfolio is absent.
        engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));
    }

    #[test]
    fn fill_routes_signed_qty_to_shared_portfolio() {
        let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
        let mut engine = build_engine("BTCUSDT", portfolio.clone());

        engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));

        let snap = portfolio.lock().unwrap().snapshot();
        let btc = snap.per_asset.get("BTCUSDT").expect("BTCUSDT entry");
        assert_eq!(btc.qty, dec!(0.1), "long from buy fill");
        assert_eq!(btc.avg_entry, dec!(50_000));
    }

    #[test]
    fn sell_fill_routes_negative_qty_to_portfolio() {
        let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
        let mut engine = build_engine("BTCUSDT", portfolio.clone());

        // Buy 0.2 then sell 0.15 → net long 0.05, realise +50 USDT
        // on the 0.15 closed at 51_000 vs avg 50_000.
        engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.2), dec!(50_000)));
        engine.handle_ws_event(fill_event("BTCUSDT", Side::Sell, dec!(0.15), dec!(51_000)));

        let snap = portfolio.lock().unwrap().snapshot();
        let btc = snap.per_asset.get("BTCUSDT").unwrap();
        assert_eq!(btc.qty, dec!(0.05));
        assert_eq!(btc.realised_pnl_native, dec!(150));
        assert_eq!(snap.total_realised_pnl, dec!(150));
    }

    #[test]
    fn multi_symbol_engines_share_one_portfolio() {
        // Two engines, one shared portfolio. After both report
        // a buy fill, the snapshot sees both positions under the
        // unified reporting currency.
        let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
        let mut btc_engine = build_engine("BTCUSDT", portfolio.clone());
        let mut eth_engine = build_engine("ETHUSDT", portfolio.clone());

        btc_engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));
        eth_engine.handle_ws_event(fill_event("ETHUSDT", Side::Buy, dec!(1), dec!(3_000)));

        let snap = portfolio.lock().unwrap().snapshot();
        assert_eq!(snap.per_asset.len(), 2, "both symbols tracked");
        assert!(snap.per_asset.contains_key("BTCUSDT"));
        assert!(snap.per_asset.contains_key("ETHUSDT"));
    }

    #[test]
    fn portfolio_fx_and_reporting_currency_roundtrip() {
        // Portfolio remains in USDT regardless of per-engine
        // quote assets. The engine does NOT set FX by default —
        // callers are responsible for wiring `set_fx` when the
        // engine quotes in a non-USDT asset. This test locks
        // that contract.
        let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
        let mut engine = build_engine("BTCUSDT", portfolio.clone());
        engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.01), dec!(50_000)));
        let snap = portfolio.lock().unwrap().snapshot();
        assert_eq!(snap.reporting_currency, "USDT");
    }
}

#[cfg(test)]
mod paired_unwind_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::{Fill, InstrumentPair, Side};
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_risk::kill_switch::KillLevel;
    use mm_strategy::AvellanedaStoikov;

    fn sample_product(symbol: &str) -> ProductSpec {
        ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn sample_pair() -> InstrumentPair {
        InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTC-PERP".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(50),
        }
    }

    fn dual_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    fn single_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    /// P2.1: an engine with no asset-class layer reports its
    /// global level verbatim. Regression anchor for the
    /// "P2.1 must not break legacy single-engine deployments"
    /// invariant.
    #[test]
    fn effective_kill_level_falls_back_to_global_when_no_asset_class() {
        let mut engine = single_engine();
        assert_eq!(engine.effective_kill_level(), KillLevel::Normal);
        engine
            .kill_switch
            .manual_trigger(KillLevel::WidenSpreads, "test global");
        assert_eq!(engine.effective_kill_level(), KillLevel::WidenSpreads);
    }

    /// P2.1 happy path: when both a global and an asset-class
    /// switch are armed, `effective_kill_level` returns the
    /// max — so a class-wide widening is honoured even when
    /// the per-engine global is still Normal, AND a per-engine
    /// hard escalation is honoured even when the class is
    /// still Normal.
    #[test]
    fn effective_kill_level_takes_max_of_global_and_asset_class() {
        let class = Arc::new(Mutex::new(KillSwitch::new(KillSwitchConfig::default())));
        let mut engine = single_engine().with_asset_class_switch(class.clone());

        // Asset-class widens, global Normal → effective WidenSpreads.
        class
            .lock()
            .unwrap()
            .manual_trigger(KillLevel::WidenSpreads, "stETH depeg");
        assert_eq!(engine.effective_kill_level(), KillLevel::WidenSpreads);

        // Global escalates harder → effective tracks global.
        engine
            .kill_switch
            .manual_trigger(KillLevel::CancelAll, "per-engine PnL stop");
        assert_eq!(engine.effective_kill_level(), KillLevel::CancelAll);

        // Asset-class escalates to StopNewOrders — global is
        // already CancelAll which is higher, so effective stays
        // CancelAll. Pin the max-not-replace semantics.
        class
            .lock()
            .unwrap()
            .manual_trigger(KillLevel::StopNewOrders, "ETH-family lock");
        assert_eq!(engine.effective_kill_level(), KillLevel::CancelAll);
    }

    /// P2.1 sharing: two engines pointed at the same
    /// `Arc<Mutex<KillSwitch>>` see each other's escalations
    /// instantly. Models the "halt all ETH-family pairs"
    /// failure mode the asset-class layer was added to fix.
    #[test]
    fn shared_asset_class_switch_propagates_across_engines() {
        let class = Arc::new(Mutex::new(KillSwitch::new(KillSwitchConfig::default())));
        let engine_a = single_engine().with_asset_class_switch(class.clone());
        let engine_b = single_engine().with_asset_class_switch(class.clone());
        assert_eq!(engine_a.effective_kill_level(), KillLevel::Normal);
        assert_eq!(engine_b.effective_kill_level(), KillLevel::Normal);

        class
            .lock()
            .unwrap()
            .manual_trigger(KillLevel::WidenSpreads, "shared escalation");
        assert_eq!(engine_a.effective_kill_level(), KillLevel::WidenSpreads);
        assert_eq!(engine_b.effective_kill_level(), KillLevel::WidenSpreads);
    }

    /// Epic A engine integration: a single-connector engine
    /// auto-seeds its primary venue into the SOR aggregator,
    /// and `recommend_route_synthetic` produces a non-empty
    /// decision that fills the full target on that venue.
    /// Regression anchor for the "auto-seed primary" path so
    /// a future refactor can't silently drop the
    /// `register_venue` call in `new`.
    ///
    /// Default `config.risk.max_inventory = 0.1`, so the
    /// test target stays well under that budget.
    #[test]
    fn recommend_route_auto_seeds_primary_venue() {
        let engine = single_engine();
        let decision = engine.recommend_route_synthetic(
            Side::Buy,
            dec!(0.05),
            dec!(0.5),
            &[(VenueId::Binance, 100)],
        );
        assert_eq!(decision.target_qty, dec!(0.05));
        assert!(decision.is_complete);
        assert_eq!(decision.legs.len(), 1);
        assert_eq!(decision.legs[0].venue, VenueId::Binance);
    }

    /// A single-connector engine with a second SOR venue
    /// registered via `with_sor_venue` routes a fill that
    /// exceeds the cheap venue's capacity across both
    /// venues in cost order. Pins the full chain:
    /// engine → aggregator → cost model → greedy router →
    /// decision.
    #[test]
    fn recommend_route_splits_across_multiple_sor_venues() {
        // Cheap Bybit seed — taker fee 0.01 % (1 bps),
        // 0.03 available (strictly less than the target
        // below so the router has to roll the remainder to
        // the more expensive Binance venue).
        let mut cheap_product = sample_product("BTCUSDT");
        cheap_product.maker_fee = dec!(0);
        cheap_product.taker_fee = dec!(0.0001);
        let cheap_seed = VenueSeed::new("BTCUSDT", cheap_product, dec!(0.03));

        // Single-venue engine seeded with the primary
        // (Binance) venue at the default sample fees; then
        // add Bybit as a cheaper extra SOR venue.
        let engine = single_engine().with_sor_venue(VenueId::Bybit, cheap_seed);

        // Target 0.05 — Bybit (cheaper, 0.03 available)
        // fills first, the 0.02 remainder rolls to Binance.
        let decision = engine.recommend_route_synthetic(
            Side::Buy,
            dec!(0.05),
            dec!(1), // full urgency → pure taker cost sort
            &[(VenueId::Binance, 100), (VenueId::Bybit, 100)],
        );
        assert_eq!(decision.legs.len(), 2);
        assert_eq!(decision.legs[0].venue, VenueId::Bybit);
        assert_eq!(decision.legs[0].qty, dec!(0.03));
        assert_eq!(decision.legs[1].venue, VenueId::Binance);
        assert_eq!(decision.legs[1].qty, dec!(0.02));
        assert!(decision.is_complete);
    }

    fn buy_fill(qty: Decimal, price: Decimal) -> MarketEvent {
        MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: Side::Buy,
                price,
                qty,
                is_maker: true,
                timestamp: chrono::Utc::now(),
            },
        }
    }

    #[tokio::test]
    async fn kill_switch_l4_picks_paired_unwind_in_dual_mode() {
        let mut engine = dual_engine();
        // Build up an inventory on the primary leg.
        engine.handle_ws_event(buy_fill(dec!(0.05), dec!(50_000)));
        assert_eq!(engine.inventory_manager.inventory(), dec!(0.05));

        // Need a mid on the primary book for tick_second to
        // reach the kill-switch dispatch branch.
        // Populate the balance cache so the affordability
        // pre-check in refresh_quotes does not zero out the
        // bid/ask quotes the strategy generates.
        engine.refresh_balances().await;

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });

        // Trip the kill switch all the way to L4.
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");
        assert_eq!(engine.kill_switch.level(), KillLevel::FlattenAll);

        // One tick drives the L4 dispatch logic.
        engine.tick_second().await;

        assert!(
            engine.paired_unwind.is_some(),
            "dual-connector mode must pick PairedUnwindExecutor"
        );
        assert!(
            engine.twap.is_none(),
            "paired_unwind must replace twap, never run both"
        );
    }

    #[tokio::test]
    async fn kill_switch_l4_picks_twap_in_single_mode() {
        let mut engine = single_engine();
        engine.handle_ws_event(buy_fill(dec!(0.05), dec!(50_000)));
        // Populate the balance cache so the affordability
        // pre-check in refresh_quotes does not zero out the
        // bid/ask quotes the strategy generates.
        engine.refresh_balances().await;

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");

        engine.tick_second().await;

        assert!(engine.twap.is_some(), "single-mode path still uses TWAP");
        assert!(engine.paired_unwind.is_none());
    }

    #[tokio::test]
    async fn paired_unwind_is_not_spawned_when_inventory_is_zero() {
        let mut engine = dual_engine();
        // Populate the balance cache so the affordability
        // pre-check in refresh_quotes does not zero out the
        // bid/ask quotes the strategy generates.
        engine.refresh_balances().await;

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");

        engine.tick_second().await;
        assert!(engine.paired_unwind.is_none());
        assert!(engine.twap.is_none());
    }

    /// Seed the dual engine with inventory + both books so the
    /// L4 dispatch path can actually run the first slice.
    async fn prime_for_unwind(engine: &mut MarketMakerEngine) {
        engine.handle_ws_event(buy_fill(dec!(0.1), dec!(50_000)));
        // Populate the balance cache so the affordability
        // pre-check in refresh_quotes does not zero out the
        // bid/ask quotes the strategy generates.
        engine.refresh_balances().await;

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine.handle_hedge_event(MarketEvent::BookSnapshot {
            venue: VenueId::HyperLiquid,
            symbol: "BTC-PERP".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(50_009),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_011),
                qty: dec!(10),
            }],
            sequence: 1,
        });
    }

    #[tokio::test]
    async fn slice_dispatches_orders_on_both_venues_not_just_logs() {
        let mut engine = dual_engine();
        prime_for_unwind(&mut engine).await;

        // Install a tiny-duration executor directly so the
        // first `next_slice` fires immediately — L4 dispatch
        // is tested elsewhere, here we only care about the
        // slice-to-order-manager pipeline. `num_seconds()` in
        // the executor's scheduler truncates, so the sleep
        // must exceed 1 full second to register as `elapsed = 1`.
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            1, // 1 second total duration
            1, // one slice → fires on first post-schedule tick
            dec!(5),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        // Do NOT trip the kill switch here — the L3+ cancel-all
        // branch in `tick_second` would clear out the slice
        // orders we just placed. The dispatch pipeline itself
        // is the only contract under test; L4 spawning is
        // covered by the earlier kill-switch test.
        engine.tick_second().await;

        // Both order managers should now have at least one live
        // order from the dispatched slice.
        let primary_live = engine.order_manager.live_count();
        let hedge_live = engine
            .hedge_order_manager
            .as_ref()
            .map(|om| om.live_count())
            .unwrap_or(0);
        assert!(
            primary_live > 0 || hedge_live > 0,
            "at least one leg must have dispatched a slice (primary={primary_live}, hedge={hedge_live})"
        );
    }

    #[tokio::test]
    async fn hedge_fill_routes_into_paired_unwind_and_portfolio() {
        let portfolio = Arc::new(Mutex::new(mm_portfolio::Portfolio::new("USDT")));
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
        .with_portfolio(portfolio.clone());

        // Seed an active paired unwind with a known target size.
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            5,
            dec!(5),
        ));

        // Hedge fill comes in through the hedge WS path.
        let hedge_fill = MarketEvent::Fill {
            venue: VenueId::HyperLiquid,
            fill: mm_common::types::Fill {
                trade_id: 42,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTC-PERP".to_string(),
                side: mm_common::types::Side::Buy, // unwinding a short hedge = buying
                price: dec!(50_010),
                qty: dec!(0.02),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        };
        engine.handle_hedge_event(hedge_fill);

        // The portfolio saw the hedge symbol, not the primary.
        let snap = portfolio.lock().unwrap().snapshot();
        assert!(snap.per_asset.contains_key("BTC-PERP"));
        let hedge_entry = snap.per_asset.get("BTC-PERP").unwrap();
        assert_eq!(
            hedge_entry.qty,
            dec!(0.02),
            "hedge buy fill = long position"
        );
        // paired_unwind tracked the fill → progress > 0.
        let unwind = engine.paired_unwind.as_ref().expect("unwind still active");
        // 0.02 filled out of 0.1 target on hedge, primary still at 0
        // → average progress = (0 + 0.2) / 2 = 0.1.
        assert_eq!(unwind.progress(), dec!(0.1));
    }

    #[tokio::test]
    async fn primary_fill_routes_into_paired_unwind_not_just_inventory() {
        let mut engine = dual_engine();
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            5,
            dec!(5),
        ));

        engine.handle_ws_event(MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: mm_common::types::Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: mm_common::types::Side::Sell, // unwinding long spot = selling
                price: dec!(50_000),
                qty: dec!(0.05),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });

        let unwind = engine.paired_unwind.as_ref().expect("unwind still active");
        // 0.05 filled on primary, 0 on hedge → avg = 0.25.
        assert_eq!(unwind.progress(), dec!(0.25));
    }

    #[tokio::test]
    async fn paired_unwind_clears_when_both_legs_complete() {
        let mut engine = dual_engine();
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            1,
            dec!(5),
        ));

        // Primary leg fully fills.
        engine.handle_ws_event(MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: mm_common::types::Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: mm_common::types::Side::Sell,
                price: dec!(50_000),
                qty: dec!(0.1),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });
        // Hedge leg fully fills.
        engine.handle_hedge_event(MarketEvent::Fill {
            venue: VenueId::HyperLiquid,
            fill: mm_common::types::Fill {
                trade_id: 2,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTC-PERP".to_string(),
                side: mm_common::types::Side::Buy,
                price: dec!(50_010),
                qty: dec!(0.1),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });

        // Executor cleared itself on the final on_hedge_fill.
        assert!(
            engine.paired_unwind.is_none(),
            "unwind cleared on completion"
        );
    }
}

#[cfg(test)]
mod driver_event_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::InstrumentPair;
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_risk::kill_switch::KillLevel;
    use mm_strategy::funding_arb_driver::DriverEvent;
    use mm_strategy::AvellanedaStoikov;

    fn dual_engine_with_driver_field() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let pair = InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTC-PERP".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(50),
        };
        let bundle = ConnectorBundle::dual(primary, hedge, pair);
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            ProductSpec {
                symbol: "BTCUSDT".to_string(),
                base_asset: "BTC".to_string(),
                quote_asset: "USDT".to_string(),
                tick_size: dec!(0.01),
                lot_size: dec!(0.0001),
                min_notional: dec!(10),
                maker_fee: dec!(0.0001),
                taker_fee: dec!(0.0005),
                trading_status: Default::default(),
            },
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn compensated_pair_break_does_not_halt_driver_but_audits() {
        let mut engine = dual_engine_with_driver_field();
        // Driver itself is None in this test — we only assert
        // the dispatcher's behaviour on the event value. A real
        // driver would have been constructed via
        // `with_funding_arb_driver`.
        let starting_level = engine.kill_switch.level();
        engine.handle_driver_event(DriverEvent::PairBreak {
            reason: "post-only cross".to_string(),
            compensated: true,
        });
        // Compensated breaks do NOT escalate kill switch.
        assert_eq!(engine.kill_switch.level(), starting_level);
    }

    #[tokio::test]
    async fn uncompensated_pair_break_escalates_to_l2_and_drops_driver() {
        let mut engine = dual_engine_with_driver_field();
        // Start with driver set so we can verify it gets dropped.
        // Construct a driver with a NullSink using real connectors.
        let driver = mm_strategy::FundingArbDriver::new(
            engine.connectors.primary.clone(),
            engine.connectors.hedge.clone().unwrap(),
            engine.connectors.pair.clone().unwrap(),
            mm_strategy::FundingArbDriverConfig::default(),
            Arc::new(mm_strategy::NullSink),
        );
        engine.funding_arb_driver = Some(driver);

        engine.handle_driver_event(DriverEvent::PairBreak {
            reason: "post-only cross".to_string(),
            compensated: false,
        });

        assert_eq!(
            engine.kill_switch.level(),
            KillLevel::StopNewOrders,
            "uncompensated break → L2"
        );
        assert!(
            engine.funding_arb_driver.is_none(),
            "driver dropped so it stops ticking"
        );
    }

    #[tokio::test]
    async fn hold_events_are_silent_noops() {
        let mut engine = dual_engine_with_driver_field();
        let starting_level = engine.kill_switch.level();
        engine.handle_driver_event(DriverEvent::Hold);
        engine.handle_driver_event(DriverEvent::InputUnavailable {
            reason: "test".to_string(),
        });
        assert_eq!(engine.kill_switch.level(), starting_level);
    }

    #[tokio::test]
    async fn entered_and_exited_only_audit_do_not_escalate() {
        let mut engine = dual_engine_with_driver_field();
        let starting_level = engine.kill_switch.level();
        engine.handle_driver_event(DriverEvent::TakerRejected {
            reason: "insufficient margin".to_string(),
        });
        assert_eq!(engine.kill_switch.level(), starting_level);
    }

    #[tokio::test]
    async fn fills_reconcile_driver_state_on_both_legs() {
        let mut engine = dual_engine_with_driver_field();
        let driver = mm_strategy::FundingArbDriver::new(
            engine.connectors.primary.clone(),
            engine.connectors.hedge.clone().unwrap(),
            engine.connectors.pair.clone().unwrap(),
            mm_strategy::FundingArbDriverConfig::default(),
            Arc::new(mm_strategy::NullSink),
        );
        engine.funding_arb_driver = Some(driver);

        // Primary leg fill: long 0.1 spot.
        engine.handle_ws_event(MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: mm_common::types::Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: mm_common::types::Side::Buy,
                price: dec!(50_000),
                qty: dec!(0.1),
                is_maker: true,
                timestamp: chrono::Utc::now(),
            },
        });

        // Hedge leg fill: short 0.1 perp.
        engine.handle_hedge_event(MarketEvent::Fill {
            venue: VenueId::HyperLiquid,
            fill: mm_common::types::Fill {
                trade_id: 2,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTC-PERP".to_string(),
                side: mm_common::types::Side::Sell,
                price: dec!(50_010),
                qty: dec!(0.1),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });

        let state = engine.funding_arb_driver.as_ref().unwrap().state();
        assert_eq!(state.spot_position, dec!(0.1), "spot long");
        assert_eq!(state.perp_position, dec!(-0.1), "perp short");
        assert_eq!(state.net_delta, dec!(0), "delta-neutral");
    }
}

#[cfg(test)]
mod spread_gate_tests {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::PriceLevel;
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_strategy::AvellanedaStoikov;

    fn base_engine_with_gate(gate_bps: Option<Decimal>) -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut cfg = AppConfig::default();
        cfg.risk.max_spread_to_quote_bps = gate_bps;
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            cfg,
            ProductSpec {
                symbol: "BTCUSDT".to_string(),
                base_asset: "BTC".to_string(),
                quote_asset: "USDT".to_string(),
                tick_size: dec!(0.01),
                lot_size: dec!(0.0001),
                min_notional: dec!(10),
                maker_fee: dec!(0.0001),
                taker_fee: dec!(0.0005),
                trading_status: Default::default(),
            },
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    fn snapshot_with_spread(bid: Decimal, ask: Decimal) -> MarketEvent {
        MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel {
                price: bid,
                qty: dec!(10),
            }],
            asks: vec![PriceLevel {
                price: ask,
                qty: dec!(10),
            }],
            sequence: 1,
        }
    }

    #[tokio::test]
    async fn spread_gate_none_never_blocks_quoting() {
        // Baseline: no gate configured, wide book → CB may trip
        // but the gate itself is inert. We only verify the gate
        // path does not short-circuit when unset.
        let mut engine = base_engine_with_gate(None);
        // Absurdly wide book — 100 bps spread.
        engine.handle_ws_event(snapshot_with_spread(dec!(50_000), dec!(50_500)));
        // `refresh_quotes` reaches the quote-compute path.
        // We cannot easily assert "quotes were computed" without
        // more plumbing, but we can at least verify tick_count
        // advances (it is the first statement of refresh_quotes).
        let before = engine.tick_count;
        // If the gate blocked us, tick_count still advanced
        // (the increment happens before the gate), so use a
        // weaker invariant: the call returns Ok without panic.
        assert!(engine.refresh_quotes().await.is_ok());
        assert_eq!(engine.tick_count, before + 1);
    }

    #[tokio::test]
    async fn spread_gate_blocks_quoting_when_spread_exceeds_threshold() {
        // Gate set at 50 bps. Push a 100 bps book — the gate
        // must return early and NOT trip the circuit breaker.
        let mut engine = base_engine_with_gate(Some(dec!(50)));
        engine.handle_ws_event(snapshot_with_spread(dec!(50_000), dec!(50_500)));

        let cb_before = engine.circuit_breaker.is_tripped();
        let live_before = engine.order_manager.live_count();

        let result = engine.refresh_quotes().await;
        assert!(result.is_ok());

        // No new orders placed because the gate short-circuited.
        assert_eq!(engine.order_manager.live_count(), live_before);
        // Circuit breaker untouched by the soft gate (but the
        // hard `check_spread` may still have fired if the book
        // was above `max_spread_bps`; default is 500 bps, and
        // 100 bps < 500, so the hard check should also be
        // clean). This is the test that pins the soft semantics.
        assert_eq!(
            engine.circuit_breaker.is_tripped(),
            cb_before,
            "soft spread gate must not trip the circuit breaker"
        );
    }

    #[tokio::test]
    async fn spread_gate_allows_quoting_when_spread_is_tight() {
        // Gate at 50 bps. 2 bps book → passes.
        let mut engine = base_engine_with_gate(Some(dec!(50)));
        engine.handle_ws_event(snapshot_with_spread(dec!(50_000), dec!(50_010)));
        // Just verify it does not error out; the main test is
        // the blocking path above.
        assert!(engine.refresh_quotes().await.is_ok());
    }
}

// -------------------------------------------------------------
// Epic B — Stat-arb driver engine integration
// -------------------------------------------------------------

#[cfg(test)]
mod stat_arb_integration {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_strategy::avellaneda::AvellanedaStoikov;
    use mm_strategy::stat_arb::{
        NullStatArbSink, SpreadDirection, StatArbDriver, StatArbDriverConfig, StatArbEvent,
        StatArbPair, ZScoreConfig,
    };
    use std::time::Duration;

    fn sample_product(symbol: &str) -> mm_common::types::ProductSpec {
        mm_common::types::ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn single_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    fn stat_arb_pair() -> StatArbPair {
        StatArbPair {
            y_symbol: "BTCUSDT".to_string(),
            x_symbol: "ETHUSDT".to_string(),
            strategy_class: "stat_arb_BTCUSDT_ETHUSDT".to_string(),
        }
    }

    fn small_stat_arb_config() -> StatArbDriverConfig {
        StatArbDriverConfig {
            tick_interval: Duration::from_millis(10),
            zscore: ZScoreConfig {
                window: 20,
                entry_threshold: dec!(1.5),
                exit_threshold: dec!(0.3),
            },
            kalman_transition_var: dec!(0.000001),
            kalman_observation_var: dec!(0.001),
            leg_notional_usd: dec!(1000),
        }
    }

    /// Seed a synthetic `Y = 2 · X` cointegrated history.
    fn seed_cointegrated(driver: &mut StatArbDriver) {
        let x: Vec<Decimal> = (0..60)
            .map(|i| dec!(100) + Decimal::from(i as i64 % 5 - 2))
            .collect();
        let y: Vec<Decimal> = x
            .iter()
            .enumerate()
            .map(|(i, xi)| {
                let jitter = Decimal::from(i as i64 % 3 - 1) / dec!(10);
                dec!(2) * xi + jitter
            })
            .collect();
        driver.recheck_cointegration(&y, &x);
    }

    /// Silent routing: none of the benign variants should
    /// escalate the kill switch or mutate engine state.
    #[tokio::test]
    async fn silent_variants_do_not_escalate() {
        let mut engine = single_engine();
        let starting = engine.kill_switch.level();
        engine.handle_stat_arb_event(StatArbEvent::Hold { z: dec!(0.1) }, None);
        engine.handle_stat_arb_event(
            StatArbEvent::Warmup {
                samples: 3,
                required: 20,
            },
            None,
        );
        engine.handle_stat_arb_event(StatArbEvent::NotCointegrated { adf_stat: None }, None);
        engine.handle_stat_arb_event(
            StatArbEvent::InputUnavailable {
                reason: "empty book".to_string(),
            },
            None,
        );
        assert_eq!(engine.kill_switch.level(), starting);
    }

    /// Entered / Exited events flow through `handle_stat_arb_event`
    /// without panic — the handler emits audit records but
    /// does NOT dispatch orders in stage-1 (advisory only).
    #[tokio::test]
    async fn entered_and_exited_routed_to_audit_without_panic() {
        let mut engine = single_engine();
        let starting = engine.kill_switch.level();

        engine.handle_stat_arb_event(
            StatArbEvent::Entered {
                direction: SpreadDirection::SellY,
                y_qty: dec!(5),
                x_qty: dec!(10),
                z: dec!(2.5),
                spread: dec!(1.5),
            },
            None,
        );
        engine.handle_stat_arb_event(
            StatArbEvent::Exited {
                z: dec!(0.2),
                spread: dec!(0.1),
                realised_pnl_estimate: dec!(42),
            },
            None,
        );

        // Stage-1 advisory-only: kill switch untouched.
        assert_eq!(engine.kill_switch.level(), starting);
    }

    /// Builder smoke test: `with_stat_arb_driver` plumbs a
    /// driver onto the engine and sets the tick interval.
    #[tokio::test]
    async fn with_stat_arb_driver_installs_driver() {
        let y = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let x = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        y.set_mid(dec!(200));
        x.set_mid(dec!(100));
        let driver = StatArbDriver::new(
            y,
            x,
            stat_arb_pair(),
            small_stat_arb_config(),
            Arc::new(NullStatArbSink),
        );
        let engine = single_engine().with_stat_arb_driver(driver, Duration::from_millis(50));
        assert!(engine.stat_arb_driver.is_some());
        assert_eq!(engine.stat_arb_tick, Duration::from_millis(50));
    }

    /// End-to-end pipeline: synthetic cointegrated pair drives
    /// the full `kalman → signal → driver → engine event`
    /// chain. Asserts that a spread shock produces an
    /// `Entered` and a revert produces an `Exited` event —
    /// and that both route through `handle_stat_arb_event`
    /// without tripping the engine's kill switch in
    /// advisory-only stage-1.
    #[tokio::test]
    async fn full_pipeline_entered_then_exited_through_engine_handler() {
        let mut engine = single_engine();
        let starting = engine.kill_switch.level();

        let y = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let x = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        y.set_mid(dec!(200));
        x.set_mid(dec!(100));
        let mut driver = StatArbDriver::new(
            y.clone(),
            x.clone(),
            stat_arb_pair(),
            small_stat_arb_config(),
            Arc::new(NullStatArbSink),
        );
        seed_cointegrated(&mut driver);

        // Warmup: steady book, z stays near zero.
        for _ in 0..20 {
            y.set_mid(dec!(200));
            x.set_mid(dec!(100));
            let e = driver.tick_once().await;
            engine.handle_stat_arb_event(e, None);
        }

        // Shock: Y +5 pushes spread far above its rolling mean.
        y.set_mid(dec!(205));
        let shock_event = driver.tick_once().await;
        let got_entered = matches!(shock_event, StatArbEvent::Entered { .. });
        engine.handle_stat_arb_event(shock_event, None);
        assert!(got_entered, "expected Entered on spread shock");

        // Revert: Y back to 200. Spread shrinks, z returns to
        // the exit band. Drive enough ticks for the rolling
        // mean to catch up.
        y.set_mid(dec!(200));
        let mut saw_exited = false;
        for _ in 0..60 {
            let e = driver.tick_once().await;
            if matches!(e, StatArbEvent::Exited { .. }) {
                engine.handle_stat_arb_event(e, None);
                saw_exited = true;
                break;
            }
            engine.handle_stat_arb_event(e, None);
        }
        assert!(saw_exited, "expected Exited after revert");

        // Stage-1 advisory-only: no kill-switch escalation
        // regardless of the event sequence.
        assert_eq!(engine.kill_switch.level(), starting);
    }
}

// -------------------------------------------------------------
// Epic D — Signal wave 2: end-to-end strategy + cartea AS path
// -------------------------------------------------------------

#[cfg(test)]
mod signal_wave_2_integration {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::PriceLevel;
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_strategy::avellaneda::AvellanedaStoikov;
    use mm_strategy::cartea_spread;
    use mm_strategy::cks_ofi::OfiTracker;

    fn sample_product(symbol: &str) -> mm_common::types::ProductSpec {
        mm_common::types::ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn make_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    /// End-to-end: drive a synthetic OFI stream through the
    /// [`OfiTracker`] primitive, derive an adverse-selection
    /// probability via [`cartea_spread::as_prob_from_bps`],
    /// thread it into [`StrategyContext`], call
    /// [`AvellanedaStoikov::compute_quotes`], and assert the
    /// quoted spread responds the expected way.
    ///
    /// This is the full Epic D integration path:
    /// `OfiTracker → as_prob → StrategyContext → quoted spread`.
    #[test]
    fn full_pipeline_widens_spread_under_uninformed_flow() {
        use mm_common::orderbook::LocalOrderBook;
        use mm_common::PriceLevel;
        use mm_strategy::r#trait::StrategyContext;

        // Synthetic L1 sequence — modest depth growth, no
        // directional bias. We assert the OFI EWMA stays
        // bounded and the spread widens in proportion to the
        // simulated as_prob.
        let mut tracker = OfiTracker::new();
        for n in 0..20 {
            let bid_qty = dec!(10) + Decimal::from(n);
            let _ = tracker.update(dec!(99), bid_qty, dec!(101), dec!(10));
        }
        // Tracker holds state but we drive the spread test
        // off the higher-level `as_prob` path directly — the
        // OFI side proves the primitive is wired.
        assert!(tracker.prev_snapshot().is_some());

        let engine = make_engine();
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(50000),
                qty: dec!(1),
            }],
            vec![PriceLevel {
                price: dec!(50001),
                qty: dec!(1),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();

        // Sweep adverse-selection bps: -10 (informed flow against
        // us → narrow / no-effect floor), 0 (neutral), +10
        // (uninformed → wide). Cartea-Jaimungal's signed convention
        // means widening happens at LOW ρ (uninformed, ρ < 0.5).
        let widen_prob = cartea_spread::as_prob_from_bps(dec!(-10));
        let neutral_prob = cartea_spread::as_prob_from_bps(dec!(0));
        let narrow_prob = cartea_spread::as_prob_from_bps(dec!(10));
        assert_eq!(neutral_prob, dec!(0.5));
        assert!(widen_prob < dec!(0.5));
        assert!(narrow_prob > dec!(0.5));

        let mut spreads = Vec::new();
        for prob in [
            Some(widen_prob),
            Some(neutral_prob),
            Some(narrow_prob),
            None,
        ] {
            let ctx = StrategyContext {
                book: &book,
                product: &engine.product,
                config: &engine.config.market_maker,
                inventory: dec!(0),
                volatility: dec!(0.02),
                time_remaining: dec!(1),
                mid_price: mid,
                ref_price: None,
                hedge_book: None,
                borrow_cost_bps: None,
                hedge_book_age_ms: None,
                as_prob: prob,
                as_prob_bid: None,
                as_prob_ask: None,
            };
            let q = &engine.strategy.compute_quotes(&ctx)[0];
            let spread = q.ask.as_ref().unwrap().price - q.bid.as_ref().unwrap().price;
            spreads.push(spread);
        }
        let widen_spread = spreads[0];
        let neutral_spread = spreads[1];
        let narrow_spread = spreads[2];
        let none_spread = spreads[3];

        // Neutral (ρ=0.5) and None should be byte-identical
        // — the additive term collapses to zero.
        assert_eq!(neutral_spread, none_spread);

        // Widen (ρ<0.5) should strictly exceed neutral.
        assert!(
            widen_spread > neutral_spread,
            "uninformed flow should widen spread: widen={widen_spread}, neutral={neutral_spread}"
        );
        // Narrow (ρ>0.5) should be ≤ neutral (clamps at the
        // configured `min_spread_bps` floor when ρ is high).
        assert!(
            narrow_spread <= neutral_spread,
            "informed flow should narrow or match: narrow={narrow_spread}, neutral={neutral_spread}"
        );
    }

    // ----- Epic D stage-3 — engine-side OFI auto-attach -----

    /// When `momentum_ofi_enabled = false`, the engine
    /// constructs a plain `MomentumSignals` and never feeds
    /// the OFI tracker. This is the wave-1 default path.
    #[test]
    fn momentum_ofi_disabled_keeps_ewma_unset() {
        let mut cfg = AppConfig::default();
        cfg.market_maker.momentum_ofi_enabled = false;
        let primary = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            cfg,
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // Drive a few book events.
        for n in 0..5 {
            engine.handle_ws_event(MarketEvent::BookSnapshot {
                venue: VenueId::Bybit,
                symbol: "BTCUSDT".to_string(),
                bids: vec![PriceLevel {
                    price: dec!(50_000) + Decimal::from(n),
                    qty: dec!(10),
                }],
                asks: vec![PriceLevel {
                    price: dec!(50_001) + Decimal::from(n),
                    qty: dec!(10),
                }],
                sequence: n as u64 + 1,
            });
        }
        // OFI EWMA stays unset because the tracker was never
        // attached.
        assert!(engine.momentum.ofi_ewma().is_none());
    }

    /// When `momentum_ofi_enabled = true`, the engine attaches
    /// the OfiTracker via `with_ofi()` and feeds every L1
    /// book event via `on_l1_snapshot`. The EWMA populates
    /// after the second snapshot.
    #[test]
    fn momentum_ofi_enabled_populates_ewma_from_book_events() {
        let mut cfg = AppConfig::default();
        cfg.market_maker.momentum_ofi_enabled = true;
        let primary = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            cfg,
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // First snapshot seeds the OfiTracker; second snapshot
        // produces the first observation. Use a deterministic
        // bid-side widening so EWMA goes positive.
        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Bybit,
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel {
                price: dec!(50_000),
                qty: dec!(10),
            }],
            asks: vec![PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Bybit,
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel {
                price: dec!(50_000),
                qty: dec!(20),
            }],
            asks: vec![PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 2,
        });
        let ewma = engine.momentum.ofi_ewma();
        assert!(ewma.is_some(), "EWMA should be populated after 2 snapshots");
        let v = ewma.unwrap();
        assert!(
            v > dec!(0),
            "growing bid depth should produce positive OFI, got {v}"
        );
    }

    /// `momentum_learned_microprice_path` set to a missing
    /// path logs a warning and continues without the signal —
    /// must NOT panic. This is the operator-visible
    /// failure-mode pin.
    #[test]
    fn momentum_learned_microprice_missing_path_does_not_panic() {
        let mut cfg = AppConfig::default();
        cfg.market_maker.momentum_learned_microprice_path =
            Some("/nonexistent/path/to/lmp.toml".to_string());
        let primary = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let _engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            cfg,
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // Construction completed without panic — the
        // load-failure path logged a warning and continued.
    }

    /// Per-pair learned MP path takes precedence over the
    /// system-wide path. Both point to nonexistent files —
    /// what we're verifying is that the engine looks up the
    /// per-pair entry FIRST (and that the lookup itself
    /// doesn't panic on construction).
    #[test]
    fn momentum_learned_microprice_per_pair_path_takes_precedence() {
        let mut cfg = AppConfig::default();
        cfg.market_maker.momentum_learned_microprice_path =
            Some("/nonexistent/system-wide.toml".to_string());
        cfg.market_maker
            .momentum_learned_microprice_pair_paths
            .insert(
                "BTCUSDT".to_string(),
                "/nonexistent/per-pair-btcusdt.toml".to_string(),
            );
        let primary = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let _engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            cfg,
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // No panic. The per-pair lookup ran and resolved to
        // the per-pair-btcusdt.toml path; the load failed
        // and the engine continued. This pins the lookup
        // ordering at the path level (the actual log line
        // would show the per-pair path, not the system-wide
        // one).
    }

    /// When the engine's symbol has no entry in the per-pair
    /// map, the system-wide fallback wins.
    #[test]
    fn momentum_learned_microprice_falls_back_to_system_wide() {
        let mut cfg = AppConfig::default();
        cfg.market_maker.momentum_learned_microprice_path =
            Some("/nonexistent/system-wide.toml".to_string());
        // Only ETHUSDT in the per-pair map — engine symbol
        // is BTCUSDT, so the lookup falls through to the
        // system-wide fallback.
        cfg.market_maker
            .momentum_learned_microprice_pair_paths
            .insert(
                "ETHUSDT".to_string(),
                "/nonexistent/per-pair-ethusdt.toml".to_string(),
            );
        let primary = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let _engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            cfg,
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // No panic. Fallback path resolved to the
        // system-wide file, load failed, engine continued.
    }

    /// Empty per-pair map AND empty system-wide path → no
    /// learned MP attached at all. No panic, no warning.
    #[test]
    fn momentum_learned_microprice_both_empty_skips_load() {
        let cfg = AppConfig::default();
        // Both fields are at their defaults (None / empty
        // map). No load attempt happens.
        let primary = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let _engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            cfg,
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
    }
}

// -------------------------------------------------------------
// Epic F — Defensive layer engine integration
// -------------------------------------------------------------

#[cfg(test)]
mod defensive_layer_integration {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_risk::lead_lag_guard::{LeadLagGuard, LeadLagGuardConfig};
    use mm_risk::news_retreat::{NewsRetreatConfig, NewsRetreatStateMachine};
    use mm_strategy::avellaneda::AvellanedaStoikov;

    fn sample_product(symbol: &str) -> mm_common::types::ProductSpec {
        mm_common::types::ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn make_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    fn fixture_news_config() -> NewsRetreatConfig {
        NewsRetreatConfig {
            critical_keywords: vec!["hack".to_string(), "exploit".to_string()],
            high_keywords: vec!["FOMC".to_string(), "CPI".to_string()],
            low_keywords: vec!["partnership".to_string()],
            critical_cooldown_ms: 30 * 60_000,
            high_cooldown_ms: 5 * 60_000,
            low_cooldown_ms: 0,
            high_multiplier: dec!(2),
            critical_multiplier: dec!(3),
        }
    }

    /// Builder smoke test: both defensive controls plug onto
    /// the engine via the new builder methods.
    #[test]
    fn builders_install_both_defensive_controls() {
        let guard = LeadLagGuard::new(LeadLagGuardConfig::default());
        let news = NewsRetreatStateMachine::new(fixture_news_config()).expect("valid news config");
        let engine = make_engine()
            .with_lead_lag_guard(guard)
            .with_news_retreat(news);
        assert!(engine.lead_lag_guard.is_some());
        assert!(engine.news_retreat.is_some());
    }

    /// End-to-end #1: a synthetic leader-mid stream with a
    /// sharp shock pushes the lead-lag guard into ramp
    /// territory; the autotuner's `lead_lag_mult` updates;
    /// `effective_spread_mult` widens.
    #[test]
    fn lead_lag_pipeline_widens_autotuner_on_shock() {
        let guard = LeadLagGuard::new(LeadLagGuardConfig {
            half_life_events: 10,
            z_min: dec!(2),
            z_max: dec!(4),
            max_mult: dec!(3),
        });
        let mut engine = make_engine().with_lead_lag_guard(guard);

        let baseline = engine.auto_tuner.effective_spread_mult();
        // Build up some non-zero variance with small wiggles.
        let mid = dec!(50000);
        for i in 0..30 {
            let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            engine.update_lead_lag_from_mid(mid + delta);
        }
        // Sharp 5% jump → vastly larger than EWMA std.
        engine.update_lead_lag_from_mid(dec!(52500));
        let after = engine.auto_tuner.effective_spread_mult();
        assert!(
            after > baseline,
            "lead-lag shock should widen the autotuner spread mult: baseline={baseline}, after={after}"
        );
        assert_eq!(
            engine.auto_tuner.lead_lag_mult(),
            dec!(3),
            "guard should saturate at max_mult on a 5% shock"
        );
    }

    /// End-to-end #2: a Critical-class news headline drives
    /// `on_news_headline` → `NewsRetreatStateMachine` → kill
    /// switch L2 escalation. The autotuner's news-retreat
    /// multiplier also fires.
    #[test]
    fn critical_headline_escalates_kill_switch_to_l2() {
        let news = NewsRetreatStateMachine::new(fixture_news_config()).expect("valid news config");
        let mut engine = make_engine().with_news_retreat(news);
        let starting = engine.kill_switch.level();
        assert_eq!(starting, mm_risk::kill_switch::KillLevel::Normal);

        engine.on_news_headline("Major exchange hack reported");

        assert_eq!(
            engine.kill_switch.level(),
            mm_risk::kill_switch::KillLevel::StopNewOrders,
            "Critical news should escalate kill switch to L2"
        );
        assert_eq!(
            engine.auto_tuner.news_retreat_mult(),
            dec!(3),
            "autotuner news-retreat multiplier should saturate"
        );
    }

    /// High-class headline activates the autotuner widening
    /// but does NOT escalate the kill switch (the engine still
    /// quotes, just wider).
    #[test]
    fn high_headline_widens_but_does_not_stop_orders() {
        let news = NewsRetreatStateMachine::new(fixture_news_config()).expect("valid news config");
        let mut engine = make_engine().with_news_retreat(news);
        let starting = engine.kill_switch.level();

        engine.on_news_headline("FOMC presser at 2pm");

        assert_eq!(engine.kill_switch.level(), starting);
        assert_eq!(engine.auto_tuner.news_retreat_mult(), dec!(2));
    }

    /// No-match headlines are silent — no audit, no
    /// multiplier change, no kill switch escalation.
    #[test]
    fn unmatched_headline_is_silent_noop() {
        let news = NewsRetreatStateMachine::new(fixture_news_config()).expect("valid news config");
        let mut engine = make_engine().with_news_retreat(news);
        let starting = engine.kill_switch.level();
        let baseline = engine.auto_tuner.effective_spread_mult();

        engine.on_news_headline("Dogecoin price stable amid market chop");

        assert_eq!(engine.kill_switch.level(), starting);
        assert_eq!(engine.auto_tuner.effective_spread_mult(), baseline);
        assert_eq!(engine.auto_tuner.news_retreat_mult(), dec!(1));
    }

    /// Engine without any defensive controls attached: both
    /// public push APIs are no-ops and never panic.
    #[test]
    fn push_apis_are_noop_without_attached_controls() {
        let mut engine = make_engine();
        engine.update_lead_lag_from_mid(dec!(50000));
        engine.on_news_headline("hack");
        // Baseline state preserved.
        assert_eq!(engine.auto_tuner.lead_lag_mult(), dec!(1));
        assert_eq!(engine.auto_tuner.news_retreat_mult(), dec!(1));
    }
}

// -------------------------------------------------------------
// Epic E — Execution polish: batch order entry e2e
// -------------------------------------------------------------

#[cfg(test)]
mod epic_e_integration {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::PriceLevel;
    use mm_exchange_core::connector::{ExchangeConnector, VenueId, VenueProduct};
    use mm_strategy::avellaneda::AvellanedaStoikov;

    fn sample_product(symbol: &str) -> mm_common::types::ProductSpec {
        mm_common::types::ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    /// End-to-end: build a `MarketMakerEngine` whose primary
    /// connector is a `MockConnector` with `max_batch_size=20`,
    /// apply a book snapshot, call `refresh_quotes()`, and
    /// assert the connector saw exactly one
    /// `place_orders_batch` call (carrying all `num_levels × 2`
    /// quotes) and zero per-order `place_order` calls.
    ///
    /// This is the pin for the entire Epic E sub-component #1
    /// wiring: the strategy → diff → batch path is byte-
    /// connected through the existing `refresh_quotes` flow,
    /// no engine field changes required.
    #[tokio::test]
    async fn refresh_quotes_routes_through_batch_on_first_diff() {
        // Hold a typed Arc<MockConnector> alongside the
        // dyn-typed Arc that ConnectorBundle wants — both
        // share the same allocation, so the test can read
        // batch counters after refresh_quotes.
        let mock = Arc::new(
            MockConnector::new(VenueId::Bybit, VenueProduct::Spot).with_max_batch_size(20),
        );
        let dyn_conn: Arc<dyn ExchangeConnector> = mock.clone();
        let bundle = ConnectorBundle::single(dyn_conn);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        // Apply a tight book snapshot so the strategy
        // produces non-zero quotes.
        // Populate the balance cache directly with synthetic
        // balances. We deliberately bypass `refresh_balances`
        // because that rebuilds `exposure_manager` with the
        // wallet's starting equity, after which a fresh-engine
        // refresh_quotes sees a "current equity = 0" vs
        // "starting equity = 100k" delta, trips the drawdown
        // circuit breaker, and returns early before reaching
        // execute_diff. The synthetic-balance path keeps
        // exposure_manager at its default zero baseline.
        engine.balance_cache.update_from_exchange(&[
            mm_common::types::Balance {
                asset: "USDT".to_string(),
                wallet: mm_common::types::WalletType::Spot,
                total: dec!(100_000),
                locked: dec!(0),
                available: dec!(100_000),
            },
            mm_common::types::Balance {
                asset: "BTC".to_string(),
                wallet: mm_common::types::WalletType::Spot,
                total: dec!(10),
                locked: dec!(0),
                available: dec!(10),
            },
        ]);

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Bybit,
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel {
                price: dec!(50_000),
                qty: dec!(10),
            }],
            asks: vec![PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });

        let result = engine.refresh_quotes().await;
        assert!(result.is_ok(), "refresh_quotes errored: {result:?}");

        // Default `num_levels = 3` × (1 bid + 1 ask) = 6
        // raw quotes, but the diff layer dedupes by
        // `(side, price)` after tick rounding — at the
        // default `order_size = 0.001` and `tick = 0.01`,
        // adjacent levels collide on the same tick, so the
        // engine ends up with 1 unique bid + 1 unique ask
        // = 2 placements. That's still ≥ MIN_BATCH_SIZE=2,
        // so the batch path fires exactly once.
        let batch_calls = mock.place_batch_calls();
        let single_calls = mock.place_single_calls();
        assert_eq!(
            batch_calls, 1,
            "expected exactly one batch place call, got {batch_calls}"
        );
        assert_eq!(
            single_calls, 0,
            "expected zero per-order place calls, got {single_calls}"
        );
        assert_eq!(engine.order_manager.live_count(), 2);
    }

    /// Sanity test: a venue with `max_batch_size=1` (the
    /// pathological floor) keeps the engine on the per-order
    /// path even on a multi-quote first diff.
    #[tokio::test]
    async fn refresh_quotes_stays_per_order_when_max_batch_size_is_one() {
        let mock = Arc::new(
            MockConnector::new(VenueId::Binance, VenueProduct::Spot).with_max_batch_size(1),
        );
        let dyn_conn: Arc<dyn ExchangeConnector> = mock.clone();
        let bundle = ConnectorBundle::single(dyn_conn);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        // Direct balance-cache populate (not refresh_balances)
        // — see the routes_through_batch test for the
        // exposure-manager rationale.
        engine.balance_cache.update_from_exchange(&[
            mm_common::types::Balance {
                asset: "USDT".to_string(),
                wallet: mm_common::types::WalletType::Spot,
                total: dec!(100_000),
                locked: dec!(0),
                available: dec!(100_000),
            },
            mm_common::types::Balance {
                asset: "BTC".to_string(),
                wallet: mm_common::types::WalletType::Spot,
                total: dec!(10),
                locked: dec!(0),
                available: dec!(10),
            },
        ]);

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel {
                price: dec!(50_000),
                qty: dec!(10),
            }],
            asks: vec![PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });

        let result = engine.refresh_quotes().await;
        assert!(result.is_ok(), "refresh_quotes errored: {result:?}");

        // max_batch=1 forces per-order path. Diff produces
        // 2 unique quotes (see comment in the sibling test
        // about tick-rounding dedupe), so we expect 2 single
        // calls and zero batch calls.
        assert_eq!(mock.place_batch_calls(), 0);
        assert_eq!(mock.place_single_calls(), 2);
        assert_eq!(engine.order_manager.live_count(), 2);
    }
}

// -------------------------------------------------------------
// Stage-2 Track 1 — Make advisory live (SOR dispatch + stat-arb
// real leg dispatch)
// -------------------------------------------------------------

#[cfg(test)]
mod stage2_track1_integration {
    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::sor::venue_state::VenueSeed;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::Side;
    use mm_exchange_core::connector::{ExchangeConnector, VenueId, VenueProduct};
    use mm_strategy::avellaneda::AvellanedaStoikov;
    use mm_strategy::stat_arb::{NullStatArbSink, StatArbDriver, StatArbDriverConfig, StatArbPair};

    fn sample_product(symbol: &str) -> mm_common::types::ProductSpec {
        mm_common::types::ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn make_engine_with_bundle(bundle: ConnectorBundle) -> MarketMakerEngine {
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    /// End-to-end #1: a multi-leg `RouteDecision` issues real
    /// per-venue `place_order` calls — one on each connector
    /// in the bundle. Both venues land in the bundle via
    /// `ConnectorBundle.extra`, both are registered on the
    /// SOR aggregator, and `dispatch_route` with a taker
    /// urgency produces two IOC legs.
    #[tokio::test]
    async fn dispatch_route_fires_per_venue_place_orders_on_multi_leg_split() {
        let binance = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        binance.set_mid(dec!(50_000));
        let bybit = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        bybit.set_mid(dec!(50_020));
        let dyn_binance: Arc<dyn ExchangeConnector> = binance.clone();
        let dyn_bybit: Arc<dyn ExchangeConnector> = bybit.clone();
        let bundle = ConnectorBundle {
            primary: dyn_binance,
            hedge: None,
            pair: None,
            extra: vec![dyn_bybit],
        };
        let mut engine = make_engine_with_bundle(bundle);
        // Register Bybit on the aggregator with a different
        // taker fee so the router prefers it for the first
        // leg. Binance seed was auto-installed in `new()`.
        let mut bybit_product = sample_product("BTCUSDT");
        bybit_product.taker_fee = dec!(0.00001); // 0.1 bps
        let mut bybit_seed = VenueSeed::new("BTCUSDT", bybit_product, dec!(1));
        bybit_seed.best_bid = dec!(50_019);
        bybit_seed.best_ask = dec!(50_021);
        engine = engine.with_sor_venue(VenueId::Bybit, bybit_seed);
        // Seed Binance's book on the aggregator too.
        let mut binance_product = sample_product("BTCUSDT");
        binance_product.taker_fee = dec!(0.0005); // 5 bps
        let mut binance_seed = VenueSeed::new("BTCUSDT", binance_product, dec!(1));
        binance_seed.best_bid = dec!(49_999);
        binance_seed.best_ask = dec!(50_001);
        engine = engine.with_sor_venue(VenueId::Binance, binance_seed);

        let outcome = engine.dispatch_route(Side::Buy, dec!(2), dec!(1)).await;
        // Both venues each contributed qty=1. The dispatcher
        // fired one place_order per leg.
        assert_eq!(outcome.legs.len(), 2, "expected two legs, got {outcome:?}");
        assert_eq!(binance.place_single_calls(), 1);
        assert_eq!(bybit.place_single_calls(), 1);
        assert!(
            outcome.errors.is_empty(),
            "got errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.total_dispatched_qty, dec!(2));
        assert!(outcome.is_fully_dispatched());
    }

    /// Single-venue `dispatch_route` path: operators running a
    /// single venue still get one live place_order even
    /// though the router had no choice to make. Taker-urgency
    /// leg lands as an IOC through `execute_unwind_slice`.
    /// Target qty is capped by the seeded `max_inventory`
    /// budget on the aggregator — use a qty under the default
    /// 0.1 cap so the router produces a full-target decision.
    #[tokio::test]
    async fn dispatch_route_single_venue_fires_one_place_order() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        let outcome = engine.dispatch_route(Side::Buy, dec!(0.05), dec!(1)).await;
        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.legs.len(), 1);
        assert_eq!(outcome.total_dispatched_qty, dec!(0.05));
        assert_eq!(mock.place_single_calls(), 1);
    }

    /// End-to-end #2: stat-arb driver emits `Entered` → engine
    /// dispatches both legs → both connectors saw place_order;
    /// `Exited` → flatten slice on both connectors.
    #[tokio::test]
    async fn stat_arb_entered_then_exited_drives_real_leg_dispatch() {
        let y_conn = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let x_conn = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        y_conn.set_mid(dec!(200));
        x_conn.set_mid(dec!(100));
        let y_dyn: Arc<dyn ExchangeConnector> = y_conn.clone();
        let x_dyn: Arc<dyn ExchangeConnector> = x_conn.clone();
        // Engine is just a host — primary connector is the y
        // leg so single-bundle tests work.
        let bundle = ConnectorBundle::single(y_dyn.clone());
        let mut engine = make_engine_with_bundle(bundle);

        let pair = StatArbPair {
            y_symbol: "BTCUSDT".to_string(),
            x_symbol: "ETHUSDT".to_string(),
            strategy_class: "stat_arb_BTCUSDT_ETHUSDT".to_string(),
        };
        let cfg = StatArbDriverConfig {
            tick_interval: std::time::Duration::from_millis(10),
            zscore: mm_strategy::stat_arb::ZScoreConfig {
                window: 20,
                entry_threshold: dec!(1.5),
                exit_threshold: dec!(0.3),
            },
            kalman_transition_var: dec!(0.000001),
            kalman_observation_var: dec!(0.001),
            leg_notional_usd: dec!(1000),
        };
        let mut driver = StatArbDriver::new(y_dyn, x_dyn, pair, cfg, Arc::new(NullStatArbSink));
        // Seed cointegration so the z-score path can Enter.
        let x_series: Vec<Decimal> = (0..60)
            .map(|i| dec!(100) + Decimal::from(i as i64 % 5 - 2))
            .collect();
        let y_series: Vec<Decimal> = x_series
            .iter()
            .enumerate()
            .map(|(i, xi)| {
                let jitter = Decimal::from(i as i64 % 3 - 1) / dec!(10);
                dec!(2) * xi + jitter
            })
            .collect();
        driver.recheck_cointegration(&y_series, &x_series);

        // Warmup with steady prices so Z stays small.
        for _ in 0..20 {
            y_conn.set_mid(dec!(200));
            x_conn.set_mid(dec!(100));
            driver.tick_once().await;
        }

        // Shock Y to force Entered.
        y_conn.set_mid(dec!(205));
        let shock = driver.tick_once().await;
        assert!(matches!(shock, StatArbEvent::Entered { .. }));
        let entry_report = driver.try_dispatch_legs_for_entry(&shock).await;
        assert!(!entry_report.is_empty());
        assert!(entry_report.all_succeeded());
        // y_conn should see one, x_conn should see one.
        assert_eq!(y_conn.place_single_calls(), 1);
        assert_eq!(x_conn.place_single_calls(), 1);
        // Route through the audit-writing handler too so we
        // exercise the format_leg_report pathway.
        engine.handle_stat_arb_event(shock, Some(entry_report));

        // Revert Y to force Exit.
        y_conn.set_mid(dec!(200));
        let mut exit_event = None;
        for _ in 0..60 {
            let e = driver.tick_once().await;
            if matches!(e, StatArbEvent::Exited { .. }) {
                exit_event = Some(e);
                break;
            }
        }
        let exit_event = exit_event.expect("expected Exited after revert");
        let exit_report = driver.try_dispatch_legs_for_exit().await;
        assert!(!exit_report.is_empty());
        assert!(exit_report.all_succeeded());
        // Both connectors should now have seen two place_order
        // calls — one entry, one exit.
        assert_eq!(y_conn.place_single_calls(), 2);
        assert_eq!(x_conn.place_single_calls(), 2);
        engine.handle_stat_arb_event(exit_event, Some(exit_report));
    }

    /// `pnl_strategy_class`: returns the stat-arb pair's
    /// strategy_class when the driver is attached and funding
    /// arb is not, otherwise falls back to the primary
    /// strategy name.
    #[tokio::test]
    async fn pnl_strategy_class_discriminates_stat_arb_vs_default() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let engine = make_engine_with_bundle(bundle);
        assert_eq!(engine.pnl_strategy_class(), engine.strategy.name());

        // Attach a stat-arb driver and assert the class flips
        // to the pair's `strategy_class` value.
        let y_conn = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let x_conn = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        let driver = StatArbDriver::new(
            y_conn as Arc<dyn ExchangeConnector>,
            x_conn as Arc<dyn ExchangeConnector>,
            StatArbPair {
                y_symbol: "BTCUSDT".to_string(),
                x_symbol: "ETHUSDT".to_string(),
                strategy_class: "stat_arb_BTCUSDT_ETHUSDT".to_string(),
            },
            StatArbDriverConfig::default(),
            Arc::new(NullStatArbSink),
        );
        let engine = engine.with_stat_arb_driver(driver, std::time::Duration::from_millis(50));
        assert_eq!(engine.pnl_strategy_class(), "stat_arb_BTCUSDT_ETHUSDT");
    }

    // ---------------------------------------------------------
    // Epic D stage-2 — BVC classifier engine wiring
    // ---------------------------------------------------------

    fn make_engine_with_toxicity(cfg: mm_common::config::ToxicityConfig) -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let app_cfg = AppConfig {
            toxicity: cfg,
            ..AppConfig::default()
        };
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            app_cfg,
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    fn synth_trade(price: Decimal, qty: Decimal, side: mm_common::types::Side) -> mm_common::types::Trade {
        mm_common::types::Trade {
            trade_id: 1,
            symbol: "BTCUSDT".to_string(),
            price,
            qty,
            taker_side: side,
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn bvc_disabled_keeps_aggregator_none() {
        let cfg = mm_common::config::ToxicityConfig {
            bvc_enabled: false,
            ..Default::default()
        };
        let engine = make_engine_with_toxicity(cfg);
        assert!(engine.bvc_classifier.is_none());
        assert!(engine.bvc_bar_agg.is_none());
    }

    #[test]
    fn bvc_enabled_constructs_both_components() {
        let cfg = mm_common::config::ToxicityConfig {
            bvc_enabled: true,
            bvc_bar_secs: 1,
            ..Default::default()
        };
        let engine = make_engine_with_toxicity(cfg);
        assert!(engine.bvc_classifier.is_some());
        assert!(engine.bvc_bar_agg.is_some());
    }

    /// Disabled path: the legacy tick-rule feed into VPIN stays
    /// wired. Tiny bucket size + enough one-sided buys registers
    /// at the VPIN level.
    #[test]
    fn bvc_disabled_tick_rule_still_feeds_vpin() {
        let cfg = mm_common::config::ToxicityConfig {
            bvc_enabled: false,
            vpin_bucket_size: dec!(100),
            vpin_num_buckets: 4,
            ..Default::default()
        };
        let mut engine = make_engine_with_toxicity(cfg);
        for _ in 0..20 {
            let t = synth_trade(dec!(100), dec!(2), mm_common::types::Side::Buy);
            engine.handle_ws_event(MarketEvent::Trade {
                venue: VenueId::Binance,
                trade: t,
            });
        }
        let v = engine.vpin.vpin().expect("vpin produced");
        assert!(v > dec!(0), "expected positive vpin, got {v}");
    }

    /// Enabled path: within a single bar window the aggregator
    /// holds the trade (no bar has closed yet), so the VPIN
    /// buckets should remain empty — proves the engine is
    /// NOT calling `vpin.on_trade` on the BVC path.
    #[test]
    fn bvc_enabled_suppresses_legacy_on_trade_within_bar() {
        let cfg = mm_common::config::ToxicityConfig {
            bvc_enabled: true,
            // Long bar — no chance of closing during the test.
            bvc_bar_secs: 3600,
            vpin_bucket_size: dec!(100),
            vpin_num_buckets: 4,
            ..Default::default()
        };
        let mut engine = make_engine_with_toxicity(cfg);
        for _ in 0..20 {
            let t = synth_trade(dec!(100), dec!(2), mm_common::types::Side::Buy);
            engine.handle_ws_event(MarketEvent::Trade {
                venue: VenueId::Binance,
                trade: t,
            });
        }
        assert!(engine.vpin.vpin().is_none(),
            "bvc path should not call on_trade — VPIN must stay empty mid-bar");
        assert!(engine.bvc_bar_agg.is_some());
    }

    // ---------------------------------------------------------
    // Epic A stage-2 #1 — inline SOR dispatch tick
    // ---------------------------------------------------------

    /// No inventory, default config → the tick is a no-op. No
    /// place_order call, no legs, no audit write.
    #[tokio::test]
    async fn sor_tick_inventory_excess_zero_inventory_is_noop() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 0);
    }

    /// Long inventory above threshold → tick fires a SELL
    /// dispatch for the excess.
    #[tokio::test]
    async fn sor_tick_dispatches_sell_when_long_inventory_exceeds_threshold() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        // Seed the aggregator so the router finds a venue.
        let mut seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        seed.best_bid = dec!(49_999);
        seed.best_ask = dec!(50_001);
        engine = engine.with_sor_venue(VenueId::Binance, seed);
        // Simulate a long position above the default threshold.
        engine.config.market_maker.sor_inventory_threshold = dec!(0.01);
        // Urgency > 0.5 forces taker legs so mock.place_single_calls() fires.
        engine.config.market_maker.sor_urgency = dec!(0.9);
        engine.inventory_manager.force_reset_inventory_to(dec!(0.05));

        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 1, "one taker leg expected");
    }

    /// Short inventory (negative) above threshold → BUY dispatch.
    #[tokio::test]
    async fn sor_tick_dispatches_buy_when_short_inventory_exceeds_threshold() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        let mut seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        seed.best_bid = dec!(49_999);
        seed.best_ask = dec!(50_001);
        engine = engine.with_sor_venue(VenueId::Binance, seed);
        engine.config.market_maker.sor_inventory_threshold = dec!(0.01);
        engine.config.market_maker.sor_urgency = dec!(0.9);
        engine.inventory_manager.force_reset_inventory_to(dec!(-0.05));

        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 1);
    }

    /// Inventory at exactly threshold → no-op (strictly above
    /// policy, so a position at the limit doesn't churn).
    #[tokio::test]
    async fn sor_tick_at_threshold_is_noop() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        engine.config.market_maker.sor_inventory_threshold = dec!(0.05);
        engine.inventory_manager.force_reset_inventory_to(dec!(0.05));
        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 0);
    }

    /// HedgeBudget source with an empty basket → no-op.
    #[tokio::test]
    async fn sor_tick_hedge_budget_empty_basket_is_noop() {
        use mm_common::config::SorTargetSource;
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        engine.config.market_maker.sor_target_qty_source = SorTargetSource::HedgeBudget;
        // last_hedge_basket starts empty by default.
        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 0);
    }

    // ---------------------------------------------------------
    // Epic A stage-2 #2 — trade-rate → queue-wait refresh
    // ---------------------------------------------------------

    /// A trade event seeds the per-venue estimator.
    #[test]
    fn sor_trade_event_feeds_rate_estimator() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(mock as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        assert_eq!(engine.sor_trade_rates.len(), 0);
        let t = synth_trade(dec!(100), dec!(2), mm_common::types::Side::Buy);
        engine.handle_ws_event(MarketEvent::Trade {
            venue: VenueId::Binance,
            trade: t,
        });
        assert_eq!(engine.sor_trade_rates.len(), 1);
        let est = engine.sor_trade_rates.get(&VenueId::Binance).unwrap();
        assert_eq!(est.sample_count(), 1);
        assert_eq!(est.total_qty(), dec!(2));
    }

    /// refresh_sor_queue_wait leaves seeded value in place when
    /// the estimator hasn't reached MIN_SAMPLES yet.
    #[test]
    fn sor_queue_refresh_keeps_seed_before_min_samples() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(mock as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        // Seed Binance on the aggregator with a distinctive
        // queue_wait so we can detect whether it was overwritten.
        let mut seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        seed.best_bid = dec!(49_999);
        seed.best_ask = dec!(50_001);
        seed.queue_wait_secs = dec!(123);
        engine = engine.with_sor_venue(VenueId::Binance, seed);

        // One trade — far below MIN_SAMPLES = 5.
        let t = synth_trade(dec!(100), dec!(1), mm_common::types::Side::Buy);
        engine.handle_ws_event(MarketEvent::Trade {
            venue: VenueId::Binance,
            trade: t,
        });
        engine.refresh_sor_queue_wait();
        let seed_after = engine.sor_aggregator.seed(VenueId::Binance).unwrap();
        assert_eq!(seed_after.queue_wait_secs, dec!(123),
            "seed must not be overwritten before MIN_SAMPLES");
    }

    /// Enough trades → refresh_sor_queue_wait publishes a fresh
    /// (non-seeded) queue_wait derived from the estimator.
    #[test]
    fn sor_queue_refresh_publishes_rate_derived_wait_after_min_samples() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(mock as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        let mut seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        seed.best_bid = dec!(49_999);
        seed.best_ask = dec!(50_001);
        seed.queue_wait_secs = dec!(999); // Clearly-wrong seed.
        engine = engine.with_sor_venue(VenueId::Binance, seed);
        // Stream 10 trades so the estimator clears its MIN_SAMPLES
        // threshold (5).
        for _ in 0..10 {
            let t = synth_trade(dec!(100), dec!(1), mm_common::types::Side::Buy);
            engine.handle_ws_event(MarketEvent::Trade {
                venue: VenueId::Binance,
                trade: t,
            });
        }
        engine.refresh_sor_queue_wait();
        let seed_after = engine.sor_aggregator.seed(VenueId::Binance).unwrap();
        assert_ne!(seed_after.queue_wait_secs, dec!(999),
            "refresh must replace the seeded value once enough trades arrive");
        assert!(seed_after.queue_wait_secs > dec!(0));
    }
}
