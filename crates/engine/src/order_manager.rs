use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use mm_common::types::{LiveOrder, OrderId, OrderType, Price, Qty, Quote, QuotePair, Side};
use mm_common::types::{ProductSpec, TimeInForce};
use mm_exchange_core::connector::{AmendOrder, ExchangeConnector, NewOrder};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

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
}

impl OrderManager {
    pub fn new() -> Self {
        Self {
            live_orders: HashMap::new(),
            price_index: HashMap::new(),
        }
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
                        let outcome =
                            self.place_one_quote_unack(symbol, quote, connector).await;
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
                        let outcome =
                            self.place_one_quote_fallback(symbol, quote, connector).await;
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
        let order = NewOrder {
            symbol: symbol.to_string(),
            side: quote.side,
            order_type: OrderType::Limit,
            price: Some(quote.price),
            qty: quote.qty,
            time_in_force: Some(TimeInForce::PostOnly),
            client_order_id: None,
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
                warn!(
                    side = ?quote.side,
                    price = %quote.price,
                    error = %e,
                    "failed to place order"
                );
                BatchPlaceOutcome::Failed {
                    reason: e.to_string(),
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
            BatchPlaceOutcome::Failed { reason } => {
                BatchPlaceOutcome::Unacknowledged { reason }
            }
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
                        let outcome =
                            self.cancel_one_outcome(symbol, *order_id, connector).await;
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
        match connector.cancel_order(symbol, order_id).await {
            Ok(_) => {
                debug!(%order_id, "cancelled stale order");
                self.remove_order(order_id);
                BatchCancelOutcome::Cancelled
            }
            Err(e) => {
                warn!(%order_id, error = %e, "failed to cancel order");
                self.remove_order(order_id);
                BatchCancelOutcome::Failed {
                    reason: e.to_string(),
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
        let price = product.round_price(quote.price);
        let qty = product.round_qty(quote.qty);
        if qty.is_zero() {
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
    pub async fn cancel_all(&mut self, connector: &Arc<dyn ExchangeConnector>, symbol: &str) {
        let ids: Vec<OrderId> = self.live_orders.keys().copied().collect();
        if ids.is_empty() {
            return;
        }
        let max_batch = connector.capabilities().max_batch_size.max(1);
        if ids.len() >= MIN_BATCH_SIZE && max_batch >= MIN_BATCH_SIZE {
            // Use batch cancel for speed on shutdown.
            for chunk in ids.chunks(max_batch) {
                match connector.cancel_orders_batch(symbol, chunk).await {
                    Ok(_) => {
                        for id in chunk {
                            self.remove_order(*id);
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "batch cancel_all failed — falling back to per-order");
                        for id in chunk {
                            let _ = connector.cancel_order(symbol, *id).await;
                            self.remove_order(*id);
                        }
                    }
                }
            }
        } else {
            for order_id in ids {
                let _ = connector.cancel_order(symbol, order_id).await;
                self.remove_order(order_id);
            }
        }
        info!("all orders cancelled");
    }

    pub(crate) fn track_order(&mut self, order: LiveOrder) {
        self.price_index
            .insert((order.side, order.price), order.order_id);
        self.live_orders.insert(order.order_id, order);
    }

    pub(crate) fn remove_order(&mut self, order_id: OrderId) {
        if let Some(order) = self.live_orders.remove(&order_id) {
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
}

impl Default for OrderManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn product_btcusdt() -> ProductSpec {
        ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
            trading_status: Default::default(),
        }
    }

    fn live(side: Side, price: Price, qty: Qty) -> LiveOrder {
        LiveOrder {
            order_id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".into(),
            side,
            price,
            qty,
            filled_qty: dec!(0),
            status: mm_common::types::OrderStatus::Open,
            created_at: chrono::Utc::now(),
        }
    }

    fn pair_bid_ask(bid_px: Price, ask_px: Price, qty: Qty) -> QuotePair {
        QuotePair {
            bid: Some(Quote {
                side: Side::Buy,
                price: bid_px,
                qty,
            }),
            ask: Some(Quote {
                side: Side::Sell,
                price: ask_px,
                qty,
            }),
        }
    }

    /// A small price tweak with the same qty must collapse into an
    /// amend instead of a cancel + place pair. This is the whole
    /// point of P1.1: the live order keeps its queue priority
    /// across the refresh.
    #[test]
    fn small_price_tweak_with_same_qty_becomes_amend() {
        let mut mgr = OrderManager::new();
        let bid = live(Side::Buy, dec!(50000.00), dec!(0.01));
        let ask = live(Side::Sell, dec!(50100.00), dec!(0.01));
        let bid_id = bid.order_id;
        let ask_id = ask.order_id;
        mgr.track_order(bid);
        mgr.track_order(ask);

        // Tweak both sides one tick. epsilon = 5 ticks → both qualify.
        let desired = vec![pair_bid_ask(dec!(50000.01), dec!(50099.99), dec!(0.01))];
        let plan = mgr.diff_orders(&desired, &product_btcusdt(), 5);

        assert!(plan.to_cancel.is_empty(), "no cancels expected");
        assert!(plan.to_place.is_empty(), "no places expected");
        assert_eq!(plan.to_amend.len(), 2);
        let bid_amend = plan
            .to_amend
            .iter()
            .find(|a| a.side == Side::Buy)
            .expect("bid amend present");
        let ask_amend = plan
            .to_amend
            .iter()
            .find(|a| a.side == Side::Sell)
            .expect("ask amend present");
        assert_eq!(bid_amend.order_id, bid_id);
        assert_eq!(bid_amend.new_price, dec!(50000.01));
        assert_eq!(ask_amend.order_id, ask_id);
        assert_eq!(ask_amend.new_price, dec!(50099.99));
    }

    /// A qty change must defeat the amend pairing — Bybit's amend
    /// RPC accepts a new qty, but resizing the order on the venue
    /// drops queue priority anyway, so it is not a P1.1 win and we
    /// keep the pair on cancel+place.
    #[test]
    fn qty_change_defeats_amend_pairing() {
        let mut mgr = OrderManager::new();
        mgr.track_order(live(Side::Buy, dec!(50000.00), dec!(0.01)));

        let desired = vec![QuotePair {
            bid: Some(Quote {
                side: Side::Buy,
                price: dec!(50000.01),
                qty: dec!(0.02),
            }),
            ask: None,
        }];
        let plan = mgr.diff_orders(&desired, &product_btcusdt(), 5);
        assert!(plan.to_amend.is_empty());
        assert_eq!(plan.to_cancel.len(), 1);
        assert_eq!(plan.to_place.len(), 1);
    }

    /// Price tweak larger than `epsilon * tick_size` falls back to
    /// cancel + place — the amend window is intentionally tight so
    /// big quote refreshes still hit the venue's risk gates.
    #[test]
    fn price_diff_outside_epsilon_defeats_amend_pairing() {
        let mut mgr = OrderManager::new();
        mgr.track_order(live(Side::Buy, dec!(50000.00), dec!(0.01)));

        // 10 ticks vs epsilon=5 → no amend.
        let desired = vec![QuotePair {
            bid: Some(Quote {
                side: Side::Buy,
                price: dec!(50000.10),
                qty: dec!(0.01),
            }),
            ask: None,
        }];
        let plan = mgr.diff_orders(&desired, &product_btcusdt(), 5);
        assert!(plan.to_amend.is_empty());
        assert_eq!(plan.to_cancel.len(), 1);
        assert_eq!(plan.to_place.len(), 1);
    }

    /// `amend_epsilon_ticks = 0` is the legacy cancel + place path:
    /// even an exact-match same-qty same-side replacement must NOT
    /// produce an amend. This is the regression anchor for the
    /// "amend disabled" config state.
    #[test]
    fn epsilon_zero_disables_amend_pairing() {
        let mut mgr = OrderManager::new();
        mgr.track_order(live(Side::Buy, dec!(50000.00), dec!(0.01)));

        let desired = vec![QuotePair {
            bid: Some(Quote {
                side: Side::Buy,
                price: dec!(50000.01),
                qty: dec!(0.01),
            }),
            ask: None,
        }];
        let plan = mgr.diff_orders(&desired, &product_btcusdt(), 0);
        assert!(plan.to_amend.is_empty());
        assert_eq!(plan.to_cancel.len(), 1);
        assert_eq!(plan.to_place.len(), 1);
    }

    /// Amend pairs by side: a stale bid must not steal a new ask
    /// even when the prices coincidentally land in the same
    /// numerical window. Catches a sloppy implementation that
    /// matches purely on `(price, qty)`.
    #[test]
    fn amend_pairing_respects_side() {
        let mut mgr = OrderManager::new();
        mgr.track_order(live(Side::Buy, dec!(50000.00), dec!(0.01)));

        // Desired ask at the same price band as the live bid.
        let desired = vec![QuotePair {
            bid: None,
            ask: Some(Quote {
                side: Side::Sell,
                price: dec!(50000.01),
                qty: dec!(0.01),
            }),
        }];
        let plan = mgr.diff_orders(&desired, &product_btcusdt(), 5);
        assert!(plan.to_amend.is_empty(), "cross-side match must not amend");
        assert_eq!(plan.to_cancel.len(), 1);
        assert_eq!(plan.to_place.len(), 1);
    }

    /// `reprice_order` (called on amend success) must move the
    /// `price_index` slot atomically: the old (side, price) key
    /// disappears, the new key points at the same OrderId, the
    /// `live_orders` entry updates its price field. Any drift in
    /// these three pieces leaves the diff machinery confused on
    /// the next tick.
    #[test]
    fn reprice_order_moves_price_index_and_preserves_id() {
        let mut mgr = OrderManager::new();
        let order = live(Side::Buy, dec!(50000.00), dec!(0.01));
        let id = order.order_id;
        mgr.track_order(order);

        mgr.reprice_order(id, dec!(50000.05));

        assert!(!mgr.price_index.contains_key(&(Side::Buy, dec!(50000.00))));
        assert_eq!(
            mgr.price_index.get(&(Side::Buy, dec!(50000.05))).copied(),
            Some(id)
        );
        assert_eq!(
            mgr.live_orders.get(&id).map(|o| o.price),
            Some(dec!(50000.05))
        );
    }

    #[test]
    fn test_locked_value_quote() {
        let mut mgr = OrderManager::new();

        let o1 = LiveOrder {
            order_id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".to_string(),
            side: Side::Buy,
            price: dec!(50000),
            qty: dec!(0.1),
            filled_qty: dec!(0),
            status: mm_common::types::OrderStatus::Open,
            created_at: chrono::Utc::now(),
        };
        let o2 = LiveOrder {
            order_id: uuid::Uuid::new_v4(),
            symbol: "BTCUSDT".to_string(),
            side: Side::Sell,
            price: dec!(51000),
            qty: dec!(0.2),
            filled_qty: dec!(0.05),
            status: mm_common::types::OrderStatus::PartiallyFilled,
            created_at: chrono::Utc::now(),
        };

        mgr.track_order(o1);
        mgr.track_order(o2);

        // o1: 50000 * 0.1 = 5000. o2: 51000 * (0.2 - 0.05) = 51000 * 0.15 = 7650.
        assert_eq!(mgr.locked_value_quote(), dec!(12650));
    }

    // -------- Epic E sub-component #1 — batch order entry --------

    use crate::test_support::MockConnector;
    use mm_exchange_core::connector::{VenueId, VenueProduct};
    use std::sync::Arc;

    fn make_quotes(n: usize) -> Vec<Quote> {
        (0..n)
            .map(|i| Quote {
                side: Side::Buy,
                price: dec!(50000) + Decimal::from(i as i64),
                qty: dec!(0.001),
            })
            .collect()
    }

    fn mock_connector(max_batch: usize) -> Arc<dyn ExchangeConnector> {
        Arc::new(
            MockConnector::new(VenueId::Bybit, VenueProduct::Spot).with_max_batch_size(max_batch),
        ) as Arc<dyn ExchangeConnector>
    }

    /// Downcast helper for the test-only assertions on
    /// MockConnector counters.
    fn as_mock(connector: &Arc<dyn ExchangeConnector>) -> &MockConnector {
        // SAFETY: every test in this module constructs a
        // MockConnector before the downcast. There is no
        // public Any impl on ExchangeConnector, so we cheat
        // with a raw pointer cast — only safe when the caller
        // guarantees the dyn really is a MockConnector.
        unsafe { &*(Arc::as_ptr(connector) as *const MockConnector) }
    }

    #[tokio::test]
    async fn batch_place_single_quote_uses_per_order_path() {
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        let quotes = make_quotes(1);
        mgr.place_quotes_batched("BTCUSDT", &quotes, &conn).await;
        assert_eq!(as_mock(&conn).place_single_calls(), 1);
        assert_eq!(as_mock(&conn).place_batch_calls(), 0);
        assert_eq!(mgr.live_count(), 1);
    }

    #[tokio::test]
    async fn batch_place_two_quotes_routes_through_batch() {
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        let quotes = make_quotes(2);
        mgr.place_quotes_batched("BTCUSDT", &quotes, &conn).await;
        assert_eq!(as_mock(&conn).place_batch_calls(), 1);
        assert_eq!(as_mock(&conn).place_single_calls(), 0);
        assert_eq!(mgr.live_count(), 2);
    }

    #[tokio::test]
    async fn batch_place_chunks_at_max_batch_size_5() {
        // 12 quotes against a Binance-futures-style max=5 →
        // chunks of (5, 5, 2) → 3 batch calls.
        let mut mgr = OrderManager::new();
        let conn = mock_connector(5);
        let quotes = make_quotes(12);
        mgr.place_quotes_batched("BTCUSDT", &quotes, &conn).await;
        assert_eq!(as_mock(&conn).place_batch_calls(), 3);
        assert_eq!(as_mock(&conn).place_single_calls(), 0);
        assert_eq!(mgr.live_count(), 12);
    }

    #[tokio::test]
    async fn batch_place_chunks_at_max_batch_size_20() {
        // 21 quotes against max=20 → chunks of (20, 1).
        // The trailing 1-element chunk goes through the
        // batch helper too because the slice already passed
        // the MIN_BATCH_SIZE gate at the top level — single-
        // quote chunking inside the loop still uses the
        // batch call. v1 trade-off; the 1-order JSON
        // overhead is negligible vs not having to think
        // about whether the trailing chunk is batchable.
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        let quotes = make_quotes(21);
        mgr.place_quotes_batched("BTCUSDT", &quotes, &conn).await;
        assert_eq!(as_mock(&conn).place_batch_calls(), 2);
        assert_eq!(as_mock(&conn).place_single_calls(), 0);
        assert_eq!(mgr.live_count(), 21);
    }

    #[tokio::test]
    async fn batch_place_failure_falls_back_to_per_order() {
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        as_mock(&conn).arm_batch_place_failure();
        let quotes = make_quotes(3);
        let outcomes = mgr.place_quotes_batched("BTCUSDT", &quotes, &conn).await;
        // Batch call attempted once and failed; fallback
        // hit place_order three times.
        assert_eq!(as_mock(&conn).place_batch_calls(), 1);
        assert_eq!(as_mock(&conn).place_single_calls(), 3);
        // All three orders ended up tracked despite the
        // batch-side failure.
        assert_eq!(mgr.live_count(), 3);
        // All outcomes should be PlacedFallback.
        assert_eq!(outcomes.len(), 3);
        for o in &outcomes {
            assert!(
                matches!(o, BatchPlaceOutcome::PlacedFallback { .. }),
                "expected PlacedFallback, got {:?}",
                o
            );
        }
    }

    #[tokio::test]
    async fn batch_place_empty_input_is_noop() {
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        let outcomes = mgr.place_quotes_batched("BTCUSDT", &[], &conn).await;
        assert_eq!(as_mock(&conn).place_batch_calls(), 0);
        assert_eq!(as_mock(&conn).place_single_calls(), 0);
        assert_eq!(mgr.live_count(), 0);
        assert!(outcomes.is_empty());
    }

    #[tokio::test]
    async fn batch_place_with_max_batch_size_one_uses_per_order() {
        // Pathological venue with max=1 should never call
        // the batch path even on a multi-quote diff.
        let mut mgr = OrderManager::new();
        let conn = mock_connector(1);
        let quotes = make_quotes(5);
        let outcomes = mgr.place_quotes_batched("BTCUSDT", &quotes, &conn).await;
        assert_eq!(as_mock(&conn).place_batch_calls(), 0);
        assert_eq!(as_mock(&conn).place_single_calls(), 5);
        // All should be Placed (per-order path, not fallback).
        for o in &outcomes {
            assert!(
                matches!(o, BatchPlaceOutcome::Placed { .. }),
                "expected Placed, got {:?}",
                o
            );
        }
    }

    #[tokio::test]
    async fn batch_cancel_two_ids_routes_through_batch() {
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        // Track two live orders so remove_order has work to do.
        mgr.track_order(live(Side::Buy, dec!(50000), dec!(0.001)));
        mgr.track_order(live(Side::Buy, dec!(49999), dec!(0.001)));
        let ids: Vec<OrderId> = mgr.live_order_ids();
        mgr.cancel_orders_batched("BTCUSDT", &ids, &conn).await;
        assert_eq!(as_mock(&conn).cancel_batch_calls(), 1);
        assert_eq!(as_mock(&conn).cancel_single_calls(), 0);
        assert_eq!(mgr.live_count(), 0);
    }

    #[tokio::test]
    async fn batch_cancel_single_id_uses_per_order_path() {
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        mgr.track_order(live(Side::Buy, dec!(50000), dec!(0.001)));
        let ids: Vec<OrderId> = mgr.live_order_ids();
        mgr.cancel_orders_batched("BTCUSDT", &ids, &conn).await;
        assert_eq!(as_mock(&conn).cancel_batch_calls(), 0);
        assert_eq!(as_mock(&conn).cancel_single_calls(), 1);
        assert_eq!(mgr.live_count(), 0);
    }

    #[tokio::test]
    async fn batch_cancel_failure_falls_back_to_per_order() {
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        as_mock(&conn).arm_batch_cancel_failure();
        for px in [50000, 49999, 49998] {
            mgr.track_order(live(Side::Buy, Decimal::from(px), dec!(0.001)));
        }
        let ids: Vec<OrderId> = mgr.live_order_ids();
        mgr.cancel_orders_batched("BTCUSDT", &ids, &conn).await;
        assert_eq!(as_mock(&conn).cancel_batch_calls(), 1);
        assert_eq!(as_mock(&conn).cancel_single_calls(), 3);
        assert_eq!(mgr.live_count(), 0);
    }

    #[tokio::test]
    async fn batch_cancel_chunks_at_max_batch_size() {
        // 12 cancels against max=5 → chunks of (5, 5, 2) →
        // 3 batch calls.
        let mut mgr = OrderManager::new();
        let conn = mock_connector(5);
        for i in 0..12 {
            mgr.track_order(live(Side::Buy, dec!(50000) - Decimal::from(i), dec!(0.001)));
        }
        let ids: Vec<OrderId> = mgr.live_order_ids();
        mgr.cancel_orders_batched("BTCUSDT", &ids, &conn).await;
        assert_eq!(as_mock(&conn).cancel_batch_calls(), 3);
        assert_eq!(as_mock(&conn).cancel_single_calls(), 0);
        assert_eq!(mgr.live_count(), 0);
    }

    #[tokio::test]
    async fn batch_cancel_empty_input_is_noop() {
        let mut mgr = OrderManager::new();
        let conn = mock_connector(20);
        mgr.cancel_orders_batched("BTCUSDT", &[], &conn).await;
        assert_eq!(as_mock(&conn).cancel_batch_calls(), 0);
        assert_eq!(as_mock(&conn).cancel_single_calls(), 0);
    }
}
