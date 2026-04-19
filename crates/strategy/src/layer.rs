//! Epic R — Layering exploit. **PENTEST ONLY.**
//!
//! Structured multi-level pressure on one side of the book. Places
//! N clustered orders within `cluster_bps` of each other, all on
//! the push side; drops the whole layer every 2 ticks so the diff
//! cancels them synchronously — the textbook layering silhouette
//! the `LayeringDetector` catches.

use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::r#trait::{Strategy, StrategyContext};

#[derive(Debug, Clone)]
pub struct LayerConfig {
    pub push_side: Side,
    pub levels: usize,
    /// Per-level spacing in bps (cluster tight = layering).
    pub cluster_bps: Decimal,
    /// Distance of the innermost level from mid (bps).
    pub offset_bps: Decimal,
    pub leg_size: Decimal,
}

impl Default for LayerConfig {
    fn default() -> Self {
        Self {
            push_side: Side::Buy,
            levels: 5,
            cluster_bps: dec!(1),
            offset_bps: dec!(5),
            leg_size: dec!(0.001),
        }
    }
}

#[derive(Debug, Default)]
pub struct LayerStrategy {
    pub config: LayerConfig,
    tick: AtomicU64,
}

impl LayerStrategy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_config(config: LayerConfig) -> Self {
        Self { config, tick: AtomicU64::new(0) }
    }
}

impl Strategy for LayerStrategy {
    fn name(&self) -> &str {
        "layer"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let tick = self.tick.fetch_add(1, Ordering::Relaxed);
        // Rest every other tick → engine's quote diff cancels the
        // layer synchronously, matching the detector's sync-cancel
        // signal.
        if !tick.is_multiple_of(2) {
            return Vec::new();
        }
        let mid = ctx.mid_price;
        if mid <= Decimal::ZERO || self.config.levels == 0 {
            return Vec::new();
        }
        let bp = dec!(10_000);
        let base_offset = self.config.offset_bps / bp * mid;
        let spacing = self.config.cluster_bps / bp * mid;
        let qty = ctx.product.round_qty(self.config.leg_size);
        let mut pairs = Vec::with_capacity(self.config.levels);
        for i in 0..self.config.levels {
            let distance = base_offset + spacing * Decimal::from(i as u64);
            let price = match self.config.push_side {
                Side::Buy => ctx.product.round_price(mid - distance),
                Side::Sell => ctx.product.round_price(mid + distance),
            };
            if price <= Decimal::ZERO
                || !ctx.product.meets_min_notional(price, qty)
            {
                continue;
            }
            let q = Quote { side: self.config.push_side, price, qty };
            pairs.push(match self.config.push_side {
                Side::Buy => QuotePair { bid: Some(q), ask: None },
                Side::Sell => QuotePair { bid: None, ask: Some(q) },
            });
        }
        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::MarketMakerConfig;
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::ProductSpec;

    fn product() -> ProductSpec {
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
    fn cfg() -> MarketMakerConfig {
        MarketMakerConfig {
            gamma: dec!(0.1), kappa: dec!(1.5), sigma: dec!(0.02),
            time_horizon_secs: 300, num_levels: 1,
            order_size: dec!(0.001), refresh_interval_ms: 500,
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
            var_guard_cvar_limit_95: None, var_guard_cvar_limit_99: None,
            cross_venue_basis_max_staleness_ms: 1500,
            strategy_capital_budget: std::collections::HashMap::new(),
            symbol_circulating_supply: std::collections::HashMap::new(),
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
    fn layer_alternates_place_and_rest() {
        let p = product();
        let c = cfg();
        let book = LocalOrderBook::new("BTCUSDT".into());
        let s = LayerStrategy::new();
        let make_ctx = || StrategyContext {
            book: &book, product: &p, config: &c,
            inventory: Decimal::ZERO,
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: dec!(30000),
            ref_price: None, hedge_book: None,
            borrow_cost_bps: None, hedge_book_age_ms: None,
            as_prob: None, as_prob_bid: None, as_prob_ask: None,
        };
        assert_eq!(s.compute_quotes(&make_ctx()).len(), 5, "tick 0: place");
        assert!(s.compute_quotes(&make_ctx()).is_empty(), "tick 1: rest");
        assert_eq!(s.compute_quotes(&make_ctx()).len(), 5, "tick 2: place again");
    }
}
