//! Offline hyperparameter search for MM strategies.
//!
//! Takes a recorded-event JSONL file plus a TOML describing the
//! parameter search space, runs the backtester once per trial,
//! scores the run with the configured loss function, and prints
//! the best-so-far + a trial log. Lets operators calibrate gamma,
//! kappa, min_spread_bps, num_levels, order_size etc. against
//! historical order-book data without touching the live engine.
//!
//! Usage:
//! ```text
//! cargo run --release --bin mm-hyperopt -- \
//!     --events data/recorded/BTCUSDT.jsonl \
//!     --space  config/hyperopt.toml \
//!     --trials 200 \
//!     --loss   sharpe \
//!     --out    data/hyperopt/run.jsonl
//! ```
//!
//! The space TOML shape:
//! ```toml
//! strategy = "avellaneda_stoikov"       # or "glft", "grid"
//! base_config = "config/default.toml"   # starting MarketMakerConfig; search overlays on top
//!
//! [[param]]
//! name = "gamma"
//! kind = "uniform"       # or "log_uniform" / "int_uniform"
//! low  = 0.01
//! high = 1.0
//!
//! [[param]]
//! name = "kappa"
//! kind = "log_uniform"
//! low  = 0.1
//! high = 10.0
//!
//! [[param]]
//! name = "num_levels"
//! kind = "int_uniform"
//! low  = 1
//! high = 10
//! ```

use anyhow::{Context, Result};
use mm_backtester::{data::load_events, simulator::FillModel, simulator::Simulator};
use mm_common::config::{AppConfig, MarketMakerConfig, StrategyType};
use mm_common::types::ProductSpec;
use mm_hyperopt::{
    CalmarLoss, LossFn, Metrics, MultiMetricLoss, Param, RandomSearch, SearchSpace, SharpeLoss,
    SortinoLoss,
};
use mm_strategy::{AvellanedaStoikov, BasisStrategy, GlftStrategy, GridStrategy, Strategy};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Deserialize)]
struct SpaceSpec {
    #[serde(default = "default_strategy")]
    strategy: String,
    #[serde(default)]
    base_config: Option<String>,
    #[serde(default)]
    param: Vec<ParamSpec>,
}

fn default_strategy() -> String {
    "avellaneda_stoikov".to_string()
}

#[derive(Debug, Deserialize)]
struct ParamSpec {
    name: String,
    kind: String,
    low: f64,
    high: f64,
}

struct Args {
    events: PathBuf,
    space: PathBuf,
    trials: usize,
    seed: u64,
    loss: String,
    out: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let mut events = None;
    let mut space = None;
    let mut trials = 100usize;
    let mut seed = 42u64;
    let mut loss = "sharpe".to_string();
    let mut out = None;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--events" => {
                events = argv.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--space" => {
                space = argv.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--trials" => {
                trials = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .context("--trials takes a number")?;
                i += 2;
            }
            "--seed" => {
                seed = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .context("--seed takes a number")?;
                i += 2;
            }
            "--loss" => {
                loss = argv.get(i + 1).cloned().context("--loss requires value")?;
                i += 2;
            }
            "--out" => {
                out = argv.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                anyhow::bail!("unknown argument: {other}. Try --help");
            }
        }
    }

    Ok(Args {
        events: events.context("--events <path> required")?,
        space: space.context("--space <path> required")?,
        trials,
        seed,
        loss,
        out,
    })
}

fn print_usage() {
    println!(
        "Usage: mm-hyperopt --events <jsonl> --space <toml> \\
         [--trials 100] [--seed 42] [--loss sharpe|sortino|calmar|multi] \\
         [--out run.jsonl]\n\n\
         Runs a random search over strategy params against a\n\
         recorded-event replay. --help for this message.\n"
    );
}

fn build_loss(name: &str) -> Box<dyn LossFn> {
    match name.to_lowercase().as_str() {
        "sharpe" => Box::new(SharpeLoss),
        "sortino" => Box::new(SortinoLoss),
        "calmar" => Box::new(CalmarLoss),
        "maxdd" | "max_drawdown" => Box::new(mm_hyperopt::MaxDrawdownLoss),
        "multi" => Box::new(MultiMetricLoss::default()),
        other => {
            warn!(loss = other, "unknown loss fn, falling back to Sharpe");
            Box::new(SharpeLoss)
        }
    }
}

fn build_search_space(spec: &SpaceSpec) -> Result<SearchSpace> {
    let mut space = SearchSpace::new();
    for p in &spec.param {
        let param = match p.kind.as_str() {
            "uniform" => Param::uniform(&p.name, p.low, p.high),
            "log_uniform" => Param::log_uniform(&p.name, p.low, p.high),
            "int_uniform" => Param::int_uniform(&p.name, p.low as i64, p.high as i64),
            other => anyhow::bail!("unknown param kind '{other}' for {}", p.name),
        };
        space = space.add(param);
    }
    Ok(space)
}

fn load_base_config(path: &Path) -> Result<MarketMakerConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading base config {}", path.display()))?;
    let app: AppConfig = toml::from_str(&text).context("parsing base config TOML")?;
    Ok(app.market_maker)
}

fn strategy_type_from_str(s: &str) -> Result<StrategyType> {
    Ok(match s.to_lowercase().as_str() {
        "avellaneda" | "avellaneda_stoikov" => StrategyType::AvellanedaStoikov,
        "glft" => StrategyType::Glft,
        "grid" => StrategyType::Grid,
        other => anyhow::bail!(
            "strategy '{other}' not tunable via hyperopt — pick avellaneda_stoikov/glft/grid"
        ),
    })
}

fn apply_params(base: &MarketMakerConfig, params: &HashMap<String, f64>) -> MarketMakerConfig {
    let mut cfg = base.clone();
    let set = |field: &str, target: &mut Decimal, val: f64| {
        if let Some(&v) = params.get(field) {
            *target = Decimal::from_f64_retain(v).unwrap_or_else(|| target.to_owned());
        }
        let _ = val;
    };
    if let Some(&v) = params.get("gamma") {
        if let Some(d) = Decimal::from_f64_retain(v) {
            cfg.gamma = d;
        }
    }
    if let Some(&v) = params.get("kappa") {
        if let Some(d) = Decimal::from_f64_retain(v) {
            cfg.kappa = d;
        }
    }
    if let Some(&v) = params.get("sigma") {
        if let Some(d) = Decimal::from_f64_retain(v) {
            cfg.sigma = d;
        }
    }
    if let Some(&v) = params.get("min_spread_bps") {
        if let Some(d) = Decimal::from_f64_retain(v) {
            cfg.min_spread_bps = d;
        }
    }
    if let Some(&v) = params.get("max_distance_bps") {
        if let Some(d) = Decimal::from_f64_retain(v) {
            cfg.max_distance_bps = d;
        }
    }
    if let Some(&v) = params.get("order_size") {
        if let Some(d) = Decimal::from_f64_retain(v) {
            cfg.order_size = d;
        }
    }
    if let Some(&v) = params.get("num_levels") {
        cfg.num_levels = v.max(1.0) as usize;
    }
    if let Some(&v) = params.get("basis_shift") {
        if let Some(d) = Decimal::from_f64_retain(v) {
            cfg.basis_shift = d;
        }
    }
    // Suppress unused-closure warning under strict clippy.
    let _ = set;
    cfg
}

fn build_strategy(kind: StrategyType) -> Box<dyn Strategy> {
    match kind {
        StrategyType::AvellanedaStoikov => Box::new(AvellanedaStoikov),
        StrategyType::Glft => Box::new(GlftStrategy::new()),
        StrategyType::Grid => Box::new(GridStrategy),
        StrategyType::Basis
        | StrategyType::FundingArb
        | StrategyType::CrossVenueBasis
        | StrategyType::StatArb => {
            // Cross-product strategies need a hedge connector at
            // runtime — the backtester replays a single symbol so
            // we fall back to a permissive `BasisStrategy` with
            // no staleness gate for offline tuning. Stat-arb's
            // real dispatch happens in `StatArbDriver` which is
            // not part of the hyperopt loop; here we just need a
            // quote-producing strategy so the backtester can
            // measure baseline PnL.
            Box::new(BasisStrategy::new(dec!(0.5), dec!(50)))
        }
        StrategyType::CrossExchange => Box::new(BasisStrategy::new(dec!(0.5), dec!(50))),
    }
}

/// Build a placeholder ProductSpec for the backtester. The
/// recorded events do not carry tick/lot metadata, so we default
/// to the hardcoded BTCUSDT shape used in simulator tests. If
/// you want to tune against a different pair, pin tick/lot via
/// a separate flag in a future version.
fn default_product() -> ProductSpec {
    ProductSpec {
        symbol: "BTCUSDT".to_string(),
        base_asset: "BTC".to_string(),
        quote_asset: "USDT".to_string(),
        tick_size: dec!(0.01),
        lot_size: dec!(0.00001),
        min_notional: dec!(5),
        maker_fee: dec!(-0.0001),
        taker_fee: dec!(0.001),
        trading_status: Default::default(),
    }
}

fn report_to_metrics(r: &mm_backtester::report::BacktestReport) -> Metrics {
    // The hyperopt Metrics struct expects risk-adjusted measures
    // (sharpe/sortino/calmar). The BacktestReport carries raw
    // PnL — we derive proxies so the search has *some* signal.
    // A proper offline pipeline will substitute a dedicated
    // attribution tracker; this is enough to compare runs.
    let total_pnl = r.total_pnl.to_f64().unwrap_or(0.0);
    let volume = r
        .pnl_attribution
        .total_volume
        .to_f64()
        .unwrap_or(1.0)
        .max(1.0);
    let edge_bps = total_pnl / volume * 10_000.0;
    // Treat "edge per volume" as the pseudo-Sharpe — gives the
    // random search a smooth, signed target.
    Metrics {
        sharpe: edge_bps,
        sortino: edge_bps,
        calmar: edge_bps,
        max_drawdown: 0.0,
        total_pnl,
        num_trades: r.total_fills as usize,
        fill_rate: if r.total_quotes > 0 {
            r.total_fills as f64 / r.total_quotes as f64
        } else {
            0.0
        },
    }
}

fn f64_to_dec(v: f64) -> Decimal {
    Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let args = parse_args()?;
    info!(
        events = %args.events.display(),
        space = %args.space.display(),
        trials = args.trials,
        loss = %args.loss,
        "starting hyperopt run"
    );

    let events = load_events(&args.events).context("loading events")?;
    if events.is_empty() {
        anyhow::bail!("no events loaded — check the --events path");
    }

    let spec_text = std::fs::read_to_string(&args.space)
        .with_context(|| format!("reading space spec {}", args.space.display()))?;
    let spec: SpaceSpec = toml::from_str(&spec_text).context("parsing space TOML")?;

    let base = match &spec.base_config {
        Some(p) => load_base_config(Path::new(p))?,
        None => load_base_config(Path::new("config/default.toml"))?,
    };
    let strategy_kind = strategy_type_from_str(&spec.strategy)?;
    let search_space = build_search_space(&spec)?;
    let loss = build_loss(&args.loss);
    let mut search: RandomSearch<Box<dyn LossFn>> =
        RandomSearch::new(search_space, loss, args.seed);

    let product = default_product();
    let mut out_writer = args
        .out
        .as_ref()
        .map(|p| -> Result<std::io::BufWriter<std::fs::File>> {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            Ok(std::io::BufWriter::new(std::fs::File::create(p)?))
        })
        .transpose()?;

    let _ = f64_to_dec; // reserved for future param wiring

    for trial_idx in 0..args.trials {
        let params = search.suggest();
        let cfg = apply_params(&base, &params);
        let fill_model = FillModel::queue_aware_log();
        let strategy = build_strategy(strategy_kind);
        let sim = Simulator::new(cfg, product.clone(), fill_model);
        let report = sim.run(strategy.as_ref(), &events);
        let metrics = report_to_metrics(&report);
        info!(
            trial = trial_idx,
            pnl = report.total_pnl.to_f64().unwrap_or(0.0),
            fills = report.total_fills,
            edge_bps = metrics.sharpe,
            "trial done"
        );
        if let Some(w) = out_writer.as_mut() {
            let line = serde_json::json!({
                "trial": trial_idx,
                "params": params,
                "total_pnl": metrics.total_pnl,
                "fills": metrics.num_trades,
                "fill_rate": metrics.fill_rate,
                "edge_bps": metrics.sharpe,
            });
            writeln!(w, "{line}")?;
        }
        search.report(params, metrics);
    }

    if let Some(w) = out_writer.as_mut() {
        w.flush()?;
    }

    match search.best_trial() {
        Some(best) => {
            println!("═══════════════════════════════════════════");
            println!("  HYPEROPT BEST TRIAL");
            println!("═══════════════════════════════════════════");
            println!("  Trials:      {}", args.trials);
            println!("  Loss fn:     {}", args.loss);
            println!("  Best loss:   {:.6}", best.loss);
            println!("  Metrics:     {:?}", best.metrics);
            println!("  Params:");
            let mut keys: Vec<&String> = best.params.keys().collect();
            keys.sort();
            for k in keys {
                println!("    {k} = {}", best.params[k]);
            }
            println!("═══════════════════════════════════════════");
        }
        None => {
            warn!("no trials completed — search space exhausted or all rejected");
        }
    }

    Ok(())
}
