    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::sor::venue_state::VenueSeed;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::Side;
    use mm_exchange_core::connector::{ExchangeConnector, VenueId, VenueProduct};
    use mm_strategy::avellaneda::AvellanedaStoikov;
    use mm_strategy::stat_arb::{NullStatArbSink, StatArbDriver, StatArbDriverConfig, StatArbPair};

    fn sample_product(symbol: &str) -> mm_common::types::ProductSpec {
        mm_common::types::ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn make_engine_with_bundle(bundle: ConnectorBundle) -> MarketMakerEngine {
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    /// End-to-end #1: a multi-leg `RouteDecision` issues real
    /// per-venue `place_order` calls — one on each connector
    /// in the bundle. Both venues land in the bundle via
    /// `ConnectorBundle.extra`, both are registered on the
    /// SOR aggregator, and `dispatch_route` with a taker
    /// urgency produces two IOC legs.
    #[tokio::test]
    async fn dispatch_route_fires_per_venue_place_orders_on_multi_leg_split() {
        let binance = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        binance.set_mid(dec!(50_000));
        let bybit = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        bybit.set_mid(dec!(50_020));
        let dyn_binance: Arc<dyn ExchangeConnector> = binance.clone();
        let dyn_bybit: Arc<dyn ExchangeConnector> = bybit.clone();
        let bundle = ConnectorBundle {
            primary: dyn_binance,
            hedge: None,
            pair: None,
            extra: vec![dyn_bybit],
        };
        let mut engine = make_engine_with_bundle(bundle);
        // Register Bybit on the aggregator with a different
        // taker fee so the router prefers it for the first
        // leg. Binance seed was auto-installed in `new()`.
        let mut bybit_product = sample_product("BTCUSDT");
        bybit_product.taker_fee = dec!(0.00001); // 0.1 bps
        let mut bybit_seed = VenueSeed::new("BTCUSDT", bybit_product, dec!(1));
        bybit_seed.best_bid = dec!(50_019);
        bybit_seed.best_ask = dec!(50_021);
        engine = engine.with_sor_venue(VenueId::Bybit, bybit_seed);
        // Seed Binance's book on the aggregator too.
        let mut binance_product = sample_product("BTCUSDT");
        binance_product.taker_fee = dec!(0.0005); // 5 bps
        let mut binance_seed = VenueSeed::new("BTCUSDT", binance_product, dec!(1));
        binance_seed.best_bid = dec!(49_999);
        binance_seed.best_ask = dec!(50_001);
        engine = engine.with_sor_venue(VenueId::Binance, binance_seed);

        let outcome = engine.dispatch_route(Side::Buy, dec!(2), dec!(1)).await;
        // Both venues each contributed qty=1. The dispatcher
        // fired one place_order per leg.
        assert_eq!(outcome.legs.len(), 2, "expected two legs, got {outcome:?}");
        assert_eq!(binance.place_single_calls(), 1);
        assert_eq!(bybit.place_single_calls(), 1);
        assert!(
            outcome.errors.is_empty(),
            "got errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.total_dispatched_qty, dec!(2));
        assert!(outcome.is_fully_dispatched());
    }

    /// Single-venue `dispatch_route` path: operators running a
    /// single venue still get one live place_order even
    /// though the router had no choice to make. Taker-urgency
    /// leg lands as an IOC through `execute_unwind_slice`.
    /// Target qty is capped by the seeded `max_inventory`
    /// budget on the aggregator — use a qty under the default
    /// 0.1 cap so the router produces a full-target decision.
    #[tokio::test]
    async fn dispatch_route_single_venue_fires_one_place_order() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        let outcome = engine.dispatch_route(Side::Buy, dec!(0.05), dec!(1)).await;
        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.legs.len(), 1);
        assert_eq!(outcome.total_dispatched_qty, dec!(0.05));
        assert_eq!(mock.place_single_calls(), 1);
    }

    /// End-to-end #2: stat-arb driver emits `Entered` → engine
    /// dispatches both legs → both connectors saw place_order;
    /// `Exited` → flatten slice on both connectors.
    #[tokio::test]
    async fn stat_arb_entered_then_exited_drives_real_leg_dispatch() {
        let y_conn = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let x_conn = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        y_conn.set_mid(dec!(200));
        x_conn.set_mid(dec!(100));
        let y_dyn: Arc<dyn ExchangeConnector> = y_conn.clone();
        let x_dyn: Arc<dyn ExchangeConnector> = x_conn.clone();
        // Engine is just a host — primary connector is the y
        // leg so single-bundle tests work.
        let bundle = ConnectorBundle::single(y_dyn.clone());
        let mut engine = make_engine_with_bundle(bundle);

        let pair = StatArbPair {
            y_symbol: "BTCUSDT".to_string(),
            x_symbol: "ETHUSDT".to_string(),
            strategy_class: "stat_arb_BTCUSDT_ETHUSDT".to_string(),
        };
        let cfg = StatArbDriverConfig {
            tick_interval: std::time::Duration::from_millis(10),
            zscore: mm_strategy::stat_arb::ZScoreConfig {
                window: 20,
                entry_threshold: dec!(1.5),
                exit_threshold: dec!(0.3),
            },
            kalman_transition_var: dec!(0.000001),
            kalman_observation_var: dec!(0.001),
            leg_notional_usd: dec!(1000),
        };
        let mut driver = StatArbDriver::new(y_dyn, x_dyn, pair, cfg, Arc::new(NullStatArbSink));
        // Seed cointegration so the z-score path can Enter.
        let x_series: Vec<Decimal> = (0..60)
            .map(|i| dec!(100) + Decimal::from(i as i64 % 5 - 2))
            .collect();
        let y_series: Vec<Decimal> = x_series
            .iter()
            .enumerate()
            .map(|(i, xi)| {
                let jitter = Decimal::from(i as i64 % 3 - 1) / dec!(10);
                dec!(2) * xi + jitter
            })
            .collect();
        driver.recheck_cointegration(&y_series, &x_series);

        // Warmup with steady prices so Z stays small.
        for _ in 0..20 {
            y_conn.set_mid(dec!(200));
            x_conn.set_mid(dec!(100));
            driver.tick_once().await;
        }

        // Shock Y to force Entered.
        y_conn.set_mid(dec!(205));
        let shock = driver.tick_once().await;
        assert!(matches!(shock, StatArbEvent::Entered { .. }));
        let entry_report = driver.try_dispatch_legs_for_entry(&shock).await;
        assert!(!entry_report.is_empty());
        assert!(entry_report.all_succeeded());
        // y_conn should see one, x_conn should see one.
        assert_eq!(y_conn.place_single_calls(), 1);
        assert_eq!(x_conn.place_single_calls(), 1);
        // Route through the audit-writing handler too so we
        // exercise the format_leg_report pathway.
        engine.handle_stat_arb_event(shock, Some(entry_report));

        // Revert Y to force Exit.
        y_conn.set_mid(dec!(200));
        let mut exit_event = None;
        for _ in 0..60 {
            let e = driver.tick_once().await;
            if matches!(e, StatArbEvent::Exited { .. }) {
                exit_event = Some(e);
                break;
            }
        }
        let exit_event = exit_event.expect("expected Exited after revert");
        let exit_report = driver.try_dispatch_legs_for_exit().await;
        assert!(!exit_report.is_empty());
        assert!(exit_report.all_succeeded());
        // Both connectors should now have seen two place_order
        // calls — one entry, one exit.
        assert_eq!(y_conn.place_single_calls(), 2);
        assert_eq!(x_conn.place_single_calls(), 2);
        engine.handle_stat_arb_event(exit_event, Some(exit_report));
    }

    /// `pnl_strategy_class`: returns the stat-arb pair's
    /// strategy_class when the driver is attached and funding
    /// arb is not, otherwise falls back to the primary
    /// strategy name.
    #[tokio::test]
    async fn pnl_strategy_class_discriminates_stat_arb_vs_default() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let engine = make_engine_with_bundle(bundle);
        assert_eq!(engine.pnl_strategy_class(), engine.strategy.name());

        // Attach a stat-arb driver and assert the class flips
        // to the pair's `strategy_class` value.
        let y_conn = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let x_conn = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        let driver = StatArbDriver::new(
            y_conn as Arc<dyn ExchangeConnector>,
            x_conn as Arc<dyn ExchangeConnector>,
            StatArbPair {
                y_symbol: "BTCUSDT".to_string(),
                x_symbol: "ETHUSDT".to_string(),
                strategy_class: "stat_arb_BTCUSDT_ETHUSDT".to_string(),
            },
            StatArbDriverConfig::default(),
            Arc::new(NullStatArbSink),
        );
        let engine = engine.with_stat_arb_driver(driver, std::time::Duration::from_millis(50));
        assert_eq!(engine.pnl_strategy_class(), "stat_arb_BTCUSDT_ETHUSDT");
    }

    // ---------------------------------------------------------
    // Epic D stage-2 — BVC classifier engine wiring
    // ---------------------------------------------------------

    fn make_engine_with_toxicity(cfg: mm_common::config::ToxicityConfig) -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let app_cfg = AppConfig {
            toxicity: cfg,
            ..AppConfig::default()
        };
        MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            app_cfg,
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
    }

    fn synth_trade(price: Decimal, qty: Decimal, side: mm_common::types::Side) -> mm_common::types::Trade {
        mm_common::types::Trade {
            trade_id: 1,
            symbol: "BTCUSDT".to_string(),
            price,
            qty,
            taker_side: side,
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn bvc_disabled_keeps_aggregator_none() {
        let cfg = mm_common::config::ToxicityConfig {
            bvc_enabled: false,
            ..Default::default()
        };
        let engine = make_engine_with_toxicity(cfg);
        assert!(engine.bvc_classifier.is_none());
        assert!(engine.bvc_bar_agg.is_none());
    }

    #[test]
    fn bvc_enabled_constructs_both_components() {
        let cfg = mm_common::config::ToxicityConfig {
            bvc_enabled: true,
            bvc_bar_secs: 1,
            ..Default::default()
        };
        let engine = make_engine_with_toxicity(cfg);
        assert!(engine.bvc_classifier.is_some());
        assert!(engine.bvc_bar_agg.is_some());
    }

    /// Disabled path: the legacy tick-rule feed into VPIN stays
    /// wired. Tiny bucket size + enough one-sided buys registers
    /// at the VPIN level.
    #[test]
    fn bvc_disabled_tick_rule_still_feeds_vpin() {
        let cfg = mm_common::config::ToxicityConfig {
            bvc_enabled: false,
            vpin_bucket_size: dec!(100),
            vpin_num_buckets: 4,
            ..Default::default()
        };
        let mut engine = make_engine_with_toxicity(cfg);
        for _ in 0..20 {
            let t = synth_trade(dec!(100), dec!(2), mm_common::types::Side::Buy);
            engine.handle_ws_event(MarketEvent::Trade {
                venue: VenueId::Binance,
                trade: t,
            });
        }
        let v = engine.vpin.vpin().expect("vpin produced");
        assert!(v > dec!(0), "expected positive vpin, got {v}");
    }

    /// Enabled path: within a single bar window the aggregator
    /// holds the trade (no bar has closed yet), so the VPIN
    /// buckets should remain empty — proves the engine is
    /// NOT calling `vpin.on_trade` on the BVC path.
    #[test]
    fn bvc_enabled_suppresses_legacy_on_trade_within_bar() {
        let cfg = mm_common::config::ToxicityConfig {
            bvc_enabled: true,
            // Long bar — no chance of closing during the test.
            bvc_bar_secs: 3600,
            vpin_bucket_size: dec!(100),
            vpin_num_buckets: 4,
            ..Default::default()
        };
        let mut engine = make_engine_with_toxicity(cfg);
        for _ in 0..20 {
            let t = synth_trade(dec!(100), dec!(2), mm_common::types::Side::Buy);
            engine.handle_ws_event(MarketEvent::Trade {
                venue: VenueId::Binance,
                trade: t,
            });
        }
        assert!(engine.vpin.vpin().is_none(),
            "bvc path should not call on_trade — VPIN must stay empty mid-bar");
        assert!(engine.bvc_bar_agg.is_some());
    }

    // ---------------------------------------------------------
    // Epic A stage-2 #1 — inline SOR dispatch tick
    // ---------------------------------------------------------

    /// No inventory, default config → the tick is a no-op. No
    /// place_order call, no legs, no audit write.
    #[tokio::test]
    async fn sor_tick_inventory_excess_zero_inventory_is_noop() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 0);
    }

    /// Long inventory above threshold → tick fires a SELL
    /// dispatch for the excess.
    #[tokio::test]
    async fn sor_tick_dispatches_sell_when_long_inventory_exceeds_threshold() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        // Seed the aggregator so the router finds a venue.
        let mut seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        seed.best_bid = dec!(49_999);
        seed.best_ask = dec!(50_001);
        engine = engine.with_sor_venue(VenueId::Binance, seed);
        // Simulate a long position above the default threshold.
        engine.config.market_maker.sor_inventory_threshold = dec!(0.01);
        // Urgency > 0.5 forces taker legs so mock.place_single_calls() fires.
        engine.config.market_maker.sor_urgency = dec!(0.9);
        engine.inventory_manager.force_reset_inventory_to(dec!(0.05));

        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 1, "one taker leg expected");
    }

    /// Short inventory (negative) above threshold → BUY dispatch.
    #[tokio::test]
    async fn sor_tick_dispatches_buy_when_short_inventory_exceeds_threshold() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        let mut seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        seed.best_bid = dec!(49_999);
        seed.best_ask = dec!(50_001);
        engine = engine.with_sor_venue(VenueId::Binance, seed);
        engine.config.market_maker.sor_inventory_threshold = dec!(0.01);
        engine.config.market_maker.sor_urgency = dec!(0.9);
        engine.inventory_manager.force_reset_inventory_to(dec!(-0.05));

        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 1);
    }

    /// Inventory at exactly threshold → no-op (strictly above
    /// policy, so a position at the limit doesn't churn).
    #[tokio::test]
    async fn sor_tick_at_threshold_is_noop() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        engine.config.market_maker.sor_inventory_threshold = dec!(0.05);
        engine.inventory_manager.force_reset_inventory_to(dec!(0.05));
        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 0);
    }

    /// HedgeBudget source with an empty basket → no-op.
    #[tokio::test]
    async fn sor_tick_hedge_budget_empty_basket_is_noop() {
        use mm_common::config::SorTargetSource;
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50_000));
        let bundle = ConnectorBundle::single(mock.clone() as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        engine.config.market_maker.sor_target_qty_source = SorTargetSource::HedgeBudget;
        // last_hedge_basket starts empty by default.
        engine.run_sor_dispatch_tick().await;
        assert_eq!(mock.place_single_calls(), 0);
    }

    // ---------------------------------------------------------
    // UX-VENUE-1 gap close — periodic L1 poll for SOR extras
    // ---------------------------------------------------------

    /// A seeded extra-venue connector with a synthetic mid gets
    /// its top-of-book republished onto `DataBus::books_l1`
    /// under the `(venue, symbol, product)` key the
    /// `/api/v1/venues/book_state` endpoint reads.
    #[tokio::test]
    async fn extra_venue_l1_poll_publishes_to_data_bus() {
        // Primary: Binance spot BTCUSDT (auto-seeded by `new()`).
        let binance = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        binance.set_mid(dec!(50_000));
        // Extra: Bybit spot BTCUSDT with a distinct mid so we can
        // tell them apart in the data bus snapshot.
        let bybit = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        bybit.set_mid(dec!(50_020));
        let bundle = ConnectorBundle {
            primary: binance.clone() as Arc<dyn ExchangeConnector>,
            hedge: None,
            pair: None,
            extra: vec![bybit.clone() as Arc<dyn ExchangeConnector>],
        };
        // Engine with a live dashboard so `publish_l1` actually
        // writes into the bus we read afterwards.
        let dash = mm_dashboard::state::DashboardState::new();
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            Some(dash.clone()),
            None,
        );
        // Seed the aggregator for the extra so the poll can map
        // `(venue, product) → symbol` back to the right book.
        let seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        engine = engine.with_sor_venue(VenueId::Bybit, seed);

        engine.poll_extra_venues_for_data_bus().await;

        let key = (
            "bybit".to_string(),
            "BTCUSDT".to_string(),
            mm_common::config::ProductType::Spot,
        );
        let snap = dash
            .data_bus()
            .get_l1(&key)
            .expect("extra venue L1 should be on the bus after poll");
        assert_eq!(snap.bid_px, Some(dec!(50_019)));
        assert_eq!(snap.ask_px, Some(dec!(50_021)));
        assert_eq!(snap.mid, Some(dec!(50_020)));
        assert!(snap.ts.is_some(), "timestamp should be stamped");
    }

    /// A venue that's in `ConnectorBundle.extra` but NOT seeded
    /// on the aggregator is skipped rather than inventing a
    /// symbol from thin air.
    #[tokio::test]
    async fn extra_venue_l1_poll_skips_unseeded_extras() {
        let binance = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        binance.set_mid(dec!(50_000));
        let bybit = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        bybit.set_mid(dec!(50_020));
        let bundle = ConnectorBundle {
            primary: binance as Arc<dyn ExchangeConnector>,
            hedge: None,
            pair: None,
            extra: vec![bybit as Arc<dyn ExchangeConnector>],
        };
        let dash = mm_dashboard::state::DashboardState::new();
        let engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            Some(dash.clone()),
            None,
        );
        // Deliberately do NOT call `with_sor_venue(VenueId::Bybit, …)`.
        engine.poll_extra_venues_for_data_bus().await;

        let bybit_key = (
            "bybit".to_string(),
            "BTCUSDT".to_string(),
            mm_common::config::ProductType::Spot,
        );
        assert!(
            dash.data_bus().get_l1(&bybit_key).is_none(),
            "unseeded extra should not produce a bus entry"
        );
    }

    // ---------------------------------------------------------
    // UX-VENUE-2 — per-venue regime classifier
    // ---------------------------------------------------------

    /// Two venue L1 streams with diverging mid behaviour
    /// produce two regime entries on the bus keyed by
    /// `(venue, symbol, product)`. The classifier bootstraps
    /// entries from the bus and publishes a label per stream.
    #[test]
    fn classify_venue_regimes_publishes_one_label_per_venue_stream() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(mock as Arc<dyn ExchangeConnector>);
        let dash = mm_dashboard::state::DashboardState::new();
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            Some(dash.clone()),
            None,
        );

        let binance_key = (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            mm_common::config::ProductType::Spot,
        );
        let bybit_key = (
            "bybit".to_string(),
            "BTCUSDT".to_string(),
            mm_common::config::ProductType::Spot,
        );

        // Seed two distinct streams on the bus.
        let bus = dash.data_bus();
        for (i, mid) in [dec!(50_000), dec!(50_010), dec!(50_020)].iter().enumerate() {
            bus.publish_l1(
                binance_key.clone(),
                mm_dashboard::data_bus::BookL1Snapshot {
                    bid_px: Some(*mid - dec!(1)),
                    ask_px: Some(*mid + dec!(1)),
                    mid: Some(*mid),
                    spread_bps: Some(dec!(0.4)),
                    ts: Some(chrono::Utc::now()),
                },
            );
            bus.publish_l1(
                bybit_key.clone(),
                mm_dashboard::data_bus::BookL1Snapshot {
                    bid_px: Some(*mid - dec!(2)),
                    ask_px: Some(*mid + dec!(2)),
                    mid: Some(*mid + Decimal::from(i as u64)),
                    spread_bps: Some(dec!(0.8)),
                    ts: Some(chrono::Utc::now()),
                },
            );
            engine.classify_venue_regimes_tick();
        }

        let binance_reg = bus.get_regime(&binance_key).expect("binance regime");
        let bybit_reg = bus.get_regime(&bybit_key).expect("bybit regime");
        // Default RegimeDetector returns `Quiet` until the
        // window is half-full; that's fine — we only assert
        // that both streams got a label and the snapshot
        // timestamp is stamped.
        assert!(!binance_reg.label.is_empty());
        assert!(!bybit_reg.label.is_empty());
        assert!(binance_reg.ts.is_some());
        assert!(bybit_reg.ts.is_some());

        // A stream for a DIFFERENT symbol must not be
        // classified by this engine (multi-engine scoping).
        let other_key = (
            "binance".to_string(),
            "ETHUSDT".to_string(),
            mm_common::config::ProductType::Spot,
        );
        bus.publish_l1(
            other_key.clone(),
            mm_dashboard::data_bus::BookL1Snapshot {
                bid_px: Some(dec!(3_000)),
                ask_px: Some(dec!(3_001)),
                mid: Some(dec!(3_000.5)),
                spread_bps: Some(dec!(1)),
                ts: Some(chrono::Utc::now()),
            },
        );
        engine.classify_venue_regimes_tick();
        assert!(
            bus.get_regime(&other_key).is_none(),
            "a stream for a different symbol must not be classified by this engine"
        );
    }

    /// Feeding identical mids between ticks must not advance
    /// the detector — otherwise a venue whose feed has stalled
    /// (extras polled every 5 s, classifier ticking every 2 s)
    /// would be biased toward Quiet by a stream of zero returns.
    #[test]
    fn classify_venue_regimes_ignores_unchanged_mid() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(mock as Arc<dyn ExchangeConnector>);
        let dash = mm_dashboard::state::DashboardState::new();
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            Some(dash.clone()),
            None,
        );

        let key = (
            "binance".to_string(),
            "BTCUSDT".to_string(),
            mm_common::config::ProductType::Spot,
        );
        let snap = mm_dashboard::data_bus::BookL1Snapshot {
            bid_px: Some(dec!(49_999)),
            ask_px: Some(dec!(50_001)),
            mid: Some(dec!(50_000)),
            spread_bps: Some(dec!(0.4)),
            ts: Some(chrono::Utc::now()),
        };
        for _ in 0..5 {
            dash.data_bus().publish_l1(key.clone(), snap.clone());
            engine.classify_venue_regimes_tick();
        }

        // No return samples fed into the detector because the
        // mid never changed → per-key `last_mid` is set but the
        // detector's internal returns ring stayed empty. We
        // can't peek into the detector, but we *can* check the
        // classifier cached the last mid once (the entry exists).
        assert_eq!(engine.venue_regime_classifiers.len(), 1);
        let slot = engine.venue_regime_classifiers.get(&key).unwrap();
        assert_eq!(slot.1, Some(dec!(50_000)));
    }

    // ---------------------------------------------------------
    // Epic A stage-2 #2 — trade-rate → queue-wait refresh
    // ---------------------------------------------------------

    /// A trade event seeds the per-venue estimator.
    #[test]
    fn sor_trade_event_feeds_rate_estimator() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(mock as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        assert_eq!(engine.sor_trade_rates.len(), 0);
        let t = synth_trade(dec!(100), dec!(2), mm_common::types::Side::Buy);
        engine.handle_ws_event(MarketEvent::Trade {
            venue: VenueId::Binance,
            trade: t,
        });
        assert_eq!(engine.sor_trade_rates.len(), 1);
        let est = engine.sor_trade_rates.get(&VenueId::Binance).unwrap();
        assert_eq!(est.sample_count(), 1);
        assert_eq!(est.total_qty(), dec!(2));
    }

    /// refresh_sor_queue_wait leaves seeded value in place when
    /// the estimator hasn't reached MIN_SAMPLES yet.
    #[test]
    fn sor_queue_refresh_keeps_seed_before_min_samples() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(mock as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        // Seed Binance on the aggregator with a distinctive
        // queue_wait so we can detect whether it was overwritten.
        let mut seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        seed.best_bid = dec!(49_999);
        seed.best_ask = dec!(50_001);
        seed.queue_wait_secs = dec!(123);
        engine = engine.with_sor_venue(VenueId::Binance, seed);

        // One trade — far below MIN_SAMPLES = 5.
        let t = synth_trade(dec!(100), dec!(1), mm_common::types::Side::Buy);
        engine.handle_ws_event(MarketEvent::Trade {
            venue: VenueId::Binance,
            trade: t,
        });
        engine.refresh_sor_queue_wait();
        let seed_after = engine.sor_aggregator.seed(VenueId::Binance).unwrap();
        assert_eq!(seed_after.queue_wait_secs, dec!(123),
            "seed must not be overwritten before MIN_SAMPLES");
    }

    /// Enough trades → refresh_sor_queue_wait publishes a fresh
    /// (non-seeded) queue_wait derived from the estimator.
    #[test]
    fn sor_queue_refresh_publishes_rate_derived_wait_after_min_samples() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(mock as Arc<dyn ExchangeConnector>);
        let mut engine = make_engine_with_bundle(bundle);
        let mut seed = VenueSeed::new("BTCUSDT", sample_product("BTCUSDT"), dec!(1));
        seed.best_bid = dec!(49_999);
        seed.best_ask = dec!(50_001);
        seed.queue_wait_secs = dec!(999); // Clearly-wrong seed.
        engine = engine.with_sor_venue(VenueId::Binance, seed);
        // Stream 10 trades so the estimator clears its MIN_SAMPLES
        // threshold (5).
        for _ in 0..10 {
            let t = synth_trade(dec!(100), dec!(1), mm_common::types::Side::Buy);
            engine.handle_ws_event(MarketEvent::Trade {
                venue: VenueId::Binance,
                trade: t,
            });
        }
        engine.refresh_sor_queue_wait();
        let seed_after = engine.sor_aggregator.seed(VenueId::Binance).unwrap();
        assert_ne!(seed_after.queue_wait_secs, dec!(999),
            "refresh must replace the seeded value once enough trades arrive");
        assert!(seed_after.queue_wait_secs > dec!(0));
    }
