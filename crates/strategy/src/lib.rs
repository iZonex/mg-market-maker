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
