//! Epic R — Quote-stuffing exploit. **PENTEST ONLY.**
//!
//! Places a large rotating fan of orders at different tiny price
//! offsets every tick, dropping them the next tick. Produces the
//! high-orders/sec + near-zero fill-rate silhouette the
//! `QuoteStuffingDetector` catches.

use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::r#trait::{Strategy, StrategyContext};

#[derive(Debug, Clone)]
pub struct StuffConfig {
    pub push_side: Side,
    /// Orders per tick.
    pub orders_per_tick: usize,
    /// Tiny price offset step per order.
    pub step_bps: Decimal,
    pub leg_size: Decimal,
}

impl Default for StuffConfig {
    fn default() -> Self {
        Self {
            push_side: Side::Buy,
            orders_per_tick: 20,
            step_bps: dec!(0.1),
            leg_size: dec!(0.001),
        }
    }
}

#[derive(Debug, Default)]
pub struct StuffStrategy {
    pub config: StuffConfig,
    tick: AtomicU64,
}

impl StuffStrategy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_config(config: StuffConfig) -> Self {
        Self {
            config,
            tick: AtomicU64::new(0),
        }
    }
}

impl Strategy for StuffStrategy {
    fn name(&self) -> &str {
        "stuff"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let tick = self.tick.fetch_add(1, Ordering::Relaxed);
        let mid = ctx.mid_price;
        if mid <= Decimal::ZERO || self.config.orders_per_tick == 0 {
            return Vec::new();
        }
        let bp = dec!(10_000);
        let step = self.config.step_bps / bp * mid;
        let qty = ctx.product.round_qty(self.config.leg_size);
        let mut pairs = Vec::with_capacity(self.config.orders_per_tick);
        // Rotate the starting offset each tick so the diff sees
        // every order as new → all cancelled → maximum churn.
        let rotate = tick % 7;
        for i in 0..self.config.orders_per_tick {
            let offset = step * Decimal::from((i + rotate as usize + 1) as u64);
            let price = match self.config.push_side {
                Side::Buy => ctx.product.round_price(mid - offset),
                Side::Sell => ctx.product.round_price(mid + offset),
            };
            if price <= Decimal::ZERO || !ctx.product.meets_min_notional(price, qty) {
                continue;
            }
            let q = Quote {
                side: self.config.push_side,
                price,
                qty,
            };
            pairs.push(match self.config.push_side {
                Side::Buy => QuotePair {
                    bid: Some(q),
                    ask: None,
                },
                Side::Sell => QuotePair {
                    bid: None,
                    ask: Some(q),
                },
            });
        }
        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuff_defaults_yield_many_orders() {
        let s = StuffStrategy::new();
        assert_eq!(s.config.orders_per_tick, 20);
    }
}
