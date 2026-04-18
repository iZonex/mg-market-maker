//! Epic R — Surveillance infrastructure.
//!
//! Shared substrate the 15 manipulation-pattern detectors
//! (`docs/research/complince.md` §§ Surveillance 1..15) build on:
//!
//!   1. [`SurveillanceEvent`] — a compact enum of the order-lifecycle
//!      signals every detector wants (placed / cancelled / filled /
//!      amended). Fed into the bus by the engine's
//!      `order_manager` so every detector reads from the same tape.
//!
//!   2. [`OrderLifecycleTracker`] — per-symbol rolling state that
//!      answers the three questions every detector asks:
//!          · how long did my orders live before they cancelled?
//!          · what's my cancel-to-fill ratio in the recent window?
//!          · how does an individual order size compare to the
//!            average trade size on this symbol?
//!      The tracker is the *only* place we compute these — detectors
//!      read, never duplicate.
//!
//!   3. [`SpoofingDetector`] — first reference consumer. Produces a
//!      `[0..1]` likelihood score from the three signals the MiFID II
//!      rulebook on layering/spoofing points at: high cancel ratio,
//!      short order lifetime, large order size vs. recent trade
//!      average. Its shape is the template every later detector
//!      follows (Layering, Wash, QuoteStuffing, …).
//!
//! **Not a classifier on its own.** A high score means "these signals
//! match a spoofing profile" — it's advice to a kill-switch rule, not
//! an adjudication. The compliance bundle captures both the inputs
//! and the score so a reviewer can second-guess the threshold.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

/// Minimal shape every order lifecycle event shares. Detectors want
/// the timestamp + symbol + side at minimum; size / price only
/// matter for a subset.
#[derive(Debug, Clone, PartialEq)]
pub enum SurveillanceEvent {
    /// Maker-side place.
    OrderPlaced {
        order_id: String,
        symbol: String,
        side: Side,
        price: Decimal,
        qty: Decimal,
        ts: DateTime<Utc>,
    },
    /// Unilateral cancel (either ours or the venue's).
    OrderCancelled {
        order_id: String,
        symbol: String,
        ts: DateTime<Utc>,
    },
    /// Partial or full fill — detectors use the `filled_qty` to
    /// refine the fill-rate portion of ratios.
    OrderFilled {
        order_id: String,
        symbol: String,
        filled_qty: Decimal,
        price: Decimal,
        ts: DateTime<Utc>,
    },
    /// An amend is modelled as cancel + place for lifecycle
    /// statistics — amending to move a price far from the book is
    /// indistinguishable from cancel + re-place in the spoofing
    /// profile, and bucketing them separately would hide that.
    /// Included as a dedicated variant so amend-aware detectors can
    /// still tell the two apart if needed.
    OrderAmended {
        order_id: String,
        symbol: String,
        new_price: Decimal,
        ts: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// How long we keep per-order + per-trade state. One minute is
/// enough for every pattern whose signature is "ms to seconds"
/// (spoofing, quote stuffing, momentum ignition). Wash / marking
/// need a longer window and carry their own state on top.
const WINDOW_SECS: i64 = 60;

/// Per-order placement record. Populated on `OrderPlaced`, drained
/// (with its lifetime) on `OrderCancelled` / `OrderFilled`.
#[derive(Debug, Clone)]
struct OpenOrder {
    symbol: String,
    side: Side,
    price: Decimal,
    qty: Decimal,
    placed_at: DateTime<Utc>,
}

/// Per-symbol rolling lifecycle stats every detector reads.
/// `feed()` is the only mutator; `snapshot()` the only reader. Both
/// are O(1) amortised — the tracker drops entries older than
/// [`WINDOW_SECS`] on every feed.
#[derive(Debug, Default)]
pub struct OrderLifecycleTracker {
    /// Currently-open orders keyed by id so a cancel / fill can pair
    /// back to the placement for lifetime calculation.
    open: HashMap<String, OpenOrder>,
    /// Per-symbol recent cancel timestamps (order_lifetime_ms, side).
    cancels: HashMap<String, VecDeque<(DateTime<Utc>, i64, Side)>>,
    /// Per-symbol recent fill (timestamp, qty).
    fills: HashMap<String, VecDeque<(DateTime<Utc>, Decimal)>>,
    /// Recent trade sizes seen on this symbol. Used by "size vs.
    /// average trade" ratios — an order five times the avg recent
    /// trade is the classic spoofing-book marker.
    trade_sizes: HashMap<String, VecDeque<(DateTime<Utc>, Decimal)>>,
}

/// Diagnostic snapshot readers can inspect per symbol.
#[derive(Debug, Clone, Default)]
pub struct SymbolStats {
    pub cancel_count: usize,
    pub fill_count: usize,
    pub cancel_to_fill_ratio: Decimal,
    pub median_order_lifetime_ms: Option<i64>,
    pub avg_trade_size: Option<Decimal>,
}

impl OrderLifecycleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest one event. Drops stale entries from every rolling
    /// window before returning so readers never see data outside
    /// [`WINDOW_SECS`].
    pub fn feed(&mut self, ev: &SurveillanceEvent) {
        match ev {
            SurveillanceEvent::OrderPlaced {
                order_id, symbol, side, price, qty, ts,
            } => {
                self.open.insert(
                    order_id.clone(),
                    OpenOrder {
                        symbol: symbol.clone(),
                        side: *side,
                        price: *price,
                        qty: *qty,
                        placed_at: *ts,
                    },
                );
            }
            SurveillanceEvent::OrderCancelled { order_id, symbol, ts } => {
                if let Some(rec) = self.open.remove(order_id) {
                    let lifetime_ms = (*ts - rec.placed_at).num_milliseconds();
                    self.cancels
                        .entry(symbol.clone())
                        .or_default()
                        .push_back((*ts, lifetime_ms, rec.side));
                }
            }
            SurveillanceEvent::OrderFilled { symbol, filled_qty, ts, .. } => {
                // Partial fills still hold the order open — only a
                // full fill removes it. Detectors don't need that
                // distinction for ratios (a fill is a fill), so we
                // count every fill event into the symbol's fill
                // tape and leave the open-order state alone unless
                // the venue later sends the terminal cancel.
                self.fills
                    .entry(symbol.clone())
                    .or_default()
                    .push_back((*ts, *filled_qty));
                self.trade_sizes
                    .entry(symbol.clone())
                    .or_default()
                    .push_back((*ts, *filled_qty));
            }
            SurveillanceEvent::OrderAmended { order_id, new_price, ts, .. } => {
                if let Some(rec) = self.open.get_mut(order_id) {
                    rec.price = *new_price;
                    rec.placed_at = *ts;
                }
            }
        }
        self.evict(Utc::now());
    }

    fn evict(&mut self, now: DateTime<Utc>) {
        let cutoff = now - chrono::Duration::seconds(WINDOW_SECS);
        for q in self.cancels.values_mut() {
            while q.front().is_some_and(|(t, _, _)| *t < cutoff) {
                q.pop_front();
            }
        }
        for q in self.fills.values_mut() {
            while q.front().is_some_and(|(t, _)| *t < cutoff) {
                q.pop_front();
            }
        }
        for q in self.trade_sizes.values_mut() {
            while q.front().is_some_and(|(t, _)| *t < cutoff) {
                q.pop_front();
            }
        }
    }

    /// Read the symbol's rolling stats. `Default` when the tracker
    /// has seen nothing for the symbol.
    pub fn snapshot(&self, symbol: &str) -> SymbolStats {
        let cancels = self.cancels.get(symbol).map(|q| q.len()).unwrap_or(0);
        let fills = self.fills.get(symbol).map(|q| q.len()).unwrap_or(0);
        let total = cancels + fills;
        let ratio = if total == 0 {
            Decimal::ZERO
        } else {
            Decimal::from(cancels) / Decimal::from(total)
        };
        let median_lifetime = self
            .cancels
            .get(symbol)
            .map(|q| {
                let mut v: Vec<i64> = q.iter().map(|(_, ms, _)| *ms).collect();
                v.sort_unstable();
                v.get(v.len() / 2).copied()
            })
            .unwrap_or(None);
        let avg_size = self.trade_sizes.get(symbol).and_then(|q| {
            if q.is_empty() {
                None
            } else {
                let sum: Decimal = q.iter().map(|(_, q)| *q).sum();
                Some(sum / Decimal::from(q.len()))
            }
        });
        SymbolStats {
            cancel_count: cancels,
            fill_count: fills,
            cancel_to_fill_ratio: ratio,
            median_order_lifetime_ms: median_lifetime,
            avg_trade_size: avg_size,
        }
    }

    /// Count of currently-open orders for a symbol — cheap O(n)
    /// over the open map. Detectors use this for the "three+ orders
    /// one side" layering signal.
    pub fn open_count(&self, symbol: &str, side: Option<Side>) -> usize {
        self.open
            .values()
            .filter(|o| o.symbol == symbol)
            .filter(|o| side.map(|s| o.side == s).unwrap_or(true))
            .count()
    }
}

// ─── Spoofing detector ─────────────────────────────────────────

/// Output shape shared by every surveillance detector.
#[derive(Debug, Clone)]
pub struct DetectorOutput {
    /// Likelihood in `[0, 1]`.
    pub score: Decimal,
    /// Individual signals surfaced for audit / UI — lets a reviewer
    /// second-guess the aggregate.
    pub cancel_to_fill_ratio: Decimal,
    pub median_order_lifetime_ms: Option<i64>,
    /// Ratio of the biggest open-order size to the recent trade
    /// average. `None` when no trades yet.
    pub size_vs_avg_trade: Option<Decimal>,
}

/// Configurable thresholds. Defaults mirror the signal bands in
/// `docs/research/complince.md` § Spoofing.
#[derive(Debug, Clone)]
pub struct SpoofingConfig {
    /// `cancel_to_fill_ratio ≥` → full contribution to score.
    pub ratio_hot: Decimal,
    /// `median_lifetime_ms ≤` → full contribution (fast cancels).
    pub lifetime_hot_ms: i64,
    /// `order_size / avg_trade_size ≥` → full contribution (big order).
    pub size_ratio_hot: Decimal,
}

impl Default for SpoofingConfig {
    fn default() -> Self {
        Self {
            ratio_hot: dec!(0.9),
            lifetime_hot_ms: 100,
            size_ratio_hot: dec!(5),
        }
    }
}

#[derive(Debug, Default)]
pub struct SpoofingDetector {
    pub config: SpoofingConfig,
}

impl SpoofingDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: SpoofingConfig) -> Self {
        Self { config }
    }

    /// Compute a likelihood score from the tracker's state for the
    /// symbol. Score is a simple mean of three clamped-to-[0,1]
    /// sub-signals — a reviewer can trace exactly why the detector
    /// flagged this minute.
    pub fn score(
        &self,
        symbol: &str,
        tracker: &OrderLifecycleTracker,
    ) -> DetectorOutput {
        let stats = tracker.snapshot(symbol);

        // (1) cancel-to-fill ratio signal.
        let ratio_sig = if stats.cancel_to_fill_ratio >= self.config.ratio_hot {
            Decimal::ONE
        } else if self.config.ratio_hot > Decimal::ZERO {
            (stats.cancel_to_fill_ratio / self.config.ratio_hot).min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };

        // (2) short-lifetime signal. `None` → 0 (no evidence).
        let lifetime_sig = match stats.median_order_lifetime_ms {
            Some(ms) if self.config.lifetime_hot_ms > 0 => {
                if ms <= self.config.lifetime_hot_ms {
                    Decimal::ONE
                } else {
                    // Decays toward 0 as lifetime grows; 10x hot → ~0.1.
                    Decimal::from(self.config.lifetime_hot_ms) / Decimal::from(ms)
                }
            }
            _ => Decimal::ZERO,
        };

        // (3) big-order signal — compare largest open order size to
        // recent trade average.
        let largest_open = tracker
            .open
            .values()
            .filter(|o| o.symbol == symbol)
            .map(|o| o.qty)
            .max()
            .unwrap_or(Decimal::ZERO);
        let size_ratio = match stats.avg_trade_size {
            Some(avg) if avg > Decimal::ZERO => Some(largest_open / avg),
            _ => None,
        };
        let size_sig = match size_ratio {
            Some(r) if r >= self.config.size_ratio_hot => Decimal::ONE,
            Some(r) if self.config.size_ratio_hot > Decimal::ZERO => {
                (r / self.config.size_ratio_hot).min(Decimal::ONE)
            }
            _ => Decimal::ZERO,
        };

        let score = (ratio_sig + lifetime_sig + size_sig) / dec!(3);
        DetectorOutput {
            score: score.clamp(Decimal::ZERO, Decimal::ONE),
            cancel_to_fill_ratio: stats.cancel_to_fill_ratio,
            median_order_lifetime_ms: stats.median_order_lifetime_ms,
            size_vs_avg_trade: size_ratio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev_place(id: &str, sym: &str, side: Side, qty: Decimal, ts: DateTime<Utc>) -> SurveillanceEvent {
        SurveillanceEvent::OrderPlaced {
            order_id: id.into(),
            symbol: sym.into(),
            side,
            price: dec!(100),
            qty,
            ts,
        }
    }
    fn ev_cancel(id: &str, sym: &str, ts: DateTime<Utc>) -> SurveillanceEvent {
        SurveillanceEvent::OrderCancelled {
            order_id: id.into(),
            symbol: sym.into(),
            ts,
        }
    }
    fn ev_fill(id: &str, sym: &str, qty: Decimal, ts: DateTime<Utc>) -> SurveillanceEvent {
        SurveillanceEvent::OrderFilled {
            order_id: id.into(),
            symbol: sym.into(),
            filled_qty: qty,
            price: dec!(100),
            ts,
        }
    }

    #[test]
    fn tracker_pairs_place_and_cancel_into_lifetime() {
        let mut t = OrderLifecycleTracker::new();
        let t0 = Utc::now();
        t.feed(&ev_place("o1", "BTCUSDT", Side::Buy, dec!(1), t0));
        t.feed(&ev_cancel("o1", "BTCUSDT", t0 + chrono::Duration::milliseconds(50)));
        let s = t.snapshot("BTCUSDT");
        assert_eq!(s.cancel_count, 1);
        assert_eq!(s.median_order_lifetime_ms, Some(50));
    }

    #[test]
    fn tracker_cancel_to_fill_ratio() {
        let mut t = OrderLifecycleTracker::new();
        let t0 = Utc::now();
        // 9 cancels + 1 fill → ratio 0.9.
        for i in 0..9 {
            let id = format!("c{i}");
            t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
            t.feed(&ev_cancel(&id, "BTCUSDT", t0 + chrono::Duration::milliseconds(30)));
        }
        t.feed(&ev_place("f1", "BTCUSDT", Side::Buy, dec!(1), t0));
        t.feed(&ev_fill("f1", "BTCUSDT", dec!(1), t0 + chrono::Duration::milliseconds(200)));
        let s = t.snapshot("BTCUSDT");
        assert_eq!(s.cancel_to_fill_ratio, dec!(0.9));
    }

    #[test]
    fn spoofing_hot_profile_scores_high() {
        let mut t = OrderLifecycleTracker::new();
        let t0 = Utc::now();
        // Feed the trade tape so avg_trade_size is known and small.
        for i in 0..3 {
            let id = format!("trade{i}");
            t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
            t.feed(&ev_fill(&id, "BTCUSDT", dec!(1), t0 + chrono::Duration::milliseconds(500)));
        }
        // Spoofing profile: 20 cancels with 30ms lifetime, no fills,
        // plus one huge open order 10x the trade avg.
        for i in 0..20 {
            let id = format!("spoof{i}");
            t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
            t.feed(&ev_cancel(&id, "BTCUSDT", t0 + chrono::Duration::milliseconds(30)));
        }
        t.feed(&ev_place("big", "BTCUSDT", Side::Buy, dec!(10), t0));
        let det = SpoofingDetector::new();
        let out = det.score("BTCUSDT", &t);
        assert!(out.score >= dec!(0.9), "score was {}", out.score);
    }

    #[test]
    fn spoofing_clean_profile_scores_low() {
        let mut t = OrderLifecycleTracker::new();
        let t0 = Utc::now();
        // Balanced fills + cancels, long lifetimes, similar sizes.
        for i in 0..10 {
            let id = format!("fill{i}");
            t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
            t.feed(&ev_fill(&id, "BTCUSDT", dec!(1), t0 + chrono::Duration::seconds(5)));
        }
        for i in 0..2 {
            let id = format!("late{i}");
            t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
            t.feed(&ev_cancel(&id, "BTCUSDT", t0 + chrono::Duration::seconds(10)));
        }
        let det = SpoofingDetector::new();
        let out = det.score("BTCUSDT", &t);
        assert!(out.score <= dec!(0.3), "score was {}", out.score);
    }
}
