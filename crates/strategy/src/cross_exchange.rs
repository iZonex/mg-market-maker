use mm_common::types::{Price, Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

use crate::r#trait::{bps_to_frac, Strategy, StrategyContext};

/// Cross-Exchange Market Making strategy.
///
/// Make on venue A (post limit orders), hedge on venue B (take on fill).
///
/// How it works:
/// 1. Observe best bid/ask on the HEDGE venue (venue B)
/// 2. Quote on the MAKER venue (venue A) at prices that guarantee
///    profit after hedging: our bid < hedge_bid (can sell hedge profitably),
///    our ask > hedge_ask (can buy hedge profitably).
/// 3. When filled on venue A, immediately hedge on venue B.
///
/// Profit = (our_ask - hedge_ask) or (hedge_bid - our_bid) minus both venues' fees.
///
/// This strategy uses the local book (venue A) and a reference price
/// that represents venue B's mid. The reference price should be set
/// externally from the unified order book.
pub struct CrossExchangeStrategy {
    /// Reference mid price from the hedge venue.
    pub hedge_mid: Price,
    /// Hedge venue maker fee (we'll be taker on hedge venue).
    pub hedge_taker_fee: Decimal,
    /// Our venue maker fee (negative = rebate).
    pub maker_fee: Decimal,
    /// Minimum profit margin in bps after fees.
    pub min_profit_bps: Decimal,
}

impl CrossExchangeStrategy {
    pub fn new(min_profit_bps: Decimal) -> Self {
        Self {
            hedge_mid: dec!(0),
            hedge_taker_fee: dec!(0.001), // Default 0.1% taker on hedge.
            maker_fee: dec!(-0.0005),     // Default -0.05% rebate on our venue.
            min_profit_bps,
        }
    }

    /// Update the hedge venue reference price.
    pub fn set_hedge_mid(&mut self, mid: Price) {
        self.hedge_mid = mid;
    }

    /// Set hedge venue fees.
    pub fn set_fees(&mut self, maker_fee: Decimal, hedge_taker_fee: Decimal) {
        self.maker_fee = maker_fee;
        self.hedge_taker_fee = hedge_taker_fee;
    }

    /// Calculate the effective price after fees for hedging.
    /// If we BUY on hedge venue: effective = hedge_ask * (1 + taker_fee).
    /// If we SELL on hedge venue: effective = hedge_bid * (1 - taker_fee).
    pub fn effective_hedge_buy(&self, price: Price) -> Price {
        price * (dec!(1) + self.hedge_taker_fee)
    }

    pub fn effective_hedge_sell(&self, price: Price) -> Price {
        price * (dec!(1) - self.hedge_taker_fee)
    }
}

impl Strategy for CrossExchangeStrategy {
    fn name(&self) -> &str {
        "cross-exchange"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        if self.hedge_mid.is_zero() {
            // No hedge reference — can't quote.
            return vec![];
        }

        let hedge_mid = self.hedge_mid;
        let min_profit = bps_to_frac(self.min_profit_bps) * hedge_mid;
        let total_fees = self.hedge_taker_fee + self.maker_fee.abs();
        let fee_cost = total_fees * hedge_mid;

        // Our ask must be high enough that: our_ask - hedge_ask - fees > min_profit.
        // So: our_ask > hedge_mid + fee_cost + min_profit.
        let min_ask = hedge_mid + fee_cost + min_profit;

        // Our bid must be low enough that: hedge_bid - our_bid - fees > min_profit.
        // So: our_bid < hedge_mid - fee_cost - min_profit.
        let max_bid = hedge_mid - fee_cost - min_profit;

        let order_size = ctx.product.round_qty(ctx.config.order_size);

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);
        let level_step = bps_to_frac(dec!(2)) * hedge_mid; // 2 bps per level.

        for i in 0..ctx.config.num_levels {
            let offset = Decimal::from(i as u64) * level_step;

            let bid_price = ctx.product.round_price(max_bid - offset);
            let ask_price = ctx.product.round_price(min_ask + offset);

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
            strategy = "cross-exchange",
            %hedge_mid,
            %min_ask,
            %max_bid,
            %fee_cost,
            levels = quotes.len(),
            "computed cross-exchange quotes"
        );

        quotes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::{MarketMakerConfig, StrategyType};
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::{PriceLevel, ProductSpec};

    #[test]
    fn test_cross_exchange_profitable_quotes() {
        let product = ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.001),
            min_notional: dec!(10),
            maker_fee: dec!(-0.0005),
            taker_fee: dec!(0.001),
            trading_status: Default::default(),
        };
        let config = MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 2,
            order_size: dec!(0.001),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(5),
            max_distance_bps: dec!(100),
            strategy: StrategyType::AvellanedaStoikov,
            momentum_enabled: false,
            momentum_window: 200,
            basis_shift: dec!(0.5),
            market_resilience_enabled: true,
            otr_enabled: true,
            hma_enabled: true,
            hma_window: 9,
            user_stream_enabled: true,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: true,
            amend_max_ticks: 2,
            fee_tier_refresh_enabled: true,
            fee_tier_refresh_secs: 600,
            borrow_enabled: false,
            borrow_rate_refresh_secs: 1800,
            borrow_holding_secs: 3600,
            borrow_max_base: dec!(0),
            borrow_buffer_base: dec!(0),
            pair_lifecycle_enabled: true,
            pair_lifecycle_refresh_secs: 300,
            var_guard_enabled: false,
            var_guard_limit_95: None,
            var_guard_limit_99: None,
            cross_venue_basis_max_staleness_ms: 1500,
        };
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(50000),
                qty: dec!(1),
            }],
            vec![PriceLevel {
                price: dec!(50001),
                qty: dec!(1),
            }],
            1,
        );

        let mut strategy = CrossExchangeStrategy::new(dec!(3)); // 3 bps min profit.
        strategy.set_hedge_mid(dec!(50000));
        strategy.set_fees(dec!(-0.0005), dec!(0.001));

        let ctx = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
        };

        let quotes = strategy.compute_quotes(&ctx);
        assert!(!quotes.is_empty());

        // Ask must be above hedge mid (profitable to sell on hedge after buying here).
        let ask = quotes[0].ask.as_ref().unwrap();
        assert!(ask.price > dec!(50000), "ask should be above hedge mid");

        // Bid must be below hedge mid.
        let bid = quotes[0].bid.as_ref().unwrap();
        assert!(bid.price < dec!(50000), "bid should be below hedge mid");

        // Spread should cover fees + min profit.
        let spread = ask.price - bid.price;
        assert!(spread > dec!(50), "spread should cover fees");
    }
}
