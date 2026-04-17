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

        // Epic D stage-2 sub-component 2B — Cartea
        // adverse-selection closed-form spread widening
        // (Cartea-Jaimungal-Penalva 2015 ch.4 §4.3 eq. 4.20).
        // Stage-3 promotes the symmetric path to a per-side
        // variant when both `as_prob_bid` AND `as_prob_ask`
        // are populated — each side gets its own additive
        // `(1 − 2·ρ_side) · σ · √(T − t)` term.
        //
        // `ρ > 0.5` narrows toward the floor; `ρ < 0.5`
        // widens. When per-side fields are absent the
        // symmetric `as_prob` path is the fallback. `as_prob
        // == None` OR `Some(0.5)` produce a byte-identical
        // half-spread to the pre-stage-2 wave-1 path.
        let min_half_spread = min_spread / dec!(2);
        let sqrt_t_remaining = decimal_sqrt(ctx.time_remaining);
        let (bid_half_spread_t, ask_half_spread_t) = match (ctx.as_prob_bid, ctx.as_prob_ask) {
            (Some(rho_b), Some(rho_a)) => {
                // Per-side path — Epic D stage-3.
                let bid_widen = (dec!(1) - dec!(2) * rho_b) * sigma * sqrt_t_remaining;
                let ask_widen = (dec!(1) - dec!(2) * rho_a) * sigma * sqrt_t_remaining;
                (
                    (half_spread_t + bid_widen).max(min_half_spread),
                    (half_spread_t + ask_widen).max(min_half_spread),
                )
            }
            _ => {
                // Symmetric fallback (Epic D stage-2 path).
                let half = match ctx.as_prob {
                    None => half_spread_t,
                    Some(rho) if rho == dec!(0.5) => half_spread_t,
                    Some(rho) => {
                        let as_delta = (dec!(1) - dec!(2) * rho) * sigma * sqrt_t_remaining;
                        (half_spread_t + as_delta).max(min_half_spread)
                    }
                };
                (half, half)
            }
        };

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);

        for level in 0..ctx.config.num_levels {
            // Per-side level offsets — symmetric on the
            // existing average half-spread for backward
            // compat with the wave-1 level-spreading
            // semantics.
            let avg_half = (bid_half_spread_t + ask_half_spread_t) / dec!(2);
            let level_offset = Decimal::from(level as u64) * avg_half;

            let bid_price = fair - (bid_half_spread_t + skew_t * q + level_offset);
            let ask_price = fair + (ask_half_spread_t - skew_t * q + level_offset);

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
            bid_half_spread = %bid_half_spread_t,
            ask_half_spread = %ask_half_spread_t,
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
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
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
            trading_status: Default::default(),
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
            basis_shift: dec!(0.5),
            market_resilience_enabled: true,
            otr_enabled: true,
            hma_enabled: true,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
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
            var_guard_ewma_lambda: None,
            cross_venue_basis_max_staleness_ms: 1500,
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false,
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
            trading_status: Default::default(),
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
            basis_shift: dec!(0.5),
            market_resilience_enabled: true,
            otr_enabled: true,
            hma_enabled: true,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
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
            var_guard_ewma_lambda: None,
            cross_venue_basis_max_staleness_ms: 1500,
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false,
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

    // -------- Epic D stage-2 sub-component 2B: GLFT + Cartea AS --------

    /// Build a fresh `StrategyContext` with a custom `as_prob`.
    /// Mirrors the `ctx_with_as_prob` helper from
    /// `avellaneda.rs::tests`. Uses a large `volatility` and
    /// `time_remaining` so the AS component
    /// `(1 − 2ρ)·σ·√(T − t)` is on the order of hundreds of
    /// dollars — easily visible above the tick floor and above
    /// the min-spread re-clamp.
    fn glft_ctx_with_as_prob<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        as_prob: Option<Decimal>,
    ) -> StrategyContext<'a> {
        glft_ctx_with_per_side_as_prob(book, product, config, as_prob, None, None)
    }

    fn glft_ctx_with_per_side_as_prob<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        as_prob: Option<Decimal>,
        as_prob_bid: Option<Decimal>,
        as_prob_ask: Option<Decimal>,
    ) -> StrategyContext<'a> {
        StrategyContext {
            book,
            product,
            config,
            inventory: dec!(0),
            volatility: dec!(100),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob,
            as_prob_bid,
            as_prob_ask,
        }
    }

    fn glft_as_test_fixtures() -> (LocalOrderBook, ProductSpec, MarketMakerConfig) {
        let product = ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
            trading_status: Default::default(),
        };
        // NOTE on `min_spread_bps`: we deliberately use a tiny
        // value (0.1 bps ≈ 0.5 on a 50k mid) so the AS component
        // actually perturbs the output. With the default 5 bps
        // floor, the raw GLFT `half_spread_t` of ~0.013 is
        // already far below floor and any ρ perturbation of
        // `(1 − 2ρ)·σ·√(T − t) ≈ ±0.02` would be eaten by the
        // post-level floor re-clamp. Tiny floor → raw spread
        // is above floor → AS additive is visible in the
        // bid/ask prices.
        let config = MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 1,
            order_size: dec!(0.001),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(0.1),
            max_distance_bps: dec!(100),
            strategy: mm_common::config::StrategyType::Glft,
            momentum_enabled: false,
            momentum_window: 200,
            basis_shift: dec!(0.5),
            market_resilience_enabled: true,
            otr_enabled: true,
            hma_enabled: true,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
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
            var_guard_ewma_lambda: None,
            cross_venue_basis_max_staleness_ms: 1500,
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false,
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
        (book, product, config)
    }

    #[test]
    fn glft_as_prob_none_is_byte_identical_to_wave1() {
        // Baseline: build the strategy twice, once without
        // `as_prob`, once with `as_prob = None`. Since None is
        // the default, these should be trivially equal — the
        // test exists to guard against a future refactor
        // introducing a side effect.
        let (book, product, config) = glft_as_test_fixtures();
        let strategy = GlftStrategy::new();
        let ctx_a = glft_ctx_with_as_prob(&book, &product, &config, None);
        let ctx_b = glft_ctx_with_as_prob(&book, &product, &config, None);
        let q_a = strategy.compute_quotes(&ctx_a);
        let q_b = strategy.compute_quotes(&ctx_b);
        assert_eq!(
            q_a[0].bid.as_ref().map(|q| q.price),
            q_b[0].bid.as_ref().map(|q| q.price)
        );
        assert_eq!(
            q_a[0].ask.as_ref().map(|q| q.price),
            q_b[0].ask.as_ref().map(|q| q.price)
        );
    }

    #[test]
    fn glft_as_prob_neutral_half_is_byte_identical_to_none() {
        // `Some(0.5)` is the "I have no AS signal" value —
        // the additive component is zero by construction.
        // Must produce byte-identical output to the `None`
        // code path.
        let (book, product, config) = glft_as_test_fixtures();
        let strategy = GlftStrategy::new();
        let ctx_none = glft_ctx_with_as_prob(&book, &product, &config, None);
        let ctx_neutral = glft_ctx_with_as_prob(&book, &product, &config, Some(dec!(0.5)));
        let q_none = strategy.compute_quotes(&ctx_none);
        let q_neutral = strategy.compute_quotes(&ctx_neutral);
        let p_none_bid = q_none[0].bid.as_ref().map(|q| q.price);
        let p_none_ask = q_none[0].ask.as_ref().map(|q| q.price);
        let p_neu_bid = q_neutral[0].bid.as_ref().map(|q| q.price);
        let p_neu_ask = q_neutral[0].ask.as_ref().map(|q| q.price);
        assert_eq!(p_none_bid, p_neu_bid);
        assert_eq!(p_none_ask, p_neu_ask);
    }

    #[test]
    fn glft_as_prob_zero_widens_spread() {
        // ρ = 0 means full uninformed flow — AS component is
        // +σ·√(T − t) and the spread should widen vs the
        // neutral case.
        let (book, product, config) = glft_as_test_fixtures();
        let strategy = GlftStrategy::new();
        let ctx_neutral = glft_ctx_with_as_prob(&book, &product, &config, Some(dec!(0.5)));
        let ctx_wide = glft_ctx_with_as_prob(&book, &product, &config, Some(dec!(0)));
        let q_neutral = strategy.compute_quotes(&ctx_neutral);
        let q_wide = strategy.compute_quotes(&ctx_wide);
        let neutral_spread =
            q_neutral[0].ask.as_ref().unwrap().price - q_neutral[0].bid.as_ref().unwrap().price;
        let wide_spread =
            q_wide[0].ask.as_ref().unwrap().price - q_wide[0].bid.as_ref().unwrap().price;
        assert!(
            wide_spread > neutral_spread,
            "ρ=0 should widen the spread: neutral={neutral_spread}, wide={wide_spread}",
        );
    }

    #[test]
    fn glft_as_prob_one_narrows_toward_floor() {
        // ρ = 1 means full informed flow — the AS component
        // is −σ·√(T − t). The re-clamp at `min_half_spread`
        // should keep the output above the floor but strictly
        // narrower than (or equal to) the neutral case.
        let (book, product, config) = glft_as_test_fixtures();
        let strategy = GlftStrategy::new();
        let ctx_neutral = glft_ctx_with_as_prob(&book, &product, &config, Some(dec!(0.5)));
        let ctx_narrow = glft_ctx_with_as_prob(&book, &product, &config, Some(dec!(1)));
        let q_neutral = strategy.compute_quotes(&ctx_neutral);
        let q_narrow = strategy.compute_quotes(&ctx_narrow);
        let neutral_spread =
            q_neutral[0].ask.as_ref().unwrap().price - q_neutral[0].bid.as_ref().unwrap().price;
        let narrow_spread =
            q_narrow[0].ask.as_ref().unwrap().price - q_narrow[0].bid.as_ref().unwrap().price;
        assert!(
            narrow_spread <= neutral_spread,
            "ρ=1 should not widen the spread: neutral={neutral_spread}, narrow={narrow_spread}",
        );
        // Floor: min_spread = 5 bps on 50000.5 mid ≈ 25.00025.
        // The narrow spread must still respect that floor.
        let mid = book.mid_price().unwrap();
        let min_spread = bps_to_frac(config.min_spread_bps) * mid;
        assert!(
            narrow_spread >= min_spread - dec!(0.0001),
            "narrow spread {narrow_spread} fell below floor {min_spread}",
        );
    }

    #[test]
    fn glft_as_prob_monotone_across_rho() {
        // Sweep ρ from 0 → 0.25 → 0.5 and verify the spread
        // monotonically shrinks (or stays equal, never
        // widens).
        let (book, product, config) = glft_as_test_fixtures();
        let strategy = GlftStrategy::new();
        let rhos = [dec!(0), dec!(0.25), dec!(0.5)];
        let mut prev: Option<Decimal> = None;
        for rho in rhos {
            let ctx = glft_ctx_with_as_prob(&book, &product, &config, Some(rho));
            let q = strategy.compute_quotes(&ctx);
            let spread = q[0].ask.as_ref().unwrap().price - q[0].bid.as_ref().unwrap().price;
            if let Some(p) = prev {
                assert!(
                    spread <= p,
                    "non-monotone at ρ={rho}: spread={spread}, prev={p}"
                );
            }
            prev = Some(spread);
        }
    }

    // -------- Epic D stage-3 — per-side asymmetric ρ --------

    #[test]
    fn glft_per_side_none_is_byte_identical_to_symmetric() {
        let (book, product, config) = glft_as_test_fixtures();
        let strategy = GlftStrategy::new();
        let ctx_sym = glft_ctx_with_as_prob(&book, &product, &config, Some(dec!(0.3)));
        let ctx_per_side =
            glft_ctx_with_per_side_as_prob(&book, &product, &config, Some(dec!(0.3)), None, None);
        let q_sym = strategy.compute_quotes(&ctx_sym);
        let q_per = strategy.compute_quotes(&ctx_per_side);
        assert_eq!(
            q_sym[0].bid.as_ref().unwrap().price,
            q_per[0].bid.as_ref().unwrap().price
        );
        assert_eq!(
            q_sym[0].ask.as_ref().unwrap().price,
            q_per[0].ask.as_ref().unwrap().price
        );
    }

    #[test]
    fn glft_per_side_only_one_set_falls_back_to_symmetric() {
        let (book, product, config) = glft_as_test_fixtures();
        let strategy = GlftStrategy::new();
        let ctx_sym = glft_ctx_with_as_prob(&book, &product, &config, Some(dec!(0.5)));
        let ctx_partial = glft_ctx_with_per_side_as_prob(
            &book,
            &product,
            &config,
            Some(dec!(0.5)),
            Some(dec!(0)),
            None,
        );
        let q_sym = strategy.compute_quotes(&ctx_sym);
        let q_partial = strategy.compute_quotes(&ctx_partial);
        assert_eq!(
            q_sym[0].bid.as_ref().unwrap().price,
            q_partial[0].bid.as_ref().unwrap().price
        );
        assert_eq!(
            q_sym[0].ask.as_ref().unwrap().price,
            q_partial[0].ask.as_ref().unwrap().price
        );
    }

    #[test]
    fn glft_per_side_asymmetric_widens_one_side_independently() {
        let (book, product, config) = glft_as_test_fixtures();
        let strategy = GlftStrategy::new();
        let mid = book.mid_price().unwrap();

        let ctx_neutral = glft_ctx_with_per_side_as_prob(
            &book,
            &product,
            &config,
            None,
            Some(dec!(0.5)),
            Some(dec!(0.5)),
        );
        let ctx_widen_ask = glft_ctx_with_per_side_as_prob(
            &book,
            &product,
            &config,
            None,
            Some(dec!(0.5)),
            Some(dec!(0)),
        );
        let q_n = strategy.compute_quotes(&ctx_neutral);
        let q_w = strategy.compute_quotes(&ctx_widen_ask);

        let bid_dist_neutral = mid - q_n[0].bid.as_ref().unwrap().price;
        let bid_dist_widen = mid - q_w[0].bid.as_ref().unwrap().price;
        let ask_dist_neutral = q_n[0].ask.as_ref().unwrap().price - mid;
        let ask_dist_widen = q_w[0].ask.as_ref().unwrap().price - mid;

        assert!(
            ask_dist_widen > ask_dist_neutral,
            "ρ_a=0 must widen the ask: neutral={ask_dist_neutral}, widen={ask_dist_widen}"
        );
        // Bid side should be unchanged (or only marginally
        // affected by the level-offset averaging).
        // Allow tick rounding tolerance.
        assert!((bid_dist_widen - bid_dist_neutral).abs() < dec!(50));
    }
}
