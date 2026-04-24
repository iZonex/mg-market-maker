    use super::*;
    use async_trait::async_trait;
    use mm_common::types::{Balance, LiveOrder, OrderId, ProductSpec, WalletType};
    use mm_exchange_core::connector::{
        NewOrder as CoreNewOrder, VenueCapabilities, VenueId, VenueProduct,
    };
    use mm_exchange_core::events::MarketEvent;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    // -------------------- mock connector --------------------

    struct MockVenue {
        venue: VenueId,
        product: VenueProduct,
        caps: VenueCapabilities,
        bids: Mutex<Vec<PriceLevel>>,
        asks: Mutex<Vec<PriceLevel>>,
        placed: Mutex<Vec<CoreNewOrder>>,
    }

    impl MockVenue {
        fn new(venue: VenueId, product: VenueProduct, mid: Decimal) -> Self {
            Self {
                venue,
                product,
                caps: VenueCapabilities {
                    max_batch_size: 20,
                    supports_amend: false,
                    supports_ws_trading: false,
                    supports_fix: false,
                    max_order_rate: 100,
                    supports_funding_rate: false,
                    supports_margin_info: false,
                supports_margin_mode: false,
            supports_liquidation_feed: false,
            supports_set_leverage: false,
                            },
                bids: Mutex::new(vec![PriceLevel {
                    price: mid - dec!(1),
                    qty: dec!(10),
                }]),
                asks: Mutex::new(vec![PriceLevel {
                    price: mid + dec!(1),
                    qty: dec!(10),
                }]),
                placed: Mutex::new(vec![]),
            }
        }
        fn placed_count(&self) -> usize {
            self.placed.lock().unwrap().len()
        }
        fn set_mid(&self, mid: Decimal) {
            *self.bids.lock().unwrap() = vec![PriceLevel {
                price: mid - dec!(1),
                qty: dec!(10),
            }];
            *self.asks.lock().unwrap() = vec![PriceLevel {
                price: mid + dec!(1),
                qty: dec!(10),
            }];
        }
        fn clear_books(&self) {
            self.bids.lock().unwrap().clear();
            self.asks.lock().unwrap().clear();
        }
    }

    #[async_trait]
    impl ExchangeConnector for MockVenue {
        fn venue_id(&self) -> VenueId {
            self.venue
        }
        fn capabilities(&self) -> &VenueCapabilities {
            &self.caps
        }
        fn product(&self) -> VenueProduct {
            self.product
        }
        async fn subscribe(
            &self,
            _symbols: &[String],
        ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>> {
            let (_tx, rx) = mpsc::unbounded_channel();
            Ok(rx)
        }
        async fn get_orderbook(
            &self,
            _symbol: &str,
            _depth: u32,
        ) -> anyhow::Result<(Vec<PriceLevel>, Vec<PriceLevel>, u64)> {
            Ok((
                self.bids.lock().unwrap().clone(),
                self.asks.lock().unwrap().clone(),
                1,
            ))
        }
        async fn place_order(&self, order: &CoreNewOrder) -> anyhow::Result<OrderId> {
            self.placed.lock().unwrap().push(order.clone());
            Ok(OrderId::new_v4())
        }
        async fn place_orders_batch(
            &self,
            _orders: &[CoreNewOrder],
        ) -> anyhow::Result<Vec<OrderId>> {
            Ok(vec![])
        }
        async fn cancel_order(&self, _symbol: &str, _order_id: OrderId) -> anyhow::Result<()> {
            Ok(())
        }
        async fn cancel_orders_batch(
            &self,
            _symbol: &str,
            _order_ids: &[OrderId],
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn cancel_all_orders(&self, _symbol: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_open_orders(&self, _symbol: &str) -> anyhow::Result<Vec<LiveOrder>> {
            Ok(vec![])
        }
        async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
            Ok(vec![Balance {
                asset: "USDT".to_string(),
                wallet: WalletType::Spot,
                total: dec!(0),
                locked: dec!(0),
                available: dec!(0),
            }])
        }
        async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec> {
            Ok(ProductSpec {
                symbol: symbol.to_string(),
                base_asset: "BTC".to_string(),
                quote_asset: "USDT".to_string(),
                tick_size: dec!(0.01),
                lot_size: dec!(0.0001),
                min_notional: dec!(10),
                maker_fee: dec!(0.0001),
                taker_fee: dec!(0.0005),
                trading_status: Default::default(),
            })
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    // -------------------- recording sink --------------------

    struct RecordingSink {
        events: Mutex<Vec<StatArbEvent>>,
    }
    impl RecordingSink {
        fn new() -> Self {
            Self {
                events: Mutex::new(vec![]),
            }
        }
        fn snapshot(&self) -> Vec<StatArbEvent> {
            self.events.lock().unwrap().clone()
        }
    }
    impl StatArbEventSink for RecordingSink {
        fn on_event(&self, event: StatArbEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    // -------------------- fixtures --------------------

    fn pair() -> StatArbPair {
        StatArbPair {
            y_symbol: "BTCUSDT".to_string(),
            x_symbol: "ETHUSDT".to_string(),
            strategy_class: "stat_arb_BTCUSDT_ETHUSDT".to_string(),
        }
    }

    fn small_window_config() -> StatArbDriverConfig {
        StatArbDriverConfig {
            tick_interval: Duration::from_millis(10),
            zscore: ZScoreConfig {
                window: 10,
                entry_threshold: dec!(1.5),
                exit_threshold: dec!(0.5),
            },
            kalman_transition_var: dec!(0.000001),
            kalman_observation_var: dec!(0.001),
            leg_notional_usd: dec!(1000),
        }
    }

    fn make_driver(
        y_mid: Decimal,
        x_mid: Decimal,
        config: StatArbDriverConfig,
    ) -> (
        Arc<MockVenue>,
        Arc<MockVenue>,
        Arc<RecordingSink>,
        StatArbDriver,
    ) {
        let y = Arc::new(MockVenue::new(VenueId::Binance, VenueProduct::Spot, y_mid));
        let x = Arc::new(MockVenue::new(VenueId::Binance, VenueProduct::Spot, x_mid));
        let sink = Arc::new(RecordingSink::new());
        let driver = StatArbDriver::new(
            y.clone(),
            x.clone(),
            pair(),
            config,
            sink.clone() as Arc<dyn StatArbEventSink>,
        );
        (y, x, sink, driver)
    }

    /// Build a fake cointegrated price history where Y = 2 · X
    /// and then seed the driver via `recheck_cointegration`.
    fn seed_cointegrated(driver: &mut StatArbDriver) {
        let x: Vec<Decimal> = (0..60)
            .map(|i| dec!(100) + Decimal::from(i as i64 % 5 - 2))
            .collect();
        let y: Vec<Decimal> = x
            .iter()
            .enumerate()
            .map(|(i, xi)| {
                let jitter = Decimal::from(i as i64 % 3 - 1) / dec!(10);
                dec!(2) * xi + jitter
            })
            .collect();
        driver.recheck_cointegration(&y, &x);
    }

    // -------------------- tests --------------------

    #[tokio::test]
    async fn warmup_fires_until_window_full() {
        let cfg = small_window_config();
        let window = cfg.zscore.window;
        let (_y, _x, sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        seed_cointegrated(&mut driver);
        // Drive fewer ticks than the window.
        for _ in 0..(window - 1) {
            let e = driver.tick_once().await;
            assert!(matches!(e, StatArbEvent::Warmup { .. }), "got {e:?}");
        }
        let events = sink.snapshot();
        assert_eq!(events.len(), window - 1);
    }

    #[tokio::test]
    async fn not_cointegrated_when_no_seed() {
        let cfg = small_window_config();
        let (_y, _x, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg.clone());
        // Fill the window first so we're past warmup.
        for _ in 0..cfg.zscore.window {
            driver.tick_once().await;
        }
        let e = driver.tick_once().await;
        assert!(
            matches!(e, StatArbEvent::NotCointegrated { .. }),
            "got {e:?}"
        );
    }

    #[tokio::test]
    async fn input_unavailable_on_empty_books() {
        let cfg = small_window_config();
        let (y, _x, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        y.clear_books();
        let e = driver.tick_once().await;
        assert!(
            matches!(e, StatArbEvent::InputUnavailable { .. }),
            "got {e:?}"
        );
    }

    #[tokio::test]
    async fn hold_emitted_inside_dead_band() {
        let cfg = small_window_config();
        let window = cfg.zscore.window;
        let (y, x, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        seed_cointegrated(&mut driver);
        // Warmup: feed `window` ticks with small spread wiggles so
        // the z-score stays inside the entry band.
        for i in 0..window {
            let delta = Decimal::from(i as i64 % 3) / dec!(10);
            y.set_mid(dec!(200) + delta);
            x.set_mid(dec!(100));
            driver.tick_once().await;
        }
        let e = driver.tick_once().await;
        assert!(
            matches!(e, StatArbEvent::Hold { .. } | StatArbEvent::Warmup { .. }),
            "got {e:?}"
        );
    }

    #[tokio::test]
    async fn entered_then_exited_on_spread_shock_and_revert() {
        // Use a larger window so the z-score has stable statistics.
        let cfg = StatArbDriverConfig {
            tick_interval: Duration::from_millis(10),
            zscore: ZScoreConfig {
                window: 20,
                entry_threshold: dec!(1.5),
                exit_threshold: dec!(0.3),
            },
            kalman_transition_var: dec!(0.000001),
            kalman_observation_var: dec!(0.001),
            leg_notional_usd: dec!(1000),
        };
        let window = cfg.zscore.window;
        let (y, x, sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        seed_cointegrated(&mut driver);

        // Warmup with steady prices: Y=200, X=100, spread ≈ 0.
        for _ in 0..window {
            y.set_mid(dec!(200));
            x.set_mid(dec!(100));
            driver.tick_once().await;
        }

        // Shock: Y jumps 5 units → spread jumps ~5σ above mean.
        // The driver should OPEN on the shock tick.
        y.set_mid(dec!(205));
        let shock_event = driver.tick_once().await;
        assert!(
            matches!(shock_event, StatArbEvent::Entered { .. }),
            "expected Entered on shock, got {shock_event:?}"
        );

        // Revert: Y back to 200. Spread crosses back into the exit
        // band. Driver should CLOSE.
        y.set_mid(dec!(200));
        // Push a few steady ticks so the rolling mean catches up
        // and z returns to the exit band.
        let mut saw_exit = false;
        for _ in 0..50 {
            let e = driver.tick_once().await;
            if matches!(e, StatArbEvent::Exited { .. }) {
                saw_exit = true;
                break;
            }
        }
        assert!(
            saw_exit,
            "expected Exited after revert, got {:?}",
            sink.snapshot().last()
        );
    }

    #[tokio::test]
    async fn cointegration_seed_initialises_kalman_beta() {
        let cfg = small_window_config();
        let (_y, _x, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        assert_eq!(driver.current_beta(), dec!(1)); // neutral prior
        seed_cointegrated(&mut driver);
        // After seeding, Kalman should hold the Engle-Granger β
        // (≈2 for Y = 2 · X).
        let seeded = driver.current_beta();
        assert!(
            (seeded - dec!(2)).abs() < dec!(0.1),
            "Kalman not seeded: β={seeded}"
        );
    }

    #[tokio::test]
    async fn recheck_without_enough_samples_is_noop() {
        let cfg = small_window_config();
        let (_y, _x, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        driver.recheck_cointegration(&[dec!(1); 5], &[dec!(2); 5]);
        assert!(driver.cointegration().is_none());
    }

    #[tokio::test]
    async fn size_legs_is_book_neutral_in_beta() {
        let cfg = small_window_config();
        let (_y, _x, _sink, driver) = make_driver(dec!(200), dec!(100), cfg);
        let (y_qty, x_qty) = driver.size_legs(dec!(2), dec!(200), dec!(100));
        // With leg_notional_usd=1000 and Y mid=200, y_qty = 5.
        assert_eq!(y_qty, dec!(5));
        // x_qty = β · y_qty = 10.
        assert_eq!(x_qty, dec!(10));
    }

    #[tokio::test]
    async fn sink_receives_every_emitted_event() {
        let cfg = small_window_config();
        let (_y, _x, sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        for _ in 0..5 {
            driver.tick_once().await;
        }
        assert_eq!(sink.snapshot().len(), 5);
    }

    #[tokio::test]
    async fn run_loop_exits_on_shutdown_signal() {
        let cfg = small_window_config();
        let (_y, _x, _sink, driver) = make_driver(dec!(200), dec!(100), cfg);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(driver.run(rx));
        // Let it tick once.
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(true).unwrap();
        // Should exit promptly.
        let result = tokio::time::timeout(Duration::from_millis(200), handle).await;
        assert!(result.is_ok(), "run loop did not exit on shutdown");
    }

    // -------------------- stage-2 dispatch tests --------------------

    /// `entry_sides` maps SellY → (Sell, Buy) and BuyY → (Buy, Sell).
    #[test]
    fn entry_sides_map_direction_to_sides() {
        assert_eq!(entry_sides(SpreadDirection::SellY), (Side::Sell, Side::Buy));
        assert_eq!(entry_sides(SpreadDirection::BuyY), (Side::Buy, Side::Sell));
    }

    /// LegDispatchReport helpers: `empty()`, `is_empty()`,
    /// `all_succeeded()` behave as documented.
    #[test]
    fn leg_dispatch_report_all_succeeded_matches_errors() {
        let empty = LegDispatchReport::empty();
        assert!(empty.is_empty());
        // Empty report has no legs so "all succeeded" is false.
        assert!(!empty.all_succeeded());

        let ok_leg = |side: Side| LegOutcome {
            side,
            symbol: "BTCUSDT".to_string(),
            target_qty: dec!(1),
            dispatched_qty: dec!(1),
            error: None,
        };
        let all_ok = LegDispatchReport {
            y: Some(ok_leg(Side::Sell)),
            x: Some(ok_leg(Side::Buy)),
        };
        assert!(all_ok.all_succeeded());

        let mut one_fail = all_ok.clone();
        one_fail.x = Some(LegOutcome {
            side: Side::Buy,
            symbol: "ETHUSDT".to_string(),
            target_qty: dec!(1),
            dispatched_qty: dec!(0),
            error: Some("boom".to_string()),
        });
        assert!(!one_fail.all_succeeded());
    }

    /// Entry dispatch: on `StatArbEvent::Entered` both legs
    /// fire a single place_order each and the sides match the
    /// direction.
    #[tokio::test]
    async fn try_dispatch_legs_for_entry_places_both_legs() {
        let cfg = small_window_config();
        let (y_mock, x_mock, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        // Fake an entered event directly — bypasses cointegration.
        let entered = StatArbEvent::Entered {
            direction: SpreadDirection::SellY,
            y_qty: dec!(5),
            x_qty: dec!(10),
            z: dec!(2),
            spread: dec!(1),
        };
        let report = driver.try_dispatch_legs_for_entry(&entered).await;
        assert!(!report.is_empty());
        assert!(report.all_succeeded(), "got {report:?}");
        let y_leg = report.y.as_ref().unwrap();
        let x_leg = report.x.as_ref().unwrap();
        // SellY → y_side=Sell, x_side=Buy.
        assert_eq!(y_leg.side, Side::Sell);
        assert_eq!(x_leg.side, Side::Buy);
        assert_eq!(y_leg.symbol, "BTCUSDT");
        assert_eq!(x_leg.symbol, "ETHUSDT");
        // Both mocks should have received exactly one placed order.
        assert_eq!(y_mock.placed_count(), 1);
        assert_eq!(x_mock.placed_count(), 1);
    }

    /// BuyY direction maps to buy-Y / sell-X at entry.
    #[tokio::test]
    async fn try_dispatch_legs_for_entry_buyy_direction() {
        let cfg = small_window_config();
        let (_y, _x, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        let entered = StatArbEvent::Entered {
            direction: SpreadDirection::BuyY,
            y_qty: dec!(5),
            x_qty: dec!(10),
            z: dec!(-2),
            spread: dec!(-1),
        };
        let report = driver.try_dispatch_legs_for_entry(&entered).await;
        assert!(report.all_succeeded());
        assert_eq!(report.y.as_ref().unwrap().side, Side::Buy);
        assert_eq!(report.x.as_ref().unwrap().side, Side::Sell);
    }

    /// try_dispatch_legs_for_entry on any non-Entered event is a no-op.
    #[tokio::test]
    async fn try_dispatch_legs_for_entry_is_noop_on_non_entered() {
        let cfg = small_window_config();
        let (y_mock, x_mock, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        let hold = StatArbEvent::Hold { z: dec!(0) };
        let report = driver.try_dispatch_legs_for_entry(&hold).await;
        assert!(report.is_empty());
        assert_eq!(y_mock.placed_count(), 0);
        assert_eq!(x_mock.placed_count(), 0);
    }

    /// Empty book on a leg produces a LegOutcome error; the
    /// other leg still dispatches.
    #[tokio::test]
    async fn entry_leg_error_on_empty_book() {
        let cfg = small_window_config();
        let (y_mock, x_mock, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        // Clear the Y book so ref-price lookup returns None.
        y_mock.clear_books();
        let entered = StatArbEvent::Entered {
            direction: SpreadDirection::SellY,
            y_qty: dec!(5),
            x_qty: dec!(10),
            z: dec!(2),
            spread: dec!(1),
        };
        let report = driver.try_dispatch_legs_for_entry(&entered).await;
        assert!(report.y.as_ref().unwrap().error.is_some());
        assert!(report.x.as_ref().unwrap().error.is_none());
        assert_eq!(y_mock.placed_count(), 0);
        assert_eq!(x_mock.placed_count(), 1);
        assert!(!report.all_succeeded());
    }

    /// Exit dispatch: after the driver emits `Exited`, the
    /// pending_exit_legs snapshot drives a pair of IOCs on the
    /// opposite sides vs entry.
    #[tokio::test]
    async fn try_dispatch_legs_for_exit_reverses_entry_sides() {
        // Set up a larger-window driver so we can drive it
        // through a real Entered → Exited round-trip.
        let cfg = StatArbDriverConfig {
            tick_interval: Duration::from_millis(10),
            zscore: ZScoreConfig {
                window: 20,
                entry_threshold: dec!(1.5),
                exit_threshold: dec!(0.3),
            },
            kalman_transition_var: dec!(0.000001),
            kalman_observation_var: dec!(0.001),
            leg_notional_usd: dec!(1000),
        };
        let window = cfg.zscore.window;
        let (y_mock, x_mock, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        seed_cointegrated(&mut driver);
        for _ in 0..window {
            y_mock.set_mid(dec!(200));
            x_mock.set_mid(dec!(100));
            driver.tick_once().await;
        }
        // Shock → Entered.
        y_mock.set_mid(dec!(205));
        let shock = driver.tick_once().await;
        assert!(matches!(shock, StatArbEvent::Entered { .. }));
        // Dispatch entry legs so the mocks have a baseline
        // place_order count to subtract from.
        let _entry_report = driver.try_dispatch_legs_for_entry(&shock).await;
        let entry_y_side = match shock {
            StatArbEvent::Entered { direction, .. } => entry_sides(direction).0,
            _ => unreachable!(),
        };
        let entry_count_y = y_mock.placed_count();
        let entry_count_x = x_mock.placed_count();

        // Drive Y back to 200 to force an exit.
        y_mock.set_mid(dec!(200));
        let mut saw_exit = false;
        for _ in 0..50 {
            let e = driver.tick_once().await;
            if matches!(e, StatArbEvent::Exited { .. }) {
                saw_exit = true;
                break;
            }
        }
        assert!(saw_exit, "expected an Exited event after revert");

        // Now dispatch exit legs — sides must be opposite of entry.
        let exit_report = driver.try_dispatch_legs_for_exit().await;
        assert!(!exit_report.is_empty());
        assert!(exit_report.all_succeeded(), "got {exit_report:?}");
        assert_eq!(
            exit_report.y.as_ref().unwrap().side,
            entry_y_side.opposite()
        );
        // Both mocks should see one more place_order than they
        // had at entry-dispatch time.
        assert_eq!(y_mock.placed_count(), entry_count_y + 1);
        assert_eq!(x_mock.placed_count(), entry_count_x + 1);
    }

    /// Exit dispatch with no pending legs (e.g. the engine
    /// called it speculatively without an Exited event) is a
    /// no-op.
    #[tokio::test]
    async fn try_dispatch_legs_for_exit_noop_without_pending_legs() {
        let cfg = small_window_config();
        let (y_mock, x_mock, _sink, mut driver) = make_driver(dec!(200), dec!(100), cfg);
        let report = driver.try_dispatch_legs_for_exit().await;
        assert!(report.is_empty());
        assert_eq!(y_mock.placed_count(), 0);
        assert_eq!(x_mock.placed_count(), 0);
    }

    #[test]
    fn estimate_pnl_sign_matches_direction() {
        let pos_sell = StatArbPosition {
            direction: SpreadDirection::SellY,
            y_qty: dec!(10),
            x_qty: dec!(20),
            spread_at_entry: dec!(5),
        };
        // Spread shrinks: SellY profits.
        assert_eq!(estimate_realised_pnl(&pos_sell, dec!(1)), dec!(40));

        let pos_buy = StatArbPosition {
            direction: SpreadDirection::BuyY,
            y_qty: dec!(10),
            x_qty: dec!(20),
            spread_at_entry: dec!(-3),
        };
        // Spread widens (becomes less negative): BuyY profits.
        assert_eq!(estimate_realised_pnl(&pos_buy, dec!(2)), dec!(50));
    }
