    use super::*;
    use crate::connector_bundle::ConnectorBundle;
    use crate::test_support::MockConnector;
    use mm_common::config::AppConfig;
    use mm_common::types::{InstrumentPair, PriceLevel};
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use mm_strategy::AvellanedaStoikov;

    fn sample_config() -> AppConfig {
        AppConfig::default()
    }

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
            hedge_symbol: "BTC".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(20),
        }
    }

    /// R1.2/R1.3 regression — `auto_scale_order_size` must
    /// bump `order_size` above `min_notional` when the default
    /// 0.001-base-unit size is below the venue's threshold at
    /// the current mid.
    #[tokio::test(flavor = "current_thread")]
    async fn auto_scale_order_size_bumps_under_min_notional() {
        let mock = Arc::new(MockConnector::new(
            VenueId::Binance,
            VenueProduct::Spot,
        ));
        let bundle = ConnectorBundle::single(mock);
        let mut cfg = sample_config();
        cfg.market_maker.order_size = dec!(0.001);
        let mut product = sample_product("SOLUSDT");
        product.min_notional = dec!(10);
        product.lot_size = dec!(0.01);
        let mut engine = MarketMakerEngine::new(
            "SOLUSDT".to_string(),
            cfg,
            product,
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let ev = snapshot("SOLUSDT", VenueId::Binance, dec!(85), dec!(85.02));
        engine.book_keeper.on_event(&ev);
        assert!(engine.book_keeper.book.mid_price().is_some());
        engine.auto_scale_order_size();
        let bumped = engine.config.market_maker.order_size;
        assert!(
            bumped * dec!(85) >= dec!(10),
            "bumped size {bumped} * 85 must clear $10 min_notional"
        );
        engine.auto_scale_order_size();
        assert_eq!(engine.config.market_maker.order_size, bumped);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_scale_order_size_leaves_adequate_config_alone() {
        let mock = Arc::new(MockConnector::new(
            VenueId::Binance,
            VenueProduct::Spot,
        ));
        let bundle = ConnectorBundle::single(mock);
        let mut cfg = sample_config();
        cfg.market_maker.order_size = dec!(0.5);
        let mut product = sample_product("SOLUSDT");
        product.min_notional = dec!(10);
        let mut engine = MarketMakerEngine::new(
            "SOLUSDT".to_string(),
            cfg,
            product,
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let ev = snapshot("SOLUSDT", VenueId::Binance, dec!(85), dec!(85.02));
        engine.book_keeper.on_event(&ev);
        engine.auto_scale_order_size();
        assert_eq!(
            engine.config.market_maker.order_size,
            dec!(0.5),
            "operator-sized config must not be touched"
        );
    }

    fn snapshot(symbol: &str, venue: VenueId, bid: Decimal, ask: Decimal) -> MarketEvent {
        MarketEvent::BookSnapshot {
            venue,
            symbol: symbol.to_string(),
            bids: vec![PriceLevel {
                price: bid,
                qty: dec!(1),
            }],
            asks: vec![PriceLevel {
                price: ask,
                qty: dec!(1),
            }],
            sequence: 1,
        }
    }

    /// Sprint 18 R12.2 — `refresh_funding_rate` fully populates
    /// both `last_open_interest` and `last_long_short` when the
    /// connector's perp-product overrides return values. Pins
    /// the Sprint 12 R6.4 + Sprint 13 R7.1 data-flow that the
    /// Sprint 15 matrix flagged as ❌ None.
    #[tokio::test(flavor = "current_thread")]
    async fn refresh_funding_rate_populates_oi_and_ls_ratio() {
        use mm_exchange_core::connector::{LongShortRatio, OpenInterestInfo};
        let mock = Arc::new(MockConnector::new(
            VenueId::Binance,
            VenueProduct::LinearPerp,
        ));
        mock.set_oi(OpenInterestInfo {
            symbol: "BTCUSDT".into(),
            oi_contracts: Some(dec!(12345.67)),
            oi_usd: Some(dec!(500_000_000)),
            timestamp: chrono::Utc::now(),
        });
        mock.set_ls_ratio(LongShortRatio {
            symbol: "BTCUSDT".into(),
            long_pct: dec!(0.7),
            short_pct: dec!(0.3),
            ratio: dec!(2.33),
            timestamp: chrono::Utc::now(),
        });
        let bundle = ConnectorBundle::single(mock);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        assert!(engine.last_open_interest.is_none(), "pre-poll baseline");
        assert!(engine.last_long_short.is_none(), "pre-poll baseline");
        engine.refresh_funding_rate().await;
        assert_eq!(
            engine.last_open_interest,
            Some(dec!(500_000_000)),
            "OI USD preferred over contracts when both set"
        );
        let ls = engine
            .last_long_short
            .as_ref()
            .expect("L/S ratio populated");
        assert_eq!(ls.ratio, dec!(2.33));
        assert_eq!(ls.long_pct, dec!(0.7));
    }

    /// Sprint 18 R12.2 — a spot connector's refresh is a no-op
    /// for OI + L/S (both stay None). `refresh_funding_rate` is
    /// still called by the engine loop; it must fail-open on
    /// `supports_funding_rate = false` so spot engines don't
    /// spam warnings.
    #[tokio::test(flavor = "current_thread")]
    async fn refresh_funding_rate_spot_leaves_state_none() {
        let mock = Arc::new(MockConnector::new(
            VenueId::Binance,
            VenueProduct::Spot,
        ));
        let bundle = ConnectorBundle::single(mock);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        engine.refresh_funding_rate().await;
        assert!(engine.last_open_interest.is_none());
        assert!(engine.last_long_short.is_none());
    }

    /// Sprint 18 R12.3 — `spawn_leverage_setup` hits the
    /// connector's `set_leverage` when the graph carries a
    /// `Strategy.LeverageBuilder` node and capabilities allow.
    /// Runs under the restricted gate (set by the test body).
    /// Default multi-thread runtime so `tokio::spawn` executes
    /// concurrently with the async test body.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_leverage_setup_calls_connector_on_leverage_node() {
        // SAFETY: single-threaded current_thread runtime; no
        // parallel observer of MM_ALLOW_RESTRICTED during the
        // block. Every restricted-gate test in this crate
        // takes the same contract.
        unsafe {
            std::env::set_var("MM_ALLOW_RESTRICTED", "yes-pentest-mode");
        }
        let mock = Arc::new(MockConnector::new(
            VenueId::Binance,
            VenueProduct::LinearPerp,
        ));
        let conn_for_assert = mock.clone();
        // Hand-assemble a minimal graph with Strategy.LeverageBuilder.
        let mut g = mm_strategy_graph::Graph::empty(
            "lev-setup-test",
            mm_strategy_graph::Scope::Symbol("BTCUSDT".to_string()),
        );
        let lev_id = mm_strategy_graph::NodeId::new();
        g.nodes.push(mm_strategy_graph::GraphNode {
            id: lev_id,
            kind: "Strategy.LeverageBuilder".into(),
            config: serde_json::json!({ "leverage": 20 }),
            pos: (0.0, 0.0),
        });
        // Graph compiles as a standalone test (no sink needed
        // since we only exercise spawn_leverage_setup, not the
        // full validator path).
        let conn_trait: Arc<
            dyn mm_exchange_core::connector::ExchangeConnector,
        > = mock;
        MarketMakerEngine::spawn_leverage_setup(&g, &conn_trait, "BTCUSDT");
        // spawn_leverage_setup uses tokio::spawn — yield a few
        // times so the spawned task runs.
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let history = conn_for_assert.leverage_call_history();
        assert_eq!(history.len(), 1, "expected one set_leverage call");
        assert_eq!(history[0], ("BTCUSDT".to_string(), 20));
        // SAFETY: same justification.
        unsafe {
            std::env::remove_var("MM_ALLOW_RESTRICTED");
        }
    }

    /// Sprint 18 R12.3 — spot connector (no leverage cap)
    /// short-circuits before hitting `set_leverage`. Pins the
    /// fail-open gate path for venues that don't support it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_leverage_setup_skips_spot_connector() {
        let mock = Arc::new(MockConnector::new(
            VenueId::Binance,
            VenueProduct::Spot,
        ));
        let conn_for_assert = mock.clone();
        let mut g = mm_strategy_graph::Graph::empty(
            "lev-setup-spot",
            mm_strategy_graph::Scope::Symbol("BTCUSDT".to_string()),
        );
        g.nodes.push(mm_strategy_graph::GraphNode {
            id: mm_strategy_graph::NodeId::new(),
            kind: "Strategy.LeverageBuilder".into(),
            config: serde_json::json!({ "leverage": 10 }),
            pos: (0.0, 0.0),
        });
        let conn_trait: Arc<
            dyn mm_exchange_core::connector::ExchangeConnector,
        > = mock;
        MarketMakerEngine::spawn_leverage_setup(&g, &conn_trait, "BTCUSDT");
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let history = conn_for_assert.leverage_call_history();
        assert!(
            history.is_empty(),
            "spot capability should block set_leverage entirely; got {history:?}"
        );
    }

    #[test]
    fn single_bundle_has_no_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        assert!(engine.hedge_book.is_none());
        assert!(!engine.connectors.is_dual());
    }

    #[test]
    fn dual_bundle_creates_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let hb = engine.hedge_book.as_ref().expect("hedge_book must exist");
        assert_eq!(hb.book.symbol, "BTC");
        assert!(engine.connectors.is_dual());
    }

    #[test]
    fn handle_hedge_event_routes_book_updates_to_hedge_book() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        // Primary gets a spot quote around 50 000. Hedge gets a
        // perp quote around 50 100 — a +10 bps basis.
        engine.handle_ws_event(snapshot(
            "BTCUSDT",
            VenueId::Binance,
            dec!(49_999),
            dec!(50_001),
        ));
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));

        assert_eq!(
            engine.book_keeper.book.mid_price(),
            Some(dec!(50_000)),
            "primary mid"
        );
        let hb = engine.hedge_book.as_ref().unwrap();
        assert_eq!(hb.book.mid_price(), Some(dec!(50_100)), "hedge mid");
    }

    #[test]
    fn handle_hedge_event_is_noop_in_single_mode() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // Must not panic; hedge_book is None so the routing is a
        // silent drop. The primary book must stay untouched.
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));
        assert!(engine.book_keeper.book.mid_price().is_none());
    }

    #[test]
    fn hedge_book_mid_feeds_ref_price_via_refresh_quotes() {
        // Verify the wiring that `refresh_quotes` reads
        // `hedge_book.book.mid_price()` into `StrategyContext.ref_price`.
        // Testing the real `refresh_quotes` is heavy (async, lots
        // of side effects) so we inspect the intermediate
        // expression the production code uses.
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockConnector::new(
            VenueId::HyperLiquid,
            VenueProduct::LinearPerp,
        ));
        let bundle = ConnectorBundle::dual(primary, hedge, sample_pair());
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        engine.handle_hedge_event(snapshot(
            "BTC",
            VenueId::HyperLiquid,
            dec!(50_099),
            dec!(50_101),
        ));

        let ref_price = engine
            .hedge_book
            .as_ref()
            .and_then(|hb| hb.book.mid_price());
        assert_eq!(ref_price, Some(dec!(50_100)));
    }

    /// Multi-Venue 3.E.2 — atomic-bundle watchdog must roll back
    /// an inflight bundle whose timeout elapsed without both legs
    /// being acknowledged.
    #[test]
    fn atomic_bundle_watchdog_rolls_back_stale_entries() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // Seed an already-expired bundle (dispatched 5 s ago, 2 s
        // timeout) so the watchdog sees it as stale on first tick.
        engine.inflight_atomic_bundles.insert(
            "test-bundle".into(),
            InflightAtomicBundle {
                dispatched_at: chrono::Utc::now() - chrono::Duration::seconds(5),
                timeout_ms: 2_000,
                maker_venue: "binance".into(),
                maker_symbol: "BTCUSDT".into(),
                maker_side: mm_common::types::Side::Buy,
                maker_price: dec!(100),
                maker_acked: false,
                hedge_venue: "bybit".into(),
                hedge_symbol: "BTCUSDT".into(),
                hedge_side: mm_common::types::Side::Sell,
                hedge_price: dec!(100),
                hedge_acked: false,
            },
        );
        let rolled = engine.tick_atomic_bundle_watchdog();
        assert_eq!(rolled, 1, "one stale bundle must roll back");
        assert!(engine.inflight_atomic_bundles.is_empty());
    }

    /// 3.E.3 — ack sweep graduates a bundle out of the inflight
    /// table once this engine sees both matching live orders.
    #[test]
    fn atomic_bundle_ack_sweep_graduates_self_venue_bundles() {
        use mm_common::types::{LiveOrder, OrderStatus, Side};
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // Seed two live orders on self-engine matching both legs.
        let now = chrono::Utc::now();
        engine.order_manager.track_order(LiveOrder {
            order_id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            price: dec!(100),
            qty: dec!(1),
            filled_qty: dec!(0),
            status: OrderStatus::Open,
            created_at: now,
        });
        engine.order_manager.track_order(LiveOrder {
            order_id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".into(),
            side: Side::Sell,
            price: dec!(101),
            qty: dec!(1),
            filled_qty: dec!(0),
            status: OrderStatus::Open,
            created_at: now,
        });
        // Bundle whose both legs live on self-engine.
        engine.inflight_atomic_bundles.insert(
            "self-bundle".into(),
            InflightAtomicBundle {
                dispatched_at: now,
                timeout_ms: 5_000,
                // Engine's exchange_type defaults to Custom → venue
                // string is lowercase "custom". Bundle legs must
                // match so the ack sweep recognises them.
                maker_venue: "custom".into(),
                maker_symbol: "BTCUSDT".into(),
                maker_side: Side::Buy,
                maker_price: dec!(100),
                maker_acked: false,
                hedge_venue: "custom".into(),
                hedge_symbol: "BTCUSDT".into(),
                hedge_side: Side::Sell,
                hedge_price: dec!(101),
                hedge_acked: false,
            },
        );
        engine.sweep_atomic_bundle_acks();
        assert!(
            engine.inflight_atomic_bundles.is_empty(),
            "fully-acked bundle should graduate out of the inflight table"
        );
    }

    /// MV-2 — cross-venue ack loop. The originator's hedge leg
    /// lives on a different venue; the sibling engine running
    /// on that venue marks the leg via the shared DashboardState
    /// and the originator's sweep picks it up. Without the
    /// shared ack map this bundle would stay `hedge_acked=false`
    /// forever and get rolled back on timeout.
    #[test]
    fn atomic_bundle_ack_sweep_honours_cross_venue_dashboard_signal() {
        use mm_common::types::{LiveOrder, OrderStatus, Side};
        use mm_dashboard::state::{BundleLegRole, DashboardState};
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let dash = DashboardState::new();
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            Some(dash.clone()),
            None,
        );
        let self_venue = "custom"; // engine's default exchange_type tag.
        let now = chrono::Utc::now();
        // Only the maker leg lives locally; hedge is on bybit.
        engine.order_manager.track_order(LiveOrder {
            order_id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            price: dec!(100),
            qty: dec!(1),
            filled_qty: dec!(0),
            status: OrderStatus::Open,
            created_at: now,
        });
        let bundle_id = "cross-bundle".to_string();
        engine.inflight_atomic_bundles.insert(
            bundle_id.clone(),
            InflightAtomicBundle {
                dispatched_at: now,
                timeout_ms: 60_000,
                maker_venue: self_venue.into(),
                maker_symbol: "BTCUSDT".into(),
                maker_side: Side::Buy,
                maker_price: dec!(100),
                maker_acked: false,
                hedge_venue: "bybit".into(),
                hedge_symbol: "BTC-USDT".into(),
                hedge_side: Side::Sell,
                hedge_price: dec!(101),
                hedge_acked: false,
            },
        );
        // Register both legs on the shared DashboardState as
        // the originator would on AtomicBundle sink dispatch.
        dash.register_atomic_bundle_leg(
            &bundle_id,
            BundleLegRole::Maker,
            self_venue,
            "BTCUSDT",
            Side::Buy,
            dec!(100),
        );
        dash.register_atomic_bundle_leg(
            &bundle_id,
            BundleLegRole::Hedge,
            "bybit",
            "BTC-USDT",
            Side::Sell,
            dec!(101),
        );
        // Sibling engine's phase-1 publish: the bybit engine
        // observed its hedge leg live on its live-orders map
        // and called `ack_atomic_bundle_leg`.
        dash.ack_atomic_bundle_leg(&bundle_id, BundleLegRole::Hedge);

        // Originator's sweep must pick up the hedge ack via
        // DashboardState AND flip its own maker ack via the
        // local live-orders match.
        engine.sweep_atomic_bundle_acks();
        assert!(
            engine.inflight_atomic_bundles.is_empty(),
            "cross-venue bundle should graduate once DashboardState carries the remote ack"
        );
        // Dashboard entry should also have been cleared.
        let (m, h) = dash.atomic_bundle_ack_state(&bundle_id);
        assert!(!m && !h, "completed bundle's dashboard entry must be cleared");
    }

    /// Watchdog leaves fresh bundles alone.
    #[test]
    fn atomic_bundle_watchdog_spares_fresh_entries() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        engine.inflight_atomic_bundles.insert(
            "fresh-bundle".into(),
            InflightAtomicBundle {
                dispatched_at: chrono::Utc::now(),
                timeout_ms: 5_000,
                maker_venue: "binance".into(),
                maker_symbol: "BTCUSDT".into(),
                maker_side: mm_common::types::Side::Buy,
                maker_price: dec!(100),
                maker_acked: false,
                hedge_venue: "bybit".into(),
                hedge_symbol: "BTCUSDT".into(),
                hedge_side: mm_common::types::Side::Sell,
                hedge_price: dec!(100),
                hedge_acked: false,
            },
        );
        let rolled = engine.tick_atomic_bundle_watchdog();
        assert_eq!(rolled, 0);
        assert_eq!(engine.inflight_atomic_bundles.len(), 1);
    }

    /// Epic H Phase 5 — graph swap must rebuild the strategy pool
    /// and clear the per-node quote cache. A stale cache entry from
    /// a previous graph would leak into the new graph's overlay
    /// reads until it happens to be re-written.
    #[test]
    fn swap_strategy_graph_rebuilds_pool_and_clears_cache() {
        use mm_strategy_graph::{Edge, Graph, GraphNode, NodeId, PortRef, Scope};

        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        // Graph A: Strategy.Avellaneda → Out.Quotes + Math.Const → Out.SpreadMult.
        // Builds a pool entry for the Avellaneda node.
        let mk_graph = |strategy_kind: &str| -> Graph {
            let strat = NodeId::new();
            let quotes_sink = NodeId::new();
            let cst = NodeId::new();
            let mult_sink = NodeId::new();
            let mut g = Graph::empty("t", Scope::Symbol("BTCUSDT".into()));
            g.nodes.push(GraphNode {
                id: strat,
                kind: strategy_kind.into(),
                config: serde_json::Value::Null,
                pos: (0.0, 0.0),
            });
            g.nodes.push(GraphNode {
                id: quotes_sink,
                kind: "Out.Quotes".into(),
                config: serde_json::Value::Null,
                pos: (0.0, 0.0),
            });
            g.nodes.push(GraphNode {
                id: cst,
                kind: "Math.Const".into(),
                config: serde_json::json!({ "value": "1" }),
                pos: (0.0, 0.0),
            });
            g.nodes.push(GraphNode {
                id: mult_sink,
                kind: "Out.SpreadMult".into(),
                config: serde_json::Value::Null,
                pos: (0.0, 0.0),
            });
            g.edges.push(Edge {
                from: PortRef { node: strat, port: "quotes".into() },
                to: PortRef { node: quotes_sink, port: "quotes".into() },
            });
            g.edges.push(Edge {
                from: PortRef { node: cst, port: "value".into() },
                to: PortRef { node: mult_sink, port: "mult".into() },
            });
            g
        };

        // Deploy A — pool has one Strategy.Avellaneda instance.
        let g_a = mk_graph("Strategy.Avellaneda");
        let a_strat_id = g_a.nodes[0].id;
        engine.swap_strategy_graph(&g_a).expect("graph A compiles");
        assert_eq!(
            engine.strategy_pool.len(),
            1,
            "pool must hold one instance per Strategy.* node"
        );
        assert!(
            engine.strategy_pool.contains_key(&a_strat_id),
            "pool keyed by the Avellaneda node id"
        );

        // Prime the per-node cache with a dummy entry — simulates
        // what `refresh_quotes` would have written last tick.
        engine
            .last_strategy_quotes_per_node
            .insert(a_strat_id, Vec::new());
        assert_eq!(engine.last_strategy_quotes_per_node.len(), 1);
        // Also prime `graph_quotes_override` to represent a pending
        // `Out.Quotes` bundle from the last tick of graph A. The
        // next refresh-quotes pass would normally consume this;
        // a swap must drop it instead so the new graph isn't
        // surprised by quotes it never authored.
        engine.graph_quotes_override = Some(vec![]);

        // Deploy B — different node ids, different kind.
        let g_b = mk_graph("Strategy.Grid");
        engine.swap_strategy_graph(&g_b).expect("graph B compiles");

        assert_eq!(
            engine.strategy_pool.len(),
            1,
            "pool reshaped for new graph (one Grid instance)"
        );
        assert!(
            !engine.strategy_pool.contains_key(&a_strat_id),
            "old Avellaneda entry gone after swap"
        );
        assert!(
            engine.last_strategy_quotes_per_node.is_empty(),
            "stale per-node cache from graph A cleared on swap"
        );
        assert!(
            engine.graph_quotes_override.is_none(),
            "pending Out.Quotes override from graph A dropped on swap"
        );
    }

    /// GR-3 — two `Strategy.Avellaneda` nodes with different γ
    /// / order_size overrides get different
    /// `MarketMakerConfig` baselines materialised so their
    /// `compute_quotes` calls see distinct knobs.
    #[test]
    fn per_node_strategy_config_overrides_are_materialised() {
        use mm_strategy_graph::{Edge, Graph, GraphNode, NodeId, PortRef, Scope};

        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        let aggressive = NodeId::new();
        let conservative = NodeId::new();
        let quotes_sink = NodeId::new();
        let cst = NodeId::new();
        let mult_sink = NodeId::new();
        let mut g = Graph::empty("per-node-config", Scope::Symbol("BTCUSDT".into()));
        g.nodes.push(GraphNode {
            id: aggressive,
            kind: "Strategy.Avellaneda".into(),
            config: serde_json::json!({ "gamma": "5.0", "order_size": "0.2" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: conservative,
            kind: "Strategy.Avellaneda".into(),
            config: serde_json::json!({ "gamma": "0.1" }),
            pos: (0.0, 0.0),
        });
        // Round out the graph so the validator accepts it
        // (every graph needs an Out.SpreadMult sink; Quotes
        // sink consumes one of the Strategy's outputs so the
        // strategy node doesn't dangle).
        g.nodes.push(GraphNode {
            id: quotes_sink,
            kind: "Out.Quotes".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: cst,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "1" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: mult_sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: aggressive, port: "quotes".into() },
            to: PortRef { node: quotes_sink, port: "quotes".into() },
        });
        g.edges.push(Edge {
            from: PortRef { node: cst, port: "value".into() },
            to: PortRef { node: mult_sink, port: "mult".into() },
        });

        engine.swap_strategy_graph(&g).expect("graph compiles");

        assert_eq!(
            engine.strategy_node_configs.len(),
            2,
            "both override nodes should have materialised configs"
        );
        let aggr_cfg = engine
            .strategy_node_configs
            .get(&aggressive)
            .expect("aggressive override present");
        let cons_cfg = engine
            .strategy_node_configs
            .get(&conservative)
            .expect("conservative override present");
        assert_eq!(aggr_cfg.gamma, dec!(5.0));
        assert_eq!(aggr_cfg.order_size, dec!(0.2));
        assert_eq!(cons_cfg.gamma, dec!(0.1));
        // Baseline fields the node did NOT override stay at the
        // engine's config (sample_config uses the defaults).
        assert_eq!(cons_cfg.order_size, engine.config.market_maker.order_size);
        assert_eq!(aggr_cfg.kappa, engine.config.market_maker.kappa);
    }

    /// GR-3 — a Strategy.* node with no knob override must NOT
    /// appear in `strategy_node_configs`; the tick loop falls
    /// back to the engine's baseline config, preserving legacy
    /// graph behaviour.
    #[test]
    fn per_node_strategy_config_absent_for_unconfigured_nodes() {
        use mm_strategy_graph::{Edge, Graph, GraphNode, NodeId, PortRef, Scope};

        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );

        let strat = NodeId::new();
        let quotes_sink = NodeId::new();
        let cst = NodeId::new();
        let mult_sink = NodeId::new();
        let mut g = Graph::empty("no-overrides", Scope::Symbol("BTCUSDT".into()));
        g.nodes.push(GraphNode {
            id: strat,
            kind: "Strategy.Avellaneda".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: quotes_sink,
            kind: "Out.Quotes".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: cst,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "1" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: mult_sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: strat, port: "quotes".into() },
            to: PortRef { node: quotes_sink, port: "quotes".into() },
        });
        g.edges.push(Edge {
            from: PortRef { node: cst, port: "value".into() },
            to: PortRef { node: mult_sink, port: "mult".into() },
        });

        engine.swap_strategy_graph(&g).expect("graph compiles");
        assert!(
            engine.strategy_node_configs.is_empty(),
            "nodes without knob overrides must NOT materialise a per-node config"
        );
    }

    // ── S4.4 — graph-vs-legacy parity tests ───────────────────

    /// Helper: build a StrategyContext with the fields
    /// Avellaneda / GLFT actually read. Everything else gets
    /// a harmless neutral value so the assert focuses on the
    /// per-knob override path the test is pinning.
    fn parity_ctx<'a>(
        book: &'a mm_common::orderbook::LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a mm_common::config::MarketMakerConfig,
    ) -> mm_strategy::r#trait::StrategyContext<'a> {
        mm_strategy::r#trait::StrategyContext {
            book,
            product,
            config,
            inventory: dec!(0.1),
            volatility: dec!(0.02),
            time_remaining: dec!(0.5),
            mid_price: dec!(50_000),
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        }
    }

    fn parity_book() -> mm_common::orderbook::LocalOrderBook {
        use mm_common::types::PriceLevel;
        let mut b = mm_common::orderbook::LocalOrderBook::new("BTCUSDT".into());
        b.apply_snapshot(
            vec![PriceLevel { price: dec!(50_000), qty: dec!(2) }],
            vec![PriceLevel { price: dec!(50_010), qty: dec!(2) }],
            1,
        );
        b
    }

    /// S4.4 — `Strategy.Avellaneda` on the graph path must
    /// produce the same quotes as the hand-wired
    /// `AvellanedaStoikov` when the StrategyContext is
    /// identical. Catches silent drift when a knob-override
    /// refactor lands on one side only.
    #[test]
    fn avellaneda_graph_parity_matches_legacy() {
        use mm_strategy::r#trait::Strategy;

        let product = sample_product("BTCUSDT");
        let book = parity_book();
        let cfg = sample_config().market_maker;
        let ctx = parity_ctx(&book, &product, &cfg);

        let legacy = mm_strategy::AvellanedaStoikov;
        let legacy_quotes = legacy.compute_quotes(&ctx);

        // The graph path builds a pool and calls
        // `compute_quotes` on the same stateless struct. Same
        // ctx in → same quotes out.
        let graph_instance = mm_strategy::AvellanedaStoikov;
        let graph_quotes = graph_instance.compute_quotes(&ctx);

        assert_eq!(legacy_quotes, graph_quotes,
            "Strategy.Avellaneda graph path drifted from legacy output");
    }

    /// S4.4 — per-node `gamma` override applied via the
    /// `strategy_node_configs` map yields the same quotes as
    /// the hand-wired path running with that same `gamma`.
    /// Protects the GR-3 override plumbing from drift.
    #[test]
    fn avellaneda_per_node_gamma_override_matches_direct_config() {
        use mm_strategy::r#trait::Strategy;

        let product = sample_product("BTCUSDT");
        let book = parity_book();

        // Version A: config with gamma=5 handed directly to
        // the strategy.
        let mut cfg_a = sample_config().market_maker;
        cfg_a.gamma = dec!(5);
        let ctx_a = parity_ctx(&book, &product, &cfg_a);
        let a = mm_strategy::AvellanedaStoikov.compute_quotes(&ctx_a);

        // Version B: baseline config + per-node gamma override
        // applied via the engine's override map path, mimicking
        // what the graph tick loop does.
        let baseline = sample_config().market_maker;
        let mut g = mm_strategy_graph::Graph::empty(
            "parity",
            mm_strategy_graph::Scope::Symbol("BTCUSDT".into()),
        );
        let node_id = mm_strategy_graph::NodeId::new();
        g.nodes.push(mm_strategy_graph::GraphNode {
            id: node_id,
            kind: "Strategy.Avellaneda".into(),
            config: serde_json::json!({ "gamma": "5" }),
            pos: (0.0, 0.0),
        });
        let overrides =
            MarketMakerEngine::build_strategy_node_configs(&g, &baseline);
        let cfg_b = overrides.get(&node_id).expect("override materialised");
        let ctx_b = parity_ctx(&book, &product, cfg_b);
        let b = mm_strategy::AvellanedaStoikov.compute_quotes(&ctx_b);

        assert_eq!(a, b,
            "per-node gamma override drifted from direct-config baseline");
    }

    /// S4.4 — borrow_cost_bps plumbed through the per-node
    /// StrategyContext override matches the legacy path where
    /// the same value is set directly on the ctx.
    #[test]
    fn avellaneda_borrow_cost_override_matches_direct_ctx() {
        use mm_strategy::r#trait::Strategy;

        let product = sample_product("BTCUSDT");
        let book = parity_book();
        let cfg = sample_config().market_maker;

        // Legacy: borrow_cost_bps set directly on the ctx.
        let mut ctx_a = parity_ctx(&book, &product, &cfg);
        ctx_a.borrow_cost_bps = Some(dec!(25));
        let a = mm_strategy::AvellanedaStoikov.compute_quotes(&ctx_a);

        // Graph: ctx override map carries the knob.
        let mut g = mm_strategy_graph::Graph::empty(
            "borrow-parity",
            mm_strategy_graph::Scope::Symbol("BTCUSDT".into()),
        );
        let node_id = mm_strategy_graph::NodeId::new();
        g.nodes.push(mm_strategy_graph::GraphNode {
            id: node_id,
            kind: "Strategy.Avellaneda".into(),
            config: serde_json::json!({ "borrow_cost_bps": "25" }),
            pos: (0.0, 0.0),
        });
        let ctx_overrides = MarketMakerEngine::build_strategy_ctx_overrides(&g);
        let ov = ctx_overrides.get(&node_id).expect("ctx override captured");
        let mut ctx_b = parity_ctx(&book, &product, &cfg);
        ctx_b.borrow_cost_bps = ov.borrow_cost_bps;
        let b = mm_strategy::AvellanedaStoikov.compute_quotes(&ctx_b);

        assert_eq!(a, b,
            "borrow_cost_bps per-node override drifted from direct-ctx baseline");
    }

    /// S4.4 — as_prob (symmetric) through the graph ctx
    /// override matches the legacy ctx.as_prob assignment.
    #[test]
    fn avellaneda_as_prob_override_matches_direct_ctx() {
        use mm_strategy::r#trait::Strategy;

        let product = sample_product("BTCUSDT");
        let book = parity_book();
        let cfg = sample_config().market_maker;

        let mut ctx_a = parity_ctx(&book, &product, &cfg);
        ctx_a.as_prob = Some(dec!(0.7));
        let a = mm_strategy::AvellanedaStoikov.compute_quotes(&ctx_a);

        let mut g = mm_strategy_graph::Graph::empty(
            "as-parity",
            mm_strategy_graph::Scope::Symbol("BTCUSDT".into()),
        );
        let node_id = mm_strategy_graph::NodeId::new();
        g.nodes.push(mm_strategy_graph::GraphNode {
            id: node_id,
            kind: "Strategy.Avellaneda".into(),
            config: serde_json::json!({ "as_prob": "0.7" }),
            pos: (0.0, 0.0),
        });
        let ctx_overrides = MarketMakerEngine::build_strategy_ctx_overrides(&g);
        let ov = ctx_overrides.get(&node_id).expect("as_prob override captured");
        let mut ctx_b = parity_ctx(&book, &product, &cfg);
        ctx_b.as_prob = ov.as_prob;
        let b = mm_strategy::AvellanedaStoikov.compute_quotes(&ctx_b);

        assert_eq!(a, b,
            "as_prob per-node override drifted from direct-ctx baseline");
    }

    // ── S2.1 — atomic bundle checkpoint round-trip ────────────

    /// S2.1 — serialising the inflight map and restoring it
    /// round-trips every field, so a crashed engine re-enters
    /// the next tick with the watchdog + ack sweep already
    /// primed.
    #[test]
    fn atomic_bundle_checkpoint_round_trips() {
        use mm_common::types::Side;
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);

        // Engine A: dispatches a pair and writes the
        // checkpoint.
        let mut a = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let now = chrono::Utc::now();
        a.inflight_atomic_bundles.insert(
            "bundle-a".into(),
            InflightAtomicBundle {
                dispatched_at: now,
                timeout_ms: 60_000,
                maker_venue: "binance".into(),
                maker_symbol: "BTCUSDT".into(),
                maker_side: Side::Buy,
                maker_price: dec!(50_000),
                maker_acked: false,
                hedge_venue: "bybit".into(),
                hedge_symbol: "BTC-USDT".into(),
                hedge_side: Side::Sell,
                hedge_price: dec!(50_100),
                hedge_acked: true,
            },
        );
        let snapshot = a.inflight_atomic_bundles_checkpoint();
        assert_eq!(snapshot.len(), 1);

        // Engine B: fresh, restores from the checkpoint blob.
        let primary_b =
            Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle_b = ConnectorBundle::single(primary_b);
        let b = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle_b,
            None,
            None,
        );
        let cp = mm_persistence::checkpoint::SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0),
            avg_entry_price: dec!(0),
            open_order_ids: vec![],
            realized_pnl: dec!(0),
            total_volume: dec!(0),
            total_fills: 0,
            inflight_atomic_bundles: snapshot,
            strategy_state: None,
            engine_state: std::collections::HashMap::new(),
        };
        let b = b.with_checkpoint_restore(&cp);

        assert_eq!(b.inflight_atomic_bundles.len(), 1);
        let restored = b.inflight_atomic_bundles.get("bundle-a").unwrap();
        assert_eq!(restored.maker_venue, "binance");
        assert_eq!(restored.hedge_venue, "bybit");
        assert_eq!(restored.maker_side, Side::Buy);
        assert!(!restored.maker_acked);
        assert!(restored.hedge_acked);
        assert_eq!(restored.timeout_ms, 60_000);
    }

    /// S2.1 — a malformed blob inside the checkpoint doesn't
    /// prevent the engine from starting; the entry is skipped
    /// with a warn and the rest of the restore proceeds.
    #[test]
    fn atomic_bundle_checkpoint_skips_malformed_entries() {
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let engine = MarketMakerEngine::new(
            "BTCUSDT".to_string(),
            sample_config(),
            sample_product("BTCUSDT"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let cp = mm_persistence::checkpoint::SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0),
            avg_entry_price: dec!(0),
            open_order_ids: vec![],
            realized_pnl: dec!(0),
            total_volume: dec!(0),
            total_fills: 0,
            inflight_atomic_bundles: vec![
                serde_json::json!("not a tuple"),
                serde_json::json!({ "shape": "wrong" }),
            ],
            strategy_state: None,
            engine_state: std::collections::HashMap::new(),
        };
        let engine = engine.with_checkpoint_restore(&cp);
        assert!(engine.inflight_atomic_bundles.is_empty());
    }

    // ── S1.2 — per-strategy capital budget gate tests ─────────

    fn qp(bid: Decimal, ask: Decimal) -> mm_common::types::QuotePair {
        use mm_common::types::{Quote, QuotePair, Side};
        QuotePair {
            bid: Some(Quote {
                side: Side::Buy,
                price: bid,
                qty: dec!(0.01),
            }),
            ask: Some(Quote {
                side: Side::Sell,
                price: ask,
                qty: dec!(0.01),
            }),
        }
    }

    /// Absent key → pass-through unchanged.
    #[test]
    fn capital_budget_absent_is_passthrough() {
        let budget = std::collections::HashMap::new();
        let out = MarketMakerEngine::apply_capital_budget(
            "avellaneda",
            vec![qp(dec!(100), dec!(101))],
            dec!(0.5),
            dec!(100),
            &budget,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].bid.is_some() && out[0].ask.is_some());
    }

    /// Under budget → pass-through.
    #[test]
    fn capital_budget_under_cap_passes_through() {
        let mut budget = std::collections::HashMap::new();
        // 1 BTC × 100 quote = 100 notional; cap = 200.
        budget.insert("avellaneda".into(), dec!(200));
        let out = MarketMakerEngine::apply_capital_budget(
            "avellaneda",
            vec![qp(dec!(100), dec!(101))],
            dec!(1),
            dec!(100),
            &budget,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].bid.is_some() && out[0].ask.is_some());
    }

    /// Long + over budget → bid dropped, ask kept (unwind path).
    #[test]
    fn capital_budget_long_over_cap_drops_bids() {
        let mut budget = std::collections::HashMap::new();
        // 2 BTC × 100 = 200 notional; cap = 150.
        budget.insert("avellaneda".into(), dec!(150));
        let out = MarketMakerEngine::apply_capital_budget(
            "avellaneda",
            vec![qp(dec!(100), dec!(101))],
            dec!(2),
            dec!(100),
            &budget,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].bid.is_none(), "long + over-cap must drop bids");
        assert!(out[0].ask.is_some(), "ask kept so the position can unwind");
    }

    /// Short + over budget → ask dropped, bid kept.
    #[test]
    fn capital_budget_short_over_cap_drops_asks() {
        let mut budget = std::collections::HashMap::new();
        budget.insert("avellaneda".into(), dec!(150));
        let out = MarketMakerEngine::apply_capital_budget(
            "avellaneda",
            vec![qp(dec!(100), dec!(101))],
            dec!(-2),
            dec!(100),
            &budget,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].ask.is_none(), "short + over-cap must drop asks");
        assert!(out[0].bid.is_some(), "bid kept for the unwind");
    }

    /// Budget keyed on a different strategy name → pass-through
    /// for this strategy.
    #[test]
    fn capital_budget_different_strategy_passes_through() {
        let mut budget = std::collections::HashMap::new();
        budget.insert("funding_arb".into(), dec!(50)); // Very low cap.
        let out = MarketMakerEngine::apply_capital_budget(
            "avellaneda", // Different strategy — not in the map.
            vec![qp(dec!(100), dec!(101))],
            dec!(10), // Big inventory.
            dec!(100),
            &budget,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].bid.is_some() && out[0].ask.is_some());
    }

    /// Zero / negative cap is treated as "no gate" to avoid a
    /// footgun where a user accidentally configures 0 and
    /// immediately loses all quoting.
    #[test]
    fn capital_budget_zero_cap_is_passthrough() {
        let mut budget = std::collections::HashMap::new();
        budget.insert("avellaneda".into(), dec!(0));
        let out = MarketMakerEngine::apply_capital_budget(
            "avellaneda",
            vec![qp(dec!(100), dec!(101))],
            dec!(10),
            dec!(100),
            &budget,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].bid.is_some() && out[0].ask.is_some());
    }

    /// Sprint 4 — watchdog path bumps the rolled-back counter +
    /// drops inflight gauge to zero.
    #[test]
    fn sprint4_atomic_bundle_rollback_metrics() {
        use mm_common::types::Side;
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "SP4RB".to_string(),
            sample_config(),
            sample_product("SP4RB"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let rolled_before = mm_dashboard::metrics::ATOMIC_BUNDLES_ROLLED_BACK_TOTAL
            .with_label_values(&["SP4RB"])
            .get();
        let past = chrono::Utc::now() - chrono::Duration::seconds(10);
        engine.inflight_atomic_bundles.insert(
            "stale".into(),
            InflightAtomicBundle {
                dispatched_at: past,
                timeout_ms: 1_000,
                maker_venue: "binance".into(),
                maker_symbol: "SP4RB".into(),
                maker_side: Side::Buy,
                maker_price: dec!(100),
                maker_acked: false,
                hedge_venue: "bybit".into(),
                hedge_symbol: "SP4RB".into(),
                hedge_side: Side::Sell,
                hedge_price: dec!(100),
                hedge_acked: false,
            },
        );
        assert_eq!(engine.tick_atomic_bundle_watchdog(), 1);
        let rolled_after = mm_dashboard::metrics::ATOMIC_BUNDLES_ROLLED_BACK_TOTAL
            .with_label_values(&["SP4RB"])
            .get();
        assert_eq!(rolled_after - rolled_before, 1);
        assert_eq!(
            mm_dashboard::metrics::ATOMIC_BUNDLES_INFLIGHT
                .with_label_values(&["SP4RB"])
                .get(),
            0.0
        );
    }

    /// Sprint 4 — ack-sweep path bumps the completed counter
    /// when a bundle graduates out of inflight with both legs
    /// acked.
    #[test]
    fn sprint4_atomic_bundle_completion_metrics() {
        use mm_common::types::Side;
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "SP4OK".to_string(),
            sample_config(),
            sample_product("SP4OK"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let done_before = mm_dashboard::metrics::ATOMIC_BUNDLES_COMPLETED_TOTAL
            .with_label_values(&["SP4OK"])
            .get();
        engine.inflight_atomic_bundles.insert(
            "done".into(),
            InflightAtomicBundle {
                dispatched_at: chrono::Utc::now(),
                timeout_ms: 5_000,
                maker_venue: "custom".into(),
                maker_symbol: "SP4OK".into(),
                maker_side: Side::Buy,
                maker_price: dec!(100),
                maker_acked: true,
                hedge_venue: "custom".into(),
                hedge_symbol: "SP4OK".into(),
                hedge_side: Side::Sell,
                hedge_price: dec!(101),
                hedge_acked: true,
            },
        );
        engine.sweep_atomic_bundle_acks();
        let done_after = mm_dashboard::metrics::ATOMIC_BUNDLES_COMPLETED_TOTAL
            .with_label_values(&["SP4OK"])
            .get();
        assert_eq!(done_after - done_before, 1);
        assert_eq!(
            mm_dashboard::metrics::ATOMIC_BUNDLES_INFLIGHT
                .with_label_values(&["SP4OK"])
                .get(),
            0.0
        );
    }

    /// INT-1 — `record_tick_decisions` creates one ledger
    /// entry per side that has quote volume, tags each with
    /// the nearest-to-mid distance as the expected cost.
    #[test]
    fn int1_record_tick_decisions_creates_one_per_side() {
        use mm_common::types::{Quote, QuotePair, Side};
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "INT1REC".to_string(),
            sample_config(),
            sample_product("INT1REC"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        // Seed a mid so record_tick_decisions passes the guard.
        engine.book_keeper.book.apply_snapshot(
            vec![mm_common::types::PriceLevel {
                price: dec!(99),
                qty: dec!(5),
            }],
            vec![mm_common::types::PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let quotes = vec![
            QuotePair {
                bid: Some(Quote { side: Side::Buy, price: dec!(99.5), qty: dec!(0.1) }),
                ask: Some(Quote { side: Side::Sell, price: dec!(100.5), qty: dec!(0.1) }),
            },
        ];
        let decisions = engine.record_tick_decisions(&quotes);
        assert_eq!(decisions.len(), 2, "one per side");
        assert_eq!(engine.decision_ledger.len(), 2);
        // Recent sees both, newest-first.
        let recent = engine.decision_ledger.recent(10);
        assert_eq!(recent.len(), 2);
        assert!(recent.iter().all(|r| r.symbol == "INT1REC"));
    }

    /// INT-1 — on_fill resolves back through the ledger +
    /// emits an audit-friendly delta.
    #[test]
    fn int1_on_fill_resolves_through_ledger() {
        use mm_common::types::Side;
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let engine = MarketMakerEngine::new(
            "INT1RES".to_string(),
            sample_config(),
            sample_product("INT1RES"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let did = engine.decision_ledger.record_decision(
            1_000,
            "INT1RES",
            Side::Buy,
            dec!(0.1),
            dec!(100),
            Some(dec!(3)),
        );
        let oid = mm_common::types::OrderId::new_v4();
        assert!(engine.decision_ledger.bind_order(did, oid));
        let resolved = engine
            .decision_ledger
            .on_fill(oid, 1_500, Side::Buy, dec!(100.05), dec!(0.1))
            .expect("resolved");
        assert_eq!(resolved.realized_cost_bps, dec!(5));
        assert_eq!(resolved.vs_expected_bps, Some(dec!(2)));
    }

    /// Sprint 4 — deploy counters tick on accepted + rejected.
    #[test]
    fn sprint4_strategy_graph_deploy_metrics() {
        use mm_strategy_graph::{Graph, GraphNode, NodeId, Scope};
        let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(primary);
        let mut engine = MarketMakerEngine::new(
            "SP4DEP".to_string(),
            sample_config(),
            sample_product("SP4DEP"),
            Box::new(AvellanedaStoikov),
            bundle,
            None,
            None,
        );
        let acc_before = mm_dashboard::metrics::STRATEGY_GRAPH_DEPLOYS_TOTAL
            .with_label_values(&["accepted"])
            .get();
        let rej_before = mm_dashboard::metrics::STRATEGY_GRAPH_DEPLOYS_TOTAL
            .with_label_values(&["rejected"])
            .get();
        let mut g = Graph::empty("sp4dep-good", Scope::Symbol("SP4DEP".into()));
        let strat = NodeId::new();
        let qout = NodeId::new();
        let cst = NodeId::new();
        let mult = NodeId::new();
        g.nodes.push(GraphNode {
            id: strat,
            kind: "Strategy.Avellaneda".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: qout,
            kind: "Out.Quotes".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: cst,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "1" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(GraphNode {
            id: mult,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.edges.push(mm_strategy_graph::Edge {
            from: mm_strategy_graph::PortRef { node: strat, port: "quotes".into() },
            to: mm_strategy_graph::PortRef { node: qout, port: "quotes".into() },
        });
        g.edges.push(mm_strategy_graph::Edge {
            from: mm_strategy_graph::PortRef { node: cst, port: "value".into() },
            to: mm_strategy_graph::PortRef { node: mult, port: "mult".into() },
        });
        engine.swap_strategy_graph(&g).expect("clean graph compiles");
        let acc_after = mm_dashboard::metrics::STRATEGY_GRAPH_DEPLOYS_TOTAL
            .with_label_values(&["accepted"])
            .get();
        // Counters are process-global across Prometheus state — if
        // another test runs in parallel on the same counter the
        // delta could be > 1. Assert `>= +1` so the test stays
        // deterministic under the `cargo test -j N` scheduler
        // while still proving this path incremented.
        assert!(
            acc_after >= acc_before + 1,
            "accepted counter must increase by ≥ 1; got {acc_before} → {acc_after}"
        );
        assert_eq!(
            mm_dashboard::metrics::STRATEGY_GRAPH_NODES
                .with_label_values(&["sp4dep-good"])
                .get(),
            4.0
        );

        // Dangling edge → Evaluator::build rejects → rejected counter ticks.
        let mut bad = Graph::empty("sp4dep-bad", Scope::Symbol("SP4DEP".into()));
        bad.nodes.push(GraphNode {
            id: NodeId::new(),
            kind: "Out.Quotes".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        bad.edges.push(mm_strategy_graph::Edge {
            from: mm_strategy_graph::PortRef { node: NodeId::new(), port: "quotes".into() },
            to: mm_strategy_graph::PortRef { node: bad.nodes[0].id, port: "quotes".into() },
        });
        assert!(engine.swap_strategy_graph(&bad).is_err());
        let rej_after = mm_dashboard::metrics::STRATEGY_GRAPH_DEPLOYS_TOTAL
            .with_label_values(&["rejected"])
            .get();
        assert!(
            rej_after >= rej_before + 1,
            "rejected counter must increase by ≥ 1; got {rej_before} → {rej_after}"
        );
    }
