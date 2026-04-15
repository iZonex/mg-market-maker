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
}
