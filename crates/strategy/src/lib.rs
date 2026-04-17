pub mod ab_split;
pub mod autotune;
pub mod avellaneda;
pub mod basis;
pub mod cartea_spread;
pub mod cks_ofi;
pub mod cross_exchange;
pub mod exec_algo;
pub mod features;
pub mod funding_arb;
pub mod funding_arb_driver;
pub mod glft;
pub mod grid;
pub mod inventory_skew;
pub mod learned_microprice;
pub mod market_resilience;
pub mod momentum;
pub mod paired_unwind;
pub mod stat_arb;
pub mod r#trait;
pub mod twap;
pub mod volatility;
pub mod xemm;

pub use autotune::{AutoTuner, MarketRegime, RegimeParams};
pub use avellaneda::AvellanedaStoikov;
pub use basis::BasisStrategy;
pub use cross_exchange::CrossExchangeStrategy;
// Execution algorithms — TWAP / VWAP / POV / Iceberg. Wired
// into the hyperopt bin for offline tuning; pair-dispatch
// executors (FundingArbExecutor, BasisStrategy) will consume
// these when the cross-venue SOR stage-2 lands — see
// docs/research/production-mm-state-of-the-art.md §Epic A.
pub use exec_algo::{
    ExecAction, ExecAlgorithm, ExecContext, IcebergAlgo, IcebergConfig, PovAlgo, PovConfig,
    TwapAlgo, TwapConfig, VwapAlgo, VwapConfig,
};
pub use funding_arb::{FundingArbExecutor, PairDispatchOutcome, PairLegError};
pub use funding_arb_driver::{
    DriverEvent, DriverEventSink, FundingArbDriver, FundingArbDriverConfig, NullSink,
};
pub use glft::GlftStrategy;
pub use grid::GridStrategy;
pub use inventory_skew::AdvancedInventoryManager;
pub use momentum::MomentumSignals;
pub use paired_unwind::{PairedUnwindExecutor, SlicePair};
pub use r#trait::Strategy;
// Cross-exchange executor — companion to `CrossExchangeStrategy`,
// re-exported so SDK consumers can drive hedge-leg state machines
// without importing the module path directly. The executor is
// pure sync (no I/O), stage-2 of cross-venue MM will wire the
// venue dispatch into the engine.
pub use xemm::{XemmDecision, XemmExecutor};
