use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

use crate::r#trait::{bps_to_frac, Strategy, StrategyContext};
/// Avellaneda-Stoikov optimal market making strategy.
///
/// Based on "High-frequency trading in a limit order book" (2008).
///
/// The model computes the optimal bid/ask quotes as:
///   reservation_price = mid - q * γ * σ² * (T - t)
///   spread = γ * σ² * (T - t) + (2/γ) * ln(1 + γ/κ)
///
/// Where:
///   q = inventory (positive = long)
///   γ = risk aversion (higher → tighter quotes, less inventory risk)
///   σ = volatility
///   T - t = time remaining
///   κ = order arrival intensity
pub struct AvellanedaStoikov;

impl Strategy for AvellanedaStoikov {
    fn name(&self) -> &str {
        "avellaneda-stoikov"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let gamma = ctx.config.gamma;
        let kappa = ctx.config.kappa;
        let sigma = ctx.volatility;
        let t = ctx.time_remaining;
        let q = ctx.inventory;

        let sigma_sq = sigma * sigma;
        let gamma_sigma_sq_t = gamma * sigma_sq * t;

        // Reservation price: mid - q * γ * σ² * (T-t).
        // This skews the midpoint away from our inventory.
        let reservation = ctx.mid_price - q * gamma_sigma_sq_t;

        // Optimal spread: γσ²(T-t) + (2/γ) * ln(1 + γ/κ).
        let spread = if gamma.is_zero() || kappa.is_zero() {
            bps_to_frac(ctx.config.min_spread_bps) * ctx.mid_price
        } else {
            let ln_term = decimal_ln(dec!(1) + gamma / kappa);
            gamma_sigma_sq_t + dec!(2) / gamma * ln_term
        };

        // Apply minimum spread.
        let min_spread = bps_to_frac(ctx.config.min_spread_bps) * ctx.mid_price;
        let spread = spread.max(min_spread);

        // Apply maximum distance.
        let max_distance = bps_to_frac(ctx.config.max_distance_bps) * ctx.mid_price;

        let half_spread = spread / dec!(2);

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);

        for level in 0..ctx.config.num_levels {
            let level_offset = Decimal::from(level as u64) * ctx.config.order_size * dec!(0.5);

            // Bid: reservation - half_spread - level_offset.
            let raw_bid = reservation - half_spread - level_offset;
            let bid_price = ctx
                .product
                .round_price(raw_bid.max(dec!(0)).min(ctx.mid_price + max_distance));

            // Ask: reservation + half_spread + level_offset.
            let raw_ask = reservation + half_spread + level_offset;
            let ask_price = ctx
                .product
                .round_price(raw_ask.max(ctx.mid_price - max_distance));

            let order_size = ctx.product.round_qty(ctx.config.order_size);

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
            strategy = "avellaneda",
            %reservation,
            %spread,
            inventory = %q,
            sigma = %sigma,
            levels = quotes.len(),
            "computed quotes"
        );

        quotes
    }
}

/// Natural log approximation for Decimal using series expansion.
/// ln(x) for x > 0, using ln(x) = 2 * atanh((x-1)/(x+1)).
fn decimal_ln(x: Decimal) -> Decimal {
    if x <= dec!(0) {
        return dec!(0);
    }
    if x == dec!(1) {
        return dec!(0);
    }

    // For values close to 1, use series: ln(1+u) ≈ u - u²/2 + u³/3 ...
    let u = (x - dec!(1)) / (x + dec!(1));
    let u2 = u * u;
    let mut term = u;
    let mut sum = u;
    for k in 1..20 {
        term *= u2;
        let divisor = Decimal::from(2 * k + 1);
        sum += term / divisor;
    }
    dec!(2) * sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::MarketMakerConfig;
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::ProductSpec;

    fn test_product() -> ProductSpec {
        ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
        }
    }

    fn test_config() -> MarketMakerConfig {
        MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 3,
            order_size: dec!(0.001),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(5),
            max_distance_bps: dec!(100),
            strategy: mm_common::config::StrategyType::AvellanedaStoikov,
            momentum_enabled: false,
            momentum_window: 200,
            basis_shift: dec!(0.5),
        }
    }

    #[test]
    fn test_symmetric_when_no_inventory() {
        let product = test_product();
        let config = test_config();
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![mm_common::PriceLevel {
                price: dec!(50000),
                qty: dec!(1),
            }],
            vec![mm_common::PriceLevel {
                price: dec!(50001),
                qty: dec!(1),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();

        let ctx = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: mid,
            ref_price: None,
        };

        let strategy = AvellanedaStoikov;
        let quotes = strategy.compute_quotes(&ctx);

        assert!(!quotes.is_empty());
        let q = &quotes[0];
        let bid = q.bid.as_ref().unwrap();
        let ask = q.ask.as_ref().unwrap();

        // With zero inventory, quotes should be roughly symmetric around mid.
        let bid_dist = mid - bid.price;
        let ask_dist = ask.price - mid;
        let diff = (bid_dist - ask_dist).abs();
        // Allow for tick rounding.
        assert!(diff <= dec!(0.02), "diff={diff}, should be near-symmetric");
    }

    #[test]
    fn test_skew_with_long_inventory() {
        let product = test_product();
        let config = test_config();
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![mm_common::PriceLevel {
                price: dec!(50000),
                qty: dec!(1),
            }],
            vec![mm_common::PriceLevel {
                price: dec!(50001),
                qty: dec!(1),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();

        let ctx = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0.05), // Long inventory → should skew quotes lower.
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: mid,
            ref_price: None,
        };

        let strategy = AvellanedaStoikov;
        let quotes = strategy.compute_quotes(&ctx);
        let q = &quotes[0];
        let bid = q.bid.as_ref().unwrap();
        let ask = q.ask.as_ref().unwrap();

        // With long inventory, both bid and ask should shift down
        // (reservation price < mid), so ask is closer to mid.
        let ask_dist = ask.price - mid;
        let bid_dist = mid - bid.price;
        assert!(
            ask_dist < bid_dist,
            "ask_dist={ask_dist} should be < bid_dist={bid_dist} when long"
        );
    }

    #[test]
    fn test_ln_approximation() {
        let result = decimal_ln(dec!(2));
        // ln(2) ≈ 0.6931
        assert!((result - dec!(0.6931)).abs() < dec!(0.001));
    }
}
