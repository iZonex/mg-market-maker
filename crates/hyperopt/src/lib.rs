//! Hyperparameter optimisation loop for the backtester.
//!
//! The MM engine shipped without an offline parameter search: we could
//! replay events through a strategy but not systematically tune its
//! knobs. This crate closes that gap with a small, dependency-free
//! random-search engine that:
//!
//! - Takes a [`SearchSpace`] describing the parameter ranges
//! - Asks it to [`suggest`](RandomSearch::suggest) a candidate
//! - Receives [`Metrics`] from the caller's backtest run
//! - Reduces them to a scalar loss via a pluggable [`LossFn`]
//! - Tracks the best trial seen so far
//! - Persists the full trial log to JSONL for offline analysis
//!
//! Random search is the baseline — it establishes the interface and
//! is embarrassingly parallel. A Bayesian / TPE upgrade is a
//! drop-in replacement for `RandomSearch` with the same traits.
//!
//! ## Typical loop
//!
//! ```no_run
//! # use mm_hyperopt::*;
//! # fn run_backtest(_: &std::collections::HashMap<String, f64>) -> Metrics { unimplemented!() }
//! let space = SearchSpace::new()
//!     .add(Param::uniform("gamma", 0.01, 1.0))
//!     .add(Param::log_uniform("kappa", 0.1, 10.0))
//!     .add(Param::int_uniform("num_levels", 1, 10));
//!
//! let mut search = RandomSearch::new(space, SharpeLoss, 42);
//! for _ in 0..200 {
//!     let params = search.suggest();
//!     let metrics = run_backtest(&params);
//!     search.report(params, metrics);
//! }
//! let best = search.best_trial().unwrap();
//! println!("best sharpe = {}", best.metrics.sharpe);
//! ```

pub mod calibration;
pub mod de;
mod loss;
mod metrics;
mod search;
mod space;

pub use de::{DeConfig, DeResult, DifferentialEvolution};
pub use loss::{CalmarLoss, LossFn, MaxDrawdownLoss, MultiMetricLoss, SharpeLoss, SortinoLoss};
pub use metrics::Metrics;
pub use search::{RandomSearch, Trial};
pub use space::{Param, SearchSpace};
