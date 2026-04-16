//! Technical indicators — pure sync state machines, Decimal-based,
//! lookahead-safe by construction.
//!
//! Every indicator exposes the same minimal interface:
//!
//! ```ignore
//! let mut ind = Sma::new(14);
//! ind.update(price);       // feed one sample
//! let value = ind.value(); // Option<Decimal> — None until warmup
//! ```
//!
//! All state lives inside the indicator — no globals, no clocks, no
//! RNG, no peeking at the future. By construction each `update()`
//! reads only new input and `value()` depends only on samples
//! `0..=t`. (The backtester's `check_lookahead` primitive is
//! available for end-to-end property tests; the pinned numerical
//! tests in each indicator module are the canonical guarantees.)

mod atr;
mod bollinger;
mod candles;
mod ema;
mod hawkes;
mod hma;
mod rsi;
mod sma;
mod weights;

pub use atr::Atr;
pub use bollinger::{BollingerBands, BollingerValue};
pub use candles::{Candle, MultiTriggerCandles, TickCandles, TradeSide, VolumeCandles};
pub use ema::Ema;
pub use hawkes::{BivariateHawkes, HawkesIntensity};
pub use hma::{Hma, Wma};
pub use rsi::Rsi;
pub use sma::Sma;
pub use weights::{ema_weights, geometric_weights};
