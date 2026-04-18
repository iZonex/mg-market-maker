//! Epic R — Marking-the-close exploit. **PENTEST ONLY.**
//!
//! Places an aggressive cross-through order only within
//! `window_secs` of a session boundary (passed in through
//! `StrategyContext.time_remaining` — we reuse the existing field
//! as "seconds to next boundary" because plumbing a dedicated
//! one would bloat every StrategyContext). Idle outside the
//! window.

use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::r#trait::{Strategy, StrategyContext};

#[derive(Debug, Clone)]
pub struct MarkConfig {
    pub push_side: Side,
    /// Close-window size in seconds. The engine reads its session
    /// calendar and threads `seconds_to_boundary` via a per-
    /// strategy override; see engine-side adapter.
    pub window_secs: i64,
    pub burst_size: Decimal,
    pub cross_depth_bps: Decimal,
}

impl Default for MarkConfig {
    fn default() -> Self {
        Self {
            push_side: Side::Buy,
            window_secs: 60,
            burst_size: dec!(0.001),
            cross_depth_bps: dec!(30),
        }
    }
}

/// Seconds-to-boundary handed in via a thread-safe slot the
/// engine adapter writes before every tick. Simpler than
/// extending `StrategyContext`.
#[derive(Debug, Default)]
pub struct MarkStrategy {
    pub config: MarkConfig,
    pub seconds_to_boundary: std::sync::atomic::AtomicI64,
}

impl MarkStrategy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_config(config: MarkConfig) -> Self {
        Self { config, seconds_to_boundary: std::sync::atomic::AtomicI64::new(i64::MAX) }
    }
    pub fn set_seconds_to_boundary(&self, s: i64) {
        self.seconds_to_boundary
            .store(s, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Strategy for MarkStrategy {
    fn name(&self) -> &str {
        "mark"
    }

    fn on_session_tick(&self, seconds_to_boundary: i64) {
        self.set_seconds_to_boundary(seconds_to_boundary);
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let s = self
            .seconds_to_boundary
            .load(std::sync::atomic::Ordering::Relaxed);
        if s < 0 || s > self.config.window_secs {
            return Vec::new();
        }
        let mid = ctx.mid_price;
        if mid <= Decimal::ZERO {
            return Vec::new();
        }
        let cross = mid * (self.config.cross_depth_bps / dec!(10_000));
        let qty = ctx.product.round_qty(self.config.burst_size);
        match self.config.push_side {
            Side::Buy => {
                let price = ctx.product.round_price(mid + cross);
                if !ctx.product.meets_min_notional(price, qty) {
                    return Vec::new();
                }
                vec![QuotePair {
                    bid: Some(Quote { side: Side::Buy, price, qty }),
                    ask: None,
                }]
            }
            Side::Sell => {
                let price = ctx.product.round_price(mid - cross);
                if price <= Decimal::ZERO || !ctx.product.meets_min_notional(price, qty) {
                    return Vec::new();
                }
                vec![QuotePair {
                    bid: None,
                    ask: Some(Quote { side: Side::Sell, price, qty }),
                }]
            }
        }
    }
}
