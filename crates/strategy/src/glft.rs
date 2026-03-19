use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::debug;

use crate::r#trait::{bps_to_frac, Strategy, StrategyContext};
use crate::volatility::decimal_sqrt;

/// Guéant-Lehalle-Fernandez-Tapia (GLFT) optimal market making model.
///
/// Extension of Avellaneda-Stoikov with:
/// - Execution risk from the order arrival process
/// - Bounded inventory constraints
/// - Calibrated intensity parameters from real order flow
///
/// Core formulas:
///   half_spread = (1 / (ξδ)) · ln(1 + ξδ/k)
///   skew = σ · C2
///   C2 = sqrt( (γ / (2Aδk)) · (1 + ξδ/k)^(k/(ξδ)+1) )
///
///   bid = fair_price - (half_spread + skew · q)
///   ask = fair_price + (half_spread - skew · q)
pub struct GlftStrategy {
    /// Calibrated intensity parameters.
    calibration: IntensityCalibration,
}

/// Parameters from fitting λ = A · exp(-k · δ).
#[derive(Debug, Clone)]
struct IntensityCalibration {
    /// Arrival rate constant A.
    a: Decimal,
    /// Depth sensitivity k.
    k: Decimal,
    /// Recent fill depths for recalibration.
    fill_depths: VecDeque<Decimal>,
    max_samples: usize,
}

impl IntensityCalibration {
    fn new() -> Self {
        Self {
            a: dec!(1.0),
            k: dec!(1.5),
            fill_depths: VecDeque::with_capacity(500),
            max_samples: 500,
        }
    }

    /// Record a fill at a certain depth from mid.
    pub fn record_fill_depth(&mut self, depth_from_mid: Decimal) {
        self.fill_depths.push_back(depth_from_mid.abs());
        if self.fill_depths.len() > self.max_samples {
            self.fill_depths.pop_front();
        }
        if self.fill_depths.len() >= 50 {
            self.recalibrate();
        }
    }

    /// Recalibrate A and k from observed fill depths.
    ///
    /// Method: bin depths, count fills per bin, fit ln(λ) = ln(A) - k·δ.
    fn recalibrate(&mut self) {
        if self.fill_depths.len() < 50 {
            return;
        }

        // Simple: compute mean depth and use it to estimate k.
        // k ≈ 1 / mean_depth (the higher the mean depth, the lower the sensitivity).
        let n = Decimal::from(self.fill_depths.len() as u64);
        let mean_depth: Decimal = self.fill_depths.iter().sum::<Decimal>() / n;

        if mean_depth > dec!(0.000001) {
            let new_k = dec!(1) / mean_depth;
            // Smooth update.
            self.k = self.k * dec!(0.9) + new_k * dec!(0.1);
        }

        // A ≈ fill_rate (fills per second).
        // Simplified: we'll keep A at 1.0 since it cancels in the spread formula.
        debug!(k = %self.k, a = %self.a, samples = self.fill_depths.len(), "GLFT recalibrated");
    }
}

impl GlftStrategy {
    pub fn new() -> Self {
        Self {
            calibration: IntensityCalibration::new(),
        }
    }

    /// Record a fill for calibration. Call from engine on each fill.
    pub fn on_fill_depth(&mut self, depth_from_mid: Decimal) {
        self.calibration.record_fill_depth(depth_from_mid);
    }
}

impl Default for GlftStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for GlftStrategy {
    fn name(&self) -> &str {
        "glft"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let gamma = ctx.config.gamma;
        let sigma = ctx.volatility;
        let t = ctx.time_remaining;
        let q = ctx.inventory;
        let a = self.calibration.a;
        let k = self.calibration.k;

        // ξ = γ (standard simplification).
        let xi = gamma;
        let delta = dec!(1);

        // C1 = (1 / (ξδ)) · ln(1 + ξδ/k)
        let xi_delta = xi * delta;
        let c1 = if xi_delta.is_zero() || k.is_zero() {
            bps_to_frac(ctx.config.min_spread_bps) * ctx.mid_price / dec!(2)
        } else {
            let ln_arg = dec!(1) + xi_delta / k;
            let ln_val = decimal_ln_positive(ln_arg);
            ln_val / xi_delta
        };

        // C2 = sqrt( (γ / (2Aδk)) · (1 + ξδ/k)^(k/(ξδ)+1) )
        let c2 = if gamma.is_zero() || a.is_zero() || k.is_zero() || xi_delta.is_zero() {
            dec!(0.001)
        } else {
            let base = dec!(1) + xi_delta / k;
            let exponent = k / xi_delta + dec!(1);
            // Approximate base^exponent via exp(exponent * ln(base)).
            let ln_base = decimal_ln_positive(base);
            let power = decimal_exp(exponent * ln_base);
            let inner = gamma / (dec!(2) * a * delta * k) * power;
            decimal_sqrt(inner.max(dec!(0)))
        };

        // half_spread and skew.
        let half_spread = c1 * sigma;
        let skew = sigma * c2;

        // Apply time scaling.
        let half_spread_t = half_spread * t;
        let skew_t = skew * t;

        // Optimal quotes.
        let fair = ctx.mid_price;
        let min_spread = bps_to_frac(ctx.config.min_spread_bps) * fair;

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);

        for level in 0..ctx.config.num_levels {
            let level_offset = Decimal::from(level as u64) * half_spread_t;

            let bid_price = fair - (half_spread_t + skew_t * q + level_offset);
            let ask_price = fair + (half_spread_t - skew_t * q + level_offset);

            // Enforce minimum spread.
            let actual_spread = ask_price - bid_price;
            let (bid_price, ask_price) = if actual_spread < min_spread {
                let adjustment = (min_spread - actual_spread) / dec!(2);
                (bid_price - adjustment, ask_price + adjustment)
            } else {
                (bid_price, ask_price)
            };

            let bid_price = ctx.product.round_price(bid_price);
            let ask_price = ctx.product.round_price(ask_price);
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
            strategy = "glft",
            c1 = %c1,
            c2 = %c2,
            half_spread = %half_spread_t,
            skew = %skew_t,
            k = %self.calibration.k,
            inventory = %q,
            "computed GLFT quotes"
        );

        quotes
    }
}

/// Natural log for positive Decimal values.
fn decimal_ln_positive(x: Decimal) -> Decimal {
    if x <= dec!(0) {
        return dec!(0);
    }
    if x == dec!(1) {
        return dec!(0);
    }
    let u = (x - dec!(1)) / (x + dec!(1));
    let u2 = u * u;
    let mut term = u;
    let mut sum = u;
    for k in 1..20 {
        term *= u2;
        sum += term / Decimal::from(2 * k + 1);
    }
    dec!(2) * sum
}

/// Exponential function for Decimal via Taylor series.
fn decimal_exp(x: Decimal) -> Decimal {
    // Clamp to avoid overflow.
    let x = x.min(dec!(20)).max(dec!(-20));
    let mut sum = dec!(1);
    let mut term = dec!(1);
    for i in 1..30 {
        term *= x / Decimal::from(i);
        sum += term;
        if term.abs() < dec!(0.0000000001) {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::MarketMakerConfig;
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::ProductSpec;

    fn test_ctx<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        inventory: Decimal,
    ) -> StrategyContext<'a> {
        StrategyContext {
            book,
            product,
            config,
            inventory,
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
        }
    }

    #[test]
    fn test_glft_produces_quotes() {
        let product = ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
        };
        let config = MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 3,
            order_size: dec!(0.001),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(5),
            max_distance_bps: dec!(100),
            strategy: mm_common::config::StrategyType::Glft,
            momentum_enabled: false,
            momentum_window: 200,
        };
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

        let strategy = GlftStrategy::new();
        let ctx = test_ctx(&book, &product, &config, dec!(0));
        let quotes = strategy.compute_quotes(&ctx);

        assert_eq!(quotes.len(), 3);
        let q0 = &quotes[0];
        assert!(q0.bid.is_some());
        assert!(q0.ask.is_some());

        let bid = q0.bid.as_ref().unwrap();
        let ask = q0.ask.as_ref().unwrap();
        assert!(bid.price < ask.price);
    }

    #[test]
    fn test_glft_skew_with_inventory() {
        let product = ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
        };
        let config = MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 1,
            order_size: dec!(0.001),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(5),
            max_distance_bps: dec!(100),
            strategy: mm_common::config::StrategyType::Glft,
            momentum_enabled: false,
            momentum_window: 200,
        };
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

        let strategy = GlftStrategy::new();
        let mid = book.mid_price().unwrap();

        // Long inventory — ask should be closer to mid (eager to sell).
        let ctx = test_ctx(&book, &product, &config, dec!(0.05));
        let quotes = strategy.compute_quotes(&ctx);
        let ask_dist = quotes[0].ask.as_ref().unwrap().price - mid;
        let bid_dist = mid - quotes[0].bid.as_ref().unwrap().price;
        assert!(
            ask_dist < bid_dist,
            "long inventory should skew ask closer to mid"
        );
    }

    #[test]
    fn test_exp_and_ln() {
        // exp(1) ≈ 2.718
        let e = decimal_exp(dec!(1));
        assert!((e - dec!(2.718)).abs() < dec!(0.01));

        // ln(e) ≈ 1
        let ln_e = decimal_ln_positive(e);
        assert!((ln_e - dec!(1)).abs() < dec!(0.01));
    }
}
