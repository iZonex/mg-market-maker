//! Replay a recorded event stream through the `Simulator` with
//! a real-market-calibrated config. Final sanity gate before
//! running `mm-server` in paper mode against live venues.
//!
//! Usage:
//! ```bash
//! cargo run -p mm-backtester --bin mm-backtest-live -- \
//!   --events data/recorded/binance-btcusdt.jsonl \
//!   --gamma 0.1 --kappa 42 --sigma 0.00005 --order-size 0.00025
//! ```

use anyhow::{Context, Result};
use mm_backtester::data::load_events;
use mm_backtester::simulator::{FillModel, Simulator};
use mm_common::config::{MarketMakerConfig, StrategyType};
use mm_common::types::ProductSpec;
use mm_strategy::AvellanedaStoikov;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::path::PathBuf;
use std::str::FromStr;

struct Args {
    events: PathBuf,
    gamma: Decimal,
    kappa: Decimal,
    sigma: Decimal,
    order_size: Decimal,
}

fn parse_args() -> Args {
    let mut events = PathBuf::from("data/recorded/binance-btcusdt.jsonl");
    let mut gamma = dec!(0.1);
    let mut kappa = dec!(1.5);
    let mut sigma = dec!(0.02);
    let mut order_size = dec!(0.001);
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--events" => {
                events = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--gamma" => {
                gamma = Decimal::from_str(&args[i + 1]).unwrap_or(gamma);
                i += 2;
            }
            "--kappa" => {
                kappa = Decimal::from_str(&args[i + 1]).unwrap_or(kappa);
                i += 2;
            }
            "--sigma" => {
                sigma = Decimal::from_str(&args[i + 1]).unwrap_or(sigma);
                i += 2;
            }
            "--order-size" => {
                order_size = Decimal::from_str(&args[i + 1]).unwrap_or(order_size);
                i += 2;
            }
            _ => i += 1,
        }
    }
    Args { events, gamma, kappa, sigma, order_size }
}

fn main() -> Result<()> {
    let args = parse_args();
    let events = load_events(&args.events).context("load events")?;
    println!("loaded {} events", events.len());

    let product = ProductSpec {
        symbol: "BTCUSDT".into(),
        base_asset: "BTC".into(),
        quote_asset: "USDT".into(),
        tick_size: dec!(0.01),
        lot_size: dec!(0.00001),
        min_notional: dec!(10),
        maker_fee: dec!(0.001),
        taker_fee: dec!(0.001),
        trading_status: Default::default(),
    };

    let config = MarketMakerConfig {
        gamma: args.gamma,
        kappa: args.kappa,
        sigma: args.sigma,
        time_horizon_secs: 300,
        num_levels: 3,
        order_size: args.order_size,
        refresh_interval_ms: 500,
        min_spread_bps: dec!(3),
        max_distance_bps: dec!(80),
        strategy: StrategyType::AvellanedaStoikov,
        momentum_enabled: true,
        momentum_window: 200,
        basis_shift: dec!(0),
        market_resilience_enabled: true,
        otr_enabled: false,
        hma_enabled: true,
        adaptive_enabled: false,
        apply_pair_class_template: false,
        hma_window: 9,
        momentum_ofi_enabled: false,
        momentum_learned_microprice_path: None,
        momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
        momentum_learned_microprice_online: false,
        momentum_learned_microprice_horizon: 10,
        user_stream_enabled: false,
        inventory_drift_tolerance: dec!(0.0001),
        inventory_drift_auto_correct: false,
        amend_enabled: true,
        amend_max_ticks: 2,
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
        fee_tier_refresh_enabled: false,
        fee_tier_refresh_secs: 600,
        borrow_enabled: false,
        borrow_rate_refresh_secs: 1800,
        borrow_holding_secs: 3600,
        borrow_max_base: dec!(0),
        borrow_buffer_base: dec!(0),
        pair_lifecycle_enabled: false,
        pair_lifecycle_refresh_secs: 300,
        var_guard_enabled: false,
        var_guard_limit_95: None,
        var_guard_limit_99: None,
        var_guard_ewma_lambda: None,
        cross_venue_basis_max_staleness_ms: 1500,
        strategy_capital_budget: std::collections::HashMap::new(),
        cross_exchange_min_profit_bps: dec!(5),
        max_cross_venue_divergence_pct: None,
        sor_inline_enabled: false,
        sor_dispatch_interval_secs: 5,
        sor_urgency: rust_decimal_macros::dec!(0.4),
        sor_target_qty_source: mm_common::config::SorTargetSource::InventoryExcess,
        sor_inventory_threshold: rust_decimal::Decimal::ZERO,
        sor_trade_rate_window_secs: 60,
        sor_queue_refresh_secs: 2,
    };

    let sim = Simulator::new(config, product, FillModel::queue_aware_log());
    let report = sim.run(&AvellanedaStoikov, &events);
    report.print();
    Ok(())
}
