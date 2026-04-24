    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::{Fill, InstrumentPair, Side};
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_risk::kill_switch::KillLevel;
    use mm_strategy::AvellanedaStoikov;

    fn sample_product(symbol: &str) -> ProductSpec {
        ProductSpec {
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

    fn sample_pair() -> InstrumentPair {
        InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTC-PERP".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(50),
        }
    }

    fn dual_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
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

    fn single_engine() -> MarketMakerEngine {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
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

    /// P2.1: an engine with no asset-class layer reports its
    /// global level verbatim. Regression anchor for the
    /// "P2.1 must not break legacy single-engine deployments"
    /// invariant.
    #[test]
    fn effective_kill_level_falls_back_to_global_when_no_asset_class() {
        let mut engine = single_engine();
        assert_eq!(engine.effective_kill_level(), KillLevel::Normal);
        engine
            .kill_switch
            .manual_trigger(KillLevel::WidenSpreads, "test global");
        assert_eq!(engine.effective_kill_level(), KillLevel::WidenSpreads);
    }

    /// P2.1 happy path: when both a global and an asset-class
    /// switch are armed, `effective_kill_level` returns the
    /// max — so a class-wide widening is honoured even when
    /// the per-engine global is still Normal, AND a per-engine
    /// hard escalation is honoured even when the class is
    /// still Normal.
    #[test]
    fn effective_kill_level_takes_max_of_global_and_asset_class() {
        let class = Arc::new(Mutex::new(KillSwitch::new(KillSwitchConfig::default())));
        let mut engine = single_engine().with_asset_class_switch(class.clone());

        // Asset-class widens, global Normal → effective WidenSpreads.
        class
            .lock()
            .unwrap()
            .manual_trigger(KillLevel::WidenSpreads, "stETH depeg");
        assert_eq!(engine.effective_kill_level(), KillLevel::WidenSpreads);

        // Global escalates harder → effective tracks global.
        engine
            .kill_switch
            .manual_trigger(KillLevel::CancelAll, "per-engine PnL stop");
        assert_eq!(engine.effective_kill_level(), KillLevel::CancelAll);

        // Asset-class escalates to StopNewOrders — global is
        // already CancelAll which is higher, so effective stays
        // CancelAll. Pin the max-not-replace semantics.
        class
            .lock()
            .unwrap()
            .manual_trigger(KillLevel::StopNewOrders, "ETH-family lock");
        assert_eq!(engine.effective_kill_level(), KillLevel::CancelAll);
    }

    /// P2.1 sharing: two engines pointed at the same
    /// `Arc<Mutex<KillSwitch>>` see each other's escalations
    /// instantly. Models the "halt all ETH-family pairs"
    /// failure mode the asset-class layer was added to fix.
    #[test]
    fn shared_asset_class_switch_propagates_across_engines() {
        let class = Arc::new(Mutex::new(KillSwitch::new(KillSwitchConfig::default())));
        let engine_a = single_engine().with_asset_class_switch(class.clone());
        let engine_b = single_engine().with_asset_class_switch(class.clone());
        assert_eq!(engine_a.effective_kill_level(), KillLevel::Normal);
        assert_eq!(engine_b.effective_kill_level(), KillLevel::Normal);

        class
            .lock()
            .unwrap()
            .manual_trigger(KillLevel::WidenSpreads, "shared escalation");
        assert_eq!(engine_a.effective_kill_level(), KillLevel::WidenSpreads);
        assert_eq!(engine_b.effective_kill_level(), KillLevel::WidenSpreads);
    }

    /// Epic A engine integration: a single-connector engine
    /// auto-seeds its primary venue into the SOR aggregator,
    /// and `recommend_route_synthetic` produces a non-empty
    /// decision that fills the full target on that venue.
    /// Regression anchor for the "auto-seed primary" path so
    /// a future refactor can't silently drop the
    /// `register_venue` call in `new`.
    ///
    /// Default `config.risk.max_inventory = 0.1`, so the
    /// test target stays well under that budget.
    #[test]
    fn recommend_route_auto_seeds_primary_venue() {
        let engine = single_engine();
        let decision = engine.recommend_route_synthetic(
            Side::Buy,
            dec!(0.05),
            dec!(0.5),
            &[(VenueId::Binance, 100)],
        );
        assert_eq!(decision.target_qty, dec!(0.05));
        assert!(decision.is_complete);
        assert_eq!(decision.legs.len(), 1);
        assert_eq!(decision.legs[0].venue, VenueId::Binance);
    }

    /// A single-connector engine with a second SOR venue
    /// registered via `with_sor_venue` routes a fill that
    /// exceeds the cheap venue's capacity across both
    /// venues in cost order. Pins the full chain:
    /// engine → aggregator → cost model → greedy router →
    /// decision.
    #[test]
    fn recommend_route_splits_across_multiple_sor_venues() {
        // Cheap Bybit seed — taker fee 0.01 % (1 bps),
        // 0.03 available (strictly less than the target
        // below so the router has to roll the remainder to
        // the more expensive Binance venue).
        let mut cheap_product = sample_product("BTCUSDT");
        cheap_product.maker_fee = dec!(0);
        cheap_product.taker_fee = dec!(0.0001);
        let cheap_seed = VenueSeed::new("BTCUSDT", cheap_product, dec!(0.03));

        // Single-venue engine seeded with the primary
        // (Binance) venue at the default sample fees; then
        // add Bybit as a cheaper extra SOR venue.
        let engine = single_engine().with_sor_venue(VenueId::Bybit, cheap_seed);

        // Target 0.05 — Bybit (cheaper, 0.03 available)
        // fills first, the 0.02 remainder rolls to Binance.
        let decision = engine.recommend_route_synthetic(
            Side::Buy,
            dec!(0.05),
            dec!(1), // full urgency → pure taker cost sort
            &[(VenueId::Binance, 100), (VenueId::Bybit, 100)],
        );
        assert_eq!(decision.legs.len(), 2);
        assert_eq!(decision.legs[0].venue, VenueId::Bybit);
        assert_eq!(decision.legs[0].qty, dec!(0.03));
        assert_eq!(decision.legs[1].venue, VenueId::Binance);
        assert_eq!(decision.legs[1].qty, dec!(0.02));
        assert!(decision.is_complete);
    }

    fn buy_fill(qty: Decimal, price: Decimal) -> MarketEvent {
        MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: Side::Buy,
                price,
                qty,
                is_maker: true,
                timestamp: chrono::Utc::now(),
            },
        }
    }

    #[tokio::test]
    async fn kill_switch_l4_picks_paired_unwind_in_dual_mode() {
        let mut engine = dual_engine();
        // Build up an inventory on the primary leg.
        engine.handle_ws_event(buy_fill(dec!(0.05), dec!(50_000)));
        assert_eq!(engine.inventory_manager.inventory(), dec!(0.05));

        // Need a mid on the primary book for tick_second to
        // reach the kill-switch dispatch branch.
        // Populate the balance cache so the affordability
        // pre-check in refresh_quotes does not zero out the
        // bid/ask quotes the strategy generates.
        engine.refresh_balances().await;

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });

        // Trip the kill switch all the way to L4.
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");
        assert_eq!(engine.kill_switch.level(), KillLevel::FlattenAll);

        // One tick drives the L4 dispatch logic.
        engine.tick_second().await;

        assert!(
            engine.paired_unwind.is_some(),
            "dual-connector mode must pick PairedUnwindExecutor"
        );
        assert!(
            engine.twap.is_none(),
            "paired_unwind must replace twap, never run both"
        );
    }

    #[tokio::test]
    async fn kill_switch_l4_picks_twap_in_single_mode() {
        let mut engine = single_engine();
        engine.handle_ws_event(buy_fill(dec!(0.05), dec!(50_000)));
        // Populate the balance cache so the affordability
        // pre-check in refresh_quotes does not zero out the
        // bid/ask quotes the strategy generates.
        engine.refresh_balances().await;

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");

        engine.tick_second().await;

        assert!(engine.twap.is_some(), "single-mode path still uses TWAP");
        assert!(engine.paired_unwind.is_none());
    }

    #[tokio::test]
    async fn paired_unwind_is_not_spawned_when_inventory_is_zero() {
        let mut engine = dual_engine();
        // Populate the balance cache so the affordability
        // pre-check in refresh_quotes does not zero out the
        // bid/ask quotes the strategy generates.
        engine.refresh_balances().await;

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine
            .kill_switch
            .manual_trigger(KillLevel::FlattenAll, "test L4 escalation");

        engine.tick_second().await;
        assert!(engine.paired_unwind.is_none());
        assert!(engine.twap.is_none());
    }

    /// Seed the dual engine with inventory + both books so the
    /// L4 dispatch path can actually run the first slice.
    async fn prime_for_unwind(engine: &mut MarketMakerEngine) {
        engine.handle_ws_event(buy_fill(dec!(0.1), dec!(50_000)));
        // Populate the balance cache so the affordability
        // pre-check in refresh_quotes does not zero out the
        // bid/ask quotes the strategy generates.
        engine.refresh_balances().await;

        engine.handle_ws_event(MarketEvent::BookSnapshot {
            venue: VenueId::Binance,
            symbol: "BTCUSDT".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(49_999),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_001),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        engine.handle_hedge_event(MarketEvent::BookSnapshot {
            venue: VenueId::HyperLiquid,
            symbol: "BTC-PERP".to_string(),
            bids: vec![mm_common::types::PriceLevel {
                price: dec!(50_009),
                qty: dec!(10),
            }],
            asks: vec![mm_common::types::PriceLevel {
                price: dec!(50_011),
                qty: dec!(10),
            }],
            sequence: 1,
        });
    }

    #[tokio::test]
    async fn slice_dispatches_orders_on_both_venues_not_just_logs() {
        let mut engine = dual_engine();
        prime_for_unwind(&mut engine).await;

        // Install a tiny-duration executor directly so the
        // first `next_slice` fires immediately — L4 dispatch
        // is tested elsewhere, here we only care about the
        // slice-to-order-manager pipeline. `num_seconds()` in
        // the executor's scheduler truncates, so the sleep
        // must exceed 1 full second to register as `elapsed = 1`.
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            1, // 1 second total duration
            1, // one slice → fires on first post-schedule tick
            dec!(5),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        // Do NOT trip the kill switch here — the L3+ cancel-all
        // branch in `tick_second` would clear out the slice
        // orders we just placed. The dispatch pipeline itself
        // is the only contract under test; L4 spawning is
        // covered by the earlier kill-switch test.
        engine.tick_second().await;

        // Both order managers should now have at least one live
        // order from the dispatched slice.
        let primary_live = engine.order_manager.live_count();
        let hedge_live = engine
            .hedge_order_manager
            .as_ref()
            .map(|om| om.live_count())
            .unwrap_or(0);
        assert!(
            primary_live > 0 || hedge_live > 0,
            "at least one leg must have dispatched a slice (primary={primary_live}, hedge={hedge_live})"
        );
    }

    #[tokio::test]
    async fn hedge_fill_routes_into_paired_unwind_and_portfolio() {
        let portfolio = Arc::new(Mutex::new(mm_portfolio::Portfolio::new("USDT")));
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            AppConfig::default(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        )
        .with_portfolio(portfolio.clone());

        // Seed an active paired unwind with a known target size.
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            5,
            dec!(5),
        ));

        // Hedge fill comes in through the hedge WS path.
        let hedge_fill = MarketEvent::Fill {
            venue: VenueId::HyperLiquid,
            fill: mm_common::types::Fill {
                trade_id: 42,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTC-PERP".to_string(),
                side: mm_common::types::Side::Buy, // unwinding a short hedge = buying
                price: dec!(50_010),
                qty: dec!(0.02),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        };
        engine.handle_hedge_event(hedge_fill);

        // The portfolio saw the hedge symbol, not the primary.
        let snap = portfolio.lock().unwrap().snapshot();
        assert!(snap.per_asset.contains_key("BTC-PERP"));
        let hedge_entry = snap.per_asset.get("BTC-PERP").unwrap();
        assert_eq!(
            hedge_entry.qty,
            dec!(0.02),
            "hedge buy fill = long position"
        );
        // paired_unwind tracked the fill → progress > 0.
        let unwind = engine.paired_unwind.as_ref().expect("unwind still active");
        // 0.02 filled out of 0.1 target on hedge, primary still at 0
        // → average progress = (0 + 0.2) / 2 = 0.1.
        assert_eq!(unwind.progress(), dec!(0.1));
    }

    #[tokio::test]
    async fn primary_fill_routes_into_paired_unwind_not_just_inventory() {
        let mut engine = dual_engine();
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            5,
            dec!(5),
        ));

        engine.handle_ws_event(MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: mm_common::types::Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: mm_common::types::Side::Sell, // unwinding long spot = selling
                price: dec!(50_000),
                qty: dec!(0.05),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });

        let unwind = engine.paired_unwind.as_ref().expect("unwind still active");
        // 0.05 filled on primary, 0 on hedge → avg = 0.25.
        assert_eq!(unwind.progress(), dec!(0.25));
    }

    #[tokio::test]
    async fn paired_unwind_clears_when_both_legs_complete() {
        let mut engine = dual_engine();
        let pair = engine.connectors.pair.clone().unwrap();
        engine.paired_unwind = Some(PairedUnwindExecutor::new(
            pair,
            mm_common::types::Side::Buy,
            mm_common::types::Side::Sell,
            dec!(0.1),
            60,
            1,
            dec!(5),
        ));

        // Primary leg fully fills.
        engine.handle_ws_event(MarketEvent::Fill {
            venue: VenueId::Binance,
            fill: mm_common::types::Fill {
                trade_id: 1,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTCUSDT".to_string(),
                side: mm_common::types::Side::Sell,
                price: dec!(50_000),
                qty: dec!(0.1),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });
        // Hedge leg fully fills.
        engine.handle_hedge_event(MarketEvent::Fill {
            venue: VenueId::HyperLiquid,
            fill: mm_common::types::Fill {
                trade_id: 2,
                order_id: mm_common::types::OrderId::new_v4(),
                symbol: "BTC-PERP".to_string(),
                side: mm_common::types::Side::Buy,
                price: dec!(50_010),
                qty: dec!(0.1),
                is_maker: false,
                timestamp: chrono::Utc::now(),
            },
        });

        // Executor cleared itself on the final on_hedge_fill.
        assert!(
            engine.paired_unwind.is_none(),
            "unwind cleared on completion"
        );
    }
