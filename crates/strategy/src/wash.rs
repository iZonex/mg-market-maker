//! Epic R — Wash-trading exploit. **PENTEST ONLY.**
//!
//! Places a buy + a sell at the same price every tick. On a real
//! matching engine this self-crosses → two fills on our books at
//! zero economic change in inventory but visible volume on the
//! public tape. The counterpart detector `WashDetector` catches
//! exactly this pattern — deploying this strategy against our own
//! venue fires our own surveillance on the same tape.
//!
//! **Never ship into a real operator's TOML.** The composite
//! `Strategy.Wash` graph node is marked `restricted: true`.

use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::r#trait::{Strategy, StrategyContext};

#[derive(Debug, Clone)]
pub struct WashConfig {
    /// Per-leg qty. Same on buy + sell — the whole point is a
    /// paired self-trade that nets to zero inventory.
    pub leg_size: Decimal,
    /// Offset from mid in bps. 0 = trade exactly at mid, which is
    /// the most visible wash signature. Nonzero lets the operator
    /// pentest how far off-mid the detector still catches.
    pub offset_bps: Decimal,
}

impl Default for WashConfig {
    fn default() -> Self {
        Self { leg_size: dec!(0.001), offset_bps: dec!(0) }
    }
}

#[derive(Debug, Default)]
pub struct WashStrategy {
    pub config: WashConfig,
}

impl WashStrategy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_config(config: WashConfig) -> Self {
        Self { config }
    }
}

impl Strategy for WashStrategy {
    fn name(&self) -> &str {
        "wash"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let mid = ctx.mid_price;
        if mid <= Decimal::ZERO {
            return Vec::new();
        }
        let offset = mid * (self.config.offset_bps / dec!(10_000));
        let price = ctx.product.round_price(mid + offset);
        let qty = ctx.product.round_qty(self.config.leg_size);
        if !ctx.product.meets_min_notional(price, qty) {
            return Vec::new();
        }
        vec![QuotePair {
            bid: Some(Quote { side: Side::Buy, price, qty }),
            ask: Some(Quote { side: Side::Sell, price, qty }),
        }]
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
            sor_trade_rate_window_secs: 60, sor_queue_refresh_secs: 2, sor_extra_l1_poll_secs: 5, venue_regime_classify_secs: 2, }
    }

    #[test]
    fn wash_emits_both_sides_same_price() {
        let product = pt_product();
        let cfg = pt_cfg();
        let book = LocalOrderBook::new("BTCUSDT".into());
        let s = WashStrategy::new();
        let ctx = StrategyContext {
            book: &book, product: &product, config: &cfg,
            inventory: Decimal::ZERO,
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: dec!(30000),
            ref_price: None, hedge_book: None,
            borrow_cost_bps: None, hedge_book_age_ms: None,
            as_prob: None, as_prob_bid: None, as_prob_ask: None,
        };
        let pairs = s.compute_quotes(&ctx);
        assert_eq!(pairs.len(), 1);
        let p = &pairs[0];
        let (b, a) = (p.bid.as_ref().unwrap(), p.ask.as_ref().unwrap());
        assert_eq!(b.price, a.price, "wash leg prices must match");
        assert_eq!(b.qty, a.qty, "wash leg sizes must match");
    }
}
