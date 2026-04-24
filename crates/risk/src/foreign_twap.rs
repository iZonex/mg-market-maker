//! MM-7 — Foreign-TWAP periodicity detector.
//!
//! Port of the reference repo's FFT-based TWAP detector
//! (`binance_l3_est/src/engine/twap.rs`) expressed as an
//! autocorrelation-based tracker so we don't pull an FFT
//! dependency just for the one signal.
//!
//! ## What it detects
//!
//! A **foreign** TWAP / iceberg algorithm is a competing
//! participant who slices a large parent order into equal-size
//! children that arrive at a steady cadence. The silhouette:
//! trade counts (or volumes) bucketed at ~100 ms resolution
//! show a dominant autocorrelation peak at the slicing
//! interval. Random / Poisson flow has a flat autocorrelation
//! near zero past lag-1.
//!
//! ## Algorithm
//!
//! 1. Ingest every public trade's timestamp via [`record_trade`].
//! 2. Bucket into fixed-width time slots (default 500 ms) held
//!    in a ring of 128 slots → 64 s of observation window.
//! 3. On [`score`]:
//!    a. Centre the series (`x - mean`).
//!    b. Compute normalised autocorrelation `R[k]` for lag
//!    `k ∈ [2, N/2]` — lag 1 is dominated by bucket-edge
//!    discretisation and isn't a reliable TWAP signal.
//!    c. Return the max `R[k]` (clamped to `[0, 1]`) as the
//!    detector score, plus the period estimate `k * width_ms`.
//!
//! A score ≥ 0.8 is the conventional "alert grade" bar
//! matching every other Epic R detector; below ~0.4 is
//! indistinguishable from Poisson noise.

use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::collections::VecDeque;

use crate::surveillance::DetectorOutput;

/// Default bucket width in ms. 500 ms trades off between
/// detecting fast slicers (≤5 s cadence) and keeping the
/// observation window wide enough to spot slower ones.
pub const DEFAULT_BUCKET_WIDTH_MS: i64 = 500;
/// Default number of buckets retained. 128 × 500 ms = 64 s.
/// Power of two so the eventual FFT upgrade drops in cleanly.
pub const DEFAULT_BUCKETS: usize = 128;
/// Minimum trades required before the detector returns a
/// non-zero score. Below this, autocorrelation is dominated
/// by a handful of points and the signal is pure noise.
pub const MIN_TRADES_FOR_DETECTION: u64 = 60;

/// Rolling per-bucket trade-count ring. Owns its eviction —
/// callers just push timestamps in.
#[derive(Debug, Clone)]
pub struct ForeignTwapDetector {
    bucket_width_ms: i64,
    bucket_count: usize,
    /// Counts per bucket, oldest first. Length ≤ `bucket_count`.
    /// Buckets are aligned to `bucket_width_ms` boundaries so a
    /// gap in the feed reads as zero counts, not a drift.
    buckets: VecDeque<u32>,
    /// Start-of-bucket ms for the back (newest) entry in
    /// `buckets`. Used to decide whether a new trade lands in
    /// the current bucket or rolls a fresh one.
    current_bucket_start: Option<i64>,
    total_trades: u64,
}

impl Default for ForeignTwapDetector {
    fn default() -> Self {
        Self::new(DEFAULT_BUCKET_WIDTH_MS, DEFAULT_BUCKETS)
    }
}

impl ForeignTwapDetector {
    pub fn new(bucket_width_ms: i64, bucket_count: usize) -> Self {
        assert!(bucket_width_ms > 0, "bucket width must be positive");
        assert!(bucket_count >= 8, "need at least 8 buckets for autocorr");
        Self {
            bucket_width_ms,
            bucket_count,
            buckets: VecDeque::with_capacity(bucket_count),
            current_bucket_start: None,
            total_trades: 0,
        }
    }

    /// Record one public trade at `ts_ms`. Silently ignores
    /// out-of-order timestamps (ts < current bucket start) to
    /// keep the ring monotonic; WS clock skew is a real thing.
    pub fn record_trade(&mut self, ts_ms: i64) {
        let aligned = ts_ms - ts_ms.rem_euclid(self.bucket_width_ms);
        let Some(cur) = self.current_bucket_start else {
            self.current_bucket_start = Some(aligned);
            self.buckets.push_back(1);
            self.total_trades += 1;
            return;
        };
        if aligned < cur {
            return; // out-of-order — drop
        }
        if aligned == cur {
            if let Some(b) = self.buckets.back_mut() {
                *b += 1;
            }
            self.total_trades += 1;
            return;
        }
        // New bucket(s) — pad any skipped windows with zeros so
        // autocorr sees the real time spacing, not collapsed
        // activity.
        let gap = ((aligned - cur) / self.bucket_width_ms) as usize;
        for _ in 1..gap {
            self.buckets.push_back(0);
            if self.buckets.len() > self.bucket_count {
                self.buckets.pop_front();
            }
        }
        self.buckets.push_back(1);
        if self.buckets.len() > self.bucket_count {
            self.buckets.pop_front();
        }
        self.current_bucket_start = Some(aligned);
        self.total_trades += 1;
    }

    /// Compute the detector score. Returns `score ∈ [0, 1]`
    /// plus the dominant period (in bucket count) packed into
    /// `DetectorOutput.median_order_lifetime_ms` so downstream
    /// audit rows carry both pieces without a new schema.
    pub fn score(&self) -> DetectorOutput {
        if self.total_trades < MIN_TRADES_FOR_DETECTION || self.buckets.len() < 16 {
            return DetectorOutput::default();
        }
        // Centre the series.
        let n = self.buckets.len();
        let mean: f64 = self.buckets.iter().map(|c| *c as f64).sum::<f64>() / n as f64;
        let centered: Vec<f64> = self.buckets.iter().map(|c| *c as f64 - mean).collect();
        let variance: f64 = centered.iter().map(|x| x * x).sum::<f64>() / n as f64;
        if variance < f64::EPSILON {
            return DetectorOutput::default();
        }

        // Normalised autocorrelation R[k] = (1 / (n - k)) *
        // sum(centered[i] * centered[i + k]) / variance.
        let max_lag = (n / 2).min(n - 1);
        let mut r: Vec<f64> = Vec::with_capacity(max_lag + 1);
        r.push(1.0); // R[0] by definition
        for k in 1..=max_lag {
            let mut sum = 0.0f64;
            for i in 0..(n - k) {
                sum += centered[i] * centered[i + k];
            }
            r.push(sum / ((n - k) as f64 * variance));
        }

        // Find the **fundamental** period: the smallest lag
        // k ≥ 2 whose R[k] is a local maximum and within 80% of
        // the global peak. Plain "arg max R[k]" reports a
        // harmonic multiple (e.g. lag 60 on a lag-10 base signal
        // has nearly the same correlation), which would mis-
        // estimate the TWAP cadence by a factor of 2–6.
        let global_max = r[2..=max_lag].iter().copied().fold(0.0f64, f64::max);
        let mut best_k = 0usize;
        let mut best_r = 0.0f64;
        if global_max > 0.0 {
            let threshold = 0.8 * global_max;
            for k in 2..max_lag {
                if r[k] < threshold {
                    continue;
                }
                if r[k] > r[k - 1] && r[k] > r[k + 1] {
                    best_k = k;
                    best_r = r[k];
                    break;
                }
            }
            if best_k == 0 {
                // No local max cleared threshold → fall back to the
                // global-max lag. Keeps the periodic-signal test
                // from silently regressing to zero if the peak
                // happens to sit at an endpoint.
                for (k, val) in r.iter().enumerate().take(max_lag + 1).skip(2) {
                    if *val > best_r {
                        best_r = *val;
                        best_k = k;
                    }
                }
            }
        }

        let score = best_r.clamp(0.0, 1.0);
        let period_ms = (best_k as i64) * self.bucket_width_ms;
        DetectorOutput {
            score: Decimal::from_f64(score).unwrap_or(Decimal::ZERO),
            median_order_lifetime_ms: if period_ms > 0 { Some(period_ms) } else { None },
            ..Default::default()
        }
    }

    pub fn total_trades(&self) -> u64 {
        self.total_trades
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// Below the minimum sample count the detector stays silent.
    #[test]
    fn returns_zero_below_min_trades() {
        let mut d = ForeignTwapDetector::default();
        for i in 0..10 {
            d.record_trade(i * 100);
        }
        let out = d.score();
        assert_eq!(out.score, dec!(0));
    }

    /// A strictly-periodic stream (trade every 5 s) produces a
    /// dominant autocorrelation peak and a high score.
    #[test]
    fn periodic_stream_scores_high() {
        let mut d = ForeignTwapDetector::default();
        // 80 trades at a perfect 5-second cadence — cadence
        // quantises exactly onto 500 ms buckets so the signal
        // is clean.
        for i in 0..80u64 {
            d.record_trade((i as i64) * 5_000);
        }
        let out = d.score();
        assert!(
            out.score >= dec!(0.5),
            "periodic stream should score > 0.5, got {}",
            out.score
        );
        // Period estimate should land near 5000 ms (lag 10 of
        // 500 ms buckets).
        let ms = out.median_order_lifetime_ms.expect("period emitted");
        assert!(
            (4_000..=6_000).contains(&ms),
            "expected period ~5000 ms, got {ms}"
        );
    }

    /// Poisson-ish random stream doesn't trip an alert.
    #[test]
    fn random_stream_scores_low() {
        let mut d = ForeignTwapDetector::default();
        // 80 trades at jittered intervals seeded deterministically —
        // no LCG / rand dep needed, a simple rolling pseudo-random.
        let mut t = 0i64;
        let mut state: u64 = 0x1234_5678;
        for _ in 0..80 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let jitter = ((state >> 33) as i64) % 800 + 100; // 100..900 ms
            t += jitter;
            d.record_trade(t);
        }
        let out = d.score();
        assert!(
            out.score < dec!(0.5),
            "random stream scored too high: {}",
            out.score
        );
    }

    /// Out-of-order timestamps are dropped — the detector stays
    /// monotonic on its bucket ring.
    #[test]
    fn out_of_order_timestamps_are_ignored() {
        let mut d = ForeignTwapDetector::default();
        d.record_trade(10_000);
        d.record_trade(5_000); // out-of-order — dropped
        assert_eq!(d.total_trades(), 1);
    }
}
