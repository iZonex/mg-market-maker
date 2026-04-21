//! Hyperopt worker task (Epic 33 sub-epic E33.3 backend half).
//!
//! Receives `HyperoptTrigger` payloads from the dashboard admin
//! endpoint, runs a random-search hyperopt against the specified
//! recording, and stages the best trial as a `PendingCalibration`
//! for operator review.
//!
//! The worker runs inside the server binary (not the dashboard
//! crate) because hyperopt pulls in `mm-backtester` + `mm-strategy`
//! which the dashboard crate deliberately avoids depending on.
//! We keep the cycle-free shape by having the dashboard emit
//! typed messages through a channel the server holds the receiver
//! end of.
//!
//! Search space is intentionally conservative: γ and κ span one
//! order of magnitude each, σ floors from 1e-6 to 1e-3, order_size
//! log-uniform, num_levels 1–5. Matches the Avellaneda-Stoikov /
//! GLFT strategies' meaningful ranges without over-exploring
//! pathological corners.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use mm_backtester::data::load_events;
use mm_backtester::simulator::{FillModel, Simulator};
use mm_common::config::{AppConfig, MarketMakerConfig, StrategyType};
use mm_common::types::ProductSpec;
use mm_dashboard::state::{DashboardState, HyperoptTrigger, PendingCalibration};
use mm_hyperopt::{
    LossFn, MaxDrawdownLoss, Metrics, Param, RandomSearch, SearchSpace, SharpeLoss,
};
use mm_strategy::AvellanedaStoikov;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, info, warn};

/// Spawn the background worker. The returned task handle is
/// kept alive by the caller (dropping it aborts the worker).
pub fn spawn_worker(
    mut rx: UnboundedReceiver<HyperoptTrigger>,
    dashboard: DashboardState,
    base_config: AppConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(trig) = rx.recv().await {
            let dash = dashboard.clone();
            let cfg = base_config.clone();
            // Each run lives in its own spawn_blocking — the sim
            // is CPU-bound sync code, so we don't want to tie up
            // the Tokio reactor.
            if let Err(e) = tokio::task::spawn_blocking(move || run_one(&dash, &cfg, &trig))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("hyperopt panicked: {e}")))
            {
                error!(error = %e, "hyperopt run failed");
            }
        }
    })
}

fn run_one(dashboard: &DashboardState, base: &AppConfig, trig: &HyperoptTrigger) -> Result<()> {
    info!(
        symbol = %trig.symbol,
        trials = trig.num_trials,
        recording = %trig.recording_path,
        "hyperopt run starting"
    );
    let events = load_events(Path::new(&trig.recording_path))
        .with_context(|| format!("load recording {}", trig.recording_path))?;
    if events.is_empty() {
        anyhow::bail!("recording is empty");
    }

    let space = SearchSpace::new()
        .add(Param::log_uniform("gamma", 0.01, 1.0))
        .add(Param::log_uniform("kappa", 1.0, 100.0))
        .add(Param::log_uniform("sigma", 1e-6, 1e-3))
        .add(Param::uniform("min_spread_bps", 0.5, 50.0))
        .add(Param::log_uniform("order_size", 0.00001, 1.0))
        .add(Param::int_uniform("num_levels", 1, 5));

    let loss: Box<dyn LossFn> = match trig.loss_fn.as_str() {
        "maxdd" | "max_drawdown" => Box::new(MaxDrawdownLoss),
        _ => Box::new(SharpeLoss),
    };
    let mut search: RandomSearch<Box<dyn LossFn>> = RandomSearch::new(space, loss, 0);

    // Product spec — the hot-path inputs aren't known per-symbol
    // from here, so use a conservative BTCUSDT-flavoured default.
    // Mirrors what the offline `mm-hyperopt` binary does.
    let product = default_product_for_symbol(&trig.symbol);

    for trial_idx in 0..trig.num_trials {
        let params = search.suggest();
        let cfg = apply_params(&base.market_maker, &params);
        let sim = Simulator::new(cfg, product.clone(), FillModel::queue_aware_log());
        let report = sim.run(&AvellanedaStoikov, &events);

        let metrics = report_to_metrics(&report);
        search.report(params, metrics);
        if trial_idx.is_multiple_of(10) {
            info!(trial = trial_idx, "hyperopt progress");
        }
    }

    let best = match search.best_trial() {
        Some(t) => t,
        None => {
            warn!("hyperopt produced no trials");
            return Ok(());
        }
    };

    // Build current snapshot from base config so UI can diff.
    let mut current: HashMap<String, Decimal> = HashMap::new();
    current.insert("gamma".into(), base.market_maker.gamma);
    current.insert("kappa".into(), base.market_maker.kappa);
    current.insert("sigma".into(), base.market_maker.sigma);
    current.insert("min_spread_bps".into(), base.market_maker.min_spread_bps);
    current.insert("order_size".into(), base.market_maker.order_size);
    current.insert(
        "num_levels".into(),
        Decimal::from(base.market_maker.num_levels as u64),
    );

    let mut suggested: HashMap<String, Decimal> = HashMap::new();
    for (k, v) in &best.params {
        if let Some(d) = Decimal::from_f64(*v) {
            suggested.insert(k.clone(), d);
        }
    }

    let pending = PendingCalibration {
        symbol: trig.symbol.clone(),
        created_at: chrono::Utc::now(),
        trials: trig.num_trials,
        loss_fn: best.loss_fn.clone(),
        best_loss: Decimal::from_f64(best.loss).unwrap_or(dec!(0)),
        suggested,
        current,
    };
    dashboard.stage_calibration(pending);
    info!(
        symbol = %trig.symbol,
        loss = best.loss,
        "hyperopt run finished — staged for operator review"
    );
    Ok(())
}

fn apply_params(base: &MarketMakerConfig, params: &HashMap<String, f64>) -> MarketMakerConfig {
    let mut cfg = base.clone();
    let f = |k: &str, default: Decimal| -> Decimal {
        params
            .get(k)
            .and_then(|v| Decimal::from_f64(*v))
            .unwrap_or(default)
    };
    cfg.gamma = f("gamma", cfg.gamma);
    cfg.kappa = f("kappa", cfg.kappa);
    cfg.sigma = f("sigma", cfg.sigma);
    cfg.min_spread_bps = f("min_spread_bps", cfg.min_spread_bps);
    cfg.order_size = f("order_size", cfg.order_size);
    if let Some(&n) = params.get("num_levels") {
        let v = n as u64;
        cfg.num_levels = v.max(1) as usize;
    }
    cfg.strategy = StrategyType::AvellanedaStoikov;
    cfg
}

fn report_to_metrics(report: &mm_backtester::report::BacktestReport) -> Metrics {
    let pnl = report.total_pnl.to_f64().unwrap_or(0.0);
    // Degenerate-but-consistent mapping: Sharpe proxy = PnL per
    // volume (efficiency bps), Max-DD proxy = absolute inventory
    // at end, number of trades = fills. Matches what the offline
    // `mm-hyperopt` binary does.
    let volume = report.pnl_attribution.total_volume.to_f64().unwrap_or(0.0);
    let sharpe = if volume > 0.0 { pnl / volume } else { 0.0 };
    Metrics {
        total_pnl: pnl,
        max_drawdown: report.final_inventory.abs().to_f64().unwrap_or(0.0),
        sharpe,
        sortino: sharpe,
        calmar: sharpe,
        num_trades: report.total_fills as usize,
        fill_rate: if report.total_ticks > 0 {
            report.total_fills as f64 / report.total_ticks as f64
        } else {
            0.0
        },
    }
}

fn default_product_for_symbol(symbol: &str) -> ProductSpec {
    // Conservative shape — the recording itself supplies price
    // + volume level detail, so these fields only matter for
    // rounding. Defaults mirror a Binance BTCUSDT-ish product.
    ProductSpec {
        symbol: symbol.into(),
        base_asset: symbol
            .strip_suffix("USDT")
            .unwrap_or(symbol)
            .to_string(),
        quote_asset: "USDT".into(),
        tick_size: dec!(0.01),
        lot_size: dec!(0.00001),
        min_notional: dec!(10),
        maker_fee: dec!(0.001),
        taker_fee: dec!(0.001),
        trading_status: Default::default(),
    }
}
