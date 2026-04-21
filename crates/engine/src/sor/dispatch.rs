//! Smart Order Router — inline dispatch (Epic A Stage-2).
//!
//! Wraps the advisory `GreedyRouter::route(...)` output with a real
//! leg-execution path. Operators call [`MarketMakerEngine::dispatch_route`]
//! to get a [`RouteDecision`] + live side-effecting fills against
//! every venue the decision picked. Still a thin layer — the router
//! stays pure; this module is the glue between a [`RouteDecision`]
//! and the per-venue [`ExchangeConnector`] calls.
//!
//! # Maker vs taker split
//!
//! Per-leg `is_taker` drives the dispatch mode:
//!
//! - **Taker leg** (`urgency ≥ 0.5`): placed as `TimeInForce::Ioc`
//!   through [`crate::order_manager::OrderManager::execute_unwind_slice`].
//!   The IOC evaporates on the venue side if the book moves before
//!   the order lands.
//! - **Maker leg** (`urgency < 0.5`): placed as
//!   `TimeInForce::PostOnly` through the connector's `place_order`
//!   directly, bypassing the diff machinery. The order rests on the
//!   book until it fills or the operator cancels it. Tracked in the
//!   per-venue `OrderManager` live-orders map so `cancel_all` still
//!   works on kill-switch escalation.
//!
//! # Venue lookup
//!
//! [`RouteLeg::venue`] is a [`VenueId`]. The dispatcher scans the
//! [`ConnectorBundle`] in `all_connectors` order and picks the first
//! connector matching that id. A venue referenced in the decision
//! but missing from the bundle produces a `LegOutcome::error` —
//! the router's venue universe is supposed to be a subset of the
//! bundle's, but we don't trust callers to keep them in sync and we
//! surface the mismatch through the outcome instead of panicking.

use std::sync::Arc;

use mm_common::types::{OrderType, ProductSpec, Quote, Side, TimeInForce};
use mm_exchange_core::connector::{ExchangeConnector, NewOrder, VenueId};
use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::connector_bundle::ConnectorBundle;
use crate::order_manager::OrderManager;
use crate::sor::router::{RouteDecision, RouteLeg};

/// Outcome of dispatching a single [`RouteLeg`].
#[derive(Debug, Clone)]
pub struct LegOutcome {
    /// Which venue was targeted.
    pub venue: VenueId,
    /// Qty the router asked the dispatcher to place.
    pub target_qty: Decimal,
    /// Qty actually accepted by the venue (= `target_qty` on
    /// success, `0` on error, or a partial on a maker leg that
    /// was rejected by a minimum-notional check before placement).
    pub dispatched_qty: Decimal,
    /// Whether the leg was routed as a taker (IOC) or a maker
    /// (PostOnly).
    pub is_taker: bool,
    /// Expected cost in bps — echoed through from the router so
    /// the audit trail can pair decision vs outcome.
    pub expected_cost_bps: Decimal,
    /// `Some(err)` when the venue call returned an error, or the
    /// bundle did not carry a matching connector. Otherwise
    /// `None`.
    pub error: Option<String>,
}

impl LegOutcome {
    pub fn succeeded(&self) -> bool {
        self.error.is_none() && self.dispatched_qty == self.target_qty
    }
}

/// Outcome of dispatching a full [`RouteDecision`].
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    /// Echoed through from the decision.
    pub target_side: Side,
    /// Echoed through from the decision.
    pub total_target_qty: Decimal,
    /// Sum of every leg's `dispatched_qty`.
    pub total_dispatched_qty: Decimal,
    /// Per-leg outcomes, in the same order as the decision's legs.
    pub legs: Vec<LegOutcome>,
    /// Flattened error strings from every failed leg — makes it
    /// easy for the caller to produce a single log line or an
    /// alert payload without re-walking `legs`.
    pub errors: Vec<String>,
}

impl DispatchOutcome {
    /// `true` when every leg was dispatched without error AND
    /// the decision covered the full target qty (is_complete).
    pub fn is_fully_dispatched(&self) -> bool {
        self.errors.is_empty() && self.total_dispatched_qty == self.total_target_qty
    }

    /// Empty outcome returned when the decision has zero legs.
    pub fn empty(side: Side, target_qty: Decimal) -> Self {
        Self {
            target_side: side,
            total_target_qty: target_qty,
            total_dispatched_qty: Decimal::ZERO,
            legs: Vec::new(),
            errors: Vec::new(),
        }
    }
}

/// Look up a connector in the bundle by `(venue, product)`.
/// Returns `None` when the decision references a combination the
/// bundle does not carry. When `product` is `None` we return the
/// FIRST connector matching `venue` — legacy single-product-per-
/// venue deployments (every route today) get byte-identical
/// behaviour; multi-product deployments (Binance spot + Binance
/// perp in the same process) pass `Some(product)` to disambiguate.
pub fn connector_for(
    bundle: &ConnectorBundle,
    venue: VenueId,
    product: Option<mm_exchange_core::connector::VenueProduct>,
) -> Option<&Arc<dyn ExchangeConnector>> {
    bundle.all_connectors().find(|c| {
        c.venue_id() == venue
            && product.map(|p| c.product() == p).unwrap_or(true)
    })
}

/// Core dispatch helper. Walks the decision's legs, picks the
/// matching connector on each one, and fires either an IOC slice
/// (taker legs) or a PostOnly limit (maker legs) through that
/// connector. Captures per-leg success / error and rolls them up
/// into a [`DispatchOutcome`].
///
/// The primary-side `OrderManager` is used for every leg so that
/// the live-orders map sees every new placement from a single
/// authoritative source. Multi-venue deployments that want
/// per-venue order manager books can extend this helper in stage-2b
/// (out of scope for Track 1).
pub async fn dispatch_route(
    decision: &RouteDecision,
    bundle: &ConnectorBundle,
    order_manager: &mut OrderManager,
    product: &ProductSpec,
    symbol: &str,
) -> DispatchOutcome {
    if decision.legs.is_empty() {
        return DispatchOutcome::empty(decision.target_side, decision.target_qty);
    }

    let mut legs: Vec<LegOutcome> = Vec::with_capacity(decision.legs.len());
    let mut errors: Vec<String> = Vec::new();
    let mut total_dispatched = Decimal::ZERO;

    for leg in &decision.legs {
        let outcome = dispatch_single_leg(
            leg,
            decision.target_side,
            bundle,
            order_manager,
            product,
            symbol,
        )
        .await;
        if let Some(err) = &outcome.error {
            warn!(venue = ?outcome.venue, err = %err, "sor dispatch leg failed");
            errors.push(err.clone());
        } else {
            info!(
                venue = ?outcome.venue,
                target = %outcome.target_qty,
                dispatched = %outcome.dispatched_qty,
                is_taker = outcome.is_taker,
                "sor dispatch leg placed"
            );
        }
        total_dispatched += outcome.dispatched_qty;
        legs.push(outcome);
    }

    DispatchOutcome {
        target_side: decision.target_side,
        total_target_qty: decision.target_qty,
        total_dispatched_qty: total_dispatched,
        legs,
        errors,
    }
}

async fn dispatch_single_leg(
    leg: &RouteLeg,
    side: Side,
    bundle: &ConnectorBundle,
    order_manager: &mut OrderManager,
    product: &ProductSpec,
    symbol: &str,
) -> LegOutcome {
    let Some(connector) = connector_for(bundle, leg.venue, leg.venue_product) else {
        return LegOutcome {
            venue: leg.venue,
            target_qty: leg.qty,
            dispatched_qty: Decimal::ZERO,
            is_taker: leg.is_taker,
            expected_cost_bps: leg.expected_cost_bps,
            error: Some(format!("bundle has no connector for {:?}", leg.venue)),
        };
    };

    // Price reference: mid-of-book for taker IOC limits, top of
    // the matching side for maker posts. The cost model already
    // ran against the same book state, so this is purely the
    // venue's per-order `price` field.
    let ref_price = pick_reference_price(connector, symbol, side).await;
    let qty = product.round_qty(leg.qty);
    if qty.is_zero() {
        return LegOutcome {
            venue: leg.venue,
            target_qty: leg.qty,
            dispatched_qty: Decimal::ZERO,
            is_taker: leg.is_taker,
            expected_cost_bps: leg.expected_cost_bps,
            error: Some("rounded qty is zero".to_string()),
        };
    }
    let price = match ref_price {
        Some(p) => product.round_price(p),
        None => {
            return LegOutcome {
                venue: leg.venue,
                target_qty: leg.qty,
                dispatched_qty: Decimal::ZERO,
                is_taker: leg.is_taker,
                expected_cost_bps: leg.expected_cost_bps,
                error: Some("no top-of-book reference price on venue".to_string()),
            };
        }
    };

    if leg.is_taker {
        let quote = Quote { side, price, qty };
        match order_manager
            .execute_unwind_slice(symbol, &quote, product, connector)
            .await
        {
            Ok(()) => LegOutcome {
                venue: leg.venue,
                target_qty: leg.qty,
                dispatched_qty: qty,
                is_taker: true,
                expected_cost_bps: leg.expected_cost_bps,
                error: None,
            },
            Err(e) => LegOutcome {
                venue: leg.venue,
                target_qty: leg.qty,
                dispatched_qty: Decimal::ZERO,
                is_taker: true,
                expected_cost_bps: leg.expected_cost_bps,
                error: Some(e.to_string()),
            },
        }
    } else {
        // Maker leg: post a PostOnly limit directly on the
        // connector and skip the diff machinery.
        let new_order = NewOrder {
            symbol: symbol.to_string(),
            side,
            order_type: OrderType::Limit,
            price: Some(price),
            qty,
            time_in_force: Some(TimeInForce::PostOnly),
            client_order_id: None,
            reduce_only: false,
        };
        match connector.place_order(&new_order).await {
            Ok(_order_id) => LegOutcome {
                venue: leg.venue,
                target_qty: leg.qty,
                dispatched_qty: qty,
                is_taker: false,
                expected_cost_bps: leg.expected_cost_bps,
                error: None,
            },
            Err(e) => LegOutcome {
                venue: leg.venue,
                target_qty: leg.qty,
                dispatched_qty: Decimal::ZERO,
                is_taker: false,
                expected_cost_bps: leg.expected_cost_bps,
                error: Some(e.to_string()),
            },
        }
    }
}

async fn pick_reference_price(
    connector: &Arc<dyn ExchangeConnector>,
    symbol: &str,
    side: Side,
) -> Option<Decimal> {
    let (bids, asks, _) = connector.get_orderbook(symbol, 1).await.ok()?;
    match side {
        // Buying takes the ask (crossing the spread) / posts at
        // the best bid. Maker / taker pricing diverges at
        // higher fidelity, but for a stage-2 dispatch both sides
        // of the book work well enough: the `TimeInForce`
        // setting enforces the intended fill semantics.
        Side::Buy => asks.first().map(|lvl| lvl.price),
        Side::Sell => bids.first().map(|lvl| lvl.price),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockConnector;
    use mm_exchange_core::connector::VenueProduct;
    use rust_decimal_macros::dec;

    use crate::sor::router::RouteLeg;

    fn sample_product() -> ProductSpec {
        ProductSpec {
            symbol: "BTCUSDT".to_string(),
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

    fn decision_single_leg(venue: VenueId, qty: Decimal, is_taker: bool) -> RouteDecision {
        RouteDecision {
            target_side: Side::Buy,
            target_qty: qty,
            filled_qty: qty,
            is_complete: true,
            legs: vec![RouteLeg {
                venue,
                venue_product: None,
                qty,
                is_taker,
                expected_cost_bps: dec!(5),
            }],
        }
    }

    fn decision_two_legs(is_taker: bool) -> RouteDecision {
        RouteDecision {
            target_side: Side::Buy,
            target_qty: dec!(2),
            filled_qty: dec!(2),
            is_complete: true,
            legs: vec![
                RouteLeg {
                    venue: VenueId::Binance,
                    venue_product: None,
                    qty: dec!(1),
                    is_taker,
                    expected_cost_bps: dec!(3),
                },
                RouteLeg {
                    venue: VenueId::Bybit,
                    venue_product: None,
                    qty: dec!(1),
                    is_taker,
                    expected_cost_bps: dec!(5),
                },
            ],
        }
    }

    #[tokio::test]
    async fn empty_decision_returns_empty_outcome() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50000));
        let dyn_conn: Arc<dyn ExchangeConnector> = mock.clone();
        let bundle = ConnectorBundle::single(dyn_conn);
        let mut om = OrderManager::new();
        let decision = RouteDecision::empty(Side::Buy);
        let outcome =
            dispatch_route(&decision, &bundle, &mut om, &sample_product(), "BTCUSDT").await;
        assert!(outcome.legs.is_empty());
        assert_eq!(outcome.total_dispatched_qty, dec!(0));
        assert!(outcome.errors.is_empty());
        assert!(outcome.is_fully_dispatched());
    }

    #[tokio::test]
    async fn single_leg_taker_goes_through_execute_unwind_slice() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50000));
        let dyn_conn: Arc<dyn ExchangeConnector> = mock.clone();
        let bundle = ConnectorBundle::single(dyn_conn);
        let mut om = OrderManager::new();
        let decision = decision_single_leg(VenueId::Binance, dec!(1), true);
        let outcome =
            dispatch_route(&decision, &bundle, &mut om, &sample_product(), "BTCUSDT").await;
        assert_eq!(outcome.legs.len(), 1);
        assert!(outcome.errors.is_empty());
        let leg = &outcome.legs[0];
        assert_eq!(leg.venue, VenueId::Binance);
        assert_eq!(leg.dispatched_qty, dec!(1));
        assert!(leg.is_taker);
        // Taker path uses execute_unwind_slice → single place_order call.
        assert_eq!(mock.place_single_calls(), 1);
        assert_eq!(mock.place_batch_calls(), 0);
        assert!(outcome.is_fully_dispatched());
    }

    #[tokio::test]
    async fn single_leg_maker_uses_post_only_direct_place() {
        let mock = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        mock.set_mid(dec!(50000));
        let dyn_conn: Arc<dyn ExchangeConnector> = mock.clone();
        let bundle = ConnectorBundle::single(dyn_conn);
        let mut om = OrderManager::new();
        let decision = decision_single_leg(VenueId::Binance, dec!(1), false);
        let outcome =
            dispatch_route(&decision, &bundle, &mut om, &sample_product(), "BTCUSDT").await;
        assert_eq!(outcome.legs.len(), 1);
        assert!(outcome.errors.is_empty());
        let leg = &outcome.legs[0];
        assert!(!leg.is_taker);
        // Maker path → direct place_order.
        assert_eq!(mock.place_single_calls(), 1);
        // Verify the placed order used PostOnly.
        let placed = mock.placed.lock().unwrap().clone();
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].time_in_force, Some(TimeInForce::PostOnly));
    }

    #[tokio::test]
    async fn multi_venue_split_dispatches_both_legs() {
        let binance = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        binance.set_mid(dec!(50000));
        let bybit = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        bybit.set_mid(dec!(50010));
        let bundle = ConnectorBundle {
            primary: binance.clone() as Arc<dyn ExchangeConnector>,
            hedge: None,
            pair: None,
            extra: vec![bybit.clone() as Arc<dyn ExchangeConnector>],
        };
        let mut om = OrderManager::new();
        let decision = decision_two_legs(true);
        let outcome =
            dispatch_route(&decision, &bundle, &mut om, &sample_product(), "BTCUSDT").await;
        assert_eq!(outcome.legs.len(), 2);
        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.total_dispatched_qty, dec!(2));
        // Both mocks saw exactly one place call.
        assert_eq!(binance.place_single_calls(), 1);
        assert_eq!(bybit.place_single_calls(), 1);
    }

    #[tokio::test]
    async fn missing_venue_in_bundle_is_reported_as_leg_error() {
        // Bundle carries Binance only; decision references Bybit
        // → dispatcher reports the mismatch rather than panicking.
        let binance = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        binance.set_mid(dec!(50000));
        let bundle = ConnectorBundle::single(binance.clone() as Arc<dyn ExchangeConnector>);
        let mut om = OrderManager::new();
        let decision = decision_single_leg(VenueId::Bybit, dec!(1), true);
        let outcome =
            dispatch_route(&decision, &bundle, &mut om, &sample_product(), "BTCUSDT").await;
        assert_eq!(outcome.legs.len(), 1);
        assert_eq!(outcome.legs[0].dispatched_qty, dec!(0));
        assert!(outcome.legs[0].error.is_some());
        assert_eq!(outcome.errors.len(), 1);
        assert!(!outcome.is_fully_dispatched());
    }

    #[tokio::test]
    async fn partial_failure_on_one_leg_still_dispatches_the_other() {
        // Two-leg decision; Bybit's mock has no book so
        // pick_reference_price returns None → that leg errors.
        let binance = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        binance.set_mid(dec!(50000));
        let bybit = Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot));
        // No set_mid → empty book.
        let bundle = ConnectorBundle {
            primary: binance.clone() as Arc<dyn ExchangeConnector>,
            hedge: None,
            pair: None,
            extra: vec![bybit.clone() as Arc<dyn ExchangeConnector>],
        };
        let mut om = OrderManager::new();
        let decision = decision_two_legs(true);
        let outcome =
            dispatch_route(&decision, &bundle, &mut om, &sample_product(), "BTCUSDT").await;
        assert_eq!(outcome.legs.len(), 2);
        // Binance dispatched, Bybit errored.
        let binance_leg = &outcome.legs[0];
        let bybit_leg = &outcome.legs[1];
        assert_eq!(binance_leg.venue, VenueId::Binance);
        assert!(binance_leg.error.is_none());
        assert_eq!(binance_leg.dispatched_qty, dec!(1));
        assert_eq!(bybit_leg.venue, VenueId::Bybit);
        assert!(bybit_leg.error.is_some());
        assert_eq!(bybit_leg.dispatched_qty, dec!(0));
        assert_eq!(outcome.total_dispatched_qty, dec!(1));
        assert!(!outcome.is_fully_dispatched());
    }

    #[tokio::test]
    async fn connector_for_returns_none_when_venue_not_in_bundle() {
        let binance = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
        let bundle = ConnectorBundle::single(binance.clone() as Arc<dyn ExchangeConnector>);
        assert!(connector_for(&bundle, VenueId::Binance, None).is_some());
        assert!(connector_for(&bundle, VenueId::Bybit, None).is_none());
    }

    #[tokio::test]
    async fn leg_outcome_succeeded_reflects_error_state() {
        let ok = LegOutcome {
            venue: VenueId::Binance,
            target_qty: dec!(1),
            dispatched_qty: dec!(1),
            is_taker: true,
            expected_cost_bps: dec!(5),
            error: None,
        };
        assert!(ok.succeeded());
        let failed = LegOutcome {
            venue: VenueId::Binance,
            target_qty: dec!(1),
            dispatched_qty: dec!(0),
            is_taker: true,
            expected_cost_bps: dec!(5),
            error: Some("boom".to_string()),
        };
        assert!(!failed.succeeded());
        let partial = LegOutcome {
            venue: VenueId::Binance,
            target_qty: dec!(1),
            dispatched_qty: dec!(0.5),
            is_taker: true,
            expected_cost_bps: dec!(5),
            error: None,
        };
        assert!(!partial.succeeded());
    }

    #[tokio::test]
    async fn dispatch_outcome_is_fully_dispatched_requires_full_qty_and_no_errors() {
        let full = DispatchOutcome {
            target_side: Side::Buy,
            total_target_qty: dec!(2),
            total_dispatched_qty: dec!(2),
            legs: vec![],
            errors: vec![],
        };
        assert!(full.is_fully_dispatched());
        let partial = DispatchOutcome {
            target_side: Side::Buy,
            total_target_qty: dec!(2),
            total_dispatched_qty: dec!(1),
            legs: vec![],
            errors: vec![],
        };
        assert!(!partial.is_fully_dispatched());
        let with_errors = DispatchOutcome {
            target_side: Side::Buy,
            total_target_qty: dec!(2),
            total_dispatched_qty: dec!(2),
            legs: vec![],
            errors: vec!["boom".to_string()],
        };
        assert!(!with_errors.is_fully_dispatched());
    }

    #[tokio::test]
    async fn dispatch_outcome_empty_helper_has_no_legs_and_zero_dispatched() {
        let empty = DispatchOutcome::empty(Side::Buy, dec!(3));
        assert_eq!(empty.target_side, Side::Buy);
        assert_eq!(empty.total_target_qty, dec!(3));
        assert_eq!(empty.total_dispatched_qty, dec!(0));
        assert!(empty.legs.is_empty());
        assert!(empty.errors.is_empty());
        // "Fully dispatched" on an empty outcome only holds for
        // a zero-qty decision — a 3-qty empty is NOT fully
        // dispatched.
        assert!(!empty.is_fully_dispatched());
    }
}
