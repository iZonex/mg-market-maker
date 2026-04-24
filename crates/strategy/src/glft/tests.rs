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
            adaptive_enabled: false,
            apply_pair_class_template: false,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
            momentum_learned_microprice_online: false,
            momentum_learned_microprice_horizon: 10,
            user_stream_enabled: true,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: true,
            amend_max_ticks: 2,
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
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
            sor_extra_l1_poll_secs: 5, venue_regime_classify_secs: 2, };
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
            adaptive_enabled: false,
            apply_pair_class_template: false,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
            momentum_learned_microprice_online: false,
            momentum_learned_microprice_horizon: 10,
            user_stream_enabled: true,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: true,
            amend_max_ticks: 2,
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
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
            sor_extra_l1_poll_secs: 5, venue_regime_classify_secs: 2, };
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
            adaptive_enabled: false,
            apply_pair_class_template: false,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
            momentum_learned_microprice_online: false,
            momentum_learned_microprice_horizon: 10,
            user_stream_enabled: true,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: true,
            amend_max_ticks: 2,
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
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
            sor_extra_l1_poll_secs: 5, venue_regime_classify_secs: 2, };
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

    // ── Property-based tests (Epic 17) ───────────────────────

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
            adaptive_enabled: false,
            apply_pair_class_template: false,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
            momentum_learned_microprice_online: false,
            momentum_learned_microprice_horizon: 10,
            user_stream_enabled: true,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: true,
            amend_max_ticks: 2,
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
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
            sor_extra_l1_poll_secs: 5, venue_regime_classify_secs: 2, }
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
        /// Every bid < every ask for the same level. Core
        /// correctness invariant — a crossed quote self-trades.
        #[test]
        fn glft_bids_below_asks(
            inv in inv_strat(),
            mid in mid_strat(),
        ) {
            let product = pt_product();
            let config = pt_config();
            let book = seed_book(mid);
            let ctx = test_ctx(&book, &product, &config, inv);
            let strat = GlftStrategy::new();
            for q in &strat.compute_quotes(&ctx) {
                if let (Some(bid), Some(ask)) = (&q.bid, &q.ask) {
                    prop_assert!(bid.price < ask.price,
                        "crossed: bid {} >= ask {}", bid.price, ask.price);
                }
            }
        }

        /// All emitted quantities > 0. No zero-sized orders.
        #[test]
        fn glft_positive_sizes(
            inv in inv_strat(),
            mid in mid_strat(),
        ) {
            let product = pt_product();
            let config = pt_config();
            let book = seed_book(mid);
            let ctx = test_ctx(&book, &product, &config, inv);
            let strat = GlftStrategy::new();
            for q in &strat.compute_quotes(&ctx) {
                if let Some(b) = &q.bid {
                    prop_assert!(b.qty > dec!(0));
                    prop_assert!(b.price > dec!(0));
                }
                if let Some(a) = &q.ask {
                    prop_assert!(a.qty > dec!(0));
                    prop_assert!(a.price > dec!(0));
                }
            }
        }

        /// Long inventory skews the quote ladder DOWN
        /// (bid_dist ≥ ask_dist). Same invariant as Avellaneda
        /// — both strategies share the reservation-price shape.
        #[test]
        fn glft_long_inventory_skews_down(
            inv_raw in 1i64..10_000i64,
            mid in mid_strat(),
        ) {
            let product = pt_product();
            let config = pt_config();
            let book = seed_book(mid);
            let inv = Decimal::new(inv_raw, 4);
            let ctx = test_ctx(&book, &product, &config, inv);
            let strat = GlftStrategy::new();
            let quotes = strat.compute_quotes(&ctx);
            if let Some(q) = quotes.first() {
                if let (Some(bid), Some(ask)) = (&q.bid, &q.ask) {
                    let bid_dist = mid - bid.price;
                    let ask_dist = ask.price - mid;
                    prop_assert!(bid_dist >= ask_dist - dec!(0.1),
                        "long inventory did not skew down: bid_dist={}, ask_dist={}",
                        bid_dist, ask_dist);
                }
            }
        }
    }

    /// MM-2 — 50+ `on_fill` notifications drive the calibration
    /// into its recalibration branch; k moves off the constructor
    /// default. Confirms the `&self` hook actually mutates state
    /// through the `Mutex`.
    #[test]
    fn on_fill_drives_k_recalibration() {
        use crate::r#trait::{FillObservation, Strategy as _};
        use mm_common::types::Side;

        let strat = GlftStrategy::new();
        let k_before = strat.calibration.lock().unwrap().k;
        // Simulate 60 fills at depth 0.3 from a mid of 100 — a
        // stable stream should pull k toward 1/0.3 ≈ 3.33.
        for _ in 0..60u64 {
            let obs = FillObservation {
                side: Side::Buy,
                price: dec!(99.7),
                qty: dec!(0.001),
                depth_from_mid: dec!(0.3),
                mid: dec!(100),
                is_maker: true,
                ts_ms: 0,
            };
            strat.on_fill(&obs);
        }
        let k_after = strat.calibration.lock().unwrap().k;
        assert_ne!(k_before, k_after, "k should move after 60 fills");
        // Smoothed update (weight 0.1) pulls k a fraction of the
        // way toward 1/0.3 ≈ 3.33 — should exceed the 1.5 default.
        assert!(k_after > dec!(1.5), "k_after = {k_after}; expected to move past default 1.5");
    }

    /// S5.4 — `recalibrate_if_due` is a no-op below the 50-sample
    /// threshold (state.samples stays 0, `a`+`k` keep the
    /// constructor defaults) and only fires a retune when ≥30 s
    /// have elapsed since the previous one.
    #[test]
    fn recalibrate_if_due_honours_sample_gate_and_cooldown() {
        let strat = GlftStrategy::new();
        // Below-threshold call: no retune.
        strat.recalibrate_if_due(1_000_000);
        let s1 = strat.calibration_state().expect("poisoned mutex");
        assert_eq!(s1.samples, 0);
        assert!(s1.last_recalibrated_ms.is_none());
        assert_eq!(s1.k, dec!(1.5));

        // Seed 60 samples at a skewed mean so `k` moves.
        for _ in 0..60 {
            strat.record_fill_depth(dec!(0.3));
        }
        // `record_fill_depth`'s implicit retune has already fired;
        // a periodic retune at t=0 must update the timestamp.
        strat.recalibrate_if_due(0);
        let s2 = strat.calibration_state().unwrap();
        assert_eq!(s2.samples, 60);
        assert!(s2.last_recalibrated_ms.is_some());

        // Same-second call: cooldown gate blocks the retune, so
        // `last_recalibrated_ms` remains unchanged.
        let ts2 = s2.last_recalibrated_ms.unwrap();
        strat.recalibrate_if_due(ts2 + 10_000);
        let s3 = strat.calibration_state().unwrap();
        assert_eq!(s3.last_recalibrated_ms, Some(ts2));

        // 30 s later: cooldown lifts, timestamp advances.
        strat.recalibrate_if_due(ts2 + 30_000);
        let s4 = strat.calibration_state().unwrap();
        assert_eq!(s4.last_recalibrated_ms, Some(ts2 + 30_000));
    }

    /// 22B-1 — checkpoint_state → restore_state round trip. Feed
    /// 80 synthetic fills to warm the calibration past the 50-
    /// sample retune threshold, snapshot, restore into a fresh
    /// strategy, and assert (A, k) + sample count + timestamp
    /// all survive.
    #[test]
    fn checkpoint_round_trip_preserves_calibration() {
        let src = GlftStrategy::new();
        let mid = dec!(50_000);
        for i in 0..80 {
            let depth = Decimal::from(i) / dec!(10); // 0.0 .. 7.9
            src.on_fill(&FillObservation {
                side: Side::Buy,
                price: mid + depth,
                qty: dec!(1),
                depth_from_mid: depth,
                mid,
                is_maker: true,
                ts_ms: 1_000 * i as i64,
            });
        }
        src.recalibrate_if_due(10_000_000);

        let snap = src.checkpoint_state().expect("snapshot");
        let before = src.calibration_state().unwrap();

        let dst = GlftStrategy::new();
        dst.restore_state(&snap).unwrap();
        let after = dst.calibration_state().unwrap();

        assert_eq!(after.a, before.a);
        assert_eq!(after.k, before.k);
        assert_eq!(after.samples, before.samples);
        assert_eq!(after.last_recalibrated_ms, before.last_recalibrated_ms);
    }

    /// 22B-1 — unsupported schema_version returns Err; the
    /// strategy keeps its defaults unchanged.
    #[test]
    fn restore_rejects_wrong_schema_version() {
        let s = GlftStrategy::new();
        let before = s.calibration_state().unwrap();
        let bogus = serde_json::json!({
            "schema_version": 999,
            "a": "1.0",
            "k": "1.5",
            "fill_depths": [],
            "last_recalibrated_ms": null,
        });
        let err = s.restore_state(&bogus).unwrap_err();
        assert!(err.contains("schema_version"), "{err}");
        let after = s.calibration_state().unwrap();
        assert_eq!(after.a, before.a);
        assert_eq!(after.k, before.k);
    }

    /// 22B-1 — a stateless strategy returns `None` from
    /// checkpoint_state (default trait impl). Regression guard
    /// against accidentally making every strategy spam state
    /// into the checkpoint.
    #[test]
    fn stateless_strategy_returns_no_checkpoint() {
        use crate::AvellanedaStoikov;
        let s = AvellanedaStoikov;
        assert!(s.checkpoint_state().is_none());
    }
