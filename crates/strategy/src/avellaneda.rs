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
        let mut reservation = ctx.mid_price - q * gamma_sigma_sq_t;
        // P1.3 borrow-cost shim: when the engine threads in a
        // non-zero `borrow_cost_bps`, push the reservation UP by
        // that fraction of mid so we are less willing to be
        // short (the side that accrues the carry cost).
        if let Some(bps) = ctx.borrow_cost_bps {
            if bps > dec!(0) {
                reservation += bps_to_frac(bps) * ctx.mid_price;
            }
        }

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

        // Epic D sub-component #4 — Cartea adverse-selection
        // closed-form spread widening (CJP 2015 ch.4 §4.3 eq.
        // 4.20). Stage-3 promotes the symmetric path to a
        // per-side variant when both `as_prob_bid` AND
        // `as_prob_ask` are populated:
        //
        //   bid_half = base + (1 − 2·ρ_b) · σ · √(T−t)
        //   ask_half = base + (1 − 2·ρ_a) · σ · √(T−t)
        //
        // (each side individually clamped at min_spread/2)
        //
        // Symmetric `as_prob` stays as the fallback when
        // per-side fields are absent. ρ > 0.5 (informed flow)
        // shrinks a side; ρ < 0.5 (uninformed flow) widens it.
        let half_min = min_spread / dec!(2);
        let sqrt_t = crate::volatility::decimal_sqrt(t);
        let (bid_half_spread, ask_half_spread) = match (ctx.as_prob_bid, ctx.as_prob_ask) {
            (Some(rho_b), Some(rho_a)) => {
                // Per-side path — Epic D stage-3.
                let base = spread / dec!(2);
                let bid_widen = (dec!(1) - dec!(2) * rho_b) * sigma * sqrt_t;
                let ask_widen = (dec!(1) - dec!(2) * rho_a) * sigma * sqrt_t;
                (
                    (base + bid_widen).max(half_min),
                    (base + ask_widen).max(half_min),
                )
            }
            _ => {
                // Symmetric fallback (Epic D stage-2 path).
                let spread = if let Some(rho) = ctx.as_prob {
                    let as_delta = (dec!(1) - dec!(2) * rho) * sigma * sqrt_t;
                    (spread + as_delta).max(min_spread)
                } else {
                    spread
                };
                let half = spread / dec!(2);
                (half, half)
            }
        };

        // Apply maximum distance.
        let max_distance = bps_to_frac(ctx.config.max_distance_bps) * ctx.mid_price;

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);

        for level in 0..ctx.config.num_levels {
            let level_offset = Decimal::from(level as u64) * ctx.config.order_size * dec!(0.5);

            // Bid: reservation - bid_half_spread - level_offset.
            let raw_bid = reservation - bid_half_spread - level_offset;
            let bid_price = ctx
                .product
                .round_price(raw_bid.max(dec!(0)).min(ctx.mid_price + max_distance));

            // Ask: reservation + ask_half_spread + level_offset.
            let raw_ask = reservation + ask_half_spread + level_offset;
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
            %bid_half_spread,
            %ask_half_spread,
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
            trading_status: Default::default(),
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
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
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
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
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

    /// P1.3 borrow-cost shim: a non-zero `borrow_cost_bps` must
    /// shift BOTH bid and ask up by the same fraction-of-mid
    /// amount. Catches the regression of accidentally treating
    /// the surcharge as a one-sided ask widening, which would
    /// break the existing inventory-skew tests.
    #[test]
    fn borrow_cost_shifts_reservation_up() {
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

        let ctx_off = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
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
        };
        let ctx_on = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: mid,
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: Some(dec!(10)),
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        };
        let strategy = AvellanedaStoikov;
        let q_off = &strategy.compute_quotes(&ctx_off)[0];
        let q_on = &strategy.compute_quotes(&ctx_on)[0];
        let bid_off = q_off.bid.as_ref().unwrap().price;
        let ask_off = q_off.ask.as_ref().unwrap().price;
        let bid_on = q_on.bid.as_ref().unwrap().price;
        let ask_on = q_on.ask.as_ref().unwrap().price;

        // Both sides shifted up by a positive amount.
        assert!(
            bid_on > bid_off,
            "bid did not shift up: {bid_off} -> {bid_on}"
        );
        assert!(
            ask_on > ask_off,
            "ask did not shift up: {ask_off} -> {ask_on}"
        );
        // 10 bps × 50000.5 = 50.0005 in price terms; both
        // sides should have moved by roughly that amount.
        let bid_shift = bid_on - bid_off;
        let ask_shift = ask_on - ask_off;
        assert!(
            (bid_shift - ask_shift).abs() <= dec!(0.02),
            "bid and ask shifts must match: bid={bid_shift}, ask={ask_shift}"
        );
    }

    #[test]
    fn test_ln_approximation() {
        let result = decimal_ln(dec!(2));
        // ln(2) ≈ 0.6931
        assert!((result - dec!(0.6931)).abs() < dec!(0.001));
    }

    // --- Epic D sub-component #4 — Cartea AS widening ---

    /// Helper: build a simple `StrategyContext` with a
    /// configurable `as_prob`, holding everything else fixed.
    /// Per-side `as_prob_bid` / `as_prob_ask` default to `None`
    /// (symmetric path); the per-side overload below sets them.
    fn ctx_with_as_prob<'a>(
        book: &'a mm_common::orderbook::LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        as_prob: Option<Decimal>,
    ) -> StrategyContext<'a> {
        ctx_with_per_side_as_prob(book, product, config, as_prob, None, None)
    }

    fn ctx_with_per_side_as_prob<'a>(
        book: &'a mm_common::orderbook::LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        as_prob: Option<Decimal>,
        as_prob_bid: Option<Decimal>,
        as_prob_ask: Option<Decimal>,
    ) -> StrategyContext<'a> {
        let mid = book.mid_price().unwrap();
        StrategyContext {
            book,
            product,
            config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: mid,
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob,
            as_prob_bid,
            as_prob_ask,
        }
    }

    fn seeded_book() -> mm_common::orderbook::LocalOrderBook {
        let mut b = mm_common::orderbook::LocalOrderBook::new("BTCUSDT".into());
        b.apply_snapshot(
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
        b
    }

    #[test]
    fn as_prob_none_is_byte_identical_to_pre_epic_d() {
        let product = test_product();
        let config = test_config();
        let book = seeded_book();
        // `None` and `Some(0.5)` should both reduce to the
        // wave-1 formula (the AS additive collapses to zero).
        let ctx_none = ctx_with_as_prob(&book, &product, &config, None);
        let ctx_neutral = ctx_with_as_prob(&book, &product, &config, Some(dec!(0.5)));
        let strategy = AvellanedaStoikov;
        let q_none = &strategy.compute_quotes(&ctx_none)[0];
        let q_neutral = &strategy.compute_quotes(&ctx_neutral)[0];
        assert_eq!(
            q_none.bid.as_ref().unwrap().price,
            q_neutral.bid.as_ref().unwrap().price
        );
        assert_eq!(
            q_none.ask.as_ref().unwrap().price,
            q_neutral.ask.as_ref().unwrap().price
        );
    }

    #[test]
    fn as_prob_low_widens_the_spread() {
        // ρ = 0 → maximal widening.
        let product = test_product();
        let config = test_config();
        let book = seeded_book();
        let ctx_neutral = ctx_with_as_prob(&book, &product, &config, Some(dec!(0.5)));
        let ctx_wide = ctx_with_as_prob(&book, &product, &config, Some(dec!(0)));
        let strategy = AvellanedaStoikov;
        let q_n = &strategy.compute_quotes(&ctx_neutral)[0];
        let q_w = &strategy.compute_quotes(&ctx_wide)[0];
        let mid = book.mid_price().unwrap();
        let spread_neutral = q_n.ask.as_ref().unwrap().price - q_n.bid.as_ref().unwrap().price;
        let spread_wide = q_w.ask.as_ref().unwrap().price - q_w.bid.as_ref().unwrap().price;
        assert!(
            spread_wide > spread_neutral,
            "ρ=0 should widen the spread: neutral={spread_neutral}, wide={spread_wide}"
        );
        // Sanity: the widening is centered — both sides moved
        // by roughly the same amount.
        let ask_shift = q_w.ask.as_ref().unwrap().price - q_n.ask.as_ref().unwrap().price;
        let bid_shift = q_n.bid.as_ref().unwrap().price - q_w.bid.as_ref().unwrap().price;
        assert!(ask_shift > dec!(0));
        assert!(bid_shift > dec!(0));
        // Ensure mid didn't run away.
        let _ = mid;
    }

    #[test]
    fn as_prob_high_shrinks_the_spread_toward_min() {
        // ρ = 1 → maximal narrowing. Floor is min_spread.
        let product = test_product();
        let config = test_config();
        let book = seeded_book();
        let ctx_neutral = ctx_with_as_prob(&book, &product, &config, Some(dec!(0.5)));
        let ctx_narrow = ctx_with_as_prob(&book, &product, &config, Some(dec!(1)));
        let strategy = AvellanedaStoikov;
        let q_n = &strategy.compute_quotes(&ctx_neutral)[0];
        let q_t = &strategy.compute_quotes(&ctx_narrow)[0];
        let spread_neutral = q_n.ask.as_ref().unwrap().price - q_n.bid.as_ref().unwrap().price;
        let spread_narrow = q_t.ask.as_ref().unwrap().price - q_t.bid.as_ref().unwrap().price;
        assert!(
            spread_narrow <= spread_neutral,
            "ρ=1 should narrow or match: neutral={spread_neutral}, narrow={spread_narrow}"
        );
        // Floor invariant: never below `min_spread_bps` (modulo
        // tick rounding).
        let mid = book.mid_price().unwrap();
        let min_spread = bps_to_frac(config.min_spread_bps) * mid;
        assert!(
            spread_narrow + dec!(0.02) >= min_spread,
            "spread must respect the min_spread_bps floor"
        );
    }

    // -------- Epic D stage-3 — per-side asymmetric ρ --------

    #[test]
    fn per_side_none_is_byte_identical_to_symmetric() {
        // When per-side fields are both None, the strategy
        // must produce byte-identical quotes to the
        // symmetric-only path.
        let product = test_product();
        let config = test_config();
        let book = seeded_book();
        let ctx_sym = ctx_with_as_prob(&book, &product, &config, Some(dec!(0.3)));
        let ctx_per_side =
            ctx_with_per_side_as_prob(&book, &product, &config, Some(dec!(0.3)), None, None);
        let strategy = AvellanedaStoikov;
        let q_sym = &strategy.compute_quotes(&ctx_sym)[0];
        let q_per = &strategy.compute_quotes(&ctx_per_side)[0];
        assert_eq!(
            q_sym.bid.as_ref().unwrap().price,
            q_per.bid.as_ref().unwrap().price
        );
        assert_eq!(
            q_sym.ask.as_ref().unwrap().price,
            q_per.ask.as_ref().unwrap().price
        );
    }

    #[test]
    fn per_side_only_one_set_falls_back_to_symmetric() {
        // If only as_prob_bid is Some (as_prob_ask is None),
        // the strategy must NOT use the per-side path — it
        // falls back to the symmetric `as_prob`.
        let product = test_product();
        let config = test_config();
        let book = seeded_book();
        let ctx_sym = ctx_with_as_prob(&book, &product, &config, Some(dec!(0.5)));
        let ctx_partial = ctx_with_per_side_as_prob(
            &book,
            &product,
            &config,
            Some(dec!(0.5)),
            Some(dec!(0)),
            None,
        );
        let strategy = AvellanedaStoikov;
        let q_sym = &strategy.compute_quotes(&ctx_sym)[0];
        let q_partial = &strategy.compute_quotes(&ctx_partial)[0];
        // Only one per-side field set → symmetric fallback
        // → outputs identical to the symmetric-only path.
        assert_eq!(
            q_sym.bid.as_ref().unwrap().price,
            q_partial.bid.as_ref().unwrap().price
        );
        assert_eq!(
            q_sym.ask.as_ref().unwrap().price,
            q_partial.ask.as_ref().unwrap().price
        );
    }

    #[test]
    fn per_side_asymmetric_widens_one_side_independently() {
        // ρ_b = 0 (uninformed bid → widen bid),
        // ρ_a = 0.5 (neutral ask → no change).
        // Bid should move further from mid; ask should
        // stay near the symmetric-baseline ask.
        let product = test_product();
        let config = test_config();
        let book = seeded_book();
        let mid = book.mid_price().unwrap();
        let ctx_neutral = ctx_with_per_side_as_prob(
            &book,
            &product,
            &config,
            None,
            Some(dec!(0.5)),
            Some(dec!(0.5)),
        );
        let ctx_widen_bid = ctx_with_per_side_as_prob(
            &book,
            &product,
            &config,
            None,
            Some(dec!(0)),
            Some(dec!(0.5)),
        );
        let strategy = AvellanedaStoikov;
        let q_n = &strategy.compute_quotes(&ctx_neutral)[0];
        let q_w = &strategy.compute_quotes(&ctx_widen_bid)[0];

        let bid_dist_neutral = mid - q_n.bid.as_ref().unwrap().price;
        let bid_dist_widen = mid - q_w.bid.as_ref().unwrap().price;
        let ask_dist_neutral = q_n.ask.as_ref().unwrap().price - mid;
        let ask_dist_widen = q_w.ask.as_ref().unwrap().price - mid;

        assert!(
            bid_dist_widen > bid_dist_neutral,
            "ρ_b=0 must widen the bid: neutral={bid_dist_neutral}, widen={bid_dist_widen}"
        );
        // Ask side untouched by the bid-only widening.
        assert_eq!(ask_dist_widen, ask_dist_neutral);
    }

    // ── Property-based tests (Epic 17) ───────────────────────

    use proptest::prelude::*;
    use proptest::sample::select;

    fn mk_ctx<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        inventory: Decimal,
        volatility: Decimal,
        mid: Decimal,
    ) -> StrategyContext<'a> {
        StrategyContext {
            book,
            product,
            config,
            inventory,
            volatility,
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

    prop_compose! {
        fn mid_strat()(cents in 100_000i64..10_000_000i64) -> Decimal {
            Decimal::new(cents, 2)
        }
    }
    prop_compose! {
        fn inv_strat()(units in -10_000i64..10_000i64) -> Decimal {
            Decimal::new(units, 4)
        }
    }
    prop_compose! {
        fn sigma_strat()(raw in 1i64..1000i64) -> Decimal {
            Decimal::new(raw, 4)  // 0.0001 .. 0.1
        }
    }

    fn seed_book(mid: Decimal) -> LocalOrderBook {
        let mut b = LocalOrderBook::new("BTCUSDT".into());
        b.apply_snapshot(
            vec![mm_common::PriceLevel { price: mid - dec!(0.5), qty: dec!(1) }],
            vec![mm_common::PriceLevel { price: mid + dec!(0.5), qty: dec!(1) }],
            1,
        );
        b
    }

    proptest! {
        /// Every emitted bid < every emitted ask for the same
        /// level. A crossed quote would self-trade — the core
        /// correctness invariant of any maker strategy.
        #[test]
        fn bids_below_asks_on_every_level(
            inventory in inv_strat(),
            sigma in sigma_strat(),
            mid in mid_strat(),
        ) {
            let product = test_product();
            let config = test_config();
            let book = seed_book(mid);
            let ctx = mk_ctx(&book, &product, &config, inventory, sigma, mid);
            for q in &AvellanedaStoikov.compute_quotes(&ctx) {
                if let (Some(bid), Some(ask)) = (&q.bid, &q.ask) {
                    prop_assert!(bid.price < ask.price,
                        "crossed: bid {} >= ask {} (inv={}, sigma={})",
                        bid.price, ask.price, inventory, sigma);
                }
            }
        }

        /// All emitted prices land inside [mid - max_distance,
        /// mid + max_distance] — the config-level clamp.
        #[test]
        fn quotes_respect_max_distance(
            inventory in inv_strat(),
            sigma in sigma_strat(),
            mid in mid_strat(),
        ) {
            let product = test_product();
            let config = test_config();
            let book = seed_book(mid);
            let ctx = mk_ctx(&book, &product, &config, inventory, sigma, mid);
            let max_dist = mid * config.max_distance_bps / dec!(10_000);
            let lo = mid - max_dist;
            let hi = mid + max_dist;
            for q in &AvellanedaStoikov.compute_quotes(&ctx) {
                if let Some(b) = &q.bid {
                    prop_assert!(b.price <= hi + dec!(1),
                        "bid {} beyond hi {}", b.price, hi);
                }
                if let Some(a) = &q.ask {
                    prop_assert!(a.price >= lo - dec!(1),
                        "ask {} beyond lo {}", a.price, lo);
                }
            }
        }

        /// Long inventory (q > 0) shifts the reservation DOWN,
        /// so bid_distance ≥ ask_distance from mid. Short
        /// inventory should skew the other way. Zero inventory
        /// is approximately symmetric (hand test covers that).
        #[test]
        fn long_inventory_skews_down(
            inv_raw in 1i64..10_000i64,
            sigma in sigma_strat(),
            mid in mid_strat(),
        ) {
            let product = test_product();
            let config = test_config();
            let book = seed_book(mid);
            let inv = Decimal::new(inv_raw, 4);
            let ctx = mk_ctx(&book, &product, &config, inv, sigma, mid);
            let quotes = AvellanedaStoikov.compute_quotes(&ctx);
            if let Some(q) = quotes.first() {
                if let (Some(bid), Some(ask)) = (&q.bid, &q.ask) {
                    let bid_dist = mid - bid.price;
                    let ask_dist = ask.price - mid;
                    prop_assert!(bid_dist >= ask_dist - dec!(0.02),
                        "long inventory did not skew down: bid_dist={}, ask_dist={}",
                        bid_dist, ask_dist);
                }
            }
        }

        /// All emitted quantities are >= lot_size (rounded). No
        /// zero-sized orders ever leak into the venue.
        #[test]
        fn all_sizes_at_or_above_lot(
            inventory in inv_strat(),
            sigma in sigma_strat(),
            mid in mid_strat(),
        ) {
            let product = test_product();
            let config = test_config();
            let book = seed_book(mid);
            let ctx = mk_ctx(&book, &product, &config, inventory, sigma, mid);
            for q in &AvellanedaStoikov.compute_quotes(&ctx) {
                if let Some(b) = &q.bid {
                    prop_assert!(b.qty > dec!(0));
                }
                if let Some(a) = &q.ask {
                    prop_assert!(a.qty > dec!(0));
                }
            }
        }

        /// as_prob extremes produce bounded responses — ρ=0
        /// (always uninformed) and ρ=1 (always informed) should
        /// not crash or emit NaN-like behaviour.
        #[test]
        fn as_prob_extremes_remain_well_defined(
            rho in select(vec![dec!(0), dec!(0.25), dec!(0.5), dec!(0.75), dec!(1)]),
            mid in mid_strat(),
        ) {
            let product = test_product();
            let config = test_config();
            let book = seed_book(mid);
            let mut ctx = mk_ctx(&book, &product, &config, dec!(0), dec!(0.01), mid);
            ctx.as_prob = Some(rho);
            let quotes = AvellanedaStoikov.compute_quotes(&ctx);
            for q in &quotes {
                if let (Some(b), Some(a)) = (&q.bid, &q.ask) {
                    prop_assert!(b.price > dec!(0));
                    prop_assert!(a.price > dec!(0));
                    prop_assert!(b.price < a.price);
                }
            }
        }
    }
}
