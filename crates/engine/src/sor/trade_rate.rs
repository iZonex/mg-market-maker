//! Per-venue trade-rate estimator (Epic A stage-2 #2).
//!
//! Feeds a live, sliding-window estimate of
//! `venue_trade_rate (base-qty / sec)` into the SOR's cost
//! model. Replaces the v1 seeded-constant `queue_wait_secs`
//! with a number derived from the actual tape.
//!
//! # Design
//!
//! Every public `Trade` we see on a venue's WS feed is
//! recorded with its timestamp (wall-clock ns). On query we
//! prune observations older than `window_secs` and divide the
//! remaining total base-qty by the window length.
//!
//! The derived `expected_queue_wait_secs(depth_qty)` scales
//! the depth ahead of our order by the inverse rate — what
//! the GLFT / Cartea literature calls the maker queue time
//! under Poisson fill arrivals:
//!
//! ```text
//! queue_wait_secs ≈ depth_qty / trade_rate_per_sec
//! ```
//!
//! Callers should fall back to a config constant when the
//! tracker has seen fewer than `MIN_SAMPLES` prints — a
//! freshly-booted engine or a thinly-traded symbol will
//! surface `rate_per_sec() = None` until the window starts
//! filling.

use rust_decimal::Decimal;
use std::collections::VecDeque;

/// Minimum number of observations before the estimator will
/// emit a non-`None` rate. Protects against the pathological
/// "two trades in the last second" → artificially huge rate.
const MIN_SAMPLES: usize = 5;

/// Trade-rate estimator fed from a single venue's
/// `MarketEvent::Trade` stream. Time injection via
/// `(now_ns, qty)` tuples keeps the estimator deterministic
/// and unit-testable — the engine pushes wall-clock `now_ns`
/// at call time.
#[derive(Debug, Clone)]
pub struct TradeRateEstimator {
    window_ns: i64,
    samples: VecDeque<(i64, Decimal)>,
    /// Rolling sum of `qty` inside the window so `rate_per_sec`
    /// doesn't re-scan the deque. Kept in sync with
    /// `samples` in every `push` / `prune`.
    total_qty: Decimal,
}

impl TradeRateEstimator {
    /// Construct with a rolling window length in seconds.
    /// A 60 s window is a reasonable default for top-of-
    /// book crypto venues; thin names should widen it so the
    /// tail isn't dominated by a single trade.
    pub fn new(window_secs: u64) -> Self {
        let secs = window_secs.max(1);
        Self {
            window_ns: (secs as i64) * 1_000_000_000,
            samples: VecDeque::new(),
            total_qty: Decimal::ZERO,
        }
    }

    /// Record one trade of size `qty` at wall-clock `now_ns`.
    /// Callers pre-compute `now_ns = chrono::Utc::now()
    /// .timestamp_nanos_opt()` — we don't touch the clock
    /// internally so tests can feed synthetic timelines.
    pub fn record(&mut self, now_ns: i64, qty: Decimal) {
        self.samples.push_back((now_ns, qty));
        self.total_qty += qty;
        self.prune(now_ns);
    }

    /// Drop observations older than `window_ns` relative to
    /// `now_ns`. Called from `record` and from the accessors
    /// so a long-idle symbol doesn't serve a stale rate.
    fn prune(&mut self, now_ns: i64) {
        while let Some(&(ts, qty)) = self.samples.front() {
            if now_ns - ts <= self.window_ns {
                break;
            }
            self.samples.pop_front();
            self.total_qty -= qty;
        }
        // Float-like arithmetic is exact on Decimal so the
        // running total should never drift negative; clamp
        // defensively just in case a future caller pushes a
        // negative qty.
        if self.total_qty < Decimal::ZERO {
            self.total_qty = Decimal::ZERO;
        }
    }

    /// Average trade-qty-per-second over the current window.
    /// Returns `None` until the estimator has accumulated
    /// `MIN_SAMPLES` observations inside the window.
    ///
    /// Callers should pass the same `now_ns` they're about to
    /// query the venue with — we prune stale samples here so
    /// the returned rate always reflects the window ending
    /// at `now_ns`.
    pub fn rate_per_sec(&mut self, now_ns: i64) -> Option<Decimal> {
        self.prune(now_ns);
        if self.samples.len() < MIN_SAMPLES {
            return None;
        }
        let window_secs = Decimal::from(self.window_ns / 1_000_000_000);
        if window_secs <= Decimal::ZERO {
            return None;
        }
        Some(self.total_qty / window_secs)
    }

    /// Estimated seconds the MM's resting order will wait
    /// behind `depth_qty` of queue. Assumes Poisson fill
    /// arrivals at the current trade rate. Returns `None`
    /// when the rate is unknown so callers can fall back to
    /// a config constant cleanly.
    pub fn expected_queue_wait_secs(
        &mut self,
        now_ns: i64,
        depth_qty: Decimal,
    ) -> Option<Decimal> {
        let rate = self.rate_per_sec(now_ns)?;
        if rate <= Decimal::ZERO {
            return None;
        }
        Some(depth_qty.abs() / rate)
    }

    /// Number of observations currently inside the window.
    /// Used by observability + tests.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Total `qty` currently inside the window.
    pub fn total_qty(&self) -> Decimal {
        self.total_qty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const NS_PER_SEC: i64 = 1_000_000_000;

    #[test]
    fn empty_estimator_returns_none() {
        let mut e = TradeRateEstimator::new(60);
        assert!(e.rate_per_sec(0).is_none());
    }

    #[test]
    fn below_min_samples_returns_none() {
        let mut e = TradeRateEstimator::new(60);
        // Push fewer than MIN_SAMPLES (5).
        for i in 0..4 {
            e.record(i * NS_PER_SEC, dec!(1));
        }
        assert!(e.rate_per_sec(4 * NS_PER_SEC).is_none());
    }

    #[test]
    fn at_min_samples_returns_rate() {
        let mut e = TradeRateEstimator::new(60);
        for i in 0..5 {
            e.record(i * NS_PER_SEC, dec!(1));
        }
        // 5 trades × 1 qty over 60 s window = 5/60.
        let r = e.rate_per_sec(5 * NS_PER_SEC).expect("rate");
        assert_eq!(r, dec!(5) / dec!(60));
    }

    #[test]
    fn window_evicts_stale_samples() {
        let mut e = TradeRateEstimator::new(10); // 10s window.
        // Seed 6 trades at t=0..5 with qty=1.
        for i in 0..6 {
            e.record(i * NS_PER_SEC, dec!(1));
        }
        // Now query at t=20 — every sample is stale.
        assert_eq!(e.rate_per_sec(20 * NS_PER_SEC), None);
        assert_eq!(e.sample_count(), 0);
        assert_eq!(e.total_qty(), dec!(0));
    }

    #[test]
    fn expected_queue_wait_scales_with_depth() {
        let mut e = TradeRateEstimator::new(10);
        // 10 trades × 1 qty over 10s = 1 qty/sec.
        for i in 0..10 {
            e.record(i * NS_PER_SEC, dec!(1));
        }
        let q1 = e
            .expected_queue_wait_secs(10 * NS_PER_SEC, dec!(5))
            .expect("wait");
        let q2 = e
            .expected_queue_wait_secs(10 * NS_PER_SEC, dec!(10))
            .expect("wait");
        // Depth doubles → wait doubles.
        assert_eq!(q2, q1 * dec!(2));
    }

    #[test]
    fn expected_queue_wait_none_when_rate_unknown() {
        let mut e = TradeRateEstimator::new(10);
        assert!(e.expected_queue_wait_secs(0, dec!(5)).is_none());
    }

    #[test]
    fn expected_queue_wait_monotonic_in_depth() {
        let mut e = TradeRateEstimator::new(10);
        for i in 0..10 {
            e.record(i * NS_PER_SEC, dec!(1));
        }
        let now = 10 * NS_PER_SEC;
        let a = e.expected_queue_wait_secs(now, dec!(1)).unwrap();
        let b = e.expected_queue_wait_secs(now, dec!(5)).unwrap();
        let c = e.expected_queue_wait_secs(now, dec!(25)).unwrap();
        assert!(a < b && b < c);
    }

    #[test]
    fn zero_window_clamps_to_one_sec() {
        let e = TradeRateEstimator::new(0);
        assert_eq!(e.window_ns, NS_PER_SEC);
    }

    #[test]
    fn total_qty_tracks_running_sum_exact() {
        let mut e = TradeRateEstimator::new(60);
        e.record(0, dec!(1.5));
        e.record(NS_PER_SEC, dec!(2.25));
        e.record(2 * NS_PER_SEC, dec!(0.75));
        assert_eq!(e.total_qty(), dec!(4.5));
    }
}
