use serde::{Deserialize, Serialize};

/// Backtest output metrics consumed by loss functions.
///
/// The backtester produces a richer `BacktestReport`; this is the
/// subset the hyperopt loop cares about. The caller fills it out and
/// passes it to [`crate::RandomSearch::report`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub sharpe: f64,
    pub sortino: f64,
    pub calmar: f64,
    /// Max drawdown as a **positive** number in quote currency.
    /// Smaller is better.
    pub max_drawdown: f64,
    pub total_pnl: f64,
    pub num_trades: usize,
    pub fill_rate: f64,
}
