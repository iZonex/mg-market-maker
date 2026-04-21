//! Funding-arbitrage executor — atomic pair dispatcher that
//! materialises a `FundingArbEngine` signal into two matched
//! orders on the primary (spot) and hedge (perp) legs.
//!
//! Sprint H1 scope: standalone component with unit-test
//! coverage of every dispatch permutation. Engine wiring and
//! the periodic `evaluate → dispatch` loop land in Sprint H2.
//!
//! # Atomic pair dispatch (AD-8)
//!
//! Cross-product arb positions are worthless the moment the two
//! legs go out of sync: a filled spot buy without a corresponding
//! perp short converts a delta-neutral funding trade into a
//! naked long. The executor's job is to make that window as
//! small as possible and to clean up deterministically when
//! something fails.
//!
//! ```text
//! 1. Place the MARKET-TAKE leg first (shorter confirmation
//!    latency — taker orders return a fill/reject from the
//!    matching engine in one RTT, maker-post orders may rest).
//! 2. If (1) rejects: the position is still flat → nothing to
//!    unwind, return a clean Err.
//! 3. Place the MAKER-POST leg second. Post-only, GTC.
//! 4. If (3) rejects: the taker leg already filled, so we are
//!    delta-exposed. Fire a COMPENSATING market order in the
//!    REVERSE direction on the hedge venue to flatten. Log a
//!    `PairBreak` to the audit trail regardless of whether the
//!    compensating order succeeds — the operator must see both
//!    the break and the recovery attempt.
//! ```
//!
//! The asymmetry (market first, maker second) also matches the
//! natural risk profile of the funding-arb position: the hedge
//! leg carries the perpetual funding exposure; the spot leg is
//! the slower, more conservative side to build up.
//!
//! # What this file does NOT do
//!
//! - Query funding rates from the venue — the caller passes in a
//!   `FundingSignal`. Sprint H2 adds a periodic loop that reads
//!   `hedge_connector.get_funding_rate(symbol)` and feeds the
//!   result into `FundingArbEngine::evaluate`.
//! - Update the `FundingArbState` on fill — Sprint H2 wires
//!   `on_entry` / `on_exit` into the engine's `handle_ws_event`.
//! - Touch the kill switch. Broken pairs raise the kill level to
//!   `L2 StopNewOrders` in Sprint H2.

use std::sync::Arc;

use mm_common::types::{InstrumentPair, OrderId, OrderType, Side, TimeInForce};
use mm_exchange_core::connector::{ExchangeConnector, NewOrder};
use mm_persistence::funding::{FundingSignal, PerpAction, SpotAction};
use rust_decimal::Decimal;
use thiserror::Error;
use tracing::{error, info, warn};

/// Which leg of a pair dispatch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedLeg {
    /// The market-take (hedge) leg was rejected. The position is
    /// still flat — nothing to clean up.
    Taker,
    /// The maker-post (primary) leg was rejected after the taker
    /// leg already filled. The executor fired a compensating
    /// reversal; check `compensated` to see whether it succeeded.
    Maker,
}

/// Rich error type for pair dispatches. The executor turns
/// per-leg connector errors into this shape so the caller (engine
/// loop / audit trail) can distinguish clean failures (taker leg
/// rejected before anything committed) from pair breaks
/// (delta-exposed until the compensating order lands).
#[derive(Debug, Error)]
pub enum PairLegError {
    /// The taker leg was rejected before any position was opened.
    /// Safe to retry.
    #[error("taker leg rejected: {reason}")]
    TakerRejected { reason: String },

    /// The maker leg was rejected after the taker leg already
    /// filled. The executor fired a compensating market order in
    /// the reverse direction to flatten. `compensated` is `true`
    /// iff that compensating order reported success.
    #[error("maker leg rejected after taker filled (compensated={compensated}): {reason}")]
    PairBreak { reason: String, compensated: bool },
}

/// Successful dispatch outcome — both legs placed.
#[derive(Debug, Clone)]
pub struct PairDispatchOutcome {
    /// Order ID returned by the hedge venue for the taker leg.
    pub taker_order_id: OrderId,
    /// Order ID returned by the primary venue for the maker leg.
    pub maker_order_id: OrderId,
    /// Side placed on the primary (spot) leg.
    pub spot_side: Side,
    /// Side placed on the hedge (perp) leg.
    pub perp_side: Side,
    /// Size dispatched on both legs (in primary base units —
    /// hedge size = `size * pair.multiplier`).
    pub size: Decimal,
}

/// Executor that turns a `FundingSignal` into two matched orders.
///
/// The executor is stateless between calls — it holds references
/// to the two connectors and the `InstrumentPair` metadata but no
/// internal state. Sprint H2 adds the `FundingArbEngine` state
/// machine around it.
pub struct FundingArbExecutor {
    primary: Arc<dyn ExchangeConnector>,
    hedge: Arc<dyn ExchangeConnector>,
    pair: InstrumentPair,
}

impl FundingArbExecutor {
    pub fn new(
        primary: Arc<dyn ExchangeConnector>,
        hedge: Arc<dyn ExchangeConnector>,
        pair: InstrumentPair,
    ) -> Self {
        Self {
            primary,
            hedge,
            pair,
        }
    }

    pub fn pair(&self) -> &InstrumentPair {
        &self.pair
    }

    /// Dispatch a `FundingSignal::Enter` as an atomic pair.
    ///
    /// Returns a `PairDispatchOutcome` with both order IDs on
    /// success, or a `PairLegError` on failure. The error
    /// variant tells the caller whether the failure left the
    /// position flat (`TakerRejected`) or delta-exposed with a
    /// compensating reversal already attempted (`PairBreak`).
    pub async fn enter(&self, signal: &FundingSignal) -> Result<PairDispatchOutcome, PairLegError> {
        let FundingSignal::Enter {
            spot_side,
            perp_side,
            size,
        } = signal
        else {
            return Err(PairLegError::TakerRejected {
                reason: "non-enter signal passed to enter()".to_string(),
            });
        };

        let spot_side_mm = spot_action_to_side(spot_side);
        let perp_side_mm = perp_action_to_side(perp_side);
        let size = *size;

        self.dispatch(spot_side_mm, perp_side_mm, size).await
    }

    /// Dispatch a `FundingSignal::Exit` as the reverse-direction
    /// atomic pair. Caller passes in the direction of the
    /// currently open position; the executor flips each leg.
    pub async fn exit(
        &self,
        open_spot_side: Side,
        open_perp_side: Side,
        size: Decimal,
    ) -> Result<PairDispatchOutcome, PairLegError> {
        self.dispatch(open_spot_side.opposite(), open_perp_side.opposite(), size)
            .await
    }

    /// Core atomic-pair dispatcher. Market-take on the hedge leg
    /// first, maker-post on the primary leg second, compensating
    /// reversal on maker rejection.
    async fn dispatch(
        &self,
        spot_side: Side,
        perp_side: Side,
        size: Decimal,
    ) -> Result<PairDispatchOutcome, PairLegError> {
        let hedge_size = size * self.pair.multiplier;

        // --- 1. Market-take on hedge leg ---
        let taker_order = NewOrder {
            symbol: self.pair.hedge_symbol.clone(),
            side: perp_side,
            order_type: OrderType::Market,
            price: None,
            qty: hedge_size,
            time_in_force: Some(TimeInForce::Ioc),
            client_order_id: None,
            // Opening hedge leg — perp bucket may be flat. Leave
            // reduce_only off so the order is accepted.
            reduce_only: false,
        };

        let taker_order_id = match self.hedge.place_order(&taker_order).await {
            Ok(id) => {
                info!(
                    %id,
                    symbol = %self.pair.hedge_symbol,
                    side = ?perp_side,
                    qty = %hedge_size,
                    "pair dispatch: taker leg placed"
                );
                id
            }
            Err(e) => {
                warn!(error = %e, "pair dispatch: taker leg rejected — position stays flat");
                return Err(PairLegError::TakerRejected {
                    reason: e.to_string(),
                });
            }
        };

        // --- 2. Maker-post on primary leg ---
        let maker_order = NewOrder {
            symbol: self.pair.primary_symbol.clone(),
            side: spot_side,
            order_type: OrderType::Limit,
            price: None, // Caller's strategy sets the level via a different code path; this
            qty: size,
            time_in_force: Some(TimeInForce::PostOnly),
            client_order_id: None,
            // Spot-side maker — reduce_only is a perp-only flag,
            // leave off.
            reduce_only: false,
        };

        match self.primary.place_order(&maker_order).await {
            Ok(maker_order_id) => {
                info!(
                    %maker_order_id,
                    symbol = %self.pair.primary_symbol,
                    side = ?spot_side,
                    qty = %size,
                    "pair dispatch: maker leg placed"
                );
                Ok(PairDispatchOutcome {
                    taker_order_id,
                    maker_order_id,
                    spot_side,
                    perp_side,
                    size,
                })
            }
            Err(maker_err) => {
                // PAIR BREAK — taker leg already filled, maker leg
                // rejected. Flatten by firing a compensating market
                // order in the reverse direction on the hedge leg.
                error!(
                    error = %maker_err,
                    "pair dispatch: maker leg REJECTED after taker fill — firing compensation"
                );

                let compensation = NewOrder {
                    symbol: self.pair.hedge_symbol.clone(),
                    side: perp_side.opposite(),
                    order_type: OrderType::Market,
                    price: None,
                    qty: hedge_size,
                    time_in_force: Some(TimeInForce::Ioc),
                    client_order_id: None,
                    // Compensation fires ONLY after the taker leg
                    // already filled — the bucket has an open
                    // position we need to unwind. reduce_only
                    // guards against racing back into fresh
                    // exposure if the market ran away between
                    // the original fill and this retry.
                    reduce_only: true,
                };

                // S1.1 — retry the compensation with exponential
                // backoff before declaring the leg naked. Without
                // the retry loop a single transient venue hiccup
                // (429, short-lived disconnect, rate limit) left us
                // with an unhedged perp fill for however long it
                // took the operator to notice. Three attempts at
                // 100 / 200 / 400 ms covers the common transient
                // classes without turning a real outage into a
                // second-long hang on the engine's select loop.
                let compensated = place_with_retry(
                    self.hedge.as_ref(),
                    &compensation,
                    &[100, 200, 400],
                )
                .await;

                Err(PairLegError::PairBreak {
                    reason: maker_err.to_string(),
                    compensated,
                })
            }
        }
    }
}

/// S1.1 — place `order` on `connector` with exponential-backoff
/// retry. Returns `true` iff any attempt succeeds; `false` once
/// every retry has been exhausted. Each delay is in milliseconds
/// and fires BEFORE the subsequent attempt, so `[100, 200, 400]`
/// means "try once, sleep 100ms, try again, sleep 200ms, try
/// again, sleep 400ms, try one final time" for a total of 4
/// attempts. Logs every failure with the attempt counter so
/// audit trails can reconstruct the retry timeline.
async fn place_with_retry(
    connector: &dyn ExchangeConnector,
    order: &NewOrder,
    backoff_ms: &[u64],
) -> bool {
    let total_attempts = backoff_ms.len() + 1;
    for attempt in 0..=backoff_ms.len() {
        if attempt > 0 {
            let delay = backoff_ms[attempt - 1];
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        match connector.place_order(order).await {
            Ok(id) => {
                if attempt == 0 {
                    info!(
                        %id,
                        "pair dispatch: compensating reversal placed successfully"
                    );
                } else {
                    warn!(
                        %id,
                        attempt = attempt + 1,
                        total = total_attempts,
                        "pair dispatch: compensation succeeded on retry"
                    );
                }
                return true;
            }
            Err(e) => {
                error!(
                    error = %e,
                    attempt = attempt + 1,
                    total = total_attempts,
                    "pair dispatch: compensation attempt failed"
                );
            }
        }
    }
    error!(
        attempts = total_attempts,
        "pair dispatch: COMPENSATION EXHAUSTED — naked leg, manual intervention required"
    );
    false
}

fn spot_action_to_side(a: &SpotAction) -> Side {
    match a {
        SpotAction::Buy => Side::Buy,
        SpotAction::Sell => Side::Sell,
    }
}

fn perp_action_to_side(a: &PerpAction) -> Side {
    match a {
        PerpAction::Long => Side::Buy,
        PerpAction::Short => Side::Sell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use mm_common::types::{Balance, LiveOrder, PriceLevel, ProductSpec, WalletType};
    use mm_exchange_core::connector::{VenueCapabilities, VenueId, VenueProduct};
    use mm_exchange_core::events::MarketEvent;
    use rust_decimal_macros::dec;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    /// Outcome a mock leg returns for the next `place_order` call.
    #[derive(Clone)]
    enum LegBehaviour {
        Ok,
        Err(String),
    }

    struct MockLeg {
        venue: VenueId,
        product: VenueProduct,
        caps: VenueCapabilities,
        /// Queue of behaviours, popped left-to-right. If empty, `Ok`.
        behaviours: Mutex<Vec<LegBehaviour>>,
        pub placed: Mutex<Vec<NewOrder>>,
    }

    impl MockLeg {
        fn new(venue: VenueId, product: VenueProduct) -> Self {
            Self {
                venue,
                product,
                caps: VenueCapabilities {
                    max_batch_size: 20,
                    supports_amend: false,
                    supports_ws_trading: false,
                    supports_fix: false,
                    max_order_rate: 100,
                    supports_funding_rate: product.has_funding(),
                    supports_margin_info: false,
                supports_margin_mode: false,
            supports_liquidation_feed: false,
            supports_set_leverage: false,
                            },
                behaviours: Mutex::new(vec![]),
                placed: Mutex::new(vec![]),
            }
        }

        fn queue(&self, b: LegBehaviour) {
            self.behaviours.lock().unwrap().push(b);
        }
    }

    #[async_trait]
    impl ExchangeConnector for MockLeg {
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
            Ok((vec![], vec![], 0))
        }
        async fn place_order(&self, order: &NewOrder) -> anyhow::Result<OrderId> {
            self.placed.lock().unwrap().push(order.clone());
            let next = {
                let mut q = self.behaviours.lock().unwrap();
                if q.is_empty() {
                    None
                } else {
                    Some(q.remove(0))
                }
            };
            match next {
                Some(LegBehaviour::Err(msg)) => Err(anyhow::anyhow!(msg)),
                _ => Ok(OrderId::new_v4()),
            }
        }
        async fn place_orders_batch(&self, _orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>> {
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

    fn pair() -> InstrumentPair {
        InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTCUSDT".to_string(),
            multiplier: dec!(1),
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(20),
        }
    }

    fn setup() -> (Arc<MockLeg>, Arc<MockLeg>, FundingArbExecutor) {
        let primary = Arc::new(MockLeg::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockLeg::new(VenueId::Binance, VenueProduct::LinearPerp));
        let exec = FundingArbExecutor::new(
            primary.clone() as Arc<dyn ExchangeConnector>,
            hedge.clone() as Arc<dyn ExchangeConnector>,
            pair(),
        );
        (primary, hedge, exec)
    }

    fn enter_signal() -> FundingSignal {
        FundingSignal::Enter {
            spot_side: SpotAction::Buy,
            perp_side: PerpAction::Short,
            size: dec!(0.1),
        }
    }

    #[tokio::test]
    async fn happy_path_places_both_legs_in_correct_order() {
        let (primary, hedge, exec) = setup();

        let outcome = exec.enter(&enter_signal()).await.unwrap();
        assert_eq!(outcome.spot_side, Side::Buy);
        assert_eq!(outcome.perp_side, Side::Sell);
        assert_eq!(outcome.size, dec!(0.1));

        let primary_orders = primary.placed.lock().unwrap();
        let hedge_orders = hedge.placed.lock().unwrap();
        assert_eq!(hedge_orders.len(), 1, "exactly one hedge order");
        assert_eq!(primary_orders.len(), 1, "exactly one primary order");

        // Taker leg is market IOC.
        assert_eq!(hedge_orders[0].order_type, OrderType::Market);
        assert_eq!(hedge_orders[0].time_in_force, Some(TimeInForce::Ioc));
        assert_eq!(hedge_orders[0].side, Side::Sell);

        // Maker leg is limit post-only.
        assert_eq!(primary_orders[0].order_type, OrderType::Limit);
        assert_eq!(primary_orders[0].time_in_force, Some(TimeInForce::PostOnly));
        assert_eq!(primary_orders[0].side, Side::Buy);
    }

    #[tokio::test]
    async fn taker_leg_rejection_leaves_position_flat() {
        let (primary, hedge, exec) = setup();
        hedge.queue(LegBehaviour::Err("insufficient margin".into()));

        let err = exec.enter(&enter_signal()).await.unwrap_err();
        match err {
            PairLegError::TakerRejected { reason } => {
                assert!(reason.contains("insufficient margin"));
            }
            _ => panic!("expected TakerRejected"),
        }

        // Primary leg must NEVER have been touched.
        assert_eq!(primary.placed.lock().unwrap().len(), 0);
        // Only the failed taker attempt was sent — no compensation.
        assert_eq!(hedge.placed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn maker_leg_rejection_fires_compensation_and_reports_pair_break() {
        let (primary, hedge, exec) = setup();
        // Taker leg: ok. Maker leg: rejected. Compensation: ok.
        primary.queue(LegBehaviour::Err("post-only would cross".into()));

        let err = exec.enter(&enter_signal()).await.unwrap_err();
        match err {
            PairLegError::PairBreak {
                reason,
                compensated,
            } => {
                assert!(reason.contains("post-only would cross"));
                assert!(compensated, "compensation must have succeeded");
            }
            _ => panic!("expected PairBreak"),
        }

        // Hedge leg saw: 1 entry attempt + 1 compensation (reverse).
        let hedge_orders = hedge.placed.lock().unwrap();
        assert_eq!(hedge_orders.len(), 2);
        assert_eq!(hedge_orders[0].side, Side::Sell, "entry taker");
        assert_eq!(hedge_orders[1].side, Side::Buy, "compensation reverses");
        assert_eq!(hedge_orders[1].order_type, OrderType::Market);

        // Primary saw exactly the one failed attempt.
        assert_eq!(primary.placed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn maker_rejection_with_failed_compensation_is_reported_as_uncompensated() {
        let (primary, hedge, exec) = setup();
        // Taker ok (default), maker fail, all 4 compensation
        // attempts (initial + 3 retries per S1.1) fail.
        primary.queue(LegBehaviour::Err("post-only would cross".into()));
        hedge.queue(LegBehaviour::Ok); // Taker leg (entry).
        for _ in 0..4 {
            hedge.queue(LegBehaviour::Err("venue down".into()));
        }

        let err = exec.enter(&enter_signal()).await.unwrap_err();
        match err {
            PairLegError::PairBreak {
                reason,
                compensated,
            } => {
                assert!(reason.contains("post-only"));
                assert!(!compensated, "compensation must have failed");
            }
            _ => panic!("expected PairBreak"),
        }
        // Entry taker (1) + 4 compensation attempts = 5 total.
        assert_eq!(hedge.placed.lock().unwrap().len(), 5);
    }

    /// S1.1 — naked-leg retry. First compensation attempt fails
    /// transiently, the retry succeeds, so the driver reports
    /// `compensated=true` and no naked position survives.
    #[tokio::test]
    async fn compensation_retry_succeeds_on_second_attempt() {
        let (primary, hedge, exec) = setup();
        primary.queue(LegBehaviour::Err("post-only would cross".into()));
        hedge.queue(LegBehaviour::Ok); // Taker leg.
        hedge.queue(LegBehaviour::Err("429 rate limit".into())); // 1st comp attempt.
        hedge.queue(LegBehaviour::Ok); // 2nd comp attempt — success.

        let err = exec.enter(&enter_signal()).await.unwrap_err();
        match err {
            PairLegError::PairBreak { compensated, .. } => {
                assert!(compensated, "retry must flip compensated to true");
            }
            _ => panic!("expected PairBreak"),
        }
        // Entry (1) + 2 compensation attempts.
        assert_eq!(hedge.placed.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn exit_reverses_both_legs() {
        let (primary, hedge, exec) = setup();

        // Open position: long spot / short perp. Exit = sell spot + buy perp.
        exec.exit(Side::Buy, Side::Sell, dec!(0.1)).await.unwrap();

        let hedge_orders = hedge.placed.lock().unwrap();
        let primary_orders = primary.placed.lock().unwrap();
        assert_eq!(hedge_orders[0].side, Side::Buy, "perp exit buys back");
        assert_eq!(primary_orders[0].side, Side::Sell, "spot exit sells");
    }

    #[tokio::test]
    async fn non_enter_signal_is_rejected() {
        let (_, _, exec) = setup();
        let err = exec.enter(&FundingSignal::Hold).await.unwrap_err();
        assert!(matches!(err, PairLegError::TakerRejected { .. }));
    }

    #[tokio::test]
    async fn multiplier_scales_hedge_size() {
        let primary = Arc::new(MockLeg::new(VenueId::Binance, VenueProduct::Spot));
        let hedge = Arc::new(MockLeg::new(VenueId::Binance, VenueProduct::LinearPerp));
        let pair = InstrumentPair {
            primary_symbol: "BTCUSDT".to_string(),
            hedge_symbol: "BTCUSDT-PERP".to_string(),
            multiplier: dec!(10), // 1 spot unit = 10 perp contracts.
            funding_interval_secs: Some(28_800),
            basis_threshold_bps: dec!(20),
        };
        let exec = FundingArbExecutor::new(
            primary.clone() as Arc<dyn ExchangeConnector>,
            hedge.clone() as Arc<dyn ExchangeConnector>,
            pair,
        );

        exec.enter(&enter_signal()).await.unwrap();

        let hedge_orders = hedge.placed.lock().unwrap();
        let primary_orders = primary.placed.lock().unwrap();
        assert_eq!(primary_orders[0].qty, dec!(0.1), "spot qty unchanged");
        assert_eq!(
            hedge_orders[0].qty,
            dec!(1),
            "perp qty = spot qty * multiplier"
        );
    }

    #[tokio::test]
    async fn negative_funding_signal_produces_sell_spot_long_perp() {
        let (primary, hedge, exec) = setup();
        let signal = FundingSignal::Enter {
            spot_side: SpotAction::Sell,
            perp_side: PerpAction::Long,
            size: dec!(0.05),
        };
        exec.enter(&signal).await.unwrap();

        let hedge_orders = hedge.placed.lock().unwrap();
        let primary_orders = primary.placed.lock().unwrap();
        assert_eq!(primary_orders[0].side, Side::Sell);
        assert_eq!(hedge_orders[0].side, Side::Buy);
    }
}
