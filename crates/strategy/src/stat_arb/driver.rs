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
        // Stat-arb legs can both open AND close positions depending
        // on the signal direction. Without a per-dispatch signal
        // flag here we default to false (allow opening). Wiring
        // "close vs open" intent through is a follow-up; in the
        // meantime an accidental flip is caught by the pair-break
        // detector.
        reduce_only: false,
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
mod tests;
