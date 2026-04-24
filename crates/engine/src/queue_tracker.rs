//! Live per-order queue-position tracker (BOOK-1 + BOOK-2).
//!
//! Wraps the Rigtorp queue model (see `mm_common::queue_model`)
//! with the bookkeeping needed to run it against a real exchange
//! feed:
//!
//! - One [`QueuePos`] per resting maker order, keyed by `OrderId`.
//! - A reverse index `(symbol, side, price) → [OrderId]` so a
//!   single trade or depth event routes to every tracker sitting
//!   at that price level without a full scan.
//! - A per-symbol EWMA of trade qty-per-second so the tracker can
//!   translate queue position into a fill-probability estimate for
//!   the [`Book.FillProbability`] graph source.
//!
//! The tracker only models **maker** orders: takers don't have a
//! queue position. Orders are attached on venue-ack and detached
//! on cancel-ack, full-fill, or amend (amend resets queue state
//! because the re-price lands at the back of the new level).

use std::collections::HashMap;

use mm_common::queue_model::{LogProbQueueFunc, QueuePos};
use mm_common::types::{OrderId, Price, Qty, Side};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;

/// Default short-horizon used when the `Book.FillProbability`
/// estimator has no explicit `horizon_sec` config. 60 seconds
/// matches the cadence that momentum / OTR / stat-arb feature
/// windows already use, so the probability number answers the
/// concrete question "would this order fill in the next minute".
const DEFAULT_PROB_HORIZON_SEC: f64 = 60.0;

/// EWMA half-life for the per-symbol trade-rate estimator. A
/// half-life of 30 s means a trade that arrived 30 seconds ago
/// has half the weight of a trade that arrived just now. Matches
/// the time scale the fill-probability estimator works on.
const TRADE_RATE_HALF_LIFE_SEC: f64 = 30.0;

/// Pre-computed decay factor for a one-second gap. Trade-rate
/// samples that happen in the same second accumulate without
/// decay; gaps advance the EWMA by `exp(-ln2 / half_life)` per
/// elapsed second.
fn decay_for_gap(gap_sec: f64) -> f64 {
    (-std::f64::consts::LN_2 * gap_sec / TRADE_RATE_HALF_LIFE_SEC).exp()
}

#[derive(Debug, Clone)]
struct OrderTrack {
    symbol: String,
    side: Side,
    price: Price,
    queue_pos: QueuePos,
}

#[derive(Debug, Clone, Copy)]
struct TradeRate {
    /// EWMA of trade qty per second.
    qty_per_sec: f64,
    /// Wall time in milliseconds of the last update. Used to
    /// compute the decay factor on the next sample.
    last_update_ms: i64,
}

/// The tracker itself — one instance per engine. Shared state
/// behind `&mut self`; the engine drives it serially from the
/// main select loop.
pub struct QueueTracker {
    orders: HashMap<OrderId, OrderTrack>,
    by_level: HashMap<(String, Side, Price), Vec<OrderId>>,
    /// Last book qty we saw at each `(symbol, side, price)` so
    /// that `on_depth_change` can compute the delta against its
    /// own history — the book-keeper only reports the new state.
    last_book_qty: HashMap<(String, Side, Price), Qty>,
    trade_rate: HashMap<String, TradeRate>,
    prob_model: LogProbQueueFunc,
}

impl Default for QueueTracker {
    fn default() -> Self {
        Self {
            orders: HashMap::new(),
            by_level: HashMap::new(),
            last_book_qty: HashMap::new(),
            trade_rate: HashMap::new(),
            prob_model: LogProbQueueFunc::new(),
        }
    }
}

impl QueueTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a queue tracker to a newly-acked maker order.
    /// `book_qty_at_price` is the resting qty on the same side
    /// at the order's price at the moment of placement — the
    /// fresh order joins the back of that queue. If the price
    /// level is empty at placement time, pass `Decimal::ZERO`.
    pub fn on_order_placed(
        &mut self,
        order_id: OrderId,
        symbol: &str,
        side: Side,
        price: Price,
        book_qty_at_price: Qty,
    ) {
        let track = OrderTrack {
            symbol: symbol.to_string(),
            side,
            price,
            queue_pos: QueuePos::new(book_qty_at_price),
        };
        self.by_level
            .entry((symbol.to_string(), side, price))
            .or_default()
            .push(order_id);
        self.orders.insert(order_id, track);
        // Seed the depth-change baseline so the first subsequent
        // book delta compares against a known-good number rather
        // than zero.
        self.last_book_qty
            .insert((symbol.to_string(), side, price), book_qty_at_price);
    }

    /// Drop a tracker when the order is cancel-acked or has been
    /// fully filled. Idempotent — calling on an unknown id is a
    /// no-op. When the last order at a price level goes away the
    /// `last_book_qty` cache entry for that level is also
    /// dropped, so the cache stays bounded to levels the engine
    /// actually has skin at.
    pub fn on_order_cancelled(&mut self, order_id: OrderId) {
        if let Some(track) = self.orders.remove(&order_id) {
            let key = (track.symbol, track.side, track.price);
            if let Some(list) = self.by_level.get_mut(&key) {
                list.retain(|id| *id != order_id);
                if list.is_empty() {
                    self.by_level.remove(&key);
                    self.last_book_qty.remove(&key);
                }
            }
        }
    }

    /// Handle an in-place amend (`OrderManager::reprice_order`)
    /// — the order_id stays the same but the price moves. The
    /// old queue state is irrelevant (we joined the back of the
    /// new level on venue re-ack), so we drop and re-attach.
    pub fn on_order_amended(
        &mut self,
        order_id: OrderId,
        new_price: Price,
        new_book_qty_at_price: Qty,
    ) {
        let Some(track) = self.orders.get(&order_id).cloned() else {
            return;
        };
        self.on_order_cancelled(order_id);
        self.on_order_placed(
            order_id,
            &track.symbol,
            track.side,
            new_price,
            new_book_qty_at_price,
        );
    }

    /// Called when our own order fills (fully or partially).
    /// Returns the queue-inferred filled qty — positive when the
    /// queue model thinks we got a fill, zero otherwise. The
    /// caller uses the actual venue-reported qty as the
    /// source of truth; this is advisory, used to detect large
    /// queue-model drift against reality.
    pub fn on_order_filled(&mut self, order_id: OrderId) -> Decimal {
        let filled = self
            .orders
            .get_mut(&order_id)
            .map(|t| t.queue_pos.consume_fill())
            .unwrap_or(Decimal::ZERO);
        // Caller removes fully-filled orders via `on_order_cancelled`
        // after `OrderManager::on_fill` tears them down.
        filled
    }

    /// Route a market trade to every tracker currently sitting at
    /// the same price level. Also feeds the per-symbol trade-rate
    /// EWMA so the fill-probability estimator sees realistic
    /// arrival intensity. `now_ms` is the wall time in
    /// milliseconds the event was observed at.
    pub fn on_trade(&mut self, symbol: &str, price: Price, qty: Qty, now_ms: i64) {
        self.bump_trade_rate(symbol, qty, now_ms);

        for side in [Side::Buy, Side::Sell] {
            let key = (symbol.to_string(), side, price);
            let Some(ids) = self.by_level.get(&key).cloned() else {
                continue;
            };
            for id in ids {
                if let Some(track) = self.orders.get_mut(&id) {
                    track.queue_pos.on_trade(qty);
                }
            }
        }
    }

    /// Route a depth-change event at `(symbol, side, price)`.
    /// `new_qty` is the fresh resting qty reported by the
    /// book-keeper. The tracker computes the delta against its
    /// own `last_book_qty` cache (book-keeper only reports the
    /// new state), splits it front/back via the probability
    /// model, and advances every queue-pos at this level.
    pub fn on_depth_change(&mut self, symbol: &str, side: Side, price: Price, new_qty: Qty) {
        let key = (symbol.to_string(), side, price);
        // Bound the cache to levels we have active orders at —
        // depth changes elsewhere aren't interesting and would
        // grow the map without limit on a busy book.
        let Some(ids) = self.by_level.get(&key).cloned() else {
            return;
        };
        let prev_qty = self.last_book_qty.get(&key).copied().unwrap_or(new_qty);
        self.last_book_qty.insert(key, new_qty);
        for id in ids {
            if let Some(track) = self.orders.get_mut(&id) {
                track
                    .queue_pos
                    .on_depth_change(prev_qty, new_qty, &self.prob_model);
            }
        }
    }

    /// Estimated probability (in `[0, 1]`) that the maker order
    /// resting at `(symbol, side, price)` fills within a
    /// 60-second horizon. Returns `None` if no such order is
    /// tracked. When multiple orders share the price level, the
    /// estimate targets the order **closest to the front** — the
    /// best case for the caller.
    ///
    /// Model: let `Q = front_q_qty` (qty ahead of us) and `λ` =
    /// per-symbol EWMA trade rate in qty/sec. Expected fill time
    /// ≈ `Q / λ`. Within horizon `T`, probability of fill is
    /// approximated as `min(1, T·λ / max(Q, ε))`. This is a
    /// Poisson-rate-matching heuristic, not the exact
    /// first-passage probability — but it's monotone in both Q
    /// and λ, bounded in [0,1], and gives strategy nodes a
    /// workable ordering signal.
    pub fn fill_probability(&self, symbol: &str, side: Side, price: Price) -> Option<Decimal> {
        let key = (symbol.to_string(), side, price);
        let ids = self.by_level.get(&key)?;
        let min_front = ids
            .iter()
            .filter_map(|id| self.orders.get(id))
            .map(|t| t.queue_pos.front_q_qty)
            .min()?;

        // Already at the front → next trade at this price fills us.
        if min_front <= Decimal::ZERO {
            return Some(Decimal::ONE);
        }

        let rate = self
            .trade_rate
            .get(symbol)
            .map(|r| r.qty_per_sec)
            .unwrap_or(0.0);
        if rate <= 0.0 {
            // No trade flow observed yet — the queue can't clear.
            return Some(Decimal::ZERO);
        }

        let front_f = min_front.to_string().parse::<f64>().unwrap_or(0.0);
        let p = (DEFAULT_PROB_HORIZON_SEC * rate / front_f.max(1e-12)).min(1.0);
        Decimal::from_f64(p)
    }

    /// Best-priced resting order on `side` for `symbol` — best
    /// bid (highest price) or best ask (lowest). Returned
    /// price is suitable as the `price` argument to
    /// `fill_probability`. `None` when no order on that side
    /// is tracked.
    pub fn best_price_on(&self, symbol: &str, side: Side) -> Option<Price> {
        self.orders
            .values()
            .filter(|t| t.symbol == symbol && t.side == side)
            .map(|t| t.price)
            .fold(None, |acc, p| match (acc, side) {
                (None, _) => Some(p),
                (Some(cur), Side::Buy) => Some(cur.max(p)),
                (Some(cur), Side::Sell) => Some(cur.min(p)),
            })
    }

    /// Read-only accessor for tests and debug assertions.
    #[cfg(test)]
    pub fn tracked_order_count(&self) -> usize {
        self.orders.len()
    }

    /// Read-only accessor for the queue position of a specific
    /// order. Used by tests to assert the front moved after a
    /// trade / depth change, and by the engine's 22C-2
    /// queue-aware paper fill gate to decide whether a
    /// crossing trade should fire a synthetic fill.
    pub fn queue_pos_of(&self, order_id: OrderId) -> Option<QueuePos> {
        self.orders.get(&order_id).map(|t| t.queue_pos)
    }

    fn bump_trade_rate(&mut self, symbol: &str, qty: Qty, now_ms: i64) {
        let qty_f = qty.to_string().parse::<f64>().unwrap_or(0.0).max(0.0);
        let entry = self
            .trade_rate
            .entry(symbol.to_string())
            .or_insert(TradeRate {
                qty_per_sec: 0.0,
                last_update_ms: now_ms,
            });
        let gap_sec = ((now_ms - entry.last_update_ms).max(0) as f64) / 1_000.0;
        let decay = decay_for_gap(gap_sec);
        // Mix the new sample in as qty-per-one-second. Samples
        // that arrive in the same millisecond bump the rate
        // without decay; samples after a long quiet window decay
        // the EWMA back down first. The "+ qty" term acts as a
        // unit-rate impulse, so a symbol with 10 units/sec of
        // trade flow converges to ~10 · (1 / (1 - decay_1sec)) —
        // scale with the half-life constant if tuning.
        entry.qty_per_sec = entry.qty_per_sec * decay + qty_f;
        entry.last_update_ms = now_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    fn oid() -> OrderId {
        Uuid::new_v4()
    }

    #[test]
    fn on_order_placed_seeds_queue_position() {
        let mut t = QueueTracker::new();
        let id = oid();
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(10));
        let q = t.queue_pos_of(id).unwrap();
        assert_eq!(q.front_q_qty, dec!(10));
    }

    #[test]
    fn trade_at_level_advances_front() {
        let mut t = QueueTracker::new();
        let id = oid();
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(10));
        t.on_trade("BTCUSDT", dec!(50_000), dec!(3), 0);
        assert_eq!(t.queue_pos_of(id).unwrap().front_q_qty, dec!(7));
    }

    #[test]
    fn trade_at_different_price_is_ignored() {
        let mut t = QueueTracker::new();
        let id = oid();
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(10));
        t.on_trade("BTCUSDT", dec!(49_999), dec!(3), 0);
        assert_eq!(t.queue_pos_of(id).unwrap().front_q_qty, dec!(10));
    }

    #[test]
    fn trade_for_different_symbol_is_ignored() {
        let mut t = QueueTracker::new();
        let id = oid();
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(10));
        t.on_trade("ETHUSDT", dec!(50_000), dec!(3), 0);
        assert_eq!(t.queue_pos_of(id).unwrap().front_q_qty, dec!(10));
    }

    #[test]
    fn depth_change_advances_front_via_prob_model() {
        let mut t = QueueTracker::new();
        let id = oid();
        // Seed: 20 units resting, we're at the back → front = 20.
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(20));
        // A cancel took 8 units out → new qty = 12. The prob
        // model will split the 8 between front and back.
        t.on_depth_change("BTCUSDT", Side::Buy, dec!(50_000), dec!(12));
        let front = t.queue_pos_of(id).unwrap().front_q_qty;
        assert!(front < dec!(20), "front must advance, got {}", front);
        assert!(front > dec!(0));
    }

    #[test]
    fn amend_resets_queue_position_at_new_price() {
        let mut t = QueueTracker::new();
        let id = oid();
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(10));
        t.on_trade("BTCUSDT", dec!(50_000), dec!(3), 0);
        assert_eq!(t.queue_pos_of(id).unwrap().front_q_qty, dec!(7));
        // Amend to a new price — queue state resets to the back
        // of the new level (which has 25 resting).
        t.on_order_amended(id, dec!(49_990), dec!(25));
        assert_eq!(t.queue_pos_of(id).unwrap().front_q_qty, dec!(25));
    }

    #[test]
    fn cancel_removes_tracker() {
        let mut t = QueueTracker::new();
        let id = oid();
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(10));
        assert_eq!(t.tracked_order_count(), 1);
        t.on_order_cancelled(id);
        assert_eq!(t.tracked_order_count(), 0);
    }

    #[test]
    fn fill_probability_is_one_at_front() {
        let mut t = QueueTracker::new();
        let id = oid();
        // Zero qty ahead of us means the next trade fills us.
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(0));
        // Trade-rate still needs some signal or we return 0 — but
        // not at the front path; that short-circuits.
        assert_eq!(
            t.fill_probability("BTCUSDT", Side::Buy, dec!(50_000)),
            Some(Decimal::ONE),
        );
    }

    #[test]
    fn fill_probability_is_zero_without_trade_flow() {
        let mut t = QueueTracker::new();
        let id = oid();
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(10));
        // No on_trade calls → rate is zero → probability is zero.
        assert_eq!(
            t.fill_probability("BTCUSDT", Side::Buy, dec!(50_000)),
            Some(Decimal::ZERO),
        );
    }

    #[test]
    fn fill_probability_scales_with_trade_flow() {
        let mut t = QueueTracker::new();
        let id = oid();
        // Park behind a deep queue (5000 units) so the
        // horizon×rate/Q term doesn't instantly saturate at 1
        // — this test wants to observe monotonicity, not the
        // clamp.
        t.on_order_placed(id, "BTCUSDT", Side::Buy, dec!(50_000), dec!(5000));
        for i in 0..5 {
            t.on_trade("BTCUSDT", dec!(49_999), dec!(1), i * 1_000);
        }
        let p_low = t
            .fill_probability("BTCUSDT", Side::Buy, dec!(50_000))
            .unwrap();
        for i in 5..10 {
            t.on_trade("BTCUSDT", dec!(49_999), dec!(10), i * 1_000);
        }
        let p_high = t
            .fill_probability("BTCUSDT", Side::Buy, dec!(50_000))
            .unwrap();
        assert!(
            p_high > p_low,
            "higher trade rate must raise fill probability: low={} high={}",
            p_low,
            p_high,
        );
        assert!(p_low < Decimal::ONE);
        assert!(p_high <= Decimal::ONE);
    }

    #[test]
    fn fill_probability_missing_order_is_none() {
        let t = QueueTracker::new();
        assert!(t
            .fill_probability("BTCUSDT", Side::Buy, dec!(50_000))
            .is_none());
    }

    #[test]
    fn multiple_orders_at_same_level_share_routing() {
        let mut t = QueueTracker::new();
        let a = oid();
        let b = oid();
        t.on_order_placed(a, "BTCUSDT", Side::Buy, dec!(50_000), dec!(10));
        t.on_order_placed(b, "BTCUSDT", Side::Buy, dec!(50_000), dec!(12));
        t.on_trade("BTCUSDT", dec!(50_000), dec!(4), 0);
        // Both trackers got the 4-unit trade.
        assert_eq!(t.queue_pos_of(a).unwrap().front_q_qty, dec!(6));
        assert_eq!(t.queue_pos_of(b).unwrap().front_q_qty, dec!(8));
    }

    #[test]
    fn fill_probability_picks_front_most_order() {
        let mut t = QueueTracker::new();
        let a = oid();
        let b = oid();
        // A is already near the front (2 ahead); B is at the
        // back (20 ahead). The estimator targets A.
        t.on_order_placed(a, "BTCUSDT", Side::Buy, dec!(50_000), dec!(2));
        t.on_order_placed(b, "BTCUSDT", Side::Buy, dec!(50_000), dec!(20));
        for i in 0..5 {
            t.on_trade("BTCUSDT", dec!(49_999), dec!(1), i * 1_000);
        }
        let p_any = t
            .fill_probability("BTCUSDT", Side::Buy, dec!(50_000))
            .unwrap();
        // With A having just 2 units ahead + ~1 unit/sec trade
        // rate × 60 sec horizon, p is clamped to 1.
        assert_eq!(p_any, Decimal::ONE);
    }
}
