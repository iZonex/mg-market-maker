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

use mm_common::types::{OrderType, PriceLevel, Side, TimeInForce};
use mm_exchange_core::connector::{ExchangeConnector, NewOrder};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::watch;
use tracing::{info, warn};

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

/// Pair of sides + qtys captured on `Exited` so the
/// stage-2 dispatch path can flatten the legs without needing
/// the event itself to carry them. Populated inside
/// `evaluate_with_mids` right before the position is cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitLegs {
    pub y_side: Side,
    pub y_qty: Decimal,
    pub x_side: Side,
    pub x_qty: Decimal,
}

/// Per-leg outcome from a `try_dispatch_legs_*` call. Mirrors
/// the engine-side SOR `LegOutcome` shape so upstream audit /
/// alerts can normalise across both dispatch paths.
#[derive(Debug, Clone)]
pub struct LegOutcome {
    pub side: Side,
    pub symbol: String,
    pub target_qty: Decimal,
    pub dispatched_qty: Decimal,
    pub error: Option<String>,
}

/// Outcome of a single driver dispatch call — one per leg.
#[derive(Debug, Clone, Default)]
pub struct LegDispatchReport {
    pub y: Option<LegOutcome>,
    pub x: Option<LegOutcome>,
}

impl LegDispatchReport {
    pub fn empty() -> Self {
        Self { y: None, x: None }
    }

    pub fn is_empty(&self) -> bool {
        self.y.is_none() && self.x.is_none()
    }

    pub fn all_succeeded(&self) -> bool {
        let y_ok = self.y.as_ref().is_some_and(|l| l.error.is_none());
        let x_ok = self.x.as_ref().is_some_and(|l| l.error.is_none());
        y_ok && x_ok
    }
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
    /// Set the moment the driver emits `Exited` — captures
    /// the exit sides/qtys before the position is cleared.
    /// Consumed by [`Self::try_dispatch_legs_for_exit`].
    pending_exit_legs: Option<ExitLegs>,
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
            pending_exit_legs: None,
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
                // Capture the exit legs BEFORE the position
                // goes out of scope so the stage-2 dispatch
                // path has the sides / qtys to flatten. Y /
                // X exit sides are the *opposite* of the
                // entry sides — this is the "close" half
                // of the round-trip.
                let (entry_y, entry_x) = entry_sides(pos.direction);
                self.pending_exit_legs = Some(ExitLegs {
                    y_side: entry_y.opposite(),
                    y_qty: pos.y_qty,
                    x_side: entry_x.opposite(),
                    x_qty: pos.x_qty,
                });
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

    /// Stage-2 entry dispatch: fire a single IOC leg per side
    /// against each connector. Y-leg takes the spread direction
    /// (SellY → Sell, BuyY → Buy); X-leg trades the opposite
    /// side to achieve market-neutral exposure.
    ///
    /// Placements go through the raw connector `place_order`
    /// path with `TimeInForce::Ioc` — same semantics as
    /// `OrderManager::execute_unwind_slice` but without the
    /// engine-side `OrderManager` lifecycle tracking. The
    /// engine wires fill-side bookkeeping separately through
    /// the existing `MarketEvent::Fill` path.
    ///
    /// Returns a [`LegDispatchReport`] with one entry per leg.
    /// Failures on one leg do NOT abort the other — the driver
    /// is already book-neutral at sizing time, so a partial
    /// dispatch leaves a known residual that the engine can
    /// reconcile through the audit trail.
    pub async fn try_dispatch_legs_for_entry(&mut self, event: &StatArbEvent) -> LegDispatchReport {
        let StatArbEvent::Entered {
            direction,
            y_qty,
            x_qty,
            ..
        } = event
        else {
            return LegDispatchReport::empty();
        };
        let (y_side, x_side) = entry_sides(*direction);
        let y_outcome = place_ioc_leg(
            &self.y_connector,
            &self.pair.y_symbol,
            y_side,
            *y_qty,
            "stat_arb_entry_y",
        )
        .await;
        let x_outcome = place_ioc_leg(
            &self.x_connector,
            &self.pair.x_symbol,
            x_side,
            *x_qty,
            "stat_arb_entry_x",
        )
        .await;
        LegDispatchReport {
            y: Some(y_outcome),
            x: Some(x_outcome),
        }
    }

    /// Stage-2 exit dispatch: the mirror image of
    /// [`Self::try_dispatch_legs_for_entry`]. Uses the cached
    /// position (captured at entry) to figure out which side
    /// each leg was opened on, and fires the opposite side as
    /// an IOC to flatten.
    ///
    /// Call this on [`StatArbEvent::Exited`] — the driver has
    /// already cleared `self.position` inside `evaluate_with_mids`
    /// before emitting the event, so we take the pre-clear
    /// snapshot via the caller-supplied payload. v1 reconstructs
    /// the legs from the event payload + the last-known
    /// spread direction; stage-2b can tighten this by caching
    /// the closed position alongside the event.
    ///
    /// Since `StatArbEvent::Exited` does not carry the
    /// direction / per-leg qty (they live on the cached
    /// position which is wiped before emit), callers must
    /// supply the closing direction + qtys via
    /// [`StatArbDriver::last_exit_legs`]. That getter returns
    /// a snapshot captured the same tick the Exited event was
    /// emitted.
    pub async fn try_dispatch_legs_for_exit(&mut self) -> LegDispatchReport {
        let Some(legs) = self.pending_exit_legs.take() else {
            return LegDispatchReport::empty();
        };
        let y_outcome = place_ioc_leg(
            &self.y_connector,
            &self.pair.y_symbol,
            legs.y_side,
            legs.y_qty,
            "stat_arb_exit_y",
        )
        .await;
        let x_outcome = place_ioc_leg(
            &self.x_connector,
            &self.pair.x_symbol,
            legs.x_side,
            legs.x_qty,
            "stat_arb_exit_x",
        )
        .await;
        LegDispatchReport {
            y: Some(y_outcome),
            x: Some(x_outcome),
        }
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

/// Map a `SpreadDirection` to the `(y_side, x_side)` pair a
/// fresh entry uses. `SellY` → sell Y, buy X. `BuyY` → buy Y,
/// sell X. Exit reverses both sides.
fn entry_sides(direction: SpreadDirection) -> (Side, Side) {
    match direction {
        SpreadDirection::SellY => (Side::Sell, Side::Buy),
        SpreadDirection::BuyY => (Side::Buy, Side::Sell),
    }
}

/// Fire a single IOC limit slice against the given connector.
/// Used by both entry and exit dispatch — same shape, just
/// different labels. The reference price is pulled from the
/// venue's top of the matching side and then rounded to the
/// venue's `ProductSpec` via
/// [`ExchangeConnector::get_product_spec`].
///
/// On any error (book unavailable, product spec fetch failure,
/// `place_order` rejection), the returned [`LegOutcome`] carries
/// the error string and `dispatched_qty = 0`. No panics — a
/// single-leg failure is a known residual, not a fatal driver
/// fault.
async fn place_ioc_leg(
    connector: &Arc<dyn ExchangeConnector>,
    symbol: &str,
    side: Side,
    qty: Decimal,
    label: &str,
) -> LegOutcome {
    if qty <= dec!(0) {
        return LegOutcome {
            side,
            symbol: symbol.to_string(),
            target_qty: qty,
            dispatched_qty: dec!(0),
            error: Some("qty <= 0".to_string()),
        };
    }
    // 1. Pull product spec for rounding.
    let product = match connector.get_product_spec(symbol).await {
        Ok(p) => p,
        Err(e) => {
            warn!(%symbol, label, error = %e, "stat_arb leg: product spec fetch failed");
            return LegOutcome {
                side,
                symbol: symbol.to_string(),
                target_qty: qty,
                dispatched_qty: dec!(0),
                error: Some(format!("get_product_spec: {e}")),
            };
        }
    };
    // 2. Pull top-of-book for the taker-price reference.
    let (bids, asks, _) = match connector.get_orderbook(symbol, 1).await {
        Ok(ob) => ob,
        Err(e) => {
            warn!(%symbol, label, error = %e, "stat_arb leg: orderbook fetch failed");
            return LegOutcome {
                side,
                symbol: symbol.to_string(),
                target_qty: qty,
                dispatched_qty: dec!(0),
                error: Some(format!("get_orderbook: {e}")),
            };
        }
    };
    let ref_price = match side {
        Side::Buy => asks.first().map(|lvl| lvl.price),
        Side::Sell => bids.first().map(|lvl| lvl.price),
    };
    let Some(price) = ref_price else {
        return LegOutcome {
            side,
            symbol: symbol.to_string(),
            target_qty: qty,
            dispatched_qty: dec!(0),
            error: Some("top-of-book unavailable".to_string()),
        };
    };
    let rounded_qty = product.round_qty(qty);
    if rounded_qty.is_zero() {
        return LegOutcome {
            side,
            symbol: symbol.to_string(),
            target_qty: qty,
            dispatched_qty: dec!(0),
            error: Some("rounded qty is zero".to_string()),
        };
    }
    let rounded_price = product.round_price(price);
    let order = NewOrder {
        symbol: symbol.to_string(),
        side,
        order_type: OrderType::Limit,
        price: Some(rounded_price),
        qty: rounded_qty,
        time_in_force: Some(TimeInForce::Ioc),
        client_order_id: Some(format!("{label}-{}", uuid_like_stub(symbol))),
    };
    match connector.place_order(&order).await {
        Ok(order_id) => {
            info!(
                %symbol,
                ?side,
                %rounded_price,
                %rounded_qty,
                %order_id,
                label,
                "stat_arb leg placed"
            );
            LegOutcome {
                side,
                symbol: symbol.to_string(),
                target_qty: qty,
                dispatched_qty: rounded_qty,
                error: None,
            }
        }
        Err(e) => {
            warn!(%symbol, label, error = %e, "stat_arb leg: place_order failed");
            LegOutcome {
                side,
                symbol: symbol.to_string(),
                target_qty: qty,
                dispatched_qty: dec!(0),
                error: Some(format!("place_order: {e}")),
            }
        }
    }
}

/// Best-effort deterministic client-order-id suffix. The driver
/// currently has no access to a shared id generator so it
/// mixes the symbol into a compact stable prefix. Production
/// deployments can extend this to an engine-side uuid via a
/// callback if needed.
fn uuid_like_stub(symbol: &str) -> String {
    format!("{symbol}-{}", chrono::Utc::now().timestamp_millis())
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
}
