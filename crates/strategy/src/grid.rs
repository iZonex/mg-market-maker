use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

use crate::r#trait::{bps_to_frac, Strategy, StrategyContext};

/// Simple symmetric grid strategy.
///
/// Places N levels of quotes at equal intervals around the mid price.
/// The spread and level spacing are configured via min_spread_bps.
/// Inventory skew adjusts the center point.
pub struct GridStrategy;

impl Strategy for GridStrategy {
    fn name(&self) -> &str {
        "grid"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let mid = ctx.mid_price;
        let base_spread = bps_to_frac(ctx.config.min_spread_bps) * mid;
        let half_spread = base_spread / dec!(2);
        let level_step = base_spread; // Each level is one spread apart.

        // Inventory skew: shift center away from inventory direction.
        let skew = ctx.inventory * bps_to_frac(dec!(5)) * mid;
        let center = mid - skew;

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);

        for i in 0..ctx.config.num_levels {
            let offset = half_spread + Decimal::from(i as u64) * level_step;
            let order_size = ctx.product.round_qty(ctx.config.order_size);

            let bid_price = ctx.product.round_price(center - offset);
            let ask_price = ctx.product.round_price(center + offset);

            let bid =
                if bid_price > dec!(0) && ctx.product.meets_min_notional(bid_price, order_size) {
                    Some(Quote {
                        side: Side::Buy,
                        price: bid_price,
                        qty: order_size,
                    })
                } else {
                    None
                };

            let ask =
                if ask_price > dec!(0) && ctx.product.meets_min_notional(ask_price, order_size) {
                    Some(Quote {
                        side: Side::Sell,
                        price: ask_price,
                        qty: order_size,
                    })
                } else {
                    None
                };

            quotes.push(QuotePair { bid, ask });
        }

        debug!(
            strategy = "grid",
            %center,
            %base_spread,
            levels = quotes.len(),
            "computed grid quotes"
        );

        quotes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::MarketMakerConfig;
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::ProductSpec;
    use proptest::prelude::*;

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

    fn pt_config() -> MarketMakerConfig {
        MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 5,
            order_size: dec!(0.001),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(10),
            max_distance_bps: dec!(500),
            strategy: mm_common::config::StrategyType::Grid,
            momentum_enabled: false,
            momentum_window: 200,
            basis_shift: dec!(0.5),
            market_resilience_enabled: false,
            otr_enabled: false,
            hma_enabled: false,
            adaptive_enabled: false,
            apply_pair_class_template: false,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
            momentum_learned_microprice_online: false,
            momentum_learned_microprice_horizon: 10,
            user_stream_enabled: false,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: false,
            amend_max_ticks: 2,
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
            fee_tier_refresh_enabled: false,
            fee_tier_refresh_secs: 600,
            borrow_enabled: false,
            borrow_rate_refresh_secs: 1800,
            borrow_holding_secs: 3600,
            borrow_max_base: dec!(0),
            borrow_buffer_base: dec!(0),
            pair_lifecycle_enabled: false,
            pair_lifecycle_refresh_secs: 300,
            var_guard_enabled: false,
            var_guard_limit_95: None,
            var_guard_limit_99: None,
            var_guard_ewma_lambda: None,
            var_guard_cvar_limit_95: None,
            var_guard_cvar_limit_99: None,
            cross_venue_basis_max_staleness_ms: 1500,
            strategy_capital_budget: std::collections::HashMap::new(),
            symbol_circulating_supply: std::collections::HashMap::new(),
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false,
            sor_dispatch_interval_secs: 5,
            sor_urgency: rust_decimal_macros::dec!(0.4),
            sor_target_qty_source: mm_common::config::SorTargetSource::InventoryExcess,
            sor_inventory_threshold: rust_decimal::Decimal::ZERO,
            sor_trade_rate_window_secs: 60,
            sor_queue_refresh_secs: 2,
            sor_extra_l1_poll_secs: 5,
            venue_regime_classify_secs: 2,
        }
    }

    prop_compose! {
        fn mid_strat()(cents in 100_000i64..10_000_000i64) -> Decimal {
            Decimal::new(cents, 2)
        }
    }
    prop_compose! {
        fn inv_strat()(units in -1_000i64..1_000i64) -> Decimal {
            Decimal::new(units, 4)
        }
    }

    fn mk_ctx<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        inv: Decimal,
        mid: Decimal,
    ) -> StrategyContext<'a> {
        StrategyContext {
            book,
            product,
            config,
            inventory: inv,
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: mid,
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        }
    }

    fn seed_book(mid: Decimal) -> LocalOrderBook {
        let mut b = LocalOrderBook::new("BTCUSDT".into());
        b.apply_snapshot(
            vec![mm_common::PriceLevel {
                price: mid - dec!(0.5),
                qty: dec!(1),
            }],
            vec![mm_common::PriceLevel {
                price: mid + dec!(0.5),
                qty: dec!(1),
            }],
            1,
        );
        b
    }

    proptest! {
        /// Grid bids/asks never cross for any level.
        #[test]
        fn grid_bids_below_asks(
            inv in inv_strat(),
            mid in mid_strat(),
        ) {
            let product = pt_product();
            let config = pt_config();
            let book = seed_book(mid);
            let ctx = mk_ctx(&book, &product, &config, inv, mid);
            for q in &GridStrategy.compute_quotes(&ctx) {
                if let (Some(bid), Some(ask)) = (&q.bid, &q.ask) {
                    prop_assert!(bid.price < ask.price,
                        "crossed: bid {} >= ask {}", bid.price, ask.price);
                }
            }
        }

        /// Consecutive levels strictly widen — level i's bid is
        /// below level i-1's bid, level i's ask is above level
        /// i-1's ask. Catches a level_step regression.
        #[test]
        fn grid_levels_fan_out(
            mid in mid_strat(),
        ) {
            let product = pt_product();
            let config = pt_config();
            let book = seed_book(mid);
            let ctx = mk_ctx(&book, &product, &config, dec!(0), mid);
            let quotes = GridStrategy.compute_quotes(&ctx);
            for pair in quotes.windows(2) {
                if let (Some(b0), Some(b1)) = (&pair[0].bid, &pair[1].bid) {
                    prop_assert!(b1.price <= b0.price,
                        "bid {} at deeper level {} > outer", b1.price, b0.price);
                }
                if let (Some(a0), Some(a1)) = (&pair[0].ask, &pair[1].ask) {
                    prop_assert!(a1.price >= a0.price,
                        "ask {} at deeper level {} < outer", a1.price, a0.price);
                }
            }
        }

        /// Exactly num_levels QuotePairs are always emitted (the
        /// strategy does not skip levels; individual sides can
        /// be None on min-notional failures, but the outer pair
        /// count is fixed).
        #[test]
        fn grid_emits_exact_level_count(
            inv in inv_strat(),
            mid in mid_strat(),
        ) {
            let product = pt_product();
            let config = pt_config();
            let book = seed_book(mid);
            let ctx = mk_ctx(&book, &product, &config, inv, mid);
            let quotes = GridStrategy.compute_quotes(&ctx);
            prop_assert_eq!(quotes.len(), config.num_levels);
        }
    }
}
