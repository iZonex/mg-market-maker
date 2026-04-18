use once_cell::sync::Lazy;
use prometheus::{register_gauge_vec, register_int_counter_vec, GaugeVec, IntCounterVec};

// Connector-level metrics live in `mm-exchange-core::metrics` so venue
// adapters can observe them without pulling in the dashboard crate.
// The `/metrics` endpoint scrapes the process-global registry so the
// timeseries still shows up here even though the Lazy lives in a
// different crate.
pub use mm_exchange_core::metrics::ORDER_ENTRY_LATENCY;

// Prometheus metrics for the market maker.

// PnL
pub static PNL_TOTAL: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!("mm_pnl_total", "Total PnL in quote asset", &["symbol"]).unwrap()
});
pub static PNL_SPREAD: Lazy<GaugeVec> =
    Lazy::new(|| register_gauge_vec!("mm_pnl_spread", "Spread capture PnL", &["symbol"]).unwrap());
pub static PNL_INVENTORY: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_pnl_inventory",
        "Inventory mark-to-market PnL",
        &["symbol"]
    )
    .unwrap()
});
pub static PNL_REBATES: Lazy<GaugeVec> =
    Lazy::new(|| register_gauge_vec!("mm_pnl_rebates", "Fee rebate income", &["symbol"]).unwrap());
/// Epic 40.3 — realised funding PnL per symbol, booked at
/// each venue funding-settlement instant. Included in
/// `mm_pnl_total` so ops dashboards do not double-count.
pub static PNL_FUNDING_REALISED: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_pnl_funding_realised",
        "Realised funding PnL (Epic 40.3) — perp-only",
        &["symbol"]
    )
    .unwrap()
});
/// Epic 40.3 — continuous MTM estimate of the funding PnL
/// accruing during the current period. Display-only;
/// excluded from `mm_pnl_total` because it flips into
/// `mm_pnl_funding_realised` at the next settle.
pub static PNL_FUNDING_MTM: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_pnl_funding_mtm",
        "Mark-to-market funding PnL inside current period (Epic 40.3)",
        &["symbol"]
    )
    .unwrap()
});

// Inventory
pub static INVENTORY: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_inventory",
        "Current inventory in base asset",
        &["symbol"]
    )
    .unwrap()
});
pub static INVENTORY_VALUE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_inventory_value",
        "Inventory value in quote asset",
        &["symbol"]
    )
    .unwrap()
});

// Orders
pub static LIVE_ORDERS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!("mm_live_orders", "Number of live orders", &["symbol"]).unwrap()
});
pub static FILLS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!("mm_fills_total", "Total fills", &["symbol", "side"]).unwrap()
});

// Epic A stage-2 #1 — SOR inline dispatch observability.
pub static SOR_DISPATCH_SUCCESS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_sor_dispatch_success_total",
        "SOR inline-dispatch tick outcomes where every leg succeeded",
        &["symbol"]
    )
    .unwrap()
});
pub static SOR_DISPATCH_ERRORS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_sor_dispatch_errors_total",
        "SOR inline-dispatch leg-level errors (one increment per failed leg)",
        &["symbol", "venue"]
    )
    .unwrap()
});
pub static SOR_DISPATCH_FILLED_QTY: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_sor_dispatch_filled_qty",
        "Base-asset qty dispatched through SOR on the last tick",
        &["symbol"]
    )
    .unwrap()
});

// Market Data
pub static MID_PRICE: Lazy<GaugeVec> =
    Lazy::new(|| register_gauge_vec!("mm_mid_price", "Current mid price", &["symbol"]).unwrap());
pub static SPREAD_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!("mm_spread_bps", "Current spread in bps", &["symbol"]).unwrap()
});
pub static VOLATILITY: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_volatility",
        "Estimated annualized volatility",
        &["symbol"]
    )
    .unwrap()
});

// Toxicity
pub static VPIN: Lazy<GaugeVec> =
    Lazy::new(|| register_gauge_vec!("mm_vpin", "VPIN toxicity indicator", &["symbol"]).unwrap());
pub static KYLE_LAMBDA: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!("mm_kyle_lambda", "Kyle's Lambda price impact", &["symbol"]).unwrap()
});
pub static ADVERSE_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_adverse_selection_bps",
        "Adverse selection cost in bps",
        &["symbol"]
    )
    .unwrap()
});
pub static MARKET_RESILIENCE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_market_resilience",
        "Market Resilience score (1.0 = fully recovered, 0.0 = fragile)",
        &["symbol"]
    )
    .unwrap()
});
pub static ORDER_TO_TRADE_RATIO: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_order_to_trade_ratio",
        "Order-to-Trade Ratio (regulatory surveillance metric)",
        &["symbol"]
    )
    .unwrap()
});
pub static HMA_VALUE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_hma_value",
        "Hull Moving Average value on mid-price (None before warmup)",
        &["symbol"]
    )
    .unwrap()
});

// Epic D wave-2 + stage-3 — wave-2 signal observability
//
// `mm_momentum_ofi_ewma` and `mm_momentum_learned_mp_drift`
// expose the Cont-Kukanov-Stoikov order flow imbalance EWMA
// and the Stoikov 2018 learned-microprice drift inside
// `MomentumSignals`. Operators see these on the dashboard /
// Grafana to A/B compare wave-1 vs wave-2 alpha contribution.
// Default zero before the corresponding optional builder is
// attached at engine construction time
// (`momentum_ofi_enabled` / `momentum_learned_microprice_path`).
pub static MOMENTUM_OFI_EWMA: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_momentum_ofi_ewma",
        "Cont-Kukanov-Stoikov L1 order flow imbalance EWMA from MomentumSignals (Epic D wave-2)",
        &["symbol"]
    )
    .unwrap()
});

pub static MOMENTUM_LEARNED_MP_DRIFT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_momentum_learned_mp_drift",
        "Stoikov 2018 learned micro-price drift from current mid (fraction; multiply by mid for absolute)",
        &["symbol"]
    )
    .unwrap()
});

// Epic D stage-3 — per-side adverse-selection probabilities.
// `mm_as_prob_bid` / `mm_as_prob_ask` expose the per-side ρ
// values the engine derives from
// `AdverseSelectionTracker::adverse_selection_bps_{bid,ask}`
// via `cartea_spread::as_prob_from_bps`. Both sit at 0.5
// (neutral) until the per-side tracker has ≥5 completed
// fills on that side, at which point they diverge from the
// symmetric `mm_adverse_selection_bps`.
pub static AS_PROB_BID: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_as_prob_bid",
        "Per-side adverse-selection probability ρ_bid (0.5 = neutral, >0.5 informed buys)",
        &["symbol"]
    )
    .unwrap()
});

pub static AS_PROB_ASK: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_as_prob_ask",
        "Per-side adverse-selection probability ρ_ask (0.5 = neutral, >0.5 informed sells)",
        &["symbol"]
    )
    .unwrap()
});

// Market impact (production service reporting)
pub static MARKET_IMPACT_MEAN_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_market_impact_mean_bps",
        "Mean market impact in bps (positive = adverse, fills move the market)",
        &["symbol"]
    )
    .unwrap()
});
pub static MARKET_IMPACT_ADVERSE_PCT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_market_impact_adverse_pct",
        "Percentage of fills with adverse market impact (> 0 bps)",
        &["symbol"]
    )
    .unwrap()
});

// Fill quality (slippage tracking)
pub static FILL_SLIPPAGE_AVG_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_fill_slippage_avg_bps",
        "Average fill slippage vs mid in bps (positive = worse than mid)",
        &["symbol"]
    )
    .unwrap()
});

// Fee schedule (refreshed by the periodic fee-tier task — P1.2)
pub static MAKER_FEE_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_maker_fee_bps",
        "Effective maker fee in bps as the venue reports it for this account (negative = rebate)",
        &["symbol"]
    )
    .unwrap()
});
pub static TAKER_FEE_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_taker_fee_bps",
        "Effective taker fee in bps as the venue reports it for this account",
        &["symbol"]
    )
    .unwrap()
});

// Smart Order Router (Epic A)

/// Latest per-venue effective cost in basis points from the
/// SOR. Pushed by the engine on every `recommend_route` call
/// that produces a non-empty decision. Non-breaking — a
/// quiet SOR leaves the gauge at its previous value.
pub static SOR_ROUTE_COST_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_sor_route_cost_bps",
        "Smart Order Router per-venue effective cost in basis points",
        &["venue"]
    )
    .unwrap()
});

/// Last-recommended per-venue fill attribution qty. The
/// operator dashboard renders this alongside the
/// hedge-basket recommendation so "we should route via
/// Binance for 2 units and Bybit for 0.5 units" is visible
/// at a glance.
pub static SOR_FILL_ATTRIBUTION: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_sor_fill_attribution",
        "Smart Order Router per-venue recommended fill quantity",
        &["venue"]
    )
    .unwrap()
});

// Cross-venue basis (P1.4 stage-1)
pub static CROSS_VENUE_BASIS_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_cross_venue_basis_bps",
        "Signed cross-venue basis (perp_mid - spot_mid) in bps of spot mid",
        &["symbol"]
    )
    .unwrap()
});

// Borrow rate (refreshed by the periodic borrow-rate task — P1.3 stage-1)
pub static BORROW_RATE_BPS_HOURLY: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_borrow_rate_bps_hourly",
        "Venue-reported borrow rate for the base asset in bps/hour",
        &["asset"]
    )
    .unwrap()
});
pub static BORROW_CARRY_BPS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_borrow_carry_bps",
        "Expected-carry surcharge in bps the strategy bakes into the ask reservation",
        &["asset"]
    )
    .unwrap()
});

// Risk
pub static KILL_SWITCH_LEVEL: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_kill_switch_level",
        "Kill switch level (0=normal, 5=disconnect)",
        &["symbol"]
    )
    .unwrap()
});

// SLA
pub static SLA_UPTIME: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!("mm_sla_uptime_pct", "SLA uptime percentage", &["symbol"]).unwrap()
});
/// P2.2 — per-pair daily presence rolled up from the 1440
/// per-minute buckets. Distinguishable from `mm_sla_uptime_pct`
/// because the latter is the lifetime average and this gauge
/// resets at UTC midnight.
pub static SLA_PRESENCE_PCT_24H: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_sla_presence_pct_24h",
        "Per-pair daily SLA presence percentage from per-minute buckets (P2.2)",
        &["symbol"]
    )
    .unwrap()
});

// Regime
pub static REGIME: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_regime",
        "Market regime (0=quiet, 1=trending, 2=volatile, 3=mean_reverting)",
        &["symbol"]
    )
    .unwrap()
});

// Portfolio — unified view across all symbols in the reporting currency.
pub static PORTFOLIO_TOTAL_EQUITY: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_portfolio_total_equity",
        "Portfolio total equity (realised + unrealised) in reporting currency",
        &["currency"]
    )
    .unwrap()
});
pub static PORTFOLIO_REALISED_PNL: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_portfolio_realised_pnl",
        "Portfolio realised PnL in reporting currency",
        &["currency"]
    )
    .unwrap()
});
pub static PORTFOLIO_UNREALISED_PNL: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_portfolio_unrealised_pnl",
        "Portfolio unrealised (mark-to-market) PnL in reporting currency",
        &["currency"]
    )
    .unwrap()
});
pub static PORTFOLIO_ASSET_QTY: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_portfolio_asset_qty",
        "Portfolio per-asset position quantity (signed)",
        &["symbol"]
    )
    .unwrap()
});
pub static PORTFOLIO_ASSET_UNREALISED: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_portfolio_asset_unrealised_reporting",
        "Portfolio per-asset unrealised PnL in reporting currency",
        &["symbol"]
    )
    .unwrap()
});
/// P2.3 Epic C sub-component #1: per-factor (base / quote)
/// delta aggregation across every registered symbol. Cross-quote
/// pairs contribute to BOTH their base factor (+qty) and their
/// quote factor (-qty·mark).
pub static PORTFOLIO_FACTOR_DELTA: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_portfolio_factor_delta",
        "Portfolio per-factor delta — signed exposure in the factor's native asset",
        &["asset"]
    )
    .unwrap()
});
/// P2.3 Epic C sub-component #2: per-strategy-class realised
/// PnL in the reporting currency, keyed by `Strategy::name()`.
/// Funding-arb, basis, avellaneda-stoikov, etc. each get their
/// own bucket so operators can distinguish which strategy is
/// carrying the book.
pub static PORTFOLIO_STRATEGY_PNL: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_portfolio_strategy_pnl",
        "Portfolio per-strategy realised PnL in reporting currency",
        &["strategy"]
    )
    .unwrap()
});

// ── Epic R Sprint 4 — surveillance observability ───────────

/// Last-computed surveillance detector score (0.0 … 1.0) per
/// (pattern, symbol). Written every engine tick by the per-node
/// sweep in `tick_strategy_graph`, so the timeseries reflects
/// the real-time signal even when the score hasn't crossed the
/// alert threshold. Alert-grade scores (`>= 0.8`) additionally
/// increment `mm_surveillance_alerts_total`.
pub static SURVEILLANCE_SCORE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_surveillance_score",
        "Last-computed surveillance detector score (0–1, ≥0.8 trips an alert)",
        &["pattern", "symbol"]
    )
    .unwrap()
});

/// Counter of post-dedupe surveillance alerts, labelled by
/// (pattern, symbol). One increment per audit row — the 60s
/// per-(pattern, node_id) dedupe is honoured so bursty scores
/// don't inflate the counter.
pub static SURVEILLANCE_ALERTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_surveillance_alerts_total",
        "Post-dedupe surveillance alerts (one increment per audit row)",
        &["pattern", "symbol"]
    )
    .unwrap()
});

// ── Multi-Venue 3.E — atomic-bundle observability ──────────

/// Number of atomic-bundle dispatches currently awaiting
/// both-leg ack. Set every time the inflight map mutates
/// (dispatch, ack sweep, watchdog rollback). Persistent
/// non-zero values mean legs are ack'ing slowly — check venue
/// ack latency.
pub static ATOMIC_BUNDLES_INFLIGHT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_atomic_bundles_inflight",
        "Atomic-bundles awaiting both-leg ack (Multi-Venue 3.E.2 watchdog input)",
        &["symbol"]
    )
    .unwrap()
});

/// Counter of atomic-bundles that hit the watchdog timeout
/// without both legs acked. Non-zero = the cross-venue hedge
/// leg is late; pair with `mm_atomic_bundles_completed_total`
/// to derive a success ratio.
pub static ATOMIC_BUNDLES_ROLLED_BACK_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_atomic_bundles_rolled_back_total",
        "Atomic-bundles rolled back by the 3.E.2 watchdog (timeout without both acks)",
        &["symbol"]
    )
    .unwrap()
});

/// Counter of atomic-bundles that graduated out of the
/// inflight table with both legs acked (Multi-Venue 3.E.3 ack
/// sweep). Pair with the rollback counter for success ratio.
pub static ATOMIC_BUNDLES_COMPLETED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_atomic_bundles_completed_total",
        "Atomic-bundles that graduated out of inflight with both legs acked",
        &["symbol"]
    )
    .unwrap()
});

// ── Epic H — strategy-graph deploy observability ───────────

/// Counter of strategy-graph deploy attempts, labelled by
/// outcome (`accepted`, `rejected`). Rejections pair with the
/// `StrategyGraphDeployRejected` audit row carrying the
/// validation error; this counter lets Prometheus key an alert
/// off a rejection burst without reading the audit log.
pub static STRATEGY_GRAPH_DEPLOYS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_strategy_graph_deploys_total",
        "Strategy-graph deploy attempts, labelled by outcome",
        &["outcome"]
    )
    .unwrap()
});

/// Current node count of the deployed strategy graph,
/// labelled by graph name. A sharp drop mid-session likely
/// means a rollback landed — corroborate with the
/// `mm_strategy_graph_deploys_total{outcome="accepted"}`
/// counter.
pub static STRATEGY_GRAPH_NODES: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_strategy_graph_nodes",
        "Node count of the currently-deployed strategy graph",
        &["graph"]
    )
    .unwrap()
});

// ── INT-1 — decision cost ledger histogram ────────────────

/// Histogram of realized cost bps per resolved decision,
/// labelled by symbol + side. Bucket boundaries cover both
/// passive MM outcomes (bps-scale, typically ±5) and
/// take-driven outcomes (10s of bps). Used to spot regime
/// changes in fill quality without reading the audit log.
pub static DECISION_REALIZED_COST_BPS: Lazy<prometheus::HistogramVec> = Lazy::new(|| {
    prometheus::register_histogram_vec!(
        "mm_decision_realized_cost_bps",
        "Realized cost of a resolved decision (bps of decision-time mid, adverse = positive)",
        &["symbol", "side"],
        vec![-50.0, -20.0, -10.0, -5.0, -2.0, -1.0, 0.0, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0]
    )
    .unwrap()
});

/// Histogram of `realized - expected` per resolved decision.
/// Centred on zero for a well-calibrated expected-cost estimator;
/// a persistent drift points at a stale fee table, a broken
/// impact_bps, or a model-vs-reality gap.
pub static DECISION_VS_EXPECTED_BPS: Lazy<prometheus::HistogramVec> = Lazy::new(|| {
    prometheus::register_histogram_vec!(
        "mm_decision_vs_expected_bps",
        "Delta between realized and expected cost bps per decision (positive = worse than expected)",
        &["symbol", "side"],
        vec![-50.0, -20.0, -10.0, -5.0, -2.0, -1.0, 0.0, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0]
    )
    .unwrap()
});

// ── INT-2 — tiered OTR gauges ───────────────────────────────

/// 4-way OTR per symbol: `{tier, window}` label pair picks
/// one of TOB×{cumulative, 5min} + Top20×{cumulative, 5min}.
/// Engine pushes on every SLA tick; Grafana alerts key off the
/// Rolling5Min series because cumulative smooths over
/// intra-session regime changes.
pub static ORDER_TO_TRADE_TIERED: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_otr_tiered",
        "Tiered + dual-timeline OTR (venue-surveillance shape)",
        &["symbol", "tier", "window"]
    )
    .unwrap()
});

/// Initialize all metrics (call once at startup).
pub fn init() {
    // Force lazy initialization.
    let _ = &*PNL_TOTAL;
    let _ = &*PNL_SPREAD;
    let _ = &*PNL_INVENTORY;
    let _ = &*PNL_REBATES;
    let _ = &*INVENTORY;
    let _ = &*INVENTORY_VALUE;
    let _ = &*LIVE_ORDERS;
    let _ = &*FILLS_TOTAL;
    let _ = &*MID_PRICE;
    let _ = &*SPREAD_BPS;
    let _ = &*VOLATILITY;
    let _ = &*VPIN;
    let _ = &*KYLE_LAMBDA;
    let _ = &*ADVERSE_BPS;
    let _ = &*MAKER_FEE_BPS;
    let _ = &*TAKER_FEE_BPS;
    let _ = &*BORROW_RATE_BPS_HOURLY;
    let _ = &*BORROW_CARRY_BPS;
    let _ = &*CROSS_VENUE_BASIS_BPS;
    let _ = &*SOR_ROUTE_COST_BPS;
    let _ = &*SOR_FILL_ATTRIBUTION;
    let _ = &*SLA_PRESENCE_PCT_24H;
    let _ = &*KILL_SWITCH_LEVEL;
    let _ = &*SLA_UPTIME;
    let _ = &*REGIME;
    let _ = &*PORTFOLIO_TOTAL_EQUITY;
    let _ = &*PORTFOLIO_REALISED_PNL;
    let _ = &*PORTFOLIO_UNREALISED_PNL;
    let _ = &*PORTFOLIO_ASSET_QTY;
    let _ = &*PORTFOLIO_FACTOR_DELTA;
    let _ = &*PORTFOLIO_STRATEGY_PNL;
    let _ = &*PORTFOLIO_ASSET_UNREALISED;
    let _ = &*ORDER_ENTRY_LATENCY;
    let _ = &*MOMENTUM_OFI_EWMA;
    let _ = &*MOMENTUM_LEARNED_MP_DRIFT;
    let _ = &*AS_PROB_BID;
    let _ = &*AS_PROB_ASK;
    let _ = &*MARKET_RESILIENCE;
    let _ = &*ORDER_TO_TRADE_RATIO;
    let _ = &*HMA_VALUE;
    let _ = &*MARKET_IMPACT_MEAN_BPS;
    let _ = &*MARKET_IMPACT_ADVERSE_PCT;
    let _ = &*FILL_SLIPPAGE_AVG_BPS;
    let _ = &*ARCHIVE_UPLOADS_TOTAL;
    let _ = &*ARCHIVE_UPLOAD_BYTES_TOTAL;
    let _ = &*ARCHIVE_UPLOAD_ERRORS_TOTAL;
    let _ = &*ARCHIVE_LAST_SUCCESS_TS;
    let _ = &*SCHEDULER_RUNS_TOTAL;
    let _ = &*SCHEDULER_FAILURES_TOTAL;
    let _ = &*SCHEDULER_LAST_SUCCESS_TS;
    let _ = &*SENTIMENT_ARTICLES_TOTAL;
    let _ = &*SENTIMENT_TICKS_TOTAL;
    let _ = &*SENTIMENT_MENTIONS_RATE;
    let _ = &*SENTIMENT_SCORE_5MIN;
    let _ = &*SOCIAL_KILL_TRIGGERS_TOTAL;
    let _ = &*SOCIAL_SPREAD_MULT;
    let _ = &*SOCIAL_SIZE_MULT;
    let _ = &*SURVEILLANCE_SCORE;
    let _ = &*SURVEILLANCE_ALERTS_TOTAL;
    let _ = &*ATOMIC_BUNDLES_INFLIGHT;
    let _ = &*ATOMIC_BUNDLES_ROLLED_BACK_TOTAL;
    let _ = &*ATOMIC_BUNDLES_COMPLETED_TOTAL;
    let _ = &*STRATEGY_GRAPH_DEPLOYS_TOTAL;
    let _ = &*STRATEGY_GRAPH_NODES;
    let _ = &*DECISION_REALIZED_COST_BPS;
    let _ = &*DECISION_VS_EXPECTED_BPS;
    let _ = &*ORDER_TO_TRADE_TIERED;
}

// ── Block B / C — archive + scheduler observability ────────

/// Counter of successful chunk uploads, labelled by logical
/// stream (`audit`, `fills`, `daily`). Increments once per
/// `put_object` that returned `Ok`. Derived by the shipper
/// loop.
pub static ARCHIVE_UPLOADS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_archive_uploads_total",
        "Successful archive chunk uploads (Block C)",
        &["stream"]
    )
    .unwrap()
});

/// Byte counter of uploaded archive payload. Lets ops size
/// S3 egress + bucket growth without reaching into CloudWatch.
pub static ARCHIVE_UPLOAD_BYTES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_archive_upload_bytes_total",
        "Bytes uploaded to the archive bucket",
        &["stream"]
    )
    .unwrap()
});

/// Error counter — shipper tick that raised, labelled by
/// logical stream. Non-zero paired with a flat
/// `mm_archive_uploads_total` is the "S3 is broken" signal.
pub static ARCHIVE_UPLOAD_ERRORS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_archive_upload_errors_total",
        "Archive shipper errors",
        &["stream"]
    )
    .unwrap()
});

/// Unix timestamp (seconds) of the most recent successful
/// upload for each stream. Alerts key off this — if
/// `now - gauge > shipper_interval_secs * 2` the bucket is
/// drifting.
pub static ARCHIVE_LAST_SUCCESS_TS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_archive_last_success_ts",
        "Unix seconds since epoch of the last successful archive upload",
        &["stream"]
    )
    .unwrap()
});

/// Counter of scheduled-report runs, labelled by cadence
/// (`daily`, `weekly`, `monthly`). Increments once per fire,
/// regardless of success.
pub static SCHEDULER_RUNS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_scheduler_runs_total",
        "Scheduled compliance-report fires (Block B)",
        &["cadence"]
    )
    .unwrap()
});

/// Counter of scheduler runs that raised an error. Paired
/// with `mm_scheduler_runs_total` so ops can derive a
/// success rate.
pub static SCHEDULER_FAILURES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_scheduler_failures_total",
        "Scheduled compliance-report failures",
        &["cadence"]
    )
    .unwrap()
});

/// Unix timestamp of the most recent successful scheduler
/// run, labelled by cadence.
pub static SCHEDULER_LAST_SUCCESS_TS: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_scheduler_last_success_ts",
        "Unix seconds since epoch of the last successful scheduled report",
        &["cadence"]
    )
    .unwrap()
});

// ── Epic G — sentiment / social-risk observability ──────────

/// Counter of sentiment articles analysed per cycle,
/// labelled by scorer (`ollama` / `keyword`). Lets operators
/// see when Ollama is down and the keyword fallback is
/// carrying the pipeline.
pub static SENTIMENT_ARTICLES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_sentiment_articles_total",
        "Sentiment articles analysed",
        &["scorer"]
    )
    .unwrap()
});

/// Counter of SentimentTicks emitted to engines, per asset.
pub static SENTIMENT_TICKS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_sentiment_ticks_total",
        "Sentiment ticks broadcast to engines",
        &["asset"]
    )
    .unwrap()
});

/// Most recent `mentions_rate` per asset. 1.0 = flat
/// baseline, 2–5 = chatter, 10+ = spike.
pub static SENTIMENT_MENTIONS_RATE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_sentiment_mentions_rate",
        "Last-observed mentions_rate per asset (5min / hourly-avg)",
        &["asset"]
    )
    .unwrap()
});

/// EWMA sentiment score, per asset. Range `[-1.0, +1.0]`.
pub static SENTIMENT_SCORE_5MIN: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_sentiment_score_5min",
        "5-minute EWMA sentiment score per asset (-1..+1)",
        &["asset"]
    )
    .unwrap()
});

/// Counter of social-risk kill-switch escalations, per
/// symbol. Non-zero = the fused `rate + vol` signal has
/// confirmed at least one crowd spike worth flattening on.
pub static SOCIAL_KILL_TRIGGERS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_social_kill_triggers_total",
        "Kill switch escalations caused by SocialRiskEngine",
        &["symbol"]
    )
    .unwrap()
});

/// Last-applied social-risk spread multiplier per symbol.
/// Always `>= 1.0`.
pub static SOCIAL_SPREAD_MULT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_social_spread_mult",
        "Last social-risk spread multiplier per symbol",
        &["symbol"]
    )
    .unwrap()
});

/// Last-applied social-risk size multiplier per symbol.
/// Range `(0, 1]`.
pub static SOCIAL_SIZE_MULT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_social_size_mult",
        "Last social-risk size multiplier per symbol",
        &["symbol"]
    )
    .unwrap()
});
