//! Epic R — Momentum ignition exploit. **PENTEST ONLY.**
//!
//! Bursts aggressive cross-through limit orders on one side for N
//! ticks, then flattens. Simulates the "aggressive taker flow →
//! fast price move → wait for reaction → exit" pattern the
//! `MomentumIgnitionDetector` catches.
//!
//! We can't emit true market-orders from the PostOnly pipeline, so
//! the exploit crosses the current touch with a limit price deep
//! into the opposite side of the book — same effect on the tape.

use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::r#trait::{Strategy, StrategyContext};

#[derive(Debug, Clone)]
pub struct IgniteConfig {
    /// Which side the ignition pushes. `Buy` forces price up.
    pub push_side: Side,
    /// Per-burst order size.
    pub burst_size: Decimal,
    /// How deep across the touch the crossing limit goes, in bps.
    /// 30 bps on a 100-dollar asset ≈ 30 cents past the opposite
    /// touch — sufficient to consume a few levels.
    pub cross_depth_bps: Decimal,
    /// Number of consecutive ticks to push before flattening.
    pub burst_ticks: u64,
    /// Number of flat ticks (no quotes) after each burst so the
    /// detector sees the classic "spike then silence".
    pub rest_ticks: u64,
}

impl Default for IgniteConfig {
    fn default() -> Self {
        Self {
            push_side: Side::Buy,
            burst_size: dec!(0.001),
            cross_depth_bps: dec!(30),
            burst_ticks: 5,
            rest_ticks: 3,
        }
    }
}

#[derive(Debug, Default)]
pub struct IgniteStrategy {
    pub config: IgniteConfig,
    /// Shared tick counter — `Strategy: Send + Sync` bound forbids
    /// interior mutability via `Cell`.
    tick: AtomicU64,
}

impl IgniteStrategy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_config(config: IgniteConfig) -> Self {
        Self { config, tick: AtomicU64::new(0) }
    }
}

impl Strategy for IgniteStrategy {
    fn name(&self) -> &str {
        "ignite"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let mid = ctx.mid_price;
        if mid <= Decimal::ZERO {
            return Vec::new();
        }

        let cycle = self.config.burst_ticks + self.config.rest_ticks;
        if cycle == 0 {
            return Vec::new();
        }
        let n = self.tick.fetch_add(1, Ordering::Relaxed) % cycle;
        if n >= self.config.burst_ticks {
            // Rest phase — no quote, which lets the diff cancel the
            // last burst's crossing order.
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
                if price <= Decimal::ZERO
                    || !ctx.product.meets_min_notional(price, qty)
                {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::MarketMakerConfig;
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::ProductSpec;

    fn pt_product() -> ProductSpec {
        ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
            trading_status: Default::default(),
        }
    }
    fn pt_cfg() -> MarketMakerConfig {
        MarketMakerConfig {
            gamma: dec!(0.1), kappa: dec!(1.5), sigma: dec!(0.02),
            time_horizon_secs: 300, num_levels: 1,
            order_size: dec!(0.01), refresh_interval_ms: 500,
            min_spread_bps: dec!(5), max_distance_bps: dec!(500),
            strategy: mm_common::config::StrategyType::Grid,
            momentum_enabled: false, momentum_window: 200,
            basis_shift: dec!(0.5), market_resilience_enabled: false,
            otr_enabled: false, hma_enabled: false,
            adaptive_enabled: false, apply_pair_class_template: false,
            hma_window: 9, momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
            momentum_learned_microprice_online: false,
            momentum_learned_microprice_horizon: 10,
            user_stream_enabled: false,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: false, amend_max_ticks: 2,
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
            fee_tier_refresh_enabled: false, fee_tier_refresh_secs: 600,
            borrow_enabled: false, borrow_rate_refresh_secs: 1800,
            borrow_holding_secs: 3600, borrow_max_base: dec!(0),
            borrow_buffer_base: dec!(0),
            pair_lifecycle_enabled: false, pair_lifecycle_refresh_secs: 300,
            var_guard_enabled: false, var_guard_limit_95: None,
            var_guard_limit_99: None, var_guard_ewma_lambda: None,
            cross_venue_basis_max_staleness_ms: 1500,
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false, sor_dispatch_interval_secs: 5,
            sor_urgency: dec!(0.4),
            sor_target_qty_source: mm_common::config::SorTargetSource::InventoryExcess,
            sor_inventory_threshold: rust_decimal::Decimal::ZERO,
            sor_trade_rate_window_secs: 60, sor_queue_refresh_secs: 2,
        }
    }

    #[test]
    fn ignite_alternates_burst_and_rest() {
        let product = pt_product();
        let cfg = pt_cfg();
        let book = LocalOrderBook::new("BTCUSDT".into());
        let s = IgniteStrategy::new(); // 5 burst + 3 rest = 8-tick cycle
        let ctx = |mid: Decimal| StrategyContext {
            book: &book, product: &product, config: &cfg,
            inventory: Decimal::ZERO,
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: mid,
            ref_price: None, hedge_book: None,
            borrow_cost_bps: None, hedge_book_age_ms: None,
            as_prob: None, as_prob_bid: None, as_prob_ask: None,
        };
        let mut burst_count = 0;
        let mut rest_count = 0;
        for _ in 0..16 {
            if s.compute_quotes(&ctx(dec!(30000))).is_empty() {
                rest_count += 1;
            } else {
                burst_count += 1;
            }
        }
        // 16 ticks × (5/8 burst + 3/8 rest) → 10 bursts + 6 rests.
        assert_eq!(burst_count, 10);
        assert_eq!(rest_count, 6);
    }
}
