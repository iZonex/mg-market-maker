use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use mm_common::types::{Fill, LiveOrder, OrderId, OrderType, Price, Qty, Quote, QuotePair, Side};
use mm_common::types::{ProductSpec, TimeInForce};
use mm_exchange_core::connector::{AmendOrder, ExchangeConnector, NewOrder};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Epic E sub-component #1 — minimum batch size below which
/// `execute_diff` stays on the per-order placement path. A
/// single-order `place_orders_batch` call adds JSON overhead
/// with no benefit over `place_order`; values ≥ 2 use the
/// batch path.
const MIN_BATCH_SIZE: usize = 2;

/// Per-order outcome from a batch placement attempt. Gives the
/// engine visibility into which orders succeeded and which
/// failed, instead of the all-or-nothing semantics of the v1
/// batch API. Epic E stage-2.
#[derive(Debug, Clone)]
pub enum BatchPlaceOutcome {
    /// Order placed successfully via the batch API.
    Placed { order_id: OrderId },
    /// Order placed successfully via per-order fallback after
    /// a batch-level failure.
    PlacedFallback { order_id: OrderId },
    /// Order failed both batch and per-order paths.
    Failed { reason: String },
    /// Venue did not acknowledge this order in the batch
    /// response (ID count mismatch) and the per-order retry
    /// also failed.
    Unacknowledged { reason: String },
}

/// Per-order outcome from a batch cancellation attempt.
#[derive(Debug, Clone)]
pub enum BatchCancelOutcome {
    Cancelled,
    CancelledFallback,
    Failed { reason: String },
}

/// One side of an in-place price tweak that preserves queue
/// priority. `OrderDiffPlan::to_amend` holds these instead of a
/// matched (cancel, place) pair when the live order can be
/// modified on the venue with a single amend RPC.
#[derive(Debug, Clone)]
pub struct AmendPlanEntry {
    pub order_id: OrderId,
    pub side: Side,
    pub old_price: Price,
    pub new_price: Price,
    pub qty: Qty,
}

/// Output of [`OrderManager::diff_orders`].
///
/// Splits the desired-vs-live reconciliation into three buckets:
/// - `to_cancel` — live orders the engine no longer wants and
///   that have no nearby same-qty replacement
/// - `to_amend` — live orders whose only delta is a small price
///   tweak (within `amend_epsilon_ticks` of the new price); these
///   can be modified in place on venues that support native amend
/// - `to_place` — brand-new quote levels with no matching live
///   order in either the cancel or amend bucket
///
/// Setting `amend_epsilon_ticks = 0` disables the amend bucket
/// entirely — the plan then degenerates to the legacy cancel +
/// place behaviour.
#[derive(Debug, Clone, Default)]
pub struct OrderDiffPlan {
    pub to_cancel: Vec<OrderId>,
    pub to_amend: Vec<AmendPlanEntry>,
    pub to_place: Vec<Quote>,
}

/// Manages live orders on the exchange.
/// Performs order diffing: only cancels/places orders that actually changed.
pub struct OrderManager {
    /// Our currently live orders on the exchange, keyed by order ID.
    live_orders: HashMap<OrderId, LiveOrder>,
    /// Map from (side, price) to order ID for quick lookup.
    price_index: HashMap<(Side, Price), OrderId>,
    /// When true, no real exchange calls are made — place/cancel/amend
    /// short-circuit to locally-tracked simulated orders with fresh
    /// UUIDs. The engine's full pipeline (quoting, diffing, PnL,
    /// inventory, reconciliation) still runs end-to-end on the real
    /// market feed; only order egress is stubbed.
    ///
    /// This is the real hard gate behind `MM_MODE=paper` — without
    /// it, a paper-tagged run with live API keys would happily send
    /// real orders to the venue.
    paper_mode: bool,
    /// Epic R — Shared surveillance tracker. When `Some`, every
    /// successful place / cancel feeds it a lifecycle event so
    /// the detector suite can reason about our order stream.
    /// `None` keeps the manager free of surveillance overhead
    /// (tests + non-surveillance deployments).
    surveillance: Option<mm_risk::surveillance::SharedTracker>,
}

impl OrderManager {
    pub fn new() -> Self {
        Self {
            live_orders: HashMap::new(),
            price_index: HashMap::new(),
            paper_mode: false,
            surveillance: None,
        }
    }

    /// Epic R — register the shared surveillance tracker. Every
    /// subsequent place / cancel feeds it a lifecycle event.
    pub fn attach_surveillance(&mut self, tracker: mm_risk::surveillance::SharedTracker) {
        self.surveillance = Some(tracker);
    }

    /// Internal helper — emits a place/cancel event to the
    /// attached tracker if any. No-op otherwise.
    fn feed_surveillance(&self, ev: mm_risk::surveillance::SurveillanceEvent) {
        let Some(t) = self.surveillance.as_ref() else {
            return;
        };
        if let Ok(mut tracker) = t.lock() {
            tracker.feed(&ev);
        }
    }

    /// Construct an `OrderManager` that refuses to touch the
    /// connector for any order action — place, cancel, or amend.
    /// Intended for the `MM_MODE=paper` code path.
    pub fn new_paper() -> Self {
        Self {
            live_orders: HashMap::new(),
            price_index: HashMap::new(),
            paper_mode: true,
            surveillance: None,
        }
    }

    /// Is this OrderManager currently running in paper mode?
    pub fn is_paper(&self) -> bool {
        self.paper_mode
    }

    /// Number of live orders.
    pub fn live_count(&self) -> usize {
        self.live_orders.len()
    }

    /// Get all live order IDs.
    pub fn live_order_ids(&self) -> Vec<OrderId> {
        self.live_orders.keys().copied().collect()
    }

    /// Total value locked in open orders (quote asset: price * remaining_qty).
    pub fn locked_value_quote(&self) -> Qty {
        self.live_orders
            .values()
            .map(|o| o.price * (o.qty - o.filled_qty))
            .sum()
    }

    /// Snapshot the live-order book for the dashboard. Returns
    /// one tuple per tracked order with the fields the UI
    /// needs (`client_order_id`, `side`, `price`,
    /// `remaining_qty`, `status`). Status is derived from the
    /// `LiveOrder::status` enum so the frontend can distinguish
    /// fully-live orders from partially-filled ones.
    pub fn live_orders_snapshot(&self) -> Vec<(OrderId, Side, Price, Qty, &'static str)> {
        self.live_orders
            .values()
            .map(|o| {
                let status = match o.status {
                    mm_common::types::OrderStatus::Open => "live",
                    mm_common::types::OrderStatus::PartiallyFilled => "live",
                    mm_common::types::OrderStatus::Filled => "filled",
                    mm_common::types::OrderStatus::Cancelled => "cancelling",
                    mm_common::types::OrderStatus::Rejected => "rejected",
                };
                let remaining = o.qty - o.filled_qty;
                (o.order_id, o.side, o.price, remaining, status)
            })
            .collect()
    }

    /// Reconcile desired quotes with live orders, opportunistically
    /// pairing a stale order with a new quote of the same side and
    /// quantity when their prices are within `amend_epsilon_ticks`
    /// of each other. Pure function — does not touch the connector.
    ///
    /// Pass `amend_epsilon_ticks = 0` to fall back to the legacy
    /// cancel + place behaviour.
    pub fn diff_orders(
        &self,
        desired: &[QuotePair],
        product: &ProductSpec,
        amend_epsilon_ticks: u32,
    ) -> OrderDiffPlan {
        let mut desired_prices: HashMap<(Side, Price), Qty> = HashMap::new();

        for pair in desired {
            if let Some(bid) = &pair.bid {
                let price = product.round_price(bid.price);
                desired_prices.insert((Side::Buy, price), bid.qty);
            }
            if let Some(ask) = &pair.ask {
                let price = product.round_price(ask.price);
                desired_prices.insert((Side::Sell, price), ask.qty);
            }
        }

        // Pure set difference: stale entries to retire, new entries
        // to create. Used as input to the amend-pairing pass.
        let mut stale: Vec<(Side, Price, Qty, OrderId)> = self
            .price_index
            .iter()
            .filter_map(|(&(side, price), &id)| {
                if desired_prices.contains_key(&(side, price)) {
                    return None;
                }
                let order = self.live_orders.get(&id)?;
                let remaining = order.qty - order.filled_qty;
                Some((side, price, remaining, id))
            })
            .collect();
        let mut new_quotes: Vec<(Side, Price, Qty)> = desired_prices
            .iter()
            .filter_map(|(&(side, price), &qty)| {
                if self.price_index.contains_key(&(side, price)) {
                    None
                } else {
                    Some((side, price, qty))
                }
            })
            .collect();

        // Deterministic order so amend pairing is reproducible across
        // ticks — without sorting the HashMap iteration order would
        // shuffle which stale order matches which new quote.
        let side_key = |s: &Side| match s {
            Side::Buy => 0u8,
            Side::Sell => 1u8,
        };
        stale.sort_by(|a, b| side_key(&a.0).cmp(&side_key(&b.0)).then(a.1.cmp(&b.1)));
        new_quotes.sort_by(|a, b| side_key(&a.0).cmp(&side_key(&b.0)).then(a.1.cmp(&b.1)));

        let mut to_amend: Vec<AmendPlanEntry> = Vec::new();
        if amend_epsilon_ticks > 0 {
            let max_distance: Decimal = product.tick_size * Decimal::from(amend_epsilon_ticks);

            // Greedy nearest-pair: for each new quote, walk the
            // remaining stale list on the same side and pick the
            // first one with matching qty whose price is within the
            // tick window.
            let mut consumed_stale: Vec<bool> = vec![false; stale.len()];
            new_quotes.retain(|(side, new_price, new_qty)| {
                let mut best_idx: Option<usize> = None;
                let mut best_distance = max_distance + Decimal::ONE;
                for (i, (s_side, s_price, s_qty, _)) in stale.iter().enumerate() {
                    if consumed_stale[i] || s_side != side || s_qty != new_qty {
                        continue;
                    }
                    let distance = (*new_price - *s_price).abs();
                    if distance <= max_distance && distance < best_distance {
                        best_distance = distance;
                        best_idx = Some(i);
                    }
                }
                if let Some(idx) = best_idx {
                    consumed_stale[idx] = true;
                    let (_, old_price, qty, order_id) = stale[idx];
                    to_amend.push(AmendPlanEntry {
                        order_id,
                        side: *side,
                        old_price,
                        new_price: *new_price,
                        qty,
                    });
                    false
                } else {
                    true
                }
            });

            // Drop the stale entries that got paired into amends.
            let mut idx = 0usize;
            stale.retain(|_| {
                let keep = !consumed_stale[idx];
                idx += 1;
                keep
            });
        }

        let to_cancel: Vec<OrderId> = stale.into_iter().map(|(_, _, _, id)| id).collect();
        let to_place: Vec<Quote> = new_quotes
            .into_iter()
            .map(|(side, price, qty)| Quote { side, price, qty })
            .collect();

        OrderDiffPlan {
            to_cancel,
            to_amend,
            to_place,
        }
    }

    /// Execute the diff: amend price tweaks in place where the venue
    /// supports it, cancel stale orders, place new ones.
    ///
    /// `amend_epsilon_ticks = 0` keeps the legacy cancel + place
    /// behaviour even on amend-capable venues. When the connector
    /// does not advertise `supports_amend`, any planned amends fall
    /// back to cancel + place so HL and other no-amend venues stay
    /// functionally correct.
    pub async fn execute_diff(
        &mut self,
        symbol: &str,
        desired: &[QuotePair],
        product: &ProductSpec,
        connector: &Arc<dyn ExchangeConnector>,
        amend_epsilon_ticks: u32,
    ) -> Result<()> {
        let venue_supports_amend = connector.capabilities().supports_amend;
        let mut plan = self.diff_orders(
            desired,
            product,
            if venue_supports_amend {
                amend_epsilon_ticks
            } else {
                0
            },
        );
        let mut amend_failures = 0usize;
        let amends_planned = plan.to_amend.len();

        // Issue amends first — they preserve queue priority, so we
        // want them committed before any cancel hits the wire.
        // Failures fall back to cancel + place by appending the entry
        // to the next-up buckets.
        for entry in std::mem::take(&mut plan.to_amend) {
            if self.paper_mode {
                debug!(
                    order_id = %entry.order_id,
                    old_price = %entry.old_price,
                    new_price = %entry.new_price,
                    "[PAPER] amended order (simulated)"
                );
                self.reprice_order(entry.order_id, entry.new_price);
                continue;
            }
            let request = AmendOrder {
                order_id: entry.order_id,
                symbol: symbol.to_string(),
                new_price: Some(entry.new_price),
                new_qty: Some(entry.qty),
            };
            match connector.amend_order(&request).await {
                Ok(_) => {
                    debug!(
                        order_id = %entry.order_id,
                        side = ?entry.side,
                        old_price = %entry.old_price,
                        new_price = %entry.new_price,
                        "amended order in place"
                    );
                    self.reprice_order(entry.order_id, entry.new_price);
                }
                Err(e) => {
                    warn!(
                        order_id = %entry.order_id,
                        error = %e,
                        "amend failed — falling back to cancel + place"
                    );
                    amend_failures += 1;
                    plan.to_cancel.push(entry.order_id);
                    plan.to_place.push(Quote {
                        side: entry.side,
                        price: entry.new_price,
                        qty: entry.qty,
                    });
                }
            }
        }

        // Cancel stale orders (batch when supported).
        let cancel_outcomes = self
            .cancel_orders_batched(symbol, &plan.to_cancel, connector)
            .await;

        // Place new orders (batch when supported).
        let place_outcomes = self
            .place_quotes_batched(symbol, &plan.to_place, connector)
            .await;

        // Summarise per-order outcomes.
        let cancel_failures = cancel_outcomes
            .iter()
            .filter(|o| matches!(o, BatchCancelOutcome::Failed { .. }))
            .count();
        let cancel_fallbacks = cancel_outcomes
            .iter()
            .filter(|o| matches!(o, BatchCancelOutcome::CancelledFallback))
            .count();
        let place_failures = place_outcomes
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    BatchPlaceOutcome::Failed { .. } | BatchPlaceOutcome::Unacknowledged { .. }
                )
            })
            .count();
        let place_fallbacks = place_outcomes
            .iter()
            .filter(|o| matches!(o, BatchPlaceOutcome::PlacedFallback { .. }))
            .count();

        if !plan.to_cancel.is_empty() || !plan.to_place.is_empty() || amends_planned > 0 {
            info!(
                amended = amends_planned - amend_failures,
                amend_failures,
                cancelled = plan.to_cancel.len(),
                cancel_failures,
                cancel_fallbacks,
                placed = plan.to_place.len(),
                place_failures,
                place_fallbacks,
                live = self.live_count(),
                "order diff executed"
            );
        }

        Ok(())
    }

    /// Epic E sub-component #1 (stage-2) — place a slice of
    /// quotes using the venue's `place_orders_batch` when ≥
    /// `MIN_BATCH_SIZE` quotes are pending and the connector
    /// advertises a non-trivial `max_batch_size`.
    ///
    /// Returns per-order outcomes so the engine has visibility
    /// into partial failures. On ID-count mismatch the
    /// unacknowledged orders are retried individually. On
    /// full batch error every order in the chunk falls back to
    /// per-order placement.
    async fn place_quotes_batched(
        &mut self,
        symbol: &str,
        quotes: &[Quote],
        connector: &Arc<dyn ExchangeConnector>,
    ) -> Vec<BatchPlaceOutcome> {
        if quotes.is_empty() {
            return Vec::new();
        }
        // Paper mode: route every quote through the per-order path
        // so the paper short-circuit inside `place_one_quote_outcome`
        // applies without duplicating simulation logic here.
        if self.paper_mode {
            let mut outcomes = Vec::with_capacity(quotes.len());
            for quote in quotes {
                outcomes.push(self.place_one_quote_outcome(symbol, quote, connector).await);
            }
            return outcomes;
        }
        let max_batch = connector.capabilities().max_batch_size.max(1);
        // Single-order or small slice → stay on per-order path.
        if quotes.len() < MIN_BATCH_SIZE || max_batch < MIN_BATCH_SIZE {
            let mut outcomes = Vec::with_capacity(quotes.len());
            for quote in quotes {
                outcomes.push(self.place_one_quote_outcome(symbol, quote, connector).await);
            }
            return outcomes;
        }
        let mut outcomes = Vec::with_capacity(quotes.len());
        for chunk in quotes.chunks(max_batch) {
            let orders: Vec<NewOrder> = chunk
                .iter()
                .map(|quote| NewOrder {
                    symbol: symbol.to_string(),
                    side: quote.side,
                    order_type: OrderType::Limit,
                    price: Some(quote.price),
                    qty: quote.qty,
                    time_in_force: Some(TimeInForce::PostOnly),
                    client_order_id: None,
                    reduce_only: false,
                })
                .collect();
            match connector.place_orders_batch(&orders).await {
                Ok(ids) if ids.len() == chunk.len() => {
                    for (order_id, quote) in ids.into_iter().zip(chunk.iter()) {
                        info!(
                            %order_id,
                            side = ?quote.side,
                            price = %quote.price,
                            qty = %quote.qty,
                            "placed order (batch)"
                        );
                        self.track_order(LiveOrder {
                            order_id,
                            symbol: symbol.to_string(),
                            side: quote.side,
                            price: quote.price,
                            qty: quote.qty,
                            filled_qty: dec!(0),
                            status: mm_common::types::OrderStatus::Open,
                            created_at: chrono::Utc::now(),
                        });
                        outcomes.push(BatchPlaceOutcome::Placed { order_id });
                    }
                }
                Ok(ids) => {
                    // ID-count mismatch: track acknowledged, retry
                    // the unacknowledged individually.
                    let ack_count = ids.len();
                    warn!(
                        returned = ack_count,
                        expected = chunk.len(),
                        "batch place returned fewer ids — retrying remainder individually"
                    );
                    for (order_id, quote) in ids.into_iter().zip(chunk.iter()) {
                        self.track_order(LiveOrder {
                            order_id,
                            symbol: symbol.to_string(),
                            side: quote.side,
                            price: quote.price,
                            qty: quote.qty,
                            filled_qty: dec!(0),
                            status: mm_common::types::OrderStatus::Open,
                            created_at: chrono::Utc::now(),
                        });
                        outcomes.push(BatchPlaceOutcome::Placed { order_id });
                    }
                    // Retry unacknowledged orders individually.
                    for quote in &chunk[ack_count..] {
                        let outcome = self.place_one_quote_unack(symbol, quote, connector).await;
                        outcomes.push(outcome);
                    }
                }
                Err(e) => {
                    warn!(
                        chunk_len = chunk.len(),
                        error = %e,
                        "batch place failed — falling back to per-order placement"
                    );
                    for quote in chunk {
                        let outcome = self
                            .place_one_quote_fallback(symbol, quote, connector)
                            .await;
                        outcomes.push(outcome);
                    }
                }
            }
        }
        outcomes
    }

    /// Per-quote placement returning a per-order outcome. Used
    /// by both the per-order path (small diffs) and batch
    /// fallback. Tracks the new `LiveOrder` on success.
    async fn place_one_quote_outcome(
        &mut self,
        symbol: &str,
        quote: &Quote,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> BatchPlaceOutcome {
        if self.paper_mode {
            let order_id = Uuid::new_v4();
            info!(
                %order_id,
                side = ?quote.side,
                price = %quote.price,
                qty = %quote.qty,
                "[PAPER] placed order (simulated)"
            );
            self.track_order(LiveOrder {
                order_id,
                symbol: symbol.to_string(),
                side: quote.side,
                price: quote.price,
                qty: quote.qty,
                filled_qty: dec!(0),
                status: mm_common::types::OrderStatus::Open,
                created_at: chrono::Utc::now(),
            });
            return BatchPlaceOutcome::Placed { order_id };
        }
        let order = NewOrder {
            symbol: symbol.to_string(),
            side: quote.side,
            order_type: OrderType::Limit,
            price: Some(quote.price),
            qty: quote.qty,
            time_in_force: Some(TimeInForce::PostOnly),
            client_order_id: None,
            reduce_only: false,
        };
        match connector.place_order(&order).await {
            Ok(order_id) => {
                info!(
                    %order_id,
                    side = ?quote.side,
                    price = %quote.price,
                    qty = %quote.qty,
                    "placed order"
                );
                self.track_order(LiveOrder {
                    order_id,
                    symbol: symbol.to_string(),
                    side: quote.side,
                    price: quote.price,
                    qty: quote.qty,
                    filled_qty: dec!(0),
                    status: mm_common::types::OrderStatus::Open,
                    created_at: chrono::Utc::now(),
                });
                BatchPlaceOutcome::Placed { order_id }
            }
            Err(e) => {
                let classified = connector.classify_error(&e);
                warn!(
                    side = ?quote.side,
                    price = %quote.price,
                    venue = %connector.venue_id(),
                    kind = %classified.kind,
                    retryable = classified.kind.is_retryable(),
                    alert = classified.kind.is_operator_alert(),
                    error = %e,
                    "failed to place order"
                );
                BatchPlaceOutcome::Failed {
                    reason: classified.to_string(),
                }
            }
        }
    }

    /// Fallback placement after a batch error — same as
    /// `place_one_quote_outcome` but tagged as `PlacedFallback`.
    async fn place_one_quote_fallback(
        &mut self,
        symbol: &str,
        quote: &Quote,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> BatchPlaceOutcome {
        match self.place_one_quote_outcome(symbol, quote, connector).await {
            BatchPlaceOutcome::Placed { order_id } => {
                BatchPlaceOutcome::PlacedFallback { order_id }
            }
            other => other,
        }
    }

    /// Retry for an unacknowledged order from a partial batch
    /// response.
    async fn place_one_quote_unack(
        &mut self,
        symbol: &str,
        quote: &Quote,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> BatchPlaceOutcome {
        match self.place_one_quote_outcome(symbol, quote, connector).await {
            BatchPlaceOutcome::Placed { order_id } => {
                BatchPlaceOutcome::PlacedFallback { order_id }
            }
            BatchPlaceOutcome::Failed { reason } => BatchPlaceOutcome::Unacknowledged { reason },
            other => other,
        }
    }

    /// Epic E sub-component #1 (stage-2) — cancel a slice of
    /// order ids with per-order outcome visibility. On batch
    /// error, falls back to per-order `cancel_order` for the
    /// entire chunk and reports per-order outcomes.
    async fn cancel_orders_batched(
        &mut self,
        symbol: &str,
        order_ids: &[OrderId],
        connector: &Arc<dyn ExchangeConnector>,
    ) -> Vec<BatchCancelOutcome> {
        if order_ids.is_empty() {
            return Vec::new();
        }
        // Paper mode: route through per-order path so the paper
        // short-circuit in `cancel_one_outcome` applies uniformly.
        if self.paper_mode {
            let mut outcomes = Vec::with_capacity(order_ids.len());
            for order_id in order_ids {
                outcomes.push(self.cancel_one_outcome(symbol, *order_id, connector).await);
            }
            return outcomes;
        }
        let max_batch = connector.capabilities().max_batch_size.max(1);
        if order_ids.len() < MIN_BATCH_SIZE || max_batch < MIN_BATCH_SIZE {
            let mut outcomes = Vec::with_capacity(order_ids.len());
            for order_id in order_ids {
                let outcome = self.cancel_one_outcome(symbol, *order_id, connector).await;
                outcomes.push(outcome);
            }
            return outcomes;
        }
        let mut outcomes = Vec::with_capacity(order_ids.len());
        for chunk in order_ids.chunks(max_batch) {
            match connector.cancel_orders_batch(symbol, chunk).await {
                Ok(_) => {
                    for order_id in chunk {
                        debug!(%order_id, "cancelled stale order (batch)");
                        self.remove_order(*order_id);
                        outcomes.push(BatchCancelOutcome::Cancelled);
                    }
                }
                Err(e) => {
                    warn!(
                        chunk_len = chunk.len(),
                        error = %e,
                        "batch cancel failed — falling back to per-order cancellation"
                    );
                    for order_id in chunk {
                        let outcome = self.cancel_one_outcome(symbol, *order_id, connector).await;
                        let outcome = match outcome {
                            BatchCancelOutcome::Cancelled => BatchCancelOutcome::CancelledFallback,
                            other => other,
                        };
                        outcomes.push(outcome);
                    }
                }
            }
        }
        outcomes
    }

    /// Per-cancel helper with outcome reporting. Removes the
    /// local tracking entry regardless of venue outcome — a
    /// failed cancel still drops the local state because the
    /// next diff will reconcile against the venue's actual
    /// open orders via `get_open_orders` reconciliation.
    async fn cancel_one_outcome(
        &mut self,
        symbol: &str,
        order_id: OrderId,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> BatchCancelOutcome {
        if self.paper_mode {
            debug!(%order_id, "[PAPER] cancelled order (simulated)");
            self.remove_order(order_id);
            return BatchCancelOutcome::Cancelled;
        }
        match connector.cancel_order(symbol, order_id).await {
            Ok(_) => {
                debug!(%order_id, "cancelled stale order");
                self.remove_order(order_id);
                BatchCancelOutcome::Cancelled
            }
            Err(e) => {
                let classified = connector.classify_error(&e);
                warn!(
                    %order_id,
                    venue = %connector.venue_id(),
                    kind = %classified.kind,
                    retryable = classified.kind.is_retryable(),
                    error = %e,
                    "failed to cancel order"
                );
                self.remove_order(order_id);
                BatchCancelOutcome::Failed {
                    reason: classified.to_string(),
                }
            }
        }
    }

    /// Update local state after a successful in-place amend: the
    /// order keeps its `OrderId` (and queue priority) but the
    /// `price_index` slot moves from the old price to the new one.
    fn reprice_order(&mut self, order_id: OrderId, new_price: Price) {
        let Some(order) = self.live_orders.get_mut(&order_id) else {
            return;
        };
        let side = order.side;
        let old_price = order.price;
        order.price = new_price;
        self.price_index.remove(&(side, old_price));
        self.price_index.insert((side, new_price), order_id);
    }

    /// Place a single unwind slice on the venue without going
    /// through the diff machinery. Used by kill-switch L4
    /// executors (`TwapExecutor`, `PairedUnwindExecutor`) where
    /// each tick emits a fresh IOC-ish slice that either fills
    /// immediately or gets cleaned up on shutdown.
    ///
    /// The order is placed as a limit with `TimeInForce::Ioc`
    /// so a non-crossing slice evaporates on the venue side
    /// instead of resting and interfering with future slices.
    /// Tracked in `live_orders` so `cancel_all` + fill routing
    /// still work.
    pub async fn execute_unwind_slice(
        &mut self,
        symbol: &str,
        quote: &Quote,
        product: &ProductSpec,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> Result<()> {
        self.execute_unwind_slice_flagged(symbol, quote, product, connector, false)
            .await
    }

    /// Perp-safe unwind slice. Same shape as `execute_unwind_slice`
    /// but sets `reduce_only` on the venue request so the fill can
    /// ONLY lower position. Callers who genuinely want to reduce
    /// (MarginGuard `Reduce` band, kill-switch L4 flatten,
    /// funding-arb compensating close) must use this path — the
    /// plain `execute_unwind_slice` leaves the flag off, matching
    /// legacy behaviour and staying safe on spot venues that
    /// refuse the flag.
    pub async fn execute_reduce_slice(
        &mut self,
        symbol: &str,
        quote: &Quote,
        product: &ProductSpec,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> Result<()> {
        self.execute_unwind_slice_flagged(symbol, quote, product, connector, true)
            .await
    }

    async fn execute_unwind_slice_flagged(
        &mut self,
        symbol: &str,
        quote: &Quote,
        product: &ProductSpec,
        connector: &Arc<dyn ExchangeConnector>,
        reduce_only: bool,
    ) -> Result<()> {
        let price = product.round_price(quote.price);
        let qty = product.round_qty(quote.qty);
        if qty.is_zero() {
            return Ok(());
        }
        if self.paper_mode {
            let order_id = Uuid::new_v4();
            info!(
                %order_id,
                side = ?quote.side,
                %price,
                %qty,
                reduce_only,
                "[PAPER] placed unwind slice (simulated)"
            );
            self.track_order(LiveOrder {
                order_id,
                symbol: symbol.to_string(),
                side: quote.side,
                price,
                qty,
                filled_qty: dec!(0),
                status: mm_common::types::OrderStatus::Open,
                created_at: chrono::Utc::now(),
            });
            return Ok(());
        }
        let order = NewOrder {
            symbol: symbol.to_string(),
            side: quote.side,
            order_type: OrderType::Limit,
            price: Some(price),
            qty,
            time_in_force: Some(TimeInForce::Ioc),
            client_order_id: None,
            reduce_only,
        };
        match connector.place_order(&order).await {
            Ok(order_id) => {
                info!(
                    %order_id,
                    side = ?quote.side,
                    %price,
                    %qty,
                    "placed unwind slice"
                );
                self.track_order(LiveOrder {
                    order_id,
                    symbol: symbol.to_string(),
                    side: quote.side,
                    price,
                    qty,
                    filled_qty: dec!(0),
                    status: mm_common::types::OrderStatus::Open,
                    created_at: chrono::Utc::now(),
                });
                Ok(())
            }
            Err(e) => {
                warn!(error = %e, "unwind slice placement failed");
                Err(e)
            }
        }
    }

    /// Cancel all live orders (emergency or shutdown).
    ///
    /// Returns `Ok(())` when every tracked order is confirmed
    /// cancelled or gone from the venue, `Err` listing the
    /// still-open order IDs otherwise. Callers that escalate to
    /// `FlattenAll` (kill-switch L4) MUST check this return — a
    /// surviving order leaves locked balance on the venue that
    /// `TwapExecutor` cannot unwind against, producing the stuck
    /// executor described in the Apr 17 audit.
    ///
    /// Verification path: after the per-order / per-batch cancel
    /// pass, we query `get_open_orders` and remove every id that
    /// is no longer present on the venue from local state (a
    /// cancel that raced a fill is still "gone" in the sense that
    /// matters). Any id still in the venue set is reported.
    pub async fn cancel_all(
        &mut self,
        connector: &Arc<dyn ExchangeConnector>,
        symbol: &str,
    ) -> Result<()> {
        let ids: Vec<OrderId> = self.live_orders.keys().copied().collect();
        if ids.is_empty() {
            return Ok(());
        }
        if self.paper_mode {
            for id in &ids {
                self.remove_order(*id);
            }
            info!(
                count = ids.len(),
                "[PAPER] cancelled all orders (simulated)"
            );
            return Ok(());
        }
        let max_batch = connector.capabilities().max_batch_size.max(1);
        if ids.len() >= MIN_BATCH_SIZE && max_batch >= MIN_BATCH_SIZE {
            for chunk in ids.chunks(max_batch) {
                if let Err(e) = connector.cancel_orders_batch(symbol, chunk).await {
                    warn!(
                        error = %e,
                        chunk_len = chunk.len(),
                        "batch cancel_all failed — falling back to per-order"
                    );
                    for id in chunk {
                        if let Err(e2) = connector.cancel_order(symbol, *id).await {
                            warn!(order_id = %id, error = %e2, "per-order cancel fallback failed");
                        }
                    }
                }
            }
        } else {
            for order_id in &ids {
                if let Err(e) = connector.cancel_order(symbol, *order_id).await {
                    warn!(order_id = %order_id, error = %e, "cancel failed");
                }
            }
        }

        // Verify: pull open orders from venue and sync local state
        // against truth. Ids that are no longer on the venue are
        // removed from `live_orders`. Ids still present get retried
        // once more — the most common cause for a survivor here is
        // a rate-limited batch that threw before the venue saw it.
        match connector.get_open_orders(symbol).await {
            Ok(venue_orders) => {
                let still_open: std::collections::HashSet<OrderId> =
                    venue_orders.iter().map(|o| o.order_id).collect();
                for id in &ids {
                    if !still_open.contains(id) {
                        self.remove_order(*id);
                    }
                }
                let mut survivors: Vec<OrderId> = ids
                    .iter()
                    .copied()
                    .filter(|id| still_open.contains(id))
                    .collect();
                if survivors.is_empty() {
                    info!(count = ids.len(), "all orders cancelled (verified)");
                    return Ok(());
                }
                // One final per-order retry.
                warn!(
                    survivors = survivors.len(),
                    "cancel_all: {} orders still open after first pass — retrying",
                    survivors.len()
                );
                for id in &survivors {
                    if let Err(e) = connector.cancel_order(symbol, *id).await {
                        warn!(order_id = %id, error = %e, "retry cancel failed");
                    }
                }
                // Re-verify.
                match connector.get_open_orders(symbol).await {
                    Ok(final_orders) => {
                        let final_open: std::collections::HashSet<OrderId> =
                            final_orders.iter().map(|o| o.order_id).collect();
                        survivors.retain(|id| final_open.contains(id));
                        // Sync anything that's gone.
                        for id in &ids {
                            if !final_open.contains(id) {
                                self.remove_order(*id);
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "cancel_all verify (retry) failed — assuming survivors still open");
                    }
                }
                if survivors.is_empty() {
                    info!("all orders cancelled after retry");
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "cancel_all left {} order(s) still open on venue: {:?}",
                        survivors.len(),
                        survivors
                    ))
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    tracked = ids.len(),
                    "cancel_all: get_open_orders failed — removing local tracking \
                     optimistically (next reconcile will re-detect any phantoms)"
                );
                // Without venue truth we clear local state — next
                // reconcile cycle will re-attach any phantoms.
                for id in &ids {
                    self.remove_order(*id);
                }
                // We did issue the cancels, we just can't verify.
                // Return Ok so shutdown/kill paths do not stall on
                // a transient REST failure; the periodic reconcile
                // will catch any straggler.
                Ok(())
            }
        }
    }

    pub(crate) fn track_order(&mut self, order: LiveOrder) {
        // Epic R — feed placement into the surveillance tape. Fires
        // from both real-venue and paper-mode paths since both go
        // through `track_order`.
        self.feed_surveillance(mm_risk::surveillance::SurveillanceEvent::OrderPlaced {
            order_id: format!("{:?}", order.order_id),
            symbol: order.symbol.clone(),
            side: match order.side {
                Side::Buy => mm_risk::surveillance::Side::Buy,
                Side::Sell => mm_risk::surveillance::Side::Sell,
            },
            price: order.price,
            qty: order.qty,
            ts: chrono::Utc::now(),
        });
        self.price_index
            .insert((order.side, order.price), order.order_id);
        self.live_orders.insert(order.order_id, order);
    }

    pub(crate) fn remove_order(&mut self, order_id: OrderId) {
        if let Some(order) = self.live_orders.remove(&order_id) {
            // Epic R — cancellation tape. `remove_order` runs on
            // venue-confirmed cancels; the tracker pairs this with
            // the earlier placement to compute order lifetime.
            self.feed_surveillance(mm_risk::surveillance::SurveillanceEvent::OrderCancelled {
                order_id: format!("{:?}", order_id),
                symbol: order.symbol.clone(),
                ts: chrono::Utc::now(),
            });
            self.price_index.remove(&(order.side, order.price));
        }
    }

    /// Handle a fill event — update or remove the filled order.
    pub fn on_fill(&mut self, order_id: OrderId, filled_qty: Qty) {
        if let Some(order) = self.live_orders.get_mut(&order_id) {
            order.filled_qty += filled_qty;
            if order.filled_qty >= order.qty {
                let id = order.order_id;
                self.remove_order(id);
            }
        }
    }

    /// Paper-mode fill simulation driven by a public trade. Only
    /// active when `paper_mode` is set — returns an empty vec
    /// otherwise so the real path stays untouched.
    ///
    /// Model: a taker trade at `trade_price` with `taker_side`
    /// fills every resting paper order whose price the trade
    /// crossed on the opposite side:
    /// - taker Buy (hit ask)  ⇒ fills every Sell order at price ≤ trade_price
    /// - taker Sell (hit bid) ⇒ fills every Buy order at price ≥ trade_price
    ///
    /// The returned `Fill` records have `is_maker = true` — the
    /// MM sat resting on the book — and the order is removed from
    /// local tracking after fill. This is an aggressive model
    /// (every crossing trade fills every eligible resting quote
    /// fully) but it's the simplest first-order simulation that
    /// lets PnL / inventory / SLA accumulate during a paper run.
    /// Queue-position-aware simulation lives in `mm-backtester`;
    /// here we just want "something fills" so the pipeline
    /// exercises the real fill path.
    pub fn paper_match_trade(&mut self, trade_price: Price, taker_side: Side) -> Vec<Fill> {
        // Build an id → price lookup so the filter can return the
        // original quote price as the fill price (no slippage).
        let prices: HashMap<OrderId, Price> = self
            .live_orders
            .values()
            .map(|o| (o.order_id, o.price))
            .collect();
        self.paper_match_trade_filtered(trade_price, taker_side, |id| prices.get(&id).copied())
    }

    /// 22C-2 — queue-aware paper fill path. Same shape as
    /// `paper_match_trade` but consults `should_fill(order_id)`
    /// before firing each candidate. The engine passes a
    /// closure that checks `QueueTracker::queue_pos_of(id)` —
    /// orders with non-zero front_q_qty get skipped because
    /// the book still has liquidity ahead of them that would
    /// have absorbed the trade first.
    ///
    /// Closes the backtester / paper-mode fill-model parity
    /// gap: the old unconditional aggressive fill fired every
    /// eligible resting quote on every crossing trade,
    /// producing paper PnL that diverged from the backtester's
    /// queue-position-aware simulator (and from live venue
    /// behaviour). Operators who want the legacy aggressive
    /// fill still get it via `paper_match_trade` — the filter
    /// defaults to always-fill.
    pub fn paper_match_trade_filtered(
        &mut self,
        trade_price: Price,
        taker_side: Side,
        mut should_fill: impl FnMut(OrderId) -> Option<Price>,
    ) -> Vec<Fill> {
        if !self.paper_mode {
            return Vec::new();
        }
        // Collect (id, override_price) for each candidate. `should_fill`
        // returning `None` means the candidate is rejected (queue gate,
        // probability gate, etc.). `Some(p)` means fill at `p` — the
        // override lets the caller apply slippage or other price
        // perturbations (23-P1-5 PaperFillCfg wiring).
        let candidates: Vec<(OrderId, Price)> = self
            .live_orders
            .values()
            .filter(|o| match (taker_side, o.side) {
                (Side::Buy, Side::Sell) => o.price <= trade_price,
                (Side::Sell, Side::Buy) => o.price >= trade_price,
                _ => false,
            })
            .map(|o| o.order_id)
            .filter_map(|id| should_fill(id).map(|p| (id, p)))
            .collect();
        let mut fills = Vec::with_capacity(candidates.len());
        for (id, fill_price) in candidates {
            if let Some(order) = self.live_orders.remove(&id) {
                self.price_index.remove(&(order.side, order.price));
                fills.push(Fill {
                    trade_id: rand_trade_id(),
                    order_id: order.order_id,
                    symbol: order.symbol.clone(),
                    side: order.side,
                    price: fill_price,
                    qty: order.qty - order.filled_qty,
                    is_maker: true,
                    timestamp: chrono::Utc::now(),
                });
            }
        }
        fills
    }
}

/// Monotonically-unique synthetic trade id for paper fills.
/// `u64` is big enough to never collide with real venue trade ids
/// within a single run — the paper path does not need to stitch
/// into venue-wide sequences.
fn rand_trade_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Default for OrderManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
