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
