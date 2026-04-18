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
            side: Side::Buy,
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
    fn layering_cluster_of_five_bids_scores_high() {
        let mut t = OrderLifecycleTracker::new();
        let t0 = Utc::now();
        // 6 buy orders clustered within 3 bps of each other.
        for (i, px) in [100.00, 100.01, 100.02, 100.01, 100.03, 100.02]
            .iter()
            .enumerate()
        {
            let id = format!("L{i}");
            t.feed(&SurveillanceEvent::OrderPlaced {
                order_id: id.clone(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                price: Decimal::from_f64_retain(*px).unwrap(),
                qty: dec!(1),
                ts: t0,
            });
        }
        let d = LayeringDetector::new();
        let out = d.score("BTCUSDT", &t);
        assert!(out.score >= dec!(0.5), "layering score was {}", out.score);
    }

    #[test]
    fn quote_stuffing_high_rate_low_fill_scores_high() {
        let mut t = OrderLifecycleTracker::new();
        let t0 = Utc::now();
        // Feed enough cancels to clear the 50 orders/sec × 60s bar
        // (3000 total). All fast-cancelled, zero fills — classic
        // stuffing silhouette.
        for i in 0..3100 {
            let id = format!("S{i}");
            t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
            t.feed(&ev_cancel(
                &id,
                "BTCUSDT",
                t0 + chrono::Duration::milliseconds(20),
            ));
        }
        let d = QuoteStuffingDetector::new();
        let out = d.score("BTCUSDT", &t);
        assert!(out.score >= dec!(0.9), "stuffing score was {}", out.score);
    }

    #[test]
    fn wash_pairs_own_buy_and_sell_at_same_price() {
        let t0 = Utc::now();
        let fills = vec![
            WashFillView { ts: t0, side: Side::Buy, price: dec!(100) },
            WashFillView {
                ts: t0 + chrono::Duration::milliseconds(100),
                side: Side::Sell,
                price: dec!(100),
            },
            WashFillView {
                ts: t0 + chrono::Duration::milliseconds(200),
                side: Side::Buy,
                price: dec!(100),
            },
            WashFillView {
                ts: t0 + chrono::Duration::milliseconds(300),
                side: Side::Sell,
                price: dec!(100),
            },
        ];
        let d = WashDetector::new();
        let out = d.score_from_fills(&fills);
        // Four fills → pairs = 4 (every opposite-side within 500ms
        // same price). With pair_count_hot=3, score clamps to 1.
        assert!(out.score >= dec!(0.9), "wash score was {}", out.score);
    }

    #[test]
    fn wash_ignores_distant_prices() {
        let t0 = Utc::now();
        let fills = vec![
            WashFillView { ts: t0, side: Side::Buy, price: dec!(100) },
            WashFillView {
                ts: t0 + chrono::Duration::milliseconds(100),
                side: Side::Sell,
                price: dec!(105),
            },
        ];
        let d = WashDetector::new();
        assert_eq!(d.score_from_fills(&fills).score, dec!(0));
    }

    #[test]
    fn momentum_burst_dominant_side_scores_high() {
        let t0 = Utc::now();
        let mut trades: Vec<PublicTradeSample> = Vec::new();
        // 40 trades over 1.5 s all aggressor-buy + price drifts up 50 bps.
        for i in 0..40 {
            let ts = t0 + chrono::Duration::milliseconds((i * 30) as i64);
            let px = dec!(100) + Decimal::from(i) / dec!(20); // 100 → 101.95
            trades.push(PublicTradeSample {
                ts,
                price: px,
                qty: dec!(1),
                aggressor: Some(Side::Buy),
            });
        }
        let d = MomentumIgnitionDetector::new();
        let out = d.score(&trades);
        assert!(out.score >= dec!(0.9), "mi score was {}", out.score);
    }

    #[test]
    fn momentum_balanced_flow_scores_low() {
        let t0 = Utc::now();
        let mut trades = Vec::new();
        for i in 0u32..6 {
            trades.push(PublicTradeSample {
                ts: t0 + chrono::Duration::milliseconds((i as i64) * 200),
                price: dec!(100),
                qty: dec!(1),
                aggressor: Some(if i.is_multiple_of(2) { Side::Buy } else { Side::Sell }),
            });
        }
        let d = MomentumIgnitionDetector::new();
        let out = d.score(&trades);
        assert!(out.score <= dec!(0.5), "mi score was {}", out.score);
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
