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
//!      · how long did my orders live before they cancelled?
//!      · what's my cancel-to-fill ratio in the recent window?
//!      · order size vs. recent trade average on this symbol.
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
use rust_decimal::prelude::Signed;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Shared-ownership handle callers pass around. `Arc<Mutex<_>>` is
/// the right shape here: the tracker is written from the order
/// manager's async fill / cancel / place paths AND read from the
/// engine's strategy-graph overlay tick, on different threads.
pub type SharedTracker = Arc<Mutex<OrderLifecycleTracker>>;

/// Build a fresh tracker handle ready to share.
pub fn new_shared_tracker() -> SharedTracker {
    Arc::new(Mutex::new(OrderLifecycleTracker::new()))
}

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
    /// refine the fill-rate portion of ratios. `side` is required
    /// for Wash detection (buy + sell at the same price within a
    /// short window = self-trade).
    OrderFilled {
        order_id: String,
        symbol: String,
        side: Side,
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
    /// Epic R Week 4 — per-symbol fills annotated with side + price
    /// so the Wash detector can walk the tape and pair own buy +
    /// own sell at the same price. Separate from `fills` above so
    /// the existing stats don't change shape.
    wash_fills: HashMap<String, VecDeque<WashFillView>>,
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
            SurveillanceEvent::OrderFilled {
                symbol, side, filled_qty, price, ts, ..
            } => {
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
                // Wash-detector tape — keeps side + price per fill.
                let q = self.wash_fills.entry(symbol.clone()).or_default();
                q.push_back(WashFillView {
                    ts: *ts,
                    side: *side,
                    price: *price,
                });
                // Cap at the rolling window — same WINDOW_SECS cap
                // as the other queues. Evicted below in `evict`.
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
        for q in self.wash_fills.values_mut() {
            while q.front().is_some_and(|v| v.ts < cutoff) {
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
#[derive(Debug, Clone, Default)]
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

// ─── Layering detector ─────────────────────────────────────────
//
// `docs/research/complince.md` § 2 — "3+ orders on one side, close
// in price, synchronous cancel". The distinguishing signal vs.
// plain spoofing is *structure*: many orders layered at adjacent
// price ticks, not one big order.

/// Input knobs for [`LayeringDetector`].
#[derive(Debug, Clone)]
pub struct LayeringConfig {
    /// Orders on the same side within [`price_cluster_frac`] fraction
    /// of each other count as "one layer". Default 5 bps.
    pub price_cluster_frac: Decimal,
    /// `n_orders_hot` orders on one side + clustered → full signal.
    pub n_orders_hot: usize,
    /// `synchronous_cancel_window_ms`: cancels arriving within this
    /// of each other count toward the synchronous-cancel signal.
    pub synchronous_cancel_window_ms: i64,
}

impl Default for LayeringConfig {
    fn default() -> Self {
        Self {
            price_cluster_frac: dec!(0.0005), // 5 bps
            n_orders_hot: 5,
            synchronous_cancel_window_ms: 200,
        }
    }
}

#[derive(Debug, Default)]
pub struct LayeringDetector {
    pub config: LayeringConfig,
}

impl LayeringDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(
        &self,
        symbol: &str,
        tracker: &OrderLifecycleTracker,
    ) -> DetectorOutput {
        // Collect open orders per side; find the biggest cluster on
        // either side (consecutive orders within price_cluster_frac
        // of the median).
        let open_here: Vec<&OpenOrder> = tracker
            .open
            .values()
            .filter(|o| o.symbol == symbol)
            .collect();

        let mut biggest_cluster_per_side = 0usize;
        for side in [Side::Buy, Side::Sell] {
            let mut prices: Vec<Decimal> = open_here
                .iter()
                .filter(|o| o.side == side)
                .map(|o| o.price)
                .collect();
            if prices.len() < 2 {
                continue;
            }
            prices.sort();
            // Walking window — count the longest run where each
            // next price is within cluster_frac of the first.
            let mut i = 0;
            while i < prices.len() {
                let anchor = prices[i];
                let mut j = i;
                while j < prices.len() {
                    let delta = (prices[j] - anchor).abs();
                    if anchor == Decimal::ZERO
                        || delta / anchor <= self.config.price_cluster_frac
                    {
                        j += 1;
                    } else {
                        break;
                    }
                }
                biggest_cluster_per_side =
                    biggest_cluster_per_side.max(j - i);
                i = j.max(i + 1);
            }
        }
        let cluster_sig = if self.config.n_orders_hot == 0 {
            Decimal::ZERO
        } else {
            (Decimal::from(biggest_cluster_per_side)
                / Decimal::from(self.config.n_orders_hot))
            .min(Decimal::ONE)
        };

        // Synchronous-cancel signal: cancels within the window on the
        // same side → one big co-ordinated pull.
        let mut sync_sig = Decimal::ZERO;
        if let Some(cancels) = tracker.cancels.get(symbol) {
            let window = chrono::Duration::milliseconds(
                self.config.synchronous_cancel_window_ms,
            );
            let mut buckets: Vec<(DateTime<Utc>, Side, usize)> = Vec::new();
            for (ts, _lifetime, side) in cancels.iter() {
                if let Some(last) = buckets.last_mut() {
                    if *ts - last.0 <= window && last.1 == *side {
                        last.2 += 1;
                        continue;
                    }
                }
                buckets.push((*ts, *side, 1));
            }
            let biggest = buckets.iter().map(|(_, _, n)| *n).max().unwrap_or(0);
            if self.config.n_orders_hot > 0 {
                sync_sig = (Decimal::from(biggest)
                    / Decimal::from(self.config.n_orders_hot))
                .min(Decimal::ONE);
            }
        }

        // Aggregate: mean of cluster + sync signals. The spoofing
        // detector's cancel-ratio is deliberately not re-used here
        // — layering is about structure, not fill rate.
        let score = (cluster_sig + sync_sig) / dec!(2);
        let stats = tracker.snapshot(symbol);
        DetectorOutput {
            score: score.clamp(Decimal::ZERO, Decimal::ONE),
            cancel_to_fill_ratio: stats.cancel_to_fill_ratio,
            median_order_lifetime_ms: stats.median_order_lifetime_ms,
            size_vs_avg_trade: None,
        }
    }
}

// ─── Quote-stuffing detector ────────────────────────────────────
//
// `docs/research/complince.md` § 4 — "very high orders/sec + high
// cancel ratio + near-zero fill rate". The stuffing pattern buries
// other participants under order-book churn without ever trading.

#[derive(Debug, Clone)]
pub struct QuoteStuffingConfig {
    /// `orders_per_sec ≥` → full orders-rate contribution.
    pub orders_per_sec_hot: usize,
    /// `cancel_to_fill ≥` → full cancel-ratio contribution.
    pub cancel_ratio_hot: Decimal,
    /// `fill_rate <=` → full low-fill contribution. `fill_rate` =
    /// `fills / (fills + cancels)`.
    pub fill_rate_cold: Decimal,
}

impl Default for QuoteStuffingConfig {
    fn default() -> Self {
        Self {
            orders_per_sec_hot: 50,
            cancel_ratio_hot: dec!(0.95),
            fill_rate_cold: dec!(0.02),
        }
    }
}

#[derive(Debug, Default)]
pub struct QuoteStuffingDetector {
    pub config: QuoteStuffingConfig,
}

impl QuoteStuffingDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(
        &self,
        symbol: &str,
        tracker: &OrderLifecycleTracker,
    ) -> DetectorOutput {
        let stats = tracker.snapshot(symbol);
        let total = stats.cancel_count + stats.fill_count;
        // Orders/sec across the 60s rolling window, coarse but
        // adequate for "is this an outlier" decisions.
        let orders_per_sec = total as f64 / WINDOW_SECS as f64;
        let rate_sig = if self.config.orders_per_sec_hot == 0 {
            Decimal::ZERO
        } else {
            let hot = self.config.orders_per_sec_hot as f64;
            Decimal::from_f64_retain((orders_per_sec / hot).min(1.0))
                .unwrap_or(Decimal::ZERO)
        };
        let cancel_sig = if self.config.cancel_ratio_hot > Decimal::ZERO {
            (stats.cancel_to_fill_ratio / self.config.cancel_ratio_hot)
                .min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };
        let fill_rate = if total == 0 {
            Decimal::ZERO
        } else {
            Decimal::from(stats.fill_count) / Decimal::from(total)
        };
        // "fill_rate_cold" signal: low fill_rate → high score.
        let low_fill_sig = if self.config.fill_rate_cold > Decimal::ZERO {
            if fill_rate >= self.config.fill_rate_cold {
                Decimal::ZERO
            } else {
                Decimal::ONE
                    - (fill_rate / self.config.fill_rate_cold).min(Decimal::ONE)
            }
        } else {
            Decimal::ZERO
        };
        let score = (rate_sig + cancel_sig + low_fill_sig) / dec!(3);
        DetectorOutput {
            score: score.clamp(Decimal::ZERO, Decimal::ONE),
            cancel_to_fill_ratio: stats.cancel_to_fill_ratio,
            median_order_lifetime_ms: stats.median_order_lifetime_ms,
            size_vs_avg_trade: None,
        }
    }
}

// ─── Wash-trading detector ─────────────────────────────────
//
// `docs/research/complince.md` § 3 — own buy + own sell at the same
// price within a short window = self-trade. We read the fills tape
// already in the tracker (it carries `side` after the Level 1 data
// sprint); the detector walks the recent fill list and scores on
// how many pairs of opposite-side fills at the same price sit
// within the configured window.

#[derive(Debug, Clone)]
pub struct WashConfig {
    /// Max spread (in ticks) between two fills to still count as
    /// "same price". Setting it to zero requires exact price equality.
    pub price_tolerance: Decimal,
    /// Window in milliseconds within which the buy + sell must pair.
    pub pair_window_ms: i64,
    /// `pair_count_hot` pairs → full contribution. Scales linearly
    /// below that.
    pub pair_count_hot: usize,
}

impl Default for WashConfig {
    fn default() -> Self {
        Self {
            price_tolerance: dec!(0),
            pair_window_ms: 500,
            pair_count_hot: 3,
        }
    }
}

#[derive(Debug, Default)]
pub struct WashDetector {
    pub config: WashConfig,
}

impl WashDetector {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_config(config: WashConfig) -> Self {
        Self { config }
    }

    /// Score derived from recent fills on this symbol. The tracker
    /// doesn't currently expose fill-side as a public read path, so
    /// the engine adapter passes the list in directly — keeps the
    /// tracker's public surface focused on `SymbolStats`.
    pub fn score_from_fills(&self, fills: &[WashFillView]) -> DetectorOutput {
        let mut pairs = 0usize;
        for i in 0..fills.len() {
            for j in (i + 1)..fills.len() {
                let a = &fills[i];
                let b = &fills[j];
                if a.side == b.side {
                    continue;
                }
                let dt = (b.ts - a.ts).num_milliseconds().abs();
                if dt > self.config.pair_window_ms {
                    continue;
                }
                let delta = (a.price - b.price).abs();
                if delta <= self.config.price_tolerance {
                    pairs += 1;
                }
            }
        }
        let sig = if self.config.pair_count_hot == 0 {
            Decimal::ZERO
        } else {
            (Decimal::from(pairs) / Decimal::from(self.config.pair_count_hot))
                .min(Decimal::ONE)
        };
        DetectorOutput {
            score: sig,
            cancel_to_fill_ratio: Decimal::ZERO,
            median_order_lifetime_ms: None,
            size_vs_avg_trade: None,
        }
    }
}

impl OrderLifecycleTracker {
    /// Epic R Week 4 — pull the side+price-annotated fill tape for
    /// the symbol. Used by the Wash detector.
    pub fn recent_fills(&self, symbol: &str) -> Vec<WashFillView> {
        self.wash_fills
            .get(symbol)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct WashFillView {
    pub ts: DateTime<Utc>,
    pub side: Side,
    pub price: Decimal,
}

// ─── Cross-market manipulation detector ─────────────────────
//
// § 8 — activity on an illiquid venue drives the price there;
// the actor profits on a correlated, liquid venue. We can't see
// the actor's P&L on the other venue, but we CAN see the
// correlation signature: bursty trade qty on the illiquid side +
// a correlated mid move.

#[derive(Debug, Clone)]
pub struct CrossMarketConfig {
    /// `illiquid_ratio_hot` — burst vol on illiquid leg / baseline
    /// vol to score hot. Default 5×.
    pub illiquid_ratio_hot: Decimal,
    /// Min |mid correlation| (lagged move fraction) to count at all.
    pub min_correlated_move_bps: Decimal,
}

impl Default for CrossMarketConfig {
    fn default() -> Self {
        Self {
            illiquid_ratio_hot: dec!(5),
            min_correlated_move_bps: dec!(10),
        }
    }
}

#[derive(Debug, Default)]
pub struct CrossMarketDetector {
    pub config: CrossMarketConfig,
}

impl CrossMarketDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(
        &self,
        illiquid_ratio: Decimal,
        liquid_move_bps: Decimal,
    ) -> DetectorOutput {
        let ratio_sig = if self.config.illiquid_ratio_hot > Decimal::ZERO {
            (illiquid_ratio / self.config.illiquid_ratio_hot).min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };
        let move_sig = if liquid_move_bps.abs() >= self.config.min_correlated_move_bps {
            Decimal::ONE
        } else if self.config.min_correlated_move_bps > Decimal::ZERO {
            (liquid_move_bps.abs() / self.config.min_correlated_move_bps).min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };
        DetectorOutput {
            score: ((ratio_sig + move_sig) / dec!(2))
                .clamp(Decimal::ZERO, Decimal::ONE),
            cancel_to_fill_ratio: Decimal::ZERO,
            median_order_lifetime_ms: None,
            size_vs_avg_trade: None,
        }
    }
}

// ─── Latency exploit detector ──────────────────────────────
//
// § 9 — systematic fills against stale quotes on one venue. We
// see this as: "we're the resting maker, fills arrive faster than
// a normal round-trip suggests the other side could have learned
// about our move". Proxy: a spike in `fill_to_cancel_ms` ≤ some
// tight threshold on our quotes right after we re-priced them.

#[derive(Debug, Clone)]
pub struct LatencyExploitConfig {
    /// Threshold `ms` — fills arriving within this of our last
    /// re-price count toward the score.
    pub stale_window_ms: i64,
    /// `hot_count` fills in the stale window → full contribution.
    pub hot_count: usize,
}

impl Default for LatencyExploitConfig {
    fn default() -> Self {
        Self {
            stale_window_ms: 50,
            hot_count: 3,
        }
    }
}

#[derive(Debug, Default)]
pub struct LatencyExploitDetector {
    pub config: LatencyExploitConfig,
}

impl LatencyExploitDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// `fill_deltas_ms` — time between our last re-price and each
    /// fill that followed, in ms.
    pub fn score(&self, fill_deltas_ms: &[i64]) -> DetectorOutput {
        let hits = fill_deltas_ms
            .iter()
            .filter(|d| **d <= self.config.stale_window_ms && **d >= 0)
            .count();
        let sig = if self.config.hot_count == 0 {
            Decimal::ZERO
        } else {
            (Decimal::from(hits) / Decimal::from(self.config.hot_count))
                .min(Decimal::ONE)
        };
        DetectorOutput { score: sig, ..Default::default() }
    }
}

// ─── Rebate-abuse detector ─────────────────────────────────
//
// § 10 — positive net PnL driven by rebates rather than trade
// edge. `trade_pnl < 0` + `rebate_pnl > 0` + `|trade_pnl| <
// rebate_pnl`. Strong signal we're churning volume without
// economic value.

#[derive(Debug, Clone)]
pub struct RebateAbuseConfig {
    /// Min rebate / trade-loss ratio to count as hot.
    pub ratio_hot: Decimal,
}

impl Default for RebateAbuseConfig {
    fn default() -> Self {
        Self { ratio_hot: dec!(2) }
    }
}

#[derive(Debug, Default)]
pub struct RebateAbuseDetector {
    pub config: RebateAbuseConfig,
}

impl RebateAbuseDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(&self, trade_pnl: Decimal, rebate_pnl: Decimal) -> DetectorOutput {
        if rebate_pnl <= Decimal::ZERO || trade_pnl >= Decimal::ZERO {
            return DetectorOutput::default();
        }
        let abs_loss = -trade_pnl;
        if abs_loss <= Decimal::ZERO {
            return DetectorOutput::default();
        }
        let ratio = rebate_pnl / abs_loss;
        let sig = if self.config.ratio_hot > Decimal::ZERO {
            (ratio / self.config.ratio_hot).min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };
        DetectorOutput { score: sig, ..Default::default() }
    }
}

// ─── Imbalance-manipulation detector ───────────────────────
//
// § 11 — deliberate order-book imbalance followed by fast
// reversal. Shape: `|imbalance|` above threshold AND it flips
// sign within a short window (the "pump then dump" signature).

#[derive(Debug, Clone)]
pub struct ImbalanceManipulationConfig {
    /// Imbalance magnitude to count as "skewed" (`[-1, 1]`).
    pub skew_hot: Decimal,
    /// Max window (ms) within which a sign flip counts.
    pub flip_window_ms: i64,
}

impl Default for ImbalanceManipulationConfig {
    fn default() -> Self {
        Self {
            skew_hot: dec!(0.6),
            flip_window_ms: 1_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImbalanceSample {
    pub ts: DateTime<Utc>,
    /// Book imbalance in [-1, 1] where +1 = all bids.
    pub imbalance: Decimal,
}

#[derive(Debug, Default)]
pub struct ImbalanceManipulationDetector {
    pub config: ImbalanceManipulationConfig,
}

impl ImbalanceManipulationDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(&self, samples: &[ImbalanceSample]) -> DetectorOutput {
        let mut flips = 0usize;
        for i in 0..samples.len() {
            if samples[i].imbalance.abs() < self.config.skew_hot {
                continue;
            }
            for j in (i + 1)..samples.len() {
                let dt = (samples[j].ts - samples[i].ts).num_milliseconds();
                if dt > self.config.flip_window_ms {
                    break;
                }
                if samples[j].imbalance.abs() >= self.config.skew_hot
                    && samples[j].imbalance.signum() != samples[i].imbalance.signum()
                {
                    flips += 1;
                    break;
                }
            }
        }
        let sig = if flips == 0 { Decimal::ZERO } else { Decimal::ONE };
        DetectorOutput { score: sig, ..Default::default() }
    }
}

// ─── Cancel-on-reaction detector ───────────────────────────
//
// § 12 — our order is live, some other market participant reacts
// (observable as a nearby trade), we cancel immediately after.
// Measured as: cancels whose `time_since_last_nearby_trade_ms` is
// below a reflex-threshold and whose price was touched by that
// trade.

#[derive(Debug, Clone)]
pub struct CancelOnReactionConfig {
    /// Max ms between a nearby trade and our cancel to count.
    pub reaction_window_ms: i64,
    /// Full contribution at this many reactive cancels.
    pub hot_count: usize,
}

impl Default for CancelOnReactionConfig {
    fn default() -> Self {
        Self {
            reaction_window_ms: 100,
            hot_count: 3,
        }
    }
}

#[derive(Debug, Default)]
pub struct CancelOnReactionDetector {
    pub config: CancelOnReactionConfig,
}

impl CancelOnReactionDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// `cancels_after_nearby_trade_ms` — for every cancel we
    /// fired, the time gap (ms) between the most recent nearby
    /// public trade and the cancel. `None` entries mean "no
    /// nearby trade observed" and are ignored.
    pub fn score(&self, cancels_after_nearby_trade_ms: &[Option<i64>]) -> DetectorOutput {
        let hits = cancels_after_nearby_trade_ms
            .iter()
            .filter_map(|o| *o)
            .filter(|d| *d >= 0 && *d <= self.config.reaction_window_ms)
            .count();
        let sig = if self.config.hot_count == 0 {
            Decimal::ZERO
        } else {
            (Decimal::from(hits) / Decimal::from(self.config.hot_count))
                .min(Decimal::ONE)
        };
        DetectorOutput { score: sig, ..Default::default() }
    }
}

// ─── One-sided quoting detector ────────────────────────────
//
// § 13 — engine posts only on one side for an extended window
// without an inventory reason. Visible on the own-quote log —
// count of ticks where only bid OR only ask existed in the last
// N ticks, divided by the total window.

#[derive(Debug, Clone)]
pub struct OneSidedQuotingConfig {
    /// Fraction of recent ticks that must be one-sided to score
    /// hot. 0.9 = 90% of the window.
    pub one_sided_ratio_hot: Decimal,
}

impl Default for OneSidedQuotingConfig {
    fn default() -> Self {
        Self { one_sided_ratio_hot: dec!(0.9) }
    }
}

#[derive(Debug, Default)]
pub struct OneSidedQuotingDetector {
    pub config: OneSidedQuotingConfig,
}

impl OneSidedQuotingDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(&self, one_sided_ticks: usize, total_ticks: usize) -> DetectorOutput {
        if total_ticks == 0 {
            return DetectorOutput::default();
        }
        let ratio = Decimal::from(one_sided_ticks) / Decimal::from(total_ticks);
        let sig = if self.config.one_sided_ratio_hot > Decimal::ZERO {
            (ratio / self.config.one_sided_ratio_hot).min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };
        DetectorOutput { score: sig, ..Default::default() }
    }
}

// ─── Inventory-pushing detector ────────────────────────────
//
// § 14 — inventory rises, then aggressive trading on the same
// side nudges the price in favour of unwinding. Signal: when
// `inv_delta > 0 AND price_delta > 0` (or both negative),
// correlation above threshold.

#[derive(Debug, Clone)]
pub struct InventoryPushingConfig {
    /// Threshold on `inv_delta × price_delta` (normalised) to
    /// trigger — positive means inventory is moving with price,
    /// which is the push signature.
    pub correlation_hot: Decimal,
}

impl Default for InventoryPushingConfig {
    fn default() -> Self {
        Self { correlation_hot: dec!(0.6) }
    }
}

#[derive(Debug, Default)]
pub struct InventoryPushingDetector {
    pub config: InventoryPushingConfig,
}

impl InventoryPushingDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(&self, inv_delta_norm: Decimal, price_delta_norm: Decimal) -> DetectorOutput {
        let correlation = inv_delta_norm * price_delta_norm;
        let sig = if self.config.correlation_hot > Decimal::ZERO {
            (correlation / self.config.correlation_hot)
                .max(Decimal::ZERO)
                .min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };
        DetectorOutput { score: sig, ..Default::default() }
    }
}

// ─── Strategic-non-filling detector ────────────────────────
//
// § 15 — orders near-touch but never fill. Fill rate ≈ 0 while
// sitting inside the touch for a long window. Proxy: the
// tracker's cancel_to_fill ratio but restricted to near-touch
// orders only — needs price-at-placement + best-touch-at-cancel
// context from the tracker.

#[derive(Debug, Clone)]
pub struct StrategicNonFillingConfig {
    /// Max distance from mid (bps) for an order to count as
    /// "near-touch".
    pub near_touch_bps: Decimal,
    /// `fill_rate_cold` — anything below this scores full.
    pub fill_rate_cold: Decimal,
    /// Minimum ticks placed to consider this signal stable.
    pub min_placements: usize,
}

impl Default for StrategicNonFillingConfig {
    fn default() -> Self {
        Self {
            near_touch_bps: dec!(5),
            fill_rate_cold: dec!(0.05),
            min_placements: 20,
        }
    }
}

#[derive(Debug, Default)]
pub struct StrategicNonFillingDetector {
    pub config: StrategicNonFillingConfig,
}

impl StrategicNonFillingDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(
        &self,
        near_touch_placements: usize,
        near_touch_fills: usize,
    ) -> DetectorOutput {
        if near_touch_placements < self.config.min_placements {
            return DetectorOutput::default();
        }
        let fill_rate = Decimal::from(near_touch_fills) / Decimal::from(near_touch_placements);
        let sig = if self.config.fill_rate_cold > Decimal::ZERO {
            if fill_rate >= self.config.fill_rate_cold {
                Decimal::ZERO
            } else {
                Decimal::ONE
                    - (fill_rate / self.config.fill_rate_cold).min(Decimal::ONE)
            }
        } else {
            Decimal::ZERO
        };
        DetectorOutput { score: sig, ..Default::default() }
    }
}

// ─── Marking-the-close detector ─────────────────────────────
//
// `docs/research/complince.md` § 7 — aggressive trading in the
// final seconds before a session boundary (funding window /
// settlement) to move the VWAP in the actor's favour. Shape:
// read trade volume in the closing window, compare to the
// window's typical volume — an unusual spike during the last
// `close_window_secs` is the signal.

#[derive(Debug, Clone)]
pub struct MarkingCloseConfig {
    /// How many seconds before a boundary the detector watches.
    /// Default 60 — most venues show the marking pattern inside
    /// a one-minute window.
    pub close_window_secs: i64,
    /// Ratio of closing-window volume to baseline volume
    /// (baseline = same duration window earlier in the session).
    /// `ratio_hot` → full contribution. 3.0 = triple the baseline.
    pub ratio_hot: Decimal,
}

impl Default for MarkingCloseConfig {
    fn default() -> Self {
        Self {
            close_window_secs: 60,
            ratio_hot: dec!(3),
        }
    }
}

#[derive(Debug, Default)]
pub struct MarkingCloseDetector {
    pub config: MarkingCloseConfig,
}

impl MarkingCloseDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// `seconds_to_boundary` — how far away from the next session
    /// mark we currently are (0 = at the boundary, < 0 = past).
    /// `window_volume` + `baseline_volume` come from the engine
    /// adapter (trade tape slicing).
    pub fn score(
        &self,
        seconds_to_boundary: i64,
        window_volume: Decimal,
        baseline_volume: Decimal,
    ) -> DetectorOutput {
        // Not inside the closing window → no signal.
        if seconds_to_boundary < 0 || seconds_to_boundary > self.config.close_window_secs {
            return DetectorOutput::default();
        }
        if baseline_volume <= Decimal::ZERO {
            return DetectorOutput::default();
        }
        let ratio = window_volume / baseline_volume;
        let sig = if self.config.ratio_hot > Decimal::ZERO {
            if ratio >= self.config.ratio_hot {
                Decimal::ONE
            } else {
                (ratio / self.config.ratio_hot).min(Decimal::ONE)
            }
        } else {
            Decimal::ZERO
        };
        DetectorOutput {
            score: sig,
            cancel_to_fill_ratio: Decimal::ZERO,
            median_order_lifetime_ms: None,
            size_vs_avg_trade: None,
        }
    }
}

// ─── Fake-liquidity detector ────────────────────────────────
//
// `docs/research/complince.md` § 6 — orders pulled right before
// the touch reaches them. We need two L2 snapshots: one from N ms
// ago, one now. Level on `now` that was present in `then` with
// bigger qty and whose price is now *closer to mid* = suspicious
// cancel. Kept here as a free function so the engine adapter
// passes the two snapshots in from the DataBus without having to
// marry the detector to a specific book representation.

#[derive(Debug, Clone)]
pub struct L2Level {
    pub price: Decimal,
    pub qty: Decimal,
}

#[derive(Debug, Clone)]
pub struct L2Snapshot {
    pub bids: Vec<L2Level>, // outer-first (best bid at index 0)
    pub asks: Vec<L2Level>,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct FakeLiquidityConfig {
    /// What fraction of a level's qty must have evaporated to
    /// count as a "pulled" order. 0.5 = half or more disappeared.
    pub vanish_threshold: Decimal,
    /// `pulled_levels_hot` full disappearances → full contribution.
    pub pulled_levels_hot: usize,
    /// Max distance from mid (bps) for levels to count. Irrelevant
    /// far-away levels that vanish don't register.
    pub max_distance_bps: Decimal,
}

impl Default for FakeLiquidityConfig {
    fn default() -> Self {
        Self {
            vanish_threshold: dec!(0.5),
            pulled_levels_hot: 3,
            max_distance_bps: dec!(20),
        }
    }
}

#[derive(Debug, Default)]
pub struct FakeLiquidityDetector {
    pub config: FakeLiquidityConfig,
}

impl FakeLiquidityDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compare two snapshots. `then` is the older one, `now` is
    /// the current. Score climbs with the count of "pulled"
    /// levels within `max_distance_bps` of mid.
    pub fn score(&self, then: &L2Snapshot, now: &L2Snapshot) -> DetectorOutput {
        let mid_now = match (now.bids.first(), now.asks.first()) {
            (Some(b), Some(a)) => (b.price + a.price) / dec!(2),
            _ => return DetectorOutput {
                score: Decimal::ZERO,
                cancel_to_fill_ratio: Decimal::ZERO,
                median_order_lifetime_ms: None,
                size_vs_avg_trade: None,
            },
        };
        let bp = dec!(10_000);
        let max_frac = self.config.max_distance_bps / bp;
        let in_band = |p: Decimal| -> bool {
            mid_now > Decimal::ZERO
                && (p - mid_now).abs() / mid_now <= max_frac
        };

        let mut pulled = 0usize;
        let count_side = |then_levels: &[L2Level], now_levels: &[L2Level]| -> usize {
            let mut out = 0;
            for l_then in then_levels {
                if !in_band(l_then.price) {
                    continue;
                }
                // Find matching price on the new side.
                let now_qty = now_levels
                    .iter()
                    .find(|l| l.price == l_then.price)
                    .map(|l| l.qty)
                    .unwrap_or(Decimal::ZERO);
                if l_then.qty > Decimal::ZERO {
                    let shrinkage = (l_then.qty - now_qty) / l_then.qty;
                    if shrinkage >= Decimal::ZERO
                        && shrinkage >= self.config.vanish_threshold
                    {
                        out += 1;
                    }
                }
            }
            out
        };
        pulled += count_side(&then.bids, &now.bids);
        pulled += count_side(&then.asks, &now.asks);

        let sig = if self.config.pulled_levels_hot == 0 {
            Decimal::ZERO
        } else {
            (Decimal::from(pulled)
                / Decimal::from(self.config.pulled_levels_hot))
            .min(Decimal::ONE)
        };
        DetectorOutput {
            score: sig,
            cancel_to_fill_ratio: Decimal::ZERO,
            median_order_lifetime_ms: None,
            size_vs_avg_trade: None,
        }
    }
}

// ─── Momentum-ignition detector ────────────────────────────
//
// `docs/research/complince.md` § 5 — burst taker flow drives a
// sharp price move, then the actor closes into the retracement.
// Different from the other detectors: it reads the PUBLIC trade
// tape (DataBus.trades), not our own orders, so the engine adapter
// hands a pre-sliced `Vec<PublicTradeSample>` in rather than the
// tracker.

#[derive(Debug, Clone)]
pub struct PublicTradeSample {
    pub ts: DateTime<Utc>,
    pub price: Decimal,
    pub qty: Decimal,
    /// Aggressor side, if the venue reported it.
    pub aggressor: Option<Side>,
}

#[derive(Debug, Clone)]
pub struct MomentumIgnitionConfig {
    /// Look-back window for the burst count (ms).
    pub burst_window_ms: i64,
    /// `trade_count >=` within window → full contribution.
    pub trade_count_hot: usize,
    /// Single-side volume dominance (`dominant_qty / total` ≥ this
    /// → full contribution). Catches "all aggressor flow one way".
    pub dominance_hot: Decimal,
    /// Min price move (bps) across the window to score at all.
    pub min_move_bps: Decimal,
}

impl Default for MomentumIgnitionConfig {
    fn default() -> Self {
        Self {
            burst_window_ms: 1_500,
            trade_count_hot: 30,
            dominance_hot: dec!(0.8),
            min_move_bps: dec!(10),
        }
    }
}

#[derive(Debug, Default)]
pub struct MomentumIgnitionDetector {
    pub config: MomentumIgnitionConfig,
}

impl MomentumIgnitionDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(&self, trades: &[PublicTradeSample]) -> DetectorOutput {
        if trades.is_empty() {
            return DetectorOutput {
                score: Decimal::ZERO,
                cancel_to_fill_ratio: Decimal::ZERO,
                median_order_lifetime_ms: None,
                size_vs_avg_trade: None,
            };
        }
        // Window the trades to the config burst_window_ms.
        let now = trades
            .last()
            .map(|t| t.ts)
            .unwrap_or_else(Utc::now);
        let cutoff = now - chrono::Duration::milliseconds(self.config.burst_window_ms);
        let windowed: Vec<&PublicTradeSample> =
            trades.iter().filter(|t| t.ts >= cutoff).collect();
        if windowed.is_empty() {
            return DetectorOutput {
                score: Decimal::ZERO,
                cancel_to_fill_ratio: Decimal::ZERO,
                median_order_lifetime_ms: None,
                size_vs_avg_trade: None,
            };
        }

        // (1) burst rate signal.
        let rate_sig = if self.config.trade_count_hot == 0 {
            Decimal::ZERO
        } else {
            (Decimal::from(windowed.len())
                / Decimal::from(self.config.trade_count_hot))
            .min(Decimal::ONE)
        };

        // (2) aggressor dominance.
        let mut buy_qty = Decimal::ZERO;
        let mut sell_qty = Decimal::ZERO;
        for t in &windowed {
            match t.aggressor {
                Some(Side::Buy) => buy_qty += t.qty,
                Some(Side::Sell) => sell_qty += t.qty,
                None => {}
            }
        }
        let total = buy_qty + sell_qty;
        let dominance = if total > Decimal::ZERO {
            (buy_qty.max(sell_qty)) / total
        } else {
            Decimal::ZERO
        };
        let dom_sig = if self.config.dominance_hot > Decimal::ZERO
            && dominance >= self.config.dominance_hot
        {
            Decimal::ONE
        } else if self.config.dominance_hot > Decimal::ZERO {
            (dominance / self.config.dominance_hot).min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };

        // (3) price-move signal.
        let first = windowed.first().unwrap().price;
        let last = windowed.last().unwrap().price;
        let move_bps = if first > Decimal::ZERO {
            (last - first).abs() / first * dec!(10_000)
        } else {
            Decimal::ZERO
        };
        let move_sig = if self.config.min_move_bps > Decimal::ZERO
            && move_bps >= self.config.min_move_bps * dec!(3)
        {
            Decimal::ONE
        } else if self.config.min_move_bps > Decimal::ZERO {
            (move_bps / (self.config.min_move_bps * dec!(3)))
                .min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };

        let score = (rate_sig + dom_sig + move_sig) / dec!(3);
        DetectorOutput {
            score: score.clamp(Decimal::ZERO, Decimal::ONE),
            cancel_to_fill_ratio: Decimal::ZERO,
            median_order_lifetime_ms: None,
            size_vs_avg_trade: None,
        }
    }
}

#[cfg(test)]
mod tests;
