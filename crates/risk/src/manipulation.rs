//! Epic R Phase 2 — CEX-side market-manipulation detection.
//!
//! Where [`crate::surveillance`] watches *our own* order flow so a
//! MiFID II reviewer can confirm we didn't spoof, this module
//! watches the *public market* so an MM desk can spot a symbol
//! under active manipulation (pump-dump, wash prints, thin-book
//! hype pumps) and protect itself — typically by widening
//! spreads, cutting size, or pausing quoting.
//!
//! Four components compose the module:
//!
//!   1. [`PumpDumpDetector`] — price velocity (% change across a
//!      rolling window) crossed with volume surge (current
//!      trade-rate vs trailing EWMA) → a [0, 1] score. Triggers on
//!      the RAVE / SIREN / MYX shape: a low-liquidity listing
//!      rockets on coordinated buying, runs well ahead of
//!      organic interest, then cliff-dumps.
//!
//!   2. [`WashPrintDetector`] — picks up the classic self-trade
//!      signature: N opposite-side prints of nearly the same size
//!      within a tight time window, at prices clustered around
//!      one level. Works on public trades only, so it sees the
//!      *other* participants' wash patterns on the tape.
//!
//!   3. [`ThinBookGuard`] — flags a book whose visible depth
//!      within ± 2 % is tiny relative to recent traded notional.
//!      A high market cap with a thin book is the structural
//!      precondition for every RAVE-style dump.
//!
//!   4. [`ManipulationScoreAggregator`] — weighted combination of
//!      the three sub-scores into a single [0, 1] value the
//!      engine publishes to the dashboard and pipes through
//!      `Surveillance.ManipulationScore` into graph kill-switch
//!      gates.
//!
//! All four are honest signal estimators — never classifiers on
//! their own. Thresholds and response (widen, cut size, pause)
//! are a downstream operator / kill-switch policy decision.

use chrono::{DateTime, Utc};
use mm_common::orderbook::LocalOrderBook;
use mm_common::types::{Side, Trade};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;

// ────────────────────────────────────────────────────────────
// PumpDumpDetector
// ────────────────────────────────────────────────────────────

/// Configuration for [`PumpDumpDetector`].
#[derive(Debug, Clone)]
pub struct PumpDumpConfig {
    /// Rolling window split into two equal halves for the
    /// surge comparison. Default 300 seconds → the last 5 min
    /// is compared against the 5 min before that.
    pub window_secs: i64,
    /// Price change (absolute, bps) that saturates the
    /// velocity component at 1.0. Default 500 bps (5 %). A
    /// listing putting 5 % on the board in 5 minutes without
    /// news is already manipulation territory.
    pub velocity_saturation_bps: Decimal,
    /// Ratio (recent-half notional / baseline-half notional)
    /// that saturates the surge component at 1.0. Default 5 —
    /// last half moved five times the notional of the
    /// baseline half.
    pub surge_saturation_ratio: Decimal,
}

impl Default for PumpDumpConfig {
    fn default() -> Self {
        Self {
            window_secs: 300,
            velocity_saturation_bps: dec!(500),
            surge_saturation_ratio: dec!(5),
        }
    }
}

/// Tracks recent public trades and emits a [0, 1] pump-dump
/// score. The score is a product of two sub-components — both
/// must be elevated for the signal to fire. The trailing
/// baseline is the first half of the window, the recent
/// snapshot is the second half, so a detector on a cold
/// symbol self-warms without a separate pre-seeding step.
#[derive(Debug)]
pub struct PumpDumpDetector {
    config: PumpDumpConfig,
    /// `(ts_ms, price, notional)` window, oldest at front.
    window: VecDeque<(i64, Decimal, Decimal)>,
}

impl PumpDumpDetector {
    pub fn new() -> Self {
        Self::with_config(PumpDumpConfig::default())
    }

    pub fn with_config(config: PumpDumpConfig) -> Self {
        Self {
            config,
            window: VecDeque::new(),
        }
    }

    pub fn on_trade(&mut self, trade: &Trade) {
        let ts_ms = trade.timestamp.timestamp_millis();
        let notional = trade.price * trade.qty;
        self.window.push_back((ts_ms, trade.price, notional));
        let cutoff = ts_ms - self.config.window_secs * 1000;
        while let Some(&(t, _, _)) = self.window.front() {
            if t < cutoff {
                self.window.pop_front();
            } else {
                break;
            }
        }
    }

    /// Absolute % price change across the window, in bps.
    /// Returns `Decimal::ZERO` when the window is empty or the
    /// first price is zero (degenerate).
    pub fn price_change_bps(&self) -> Decimal {
        let Some(&(_, first, _)) = self.window.front() else {
            return Decimal::ZERO;
        };
        let Some(&(_, last, _)) = self.window.back() else {
            return Decimal::ZERO;
        };
        if first.is_zero() {
            return Decimal::ZERO;
        }
        let change = (last - first) / first;
        (change * Decimal::from(10_000)).abs()
    }

    /// Ratio of second-half notional to first-half notional.
    /// Returns 0 when either half is empty (pre-warmup — need
    /// enough history for the baseline to mean anything).
    pub fn volume_surge_ratio(&self) -> Decimal {
        if self.window.is_empty() {
            return Decimal::ZERO;
        }
        let first_ts = self.window.front().map(|&(t, _, _)| t).unwrap_or(0);
        let last_ts = self.window.back().map(|&(t, _, _)| t).unwrap_or(0);
        let split_ts = first_ts + (last_ts - first_ts) / 2;
        let (mut first_half, mut second_half) = (Decimal::ZERO, Decimal::ZERO);
        for &(t, _, notional) in &self.window {
            if t <= split_ts {
                first_half += notional;
            } else {
                second_half += notional;
            }
        }
        if first_half.is_zero() {
            return Decimal::ZERO;
        }
        second_half / first_half
    }

    /// Combined [0, 1] pump-dump score. Product of velocity
    /// and surge — a quiet drift to a higher level without
    /// volume scores near zero, same for a volume surge
    /// without price movement. Real pump-dumps have both.
    pub fn score(&self) -> Decimal {
        let velocity =
            (self.price_change_bps() / self.config.velocity_saturation_bps).min(dec!(1));
        let surge =
            (self.volume_surge_ratio() / self.config.surge_saturation_ratio).min(dec!(1));
        velocity * surge
    }
}

impl Default for PumpDumpDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ────────────────────────────────────────────────────────────
// WashPrintDetector
// ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WashPrintConfig {
    /// Sliding window in which opposite-side print pairs are
    /// counted. Default 5 seconds.
    pub window_secs: i64,
    /// Price tolerance, in bps, for two prints to count as "at
    /// the same level". Default 10 bps.
    pub price_tolerance_bps: Decimal,
    /// Size tolerance — two prints count as "same size" when
    /// `|qty_a - qty_b| / max(qty_a, qty_b)` is under this.
    /// Default 0.05 (5 %).
    pub size_tolerance_pct: Decimal,
    /// Number of opposite-side size-matched pairs in the
    /// window that saturates the score at 1.0. Default 6 —
    /// three round-trips in 5 seconds is deep wash territory.
    pub saturation_pairs: u32,
}

impl Default for WashPrintConfig {
    fn default() -> Self {
        Self {
            window_secs: 5,
            price_tolerance_bps: dec!(10),
            size_tolerance_pct: dec!(0.05),
            saturation_pairs: 6,
        }
    }
}

/// Detects public wash-print signatures: opposite-side trades
/// of matching size within a tight time window, clustered near
/// one price level.
#[derive(Debug)]
pub struct WashPrintDetector {
    config: WashPrintConfig,
    window: VecDeque<(i64, Side, Decimal, Decimal)>, // (ts_ms, side, price, qty)
}

impl WashPrintDetector {
    pub fn new() -> Self {
        Self::with_config(WashPrintConfig::default())
    }

    pub fn with_config(config: WashPrintConfig) -> Self {
        Self {
            config,
            window: VecDeque::new(),
        }
    }

    pub fn on_trade(&mut self, trade: &Trade) {
        let ts_ms = trade.timestamp.timestamp_millis();
        self.window
            .push_back((ts_ms, trade.taker_side, trade.price, trade.qty));
        let cutoff = ts_ms - self.config.window_secs * 1000;
        while let Some(&(t, _, _, _)) = self.window.front() {
            if t < cutoff {
                self.window.pop_front();
            } else {
                break;
            }
        }
    }

    /// Count size-matched opposite-side pairs within the
    /// current window. Each print is used at most once so a
    /// buy-buy-sell-sell sequence counts as two pairs, not
    /// four.
    pub fn matched_pairs(&self) -> u32 {
        let trades: Vec<_> = self.window.iter().collect();
        let mut used = vec![false; trades.len()];
        let mut pairs = 0u32;
        for i in 0..trades.len() {
            if used[i] {
                continue;
            }
            for j in (i + 1)..trades.len() {
                if used[j] {
                    continue;
                }
                let (_, sa, pa, qa) = trades[i];
                let (_, sb, pb, qb) = trades[j];
                if sa == sb {
                    continue;
                }
                // Price tolerance check.
                if pa.is_zero() {
                    continue;
                }
                let px_diff_bps =
                    ((*pb - *pa) / *pa * Decimal::from(10_000)).abs();
                if px_diff_bps > self.config.price_tolerance_bps {
                    continue;
                }
                // Size tolerance check.
                let larger = (*qa).max(*qb);
                if larger.is_zero() {
                    continue;
                }
                let size_diff = (*qa - *qb).abs() / larger;
                if size_diff > self.config.size_tolerance_pct {
                    continue;
                }
                used[i] = true;
                used[j] = true;
                pairs += 1;
                break;
            }
        }
        pairs
    }

    pub fn score(&self) -> Decimal {
        if self.config.saturation_pairs == 0 {
            return Decimal::ZERO;
        }
        let n = self.matched_pairs();
        (Decimal::from(n) / Decimal::from(self.config.saturation_pairs))
            .min(dec!(1))
    }
}

impl Default for WashPrintDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ────────────────────────────────────────────────────────────
// ThinBookGuard
// ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ThinBookConfig {
    /// Price distance from mid (fraction) within which we sum
    /// quote-denominated book depth. Default 0.02 (2 %).
    pub depth_pct_from_mid: Decimal,
    /// Depth-to-volume ratio floor — below this the book is
    /// considered "thin given the volume". Default 0.1 — when
    /// visible depth ± 2 % is less than 10 % of the trailing
    /// one-minute notional, the tape is running far hotter
    /// than the book can absorb.
    pub min_ratio: Decimal,
    /// Rolling window for trailing notional (used as a
    /// reference for the ratio). Default 60 s.
    pub trailing_secs: i64,
}

impl Default for ThinBookConfig {
    fn default() -> Self {
        Self {
            // `bid_depth_within_pct_quote` expects percent units
            // (2 → 2 %), NOT a fraction.
            depth_pct_from_mid: dec!(2),
            min_ratio: dec!(0.1),
            trailing_secs: 60,
        }
    }
}

#[derive(Debug)]
pub struct ThinBookGuard {
    config: ThinBookConfig,
    /// `(ts_ms, notional)` trailing window.
    notional_window: VecDeque<(i64, Decimal)>,
    last_score: Decimal,
}

impl ThinBookGuard {
    pub fn new() -> Self {
        Self::with_config(ThinBookConfig::default())
    }

    pub fn with_config(config: ThinBookConfig) -> Self {
        Self {
            config,
            notional_window: VecDeque::new(),
            last_score: Decimal::ZERO,
        }
    }

    pub fn on_trade(&mut self, trade: &Trade) {
        let ts_ms = trade.timestamp.timestamp_millis();
        self.notional_window
            .push_back((ts_ms, trade.price * trade.qty));
        let cutoff = ts_ms - self.config.trailing_secs * 1000;
        while let Some(&(t, _)) = self.notional_window.front() {
            if t < cutoff {
                self.notional_window.pop_front();
            } else {
                break;
            }
        }
    }

    /// Evaluate the book for the current snapshot, caching
    /// the score. Called from the engine's tick loop after
    /// each book refresh.
    pub fn on_book(&mut self, book: &LocalOrderBook, now: DateTime<Utc>) {
        // Evict stale trades too — the book tick is a good
        // drive point for the window bookkeeping.
        let cutoff = now.timestamp_millis() - self.config.trailing_secs * 1000;
        while let Some(&(t, _)) = self.notional_window.front() {
            if t < cutoff {
                self.notional_window.pop_front();
            } else {
                break;
            }
        }

        let trailing_notional: Decimal =
            self.notional_window.iter().map(|(_, n)| *n).sum();
        if trailing_notional.is_zero() {
            self.last_score = Decimal::ZERO;
            return;
        }

        let bid_depth = book.bid_depth_within_pct_quote(self.config.depth_pct_from_mid);
        let ask_depth = book.ask_depth_within_pct_quote(self.config.depth_pct_from_mid);
        let depth = bid_depth + ask_depth;
        if depth.is_zero() {
            self.last_score = dec!(1);
            return;
        }
        let ratio = depth / trailing_notional;
        // Below min_ratio → saturate at 1.0. Linear ramp in
        // [min_ratio, 2*min_ratio] down to 0.
        let score = if ratio <= self.config.min_ratio {
            dec!(1)
        } else if ratio >= self.config.min_ratio * dec!(2) {
            Decimal::ZERO
        } else {
            let slope =
                (self.config.min_ratio * dec!(2) - ratio) / self.config.min_ratio;
            slope.max(Decimal::ZERO).min(dec!(1))
        };
        self.last_score = score;
    }

    pub fn score(&self) -> Decimal {
        self.last_score
    }
}

impl Default for ThinBookGuard {
    fn default() -> Self {
        Self::new()
    }
}

// ────────────────────────────────────────────────────────────
// ManipulationScoreAggregator
// ────────────────────────────────────────────────────────────

/// Weights for each sub-score. Must sum to 1.0 for the
/// aggregated score to stay in [0, 1]; the aggregator clamps
/// the result so a misconfigured weight doesn't poison
/// downstream consumers.
#[derive(Debug, Clone)]
pub struct ManipulationWeights {
    pub pump_dump: Decimal,
    pub wash: Decimal,
    pub thin_book: Decimal,
}

impl Default for ManipulationWeights {
    fn default() -> Self {
        // Pump-dump is the heaviest — it's the signal most
        // directly tied to actual operator loss. Wash + thin
        // book are contributing structural warnings.
        Self {
            pump_dump: dec!(0.5),
            wash: dec!(0.3),
            thin_book: dec!(0.2),
        }
    }
}

/// Snapshot of the aggregated manipulation view for one
/// symbol. Passed up to the dashboard and into the graph
/// source on every tick.
#[derive(Debug, Clone)]
pub struct ManipulationScoreSnapshot {
    pub pump_dump: Decimal,
    pub wash: Decimal,
    pub thin_book: Decimal,
    pub combined: Decimal,
}

#[derive(Debug)]
pub struct ManipulationScoreAggregator {
    pub pump_dump: PumpDumpDetector,
    pub wash: WashPrintDetector,
    pub thin_book: ThinBookGuard,
    weights: ManipulationWeights,
}

impl ManipulationScoreAggregator {
    pub fn new() -> Self {
        Self {
            pump_dump: PumpDumpDetector::new(),
            wash: WashPrintDetector::new(),
            thin_book: ThinBookGuard::new(),
            weights: ManipulationWeights::default(),
        }
    }

    pub fn with_weights(mut self, weights: ManipulationWeights) -> Self {
        self.weights = weights;
        self
    }

    pub fn on_trade(&mut self, trade: &Trade) {
        self.pump_dump.on_trade(trade);
        self.wash.on_trade(trade);
        self.thin_book.on_trade(trade);
    }

    pub fn on_book(&mut self, book: &LocalOrderBook, now: DateTime<Utc>) {
        self.thin_book.on_book(book, now);
    }

    pub fn snapshot(&self) -> ManipulationScoreSnapshot {
        let pd = self.pump_dump.score();
        let wa = self.wash.score();
        let tb = self.thin_book.score();
        let combined = (pd * self.weights.pump_dump
            + wa * self.weights.wash
            + tb * self.weights.thin_book)
            .min(dec!(1))
            .max(Decimal::ZERO);
        ManipulationScoreSnapshot {
            pump_dump: pd,
            wash: wa,
            thin_book: tb,
            combined,
        }
    }
}

impl Default for ManipulationScoreAggregator {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience helper used by tests + the graph source to
/// convert a `[0, 1]` score to a user-friendly f64.
pub fn score_to_f64(score: Decimal) -> f64 {
    score.to_f64().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn trade(ts_ms: i64, side: Side, price: Decimal, qty: Decimal) -> Trade {
        Trade {
            trade_id: ts_ms as u64,
            symbol: "RAVEUSDT".to_string(),
            price,
            qty,
            taker_side: side,
            timestamp: Utc.timestamp_millis_opt(ts_ms).single().unwrap(),
        }
    }

    #[test]
    fn pump_dump_fires_on_velocity_and_surge_together() {
        let mut d = PumpDumpDetector::new();
        // First half (t = 0..150 s): quiet baseline at $1, small qty.
        for i in 0..30 {
            d.on_trade(&trade(i * 5_000, Side::Buy, dec!(1.0), dec!(1)));
        }
        // Second half (t = 150..300 s): burst with climbing price
        // + 50× qty. Last price lands around $4 → > 20_000 bps
        // change, well above saturation. Notional of the second
        // half is ~150× the first, way past surge saturation.
        for i in 0..30 {
            let t = 150_000 + i * 5_000;
            let px = dec!(1.0) + Decimal::from(i) * dec!(0.1);
            d.on_trade(&trade(t, Side::Buy, px, dec!(50)));
        }
        let s = d.score();
        assert!(s > dec!(0.5), "expected pump-dump score > 0.5, got {s}");
    }

    #[test]
    fn pump_dump_quiet_market_scores_zero() {
        let mut d = PumpDumpDetector::new();
        for i in 0..50 {
            d.on_trade(&trade(i * 5_000, Side::Buy, dec!(100), dec!(1)));
        }
        assert!(d.score() < dec!(0.05));
    }

    #[test]
    fn wash_print_fires_on_matched_pairs() {
        let mut d = WashPrintDetector::new();
        // Six alternating, same-size same-price trades in 3 s.
        for i in 0..6 {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            d.on_trade(&trade(i * 500, side, dec!(10.0), dec!(5)));
        }
        let pairs = d.matched_pairs();
        assert_eq!(pairs, 3, "expected 3 matched pairs, got {pairs}");
        assert!(d.score() >= dec!(0.5));
    }

    #[test]
    fn wash_print_rejects_one_sided_flow() {
        let mut d = WashPrintDetector::new();
        for i in 0..10 {
            d.on_trade(&trade(i * 200, Side::Buy, dec!(10.0), dec!(5)));
        }
        assert_eq!(d.matched_pairs(), 0);
        assert_eq!(d.score(), Decimal::ZERO);
    }

    #[test]
    fn thin_book_fires_when_depth_tiny_vs_trailing_volume() {
        use mm_common::types::PriceLevel;
        let mut guard = ThinBookGuard::new();
        // Pile ≈ $10_000 of volume over 30 s.
        for i in 0..30 {
            guard.on_trade(&trade(i * 1_000, Side::Buy, dec!(100), dec!(3.33)));
        }
        // Book with ~$100 total visible depth ± 2 %.
        let mut book = LocalOrderBook::new("RAVEUSDT".to_string());
        book.apply_snapshot(
            vec![PriceLevel { price: dec!(99.5), qty: dec!(0.5) }],
            vec![PriceLevel { price: dec!(100.5), qty: dec!(0.5) }],
            1,
        );
        let now = Utc.timestamp_millis_opt(30_000).single().unwrap();
        guard.on_book(&book, now);
        let s = guard.score();
        assert!(s > dec!(0.5), "expected thin-book score > 0.5, got {s}");
    }

    #[test]
    fn thin_book_passes_healthy_book() {
        use mm_common::types::PriceLevel;
        let mut guard = ThinBookGuard::new();
        for i in 0..30 {
            guard.on_trade(&trade(i * 1_000, Side::Buy, dec!(100), dec!(1)));
        }
        let mut book = LocalOrderBook::new("BTCUSDT".to_string());
        book.apply_snapshot(
            vec![PriceLevel { price: dec!(99.5), qty: dec!(1000) }],
            vec![PriceLevel { price: dec!(100.5), qty: dec!(1000) }],
            1,
        );
        let now = Utc.timestamp_millis_opt(30_000).single().unwrap();
        guard.on_book(&book, now);
        assert_eq!(guard.score(), Decimal::ZERO);
    }

    #[test]
    fn aggregator_produces_weighted_combined_score() {
        let mut agg = ManipulationScoreAggregator::new();
        // First-half quiet baseline.
        for i in 0..30 {
            agg.on_trade(&trade(i * 5_000, Side::Buy, dec!(1.0), dec!(1)));
        }
        // Second-half pump.
        for i in 0..30 {
            let t = 150_000 + i * 5_000;
            let px = dec!(1.0) + Decimal::from(i) * dec!(0.1);
            agg.on_trade(&trade(t, Side::Buy, px, dec!(50)));
        }
        let snap = agg.snapshot();
        assert!(snap.pump_dump > dec!(0.3),
            "expected pump_dump > 0.3, got {}", snap.pump_dump);
        assert_eq!(snap.thin_book, Decimal::ZERO);
        // Combined = 0.5 * pump_dump + 0.3 * wash + 0.2 * thin.
        let expected = snap.pump_dump * dec!(0.5)
            + snap.wash * dec!(0.3)
            + snap.thin_book * dec!(0.2);
        assert_eq!(snap.combined, expected.min(dec!(1)));
    }
}
