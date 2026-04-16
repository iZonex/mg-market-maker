//! Stat-arb / cointegrated pairs trading (Epic B).
//!
//! Four sub-components compose into a standalone driver that
//! subscribes to two mid-price feeds and emits Enter / Exit
//! events when the spread z-score crosses configured bands:
//!
//! 1. [`cointegration`] — Engle-Granger 2-leg cointegration test
//! 2. [`kalman`] — adaptive hedge-ratio Kalman filter
//! 3. `signal` — rolling z-score + hysteresis (Sprint B-3)
//! 4. `driver` — full `StatArbDriver` tick loop (Sprint B-3 / B-4)
//!
//! Formulas and source attribution live in
//! `docs/research/stat-arb-pairs-formulas.md`.

pub mod cointegration;
pub mod driver;
#[allow(clippy::needless_range_loop, clippy::manual_range_contains)]
pub mod johansen;
pub mod kalman;
pub mod screener;
pub mod signal;

pub use cointegration::{
    CointegrationResult, EngleGrangerTest, MacKinnonLevel, mackinnon_critical_value,
};
pub use driver::{
    ExitLegs, LegDispatchReport, LegOutcome, NullStatArbSink, StatArbDriver, StatArbDriverConfig,
    StatArbEvent, StatArbEventSink, StatArbPair, StatArbPosition,
};
pub use johansen::{JohansenResult, JohansenTest};
pub use screener::{PairScreener, ScreenResult};
pub use kalman::KalmanHedgeRatio;
pub use signal::{SignalAction, SpreadDirection, ZScoreConfig, ZScoreSignal};
