//! MM-4 — Per-decision cost ledger.
//!
//! Closes the "no per-decision cost attribution" gap: the
//! cumulative PnL tracker tells us the total spread / fee /
//! inventory carry picture, but operators can't ask
//! "what did strategy X's tick N decision actually cost us?"
//!
//! ## Flow
//!
//! 1. Every tick where a strategy authors quotes, the engine
//!    calls [`DecisionLedger::record_decision`] with the
//!    symbol, side, size, mid at decision time, and the
//!    expected-cost-bps estimate from
//!    `LocalOrderBook::impact_bps` (MM-1). The ledger returns
//!    a fresh [`DecisionId`].
//! 2. When the order reaches the venue (post or take),
//!    [`DecisionLedger::bind_order`] attaches the venue
//!    `OrderId` to the decision. One decision can bind many
//!    order IDs — a multi-level ladder is one decision even
//!    though it places N orders.
//! 3. On every fill of a bound order,
//!    [`DecisionLedger::on_fill`] looks the decision up by
//!    order_id, computes realized cost in bps against the
//!    decision-time mid, and returns a [`ResolvedCost`] that
//!    the audit writer + Prometheus histogram consume.
//!
//! ## Storage
//!
//! Bounded ring of 16 384 recent decisions — roughly an hour
//! of intense quoting at 4 ticks/s. Older records evict
//! silently; their `ResolvedCost` numbers have already landed
//! in the audit trail and any Prometheus histograms, so the
//! in-memory retention is for the API readback, nothing else.
//!
//! ## Thread safety
//!
//! The ledger holds internal `Mutex`es so callers can share it
//! via `Arc`. Engine fill path + refresh_quotes path hit the
//! same instance from the same task today, but the
//! cross-thread future-proofing costs nothing.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use uuid::Uuid;

use mm_common::types::{OrderId, Side};

/// Unique identifier for a recorded decision. Wraps `Uuid` so
/// the audit rows + API responses don't leak the raw type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DecisionId(pub Uuid);

impl DecisionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DecisionId {
    fn default() -> Self {
        Self::new()
    }
}

/// One recorded quote-decision. Created by the strategy
/// refresh path, resolved by the fill path.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DecisionRecord {
    pub id: DecisionId,
    pub tick_ms: i64,
    pub symbol: String,
    pub side: Side,
    /// Target qty the strategy wanted this tick (sum across
    /// levels). Realized qty may be less (partial fills,
    /// cancels) — read via the `resolved` list.
    pub target_qty: Decimal,
    /// Mid price at decision time. Realized costs are computed
    /// as `(fill_price - mid_at_decision)` in bps to separate
    /// strategy edge from post-decision mid drift.
    pub mid_at_decision: Decimal,
    /// Cost estimate the strategy's own sizer used — from
    /// `LocalOrderBook::impact_bps` for takers, or the
    /// post-offset for passive quoters. `None` when the
    /// strategy didn't express a pre-trade estimate.
    pub expected_cost_bps: Option<Decimal>,
    /// Fills that have resolved against this decision.
    pub resolved: Vec<ResolvedCost>,
}

/// One fill resolved back to its originating decision.
/// Carries the realized cost against the decision-time mid so
/// the audit log + dashboard can show expected-vs-realized
/// drift per decision.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ResolvedCost {
    pub tick_ms: i64,
    pub fill_price: Decimal,
    pub fill_qty: Decimal,
    /// Realized cost in bps of mid at decision time. For a buy:
    /// `(fill_price - mid_at_decision) / mid_at_decision *
    /// 10_000`. Positive = we paid more than decision-mid
    /// (adverse); negative = we got a better price. Symmetric
    /// for sells with sign flip.
    pub realized_cost_bps: Decimal,
    /// Delta from the pre-trade expectation. `realized - expected`.
    /// `None` when no expectation was recorded. Positive =
    /// worse than expected.
    pub vs_expected_bps: Option<Decimal>,
}

/// Snapshot of a decision record suitable for the JSON API.
/// Cloned from the internal record so the ledger lock is
/// released before serialisation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DecisionSnapshot {
    pub id: DecisionId,
    pub tick_ms: i64,
    pub symbol: String,
    pub side: Side,
    pub target_qty: Decimal,
    pub mid_at_decision: Decimal,
    pub expected_cost_bps: Option<Decimal>,
    pub resolved: Vec<ResolvedCost>,
}

impl From<&DecisionRecord> for DecisionSnapshot {
    fn from(r: &DecisionRecord) -> Self {
        Self {
            id: r.id,
            tick_ms: r.tick_ms,
            symbol: r.symbol.clone(),
            side: r.side,
            target_qty: r.target_qty,
            mid_at_decision: r.mid_at_decision,
            expected_cost_bps: r.expected_cost_bps,
            resolved: r.resolved.clone(),
        }
    }
}

/// Maximum retained decisions. ~1 hour at 4 ticks/s.
pub const DEFAULT_RING_CAPACITY: usize = 16_384;

/// Append-only (with eviction) ledger of recent quote
/// decisions and their realized fills.
#[derive(Debug)]
pub struct DecisionLedger {
    inner: Mutex<LedgerInner>,
}

#[derive(Debug)]
struct LedgerInner {
    records: VecDeque<DecisionRecord>,
    by_id: HashMap<DecisionId, usize>,
    by_order: HashMap<OrderId, DecisionId>,
    capacity: usize,
    /// Cursor — monotonically-increasing insertion index. Used
    /// to translate `by_id`'s logical index (insertion rank)
    /// into the current `VecDeque` position after evictions.
    next_rank: usize,
    /// Rank of the front of the deque — rises by 1 on every
    /// eviction so `by_id[id] - front_rank` is always the live
    /// offset.
    front_rank: usize,
}

impl Default for DecisionLedger {
    fn default() -> Self {
        Self::new(DEFAULT_RING_CAPACITY)
    }
}

impl DecisionLedger {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be positive");
        Self {
            inner: Mutex::new(LedgerInner {
                records: VecDeque::with_capacity(capacity),
                by_id: HashMap::with_capacity(capacity),
                by_order: HashMap::new(),
                capacity,
                next_rank: 0,
                front_rank: 0,
            }),
        }
    }

    /// Record a fresh decision and return its id.
    pub fn record_decision(
        &self,
        tick_ms: i64,
        symbol: &str,
        side: Side,
        target_qty: Decimal,
        mid_at_decision: Decimal,
        expected_cost_bps: Option<Decimal>,
    ) -> DecisionId {
        let id = DecisionId::new();
        let rec = DecisionRecord {
            id,
            tick_ms,
            symbol: symbol.to_string(),
            side,
            target_qty,
            mid_at_decision,
            expected_cost_bps,
            resolved: Vec::new(),
        };
        let Ok(mut g) = self.inner.lock() else {
            return id;
        };
        // Evict at capacity.
        while g.records.len() >= g.capacity {
            let Some(dropped) = g.records.pop_front() else {
                break;
            };
            g.by_id.remove(&dropped.id);
            g.by_order.retain(|_, d| *d != dropped.id);
            g.front_rank += 1;
        }
        let rank = g.next_rank;
        g.records.push_back(rec);
        g.by_id.insert(id, rank);
        g.next_rank += 1;
        id
    }

    /// Attach an `OrderId` to a previously-recorded decision.
    /// Multiple orders (multi-level ladders) can bind to the
    /// same decision; each fill resolves independently.
    ///
    /// Returns `false` when the decision id is not (or no
    /// longer) in the ring — the caller can decide whether
    /// that's silently-accepted (eviction is expected for old
    /// decisions) or a bug.
    pub fn bind_order(&self, decision: DecisionId, order: OrderId) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return false;
        };
        if !g.by_id.contains_key(&decision) {
            return false;
        }
        g.by_order.insert(order, decision);
        true
    }

    /// Resolve a fill back to its decision. Returns the
    /// computed [`ResolvedCost`] if the order was bound to a
    /// live decision; `None` when it wasn't (typical for
    /// pre-graph fills, kill-switch emergency takes, or
    /// post-eviction fills).
    pub fn on_fill(
        &self,
        order: OrderId,
        tick_ms: i64,
        fill_side: Side,
        fill_price: Decimal,
        fill_qty: Decimal,
    ) -> Option<ResolvedCost> {
        let Ok(mut g) = self.inner.lock() else {
            return None;
        };
        let decision = g.by_order.get(&order).copied()?;
        let rank = g.by_id.get(&decision).copied()?;
        let offset = rank.checked_sub(g.front_rank)?;
        let rec = g.records.get_mut(offset)?;
        if rec.mid_at_decision <= Decimal::ZERO {
            return None;
        }
        // Sign matches "adverse = positive" convention. If the
        // fill side disagrees with the decision side (partial
        // unwind on an emergency take) flip the sign so the
        // audit row still makes sense.
        let side_mult = if fill_side == rec.side {
            dec!(1)
        } else {
            dec!(-1)
        };
        let delta = match rec.side {
            Side::Buy => fill_price - rec.mid_at_decision,
            Side::Sell => rec.mid_at_decision - fill_price,
        } * side_mult;
        let ten_k = Decimal::from(10_000);
        let realized_cost_bps = delta / rec.mid_at_decision * ten_k;
        let vs_expected_bps = rec
            .expected_cost_bps
            .map(|e| realized_cost_bps - e);
        let resolved = ResolvedCost {
            tick_ms,
            fill_price,
            fill_qty,
            realized_cost_bps,
            vs_expected_bps,
        };
        rec.resolved.push(resolved);
        Some(resolved)
    }

    /// Return up to `max` most-recent decisions, newest first,
    /// as clones suitable for JSON serialisation.
    pub fn recent(&self, max: usize) -> Vec<DecisionSnapshot> {
        let Ok(g) = self.inner.lock() else {
            return Vec::new();
        };
        g.records
            .iter()
            .rev()
            .take(max)
            .map(DecisionSnapshot::from)
            .collect()
    }

    /// Look up one decision by id. Returns a snapshot clone so
    /// the caller doesn't hold the lock.
    pub fn get(&self, id: DecisionId) -> Option<DecisionSnapshot> {
        let Ok(g) = self.inner.lock() else {
            return None;
        };
        let rank = g.by_id.get(&id).copied()?;
        let offset = rank.checked_sub(g.front_rank)?;
        g.records.get(offset).map(DecisionSnapshot::from)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.records.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_then_bind_then_resolve_round_trip() {
        let l = DecisionLedger::default();
        let id = l.record_decision(
            1_000,
            "BTCUSDT",
            Side::Buy,
            dec!(0.01),
            dec!(100),
            Some(dec!(5)),
        );
        let order = OrderId::new_v4();
        assert!(l.bind_order(id, order));
        // Fill at 100.02 — 2 bps adverse → realized = 2 bps,
        // vs_expected = 2 - 5 = -3 (better than expected).
        let resolved = l
            .on_fill(order, 1_500, Side::Buy, dec!(100.02), dec!(0.01))
            .expect("resolved");
        assert_eq!(resolved.realized_cost_bps, dec!(2));
        assert_eq!(resolved.vs_expected_bps, Some(dec!(-3)));
        let snap = l.get(id).expect("snapshot");
        assert_eq!(snap.resolved.len(), 1);
    }

    #[test]
    fn fill_without_bind_returns_none() {
        let l = DecisionLedger::default();
        let _ = l.record_decision(
            0,
            "BTCUSDT",
            Side::Buy,
            dec!(0.01),
            dec!(100),
            None,
        );
        let spurious_order = OrderId::new_v4();
        let out = l.on_fill(
            spurious_order,
            100,
            Side::Buy,
            dec!(100),
            dec!(0.01),
        );
        assert!(out.is_none());
    }

    #[test]
    fn sell_side_adverse_is_positive_cost() {
        let l = DecisionLedger::default();
        let id = l.record_decision(
            0,
            "ETHUSDT",
            Side::Sell,
            dec!(0.1),
            dec!(1_000),
            None,
        );
        let order = OrderId::new_v4();
        l.bind_order(id, order);
        // Selling at 999 is 10 bps adverse (got 10 bps less
        // than decision-mid).
        let r = l
            .on_fill(order, 10, Side::Sell, dec!(999), dec!(0.1))
            .unwrap();
        assert_eq!(r.realized_cost_bps, dec!(10));
    }

    #[test]
    fn ring_evicts_oldest_beyond_capacity() {
        let l = DecisionLedger::new(4);
        let mut ids = Vec::new();
        for i in 0..10 {
            ids.push(l.record_decision(
                i,
                "X",
                Side::Buy,
                dec!(1),
                dec!(100),
                None,
            ));
        }
        assert_eq!(l.len(), 4);
        // Oldest 6 evicted — get() returns None for them.
        for id in &ids[0..6] {
            assert!(l.get(*id).is_none(), "evicted decision still gettable");
        }
        // Last 4 are alive.
        for id in &ids[6..] {
            assert!(l.get(*id).is_some(), "fresh decision was dropped");
        }
    }

    #[test]
    fn eviction_after_bind_gracefully_loses_order_mapping() {
        let l = DecisionLedger::new(2);
        let d1 = l.record_decision(0, "X", Side::Buy, dec!(1), dec!(100), None);
        let order = OrderId::new_v4();
        l.bind_order(d1, order);
        // Push two more — evicts d1.
        l.record_decision(1, "X", Side::Buy, dec!(1), dec!(100), None);
        l.record_decision(2, "X", Side::Buy, dec!(1), dec!(100), None);
        // Fill on the evicted order returns None gracefully.
        let r = l.on_fill(order, 99, Side::Buy, dec!(100), dec!(1));
        assert!(r.is_none());
    }

    #[test]
    fn recent_returns_newest_first() {
        let l = DecisionLedger::default();
        for i in 0..5 {
            l.record_decision(i, "X", Side::Buy, dec!(1), dec!(100), None);
        }
        let recent = l.recent(3);
        assert_eq!(recent.len(), 3);
        // Newest first — tick_ms descending.
        assert!(recent[0].tick_ms > recent[1].tick_ms);
        assert!(recent[1].tick_ms > recent[2].tick_ms);
    }
}
