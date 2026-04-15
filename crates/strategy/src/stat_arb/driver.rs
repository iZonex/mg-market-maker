//! Stat-arb driver scaffolding (Epic B, sub-component #4, partial).
//!
//! Composes [`EngleGrangerTest`], [`KalmanHedgeRatio`], and
//! [`ZScoreSignal`] into a single tick state machine that
//! subscribes to two mid-price feeds and emits a
//! [`StatArbEvent`] per tick.
//!
//! Mirrors the `FundingArbDriver` pattern from v0.2.0 Sprint H:
//! a standalone async task spawned by the engine, tick-interval
//! wake-up, `StatArbEventSink` for routing events to audit /
//! metrics / PnL without pulling those crates into
//! `mm-strategy`.
//!
//! Sprint scope (B-3): state machine + sink + tick loop. Engine
//! wiring (`MarketMakerEngine::with_stat_arb_driver`), audit
//! events, and real per-pair PnL dispatch land in Sprint B-4.

use std::sync::Arc;
use std::time::Duration;

use mm_common::types::PriceLevel;
use mm_exchange_core::connector::ExchangeConnector;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::watch;
use tracing::info;

use super::cointegration::{CointegrationResult, EngleGrangerTest};
use super::kalman::KalmanHedgeRatio;
use super::signal::{SignalAction, SpreadDirection, ZScoreConfig, ZScoreSignal};

/// The two-leg pair the driver trades. `y` is the "dependent"
/// leg (the one the regression predicts), `x` is the independent
/// leg (the hedge ratio applies to it).
#[derive(Debug, Clone)]
pub struct StatArbPair {
    pub y_symbol: String,
    pub x_symbol: String,
    /// Per-strategy PnL bucket key — e.g.
    /// `"stat_arb_BTCUSDT_ETHUSDT"`. Epic C's per-strategy
    /// labeling accepts arbitrary strings so this value flows
    /// straight into `Portfolio::on_fill` in Sprint B-4.
    pub strategy_class: String,
}

/// Runtime tuning knobs.
#[derive(Debug, Clone)]
pub struct StatArbDriverConfig {
    /// How often the driver's tick loop runs. Default 60 s.
    pub tick_interval: Duration,
    /// Z-score window + hysteresis bands.
    pub zscore: ZScoreConfig,
    /// Kalman transition variance `Q`. Default 1e-6.
    pub kalman_transition_var: Decimal,
    /// Kalman observation variance `R`. Default 1e-3.
    pub kalman_observation_var: Decimal,
    /// Notional USD to commit to the Y leg at entry. X leg is
    /// sized as `β · y_qty` for book-neutral exposure.
    pub leg_notional_usd: Decimal,
}

impl Default for StatArbDriverConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(60),
            zscore: ZScoreConfig::default(),
            kalman_transition_var: dec!(0.000001),
            kalman_observation_var: dec!(0.001),
            leg_notional_usd: dec!(1000),
        }
    }
}

/// Cached open position — the driver uses this to reverse sides
/// on exit and to compute the running spread-at-entry baseline
/// for PnL attribution.
#[derive(Debug, Clone, Copy)]
pub struct StatArbPosition {
    pub direction: SpreadDirection,
    pub y_qty: Decimal,
    pub x_qty: Decimal,
    pub spread_at_entry: Decimal,
}

/// Event emitted by the driver after each tick. The engine-side
/// sink routes these to audit / metrics / PnL dispatch in
/// Sprint B-4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatArbEvent {
    /// A position was opened.
    Entered {
        direction: SpreadDirection,
        y_qty: Decimal,
        x_qty: Decimal,
        z: Decimal,
        spread: Decimal,
    },
    /// The open position was closed.
    Exited {
        z: Decimal,
        spread: Decimal,
        realised_pnl_estimate: Decimal,
    },
    /// Z-score within the dead band, or staying inside the
    /// hysteresis gap while in position.
    Hold { z: Decimal },
    /// The cached cointegration result is missing or rejected —
    /// driver refuses to enter. Existing positions are held
    /// through (no forced exit).
    NotCointegrated { adf_stat: Option<Decimal> },
    /// Z-score window still warming up.
    Warmup { samples: usize, required: usize },
    /// Either connector returned no usable top-of-book.
    InputUnavailable { reason: String },
}

/// Caller sink for events. Same shape as `funding_arb_driver::DriverEventSink`
/// but typed for stat-arb events.
pub trait StatArbEventSink: Send + Sync {
    fn on_event(&self, event: StatArbEvent);
}

/// No-op sink for tests that don't care about event routing.
pub struct NullStatArbSink;
impl StatArbEventSink for NullStatArbSink {
    fn on_event(&self, _event: StatArbEvent) {}
}

/// The driver itself.
pub struct StatArbDriver {
    y_connector: Arc<dyn ExchangeConnector>,
    x_connector: Arc<dyn ExchangeConnector>,
    pair: StatArbPair,
    config: StatArbDriverConfig,
    sink: Arc<dyn StatArbEventSink>,
    kalman: KalmanHedgeRatio,
    signal: ZScoreSignal,
    cointegration: Option<CointegrationResult>,
    position: Option<StatArbPosition>,
}

impl StatArbDriver {
    pub fn new(
        y_connector: Arc<dyn ExchangeConnector>,
        x_connector: Arc<dyn ExchangeConnector>,
        pair: StatArbPair,
        config: StatArbDriverConfig,
        sink: Arc<dyn StatArbEventSink>,
    ) -> Self {
        let kalman =
            KalmanHedgeRatio::new(config.kalman_transition_var, config.kalman_observation_var);
        let signal = ZScoreSignal::new(config.zscore.clone());
        Self {
            y_connector,
            x_connector,
            pair,
            config,
            sink,
            kalman,
            signal,
            cointegration: None,
            position: None,
        }
    }

    /// Re-run the Engle-Granger test against caller-supplied
    /// historical price series. Typically invoked on a slow
    /// cadence by the engine (default: every 60 min). Caches
    /// the result and, on the first successful pass, seeds the
    /// Kalman filter with the OLS β.
    pub fn recheck_cointegration(&mut self, y_series: &[Decimal], x_series: &[Decimal]) {
        let Some(result) = EngleGrangerTest::run(y_series, x_series) else {
            return;
        };
        let is_first_pass = self.cointegration.is_none();
        let accepted = result.is_cointegrated;
        if is_first_pass && accepted {
            // Seed Kalman with the OLS hedge ratio so the filter
            // starts near the truth instead of at β=1.
            self.kalman = KalmanHedgeRatio::with_initial_beta(
                result.beta,
                self.config.kalman_transition_var,
                self.config.kalman_observation_var,
            );
        }
        self.cointegration = Some(result);
    }

    /// Fetch both mids and run one tick. Emits one event and
    /// routes it through the sink.
    pub async fn tick_once(&mut self) -> StatArbEvent {
        let event = match self.sample_mids().await {
            Some((y_mid, x_mid)) => self.evaluate_with_mids(y_mid, x_mid),
            None => StatArbEvent::InputUnavailable {
                reason: "top-of-book unavailable for y or x".to_string(),
            },
        };
        self.sink.on_event(event.clone());
        event
    }

    /// Sync core: given pre-fetched mids, advance the state
    /// machine and return the event. Used directly by tests and
    /// internally by [`Self::tick_once`].
    pub fn evaluate_with_mids(&mut self, y_mid: Decimal, x_mid: Decimal) -> StatArbEvent {
        // 1. Kalman update → latest β.
        let beta = self.kalman.update(y_mid, x_mid);
        let spread = y_mid - beta * x_mid;

        // 2. Push spread into the z-score window.
        let z = match self.signal.update(spread) {
            Some(z) => z,
            None => {
                return StatArbEvent::Warmup {
                    samples: self.signal.sample_count(),
                    required: self.signal.window(),
                };
            }
        };

        // 3. Cointegration gate — entry only. If a position is
        //    already open, we pass through to `decide` so a Close
        //    still fires even after cointegration broke down.
        let in_position = self.position.is_some();
        let cointegrated = self
            .cointegration
            .as_ref()
            .map(|c| c.is_cointegrated)
            .unwrap_or(false);
        if !cointegrated && !in_position {
            return StatArbEvent::NotCointegrated {
                adf_stat: self.cointegration.as_ref().map(|c| c.adf_statistic),
            };
        }

        // 4. Signal decision.
        match self.signal.decide(z, in_position) {
            SignalAction::Open { direction, .. } => {
                let (y_qty, x_qty) = self.size_legs(beta, y_mid, x_mid);
                self.position = Some(StatArbPosition {
                    direction,
                    y_qty,
                    x_qty,
                    spread_at_entry: spread,
                });
                info!(
                    ?direction, %y_qty, %x_qty, %z, %spread,
                    "stat_arb entered"
                );
                StatArbEvent::Entered {
                    direction,
                    y_qty,
                    x_qty,
                    z,
                    spread,
                }
            }
            SignalAction::Close { .. } => {
                let pos = self.position.take().expect("Close only when in_position");
                let pnl = estimate_realised_pnl(&pos, spread);
                info!(%z, %spread, %pnl, "stat_arb exited");
                StatArbEvent::Exited {
                    z,
                    spread,
                    realised_pnl_estimate: pnl,
                }
            }
            SignalAction::Hold { .. } => StatArbEvent::Hold { z },
        }
    }

    /// Run the async tick loop until `shutdown_rx` fires.
    pub async fn run(mut self, mut shutdown_rx: watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(self.config.tick_interval);
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("stat_arb driver received shutdown");
                        return;
                    }
                }
                _ = interval.tick() => {
                    self.tick_once().await;
                }
            }
        }
    }

    /// Compute per-leg quantities from the current β and mid
    /// prices. Y-leg is sized at `leg_notional_usd / y_mid` and
    /// the X-leg is sized as `β · y_qty` for book-neutral
    /// exposure.
    fn size_legs(&self, beta: Decimal, y_mid: Decimal, _x_mid: Decimal) -> (Decimal, Decimal) {
        if y_mid.is_zero() {
            return (Decimal::ZERO, Decimal::ZERO);
        }
        let y_qty = self.config.leg_notional_usd / y_mid;
        let x_qty = beta * y_qty;
        (y_qty, x_qty)
    }

    async fn sample_mids(&self) -> Option<(Decimal, Decimal)> {
        let (y_bids, y_asks, _) = self
            .y_connector
            .get_orderbook(&self.pair.y_symbol, 1)
            .await
            .ok()?;
        let (x_bids, x_asks, _) = self
            .x_connector
            .get_orderbook(&self.pair.x_symbol, 1)
            .await
            .ok()?;
        let y_mid = mid_of(&y_bids, &y_asks)?;
        let x_mid = mid_of(&x_bids, &x_asks)?;
        Some((y_mid, x_mid))
    }

    /// Read-only accessors — useful for engine-side metrics /
    /// dashboards without exposing mutable state.
    pub fn current_beta(&self) -> Decimal {
        self.kalman.current_beta()
    }
    pub fn position(&self) -> Option<&StatArbPosition> {
        self.position.as_ref()
    }
    pub fn cointegration(&self) -> Option<&CointegrationResult> {
        self.cointegration.as_ref()
    }
    pub fn pair(&self) -> &StatArbPair {
        &self.pair
    }
}

fn mid_of(bids: &[PriceLevel], asks: &[PriceLevel]) -> Option<Decimal> {
    let bid = bids.first()?;
    let ask = asks.first()?;
    Some((bid.price + ask.price) / dec!(2))
}

/// Per-unit spread change × y_qty, signed by position direction.
/// Placeholder for Sprint B-4 real attribution — v1 is an
/// operator-facing estimate only.
fn estimate_realised_pnl(pos: &StatArbPosition, spread_exit: Decimal) -> Decimal {
    let delta = spread_exit - pos.spread_at_entry;
    match pos.direction {
        // SellY entered at high spread → profit when spread shrinks.
        SpreadDirection::SellY => -delta * pos.y_qty,
        // BuyY entered at low spread → profit when spread widens.
        SpreadDirection::BuyY => delta * pos.y_qty,
    }
}

#[cfg(test)]
mod tests {
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
                },
                bids: Mutex::new(vec![PriceLevel {
                    price: mid - dec!(1),
                    qty: dec!(10),
                }]),
                asks: Mutex::new(vec![PriceLevel {
                    price: mid + dec!(1),
                    qty: dec!(10),
                }]),
            }
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
        async fn place_order(&self, _order: &CoreNewOrder) -> anyhow::Result<OrderId> {
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
}
