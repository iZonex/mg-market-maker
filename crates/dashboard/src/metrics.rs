use once_cell::sync::Lazy;
use prometheus::{register_gauge_vec, register_int_counter_vec, GaugeVec, IntCounterVec};

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

// Regime
pub static REGIME: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "mm_regime",
        "Market regime (0=quiet, 1=trending, 2=volatile, 3=mean_reverting)",
        &["symbol"]
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
    let _ = &*KILL_SWITCH_LEVEL;
    let _ = &*SLA_UPTIME;
    let _ = &*REGIME;
}
