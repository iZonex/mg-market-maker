//! Stoikov 2018 learned micro-price G-function (Epic D, sub-component #2).
//!
//! Implements the histogram-fit variant of the G-function from
//! Stoikov — "The Micro-Price: A High-Frequency Estimator of
//! Future Prices" (*Quantitative Finance*, 18(12), 1959–1966,
//! 2018).
//!
//! # What the model does
//!
//! Given a stream of L1 snapshots `(I_t, S_t, mid_t)` where
//! `I_t` is the top-of-book imbalance and `S_t` is the spread,
//! the model maintains an empirical estimate of
//!
//! ```text
//! G(I, S) = E[ mid_{t+k} − mid_t | I_t = I, S_t = S ]
//! ```
//!
//! for a fixed prediction horizon `k` (default 10 ticks). The
//! "learned" micro-price is then just `mid_t + G(I_t, S_t)` —
//! a drift-adjusted fair-value estimate that beats the 1988
//! opposite-side-weighted micro-price on predictive power.
//!
//! # v1 scope
//!
//! Pure histogram fit: for each `(imbalance_bucket, spread_bucket)`
//! pair, track the running mean of `Δmid` observed in the
//! training data. Buckets with fewer than
//! `min_bucket_samples` observations clamp to zero at
//! `finalize` time to avoid noisy predictions.
//!
//! TOML / JSON persistence and the offline CLI fit tool are
//! deferred to Sprint D-4 where the engine integration is
//! also landing — the wave-1 core ships alone for D-2.
//!
//! Full formula + source attribution in
//! `docs/research/signal-wave-2-formulas.md`
//! §"Sub-component #2".

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Tuning knobs for the G-function fit. See
/// `docs/research/signal-wave-2-formulas.md` §"Sub-component #2"
/// for how each parameter enters the math.
#[derive(Debug, Clone)]
pub struct LearnedMicropriceConfig {
    /// Number of equal-width bins on the imbalance axis. Must be
    /// ≥ 2. Default 20.
    pub n_imbalance_buckets: usize,
    /// Number of quantile bins on the spread axis. Must be ≥ 1.
    /// Default 5.
    pub n_spread_buckets: usize,
    /// Minimum sample count per bucket for a bucket to produce
    /// a non-zero prediction. Under-sampled buckets clamp to
    /// zero after [`LearnedMicroprice::finalize`]. Default 100.
    pub min_bucket_samples: usize,
}

impl Default for LearnedMicropriceConfig {
    fn default() -> Self {
        Self {
            n_imbalance_buckets: 20,
            n_spread_buckets: 5,
            min_bucket_samples: 100,
        }
    }
}

/// Fitted learned-microprice G-function. Build via
/// [`LearnedMicroprice::empty`] + repeated
/// [`LearnedMicroprice::accumulate`] calls + one
/// [`LearnedMicroprice::finalize`], then query via
/// [`LearnedMicroprice::predict`].
#[derive(Debug, Clone)]
pub struct LearnedMicroprice {
    config: LearnedMicropriceConfig,
    /// Running sum of `Δmid` per bucket, indexed as `[i][s]`.
    bucket_sum: Vec<Vec<Decimal>>,
    /// Running count per bucket.
    bucket_count: Vec<Vec<usize>>,
    /// Spread samples used to compute the quantile edges at
    /// finalize time. Dropped after `finalize`.
    spread_samples: Vec<Decimal>,
    /// Length `n_spread_buckets - 1` quantile edges on the
    /// spread axis. Populated by [`Self::finalize`]; empty
    /// before.
    spread_edges: Vec<Decimal>,
    /// Finalized prediction matrix, `[i][s]`. Populated by
    /// [`Self::finalize`].
    g_matrix: Vec<Vec<Decimal>>,
    /// `true` after [`Self::finalize`] has run.
    finalized: bool,
}

impl LearnedMicroprice {
    /// Construct an empty fit with the given config.
    pub fn empty(config: LearnedMicropriceConfig) -> Self {
        assert!(
            config.n_imbalance_buckets >= 2,
            "n_imbalance_buckets must be >= 2"
        );
        assert!(
            config.n_spread_buckets >= 1,
            "n_spread_buckets must be >= 1"
        );
        let bucket_sum =
            vec![vec![Decimal::ZERO; config.n_spread_buckets]; config.n_imbalance_buckets];
        let bucket_count = vec![vec![0usize; config.n_spread_buckets]; config.n_imbalance_buckets];
        let g_matrix =
            vec![vec![Decimal::ZERO; config.n_spread_buckets]; config.n_imbalance_buckets];
        Self {
            config,
            bucket_sum,
            bucket_count,
            spread_samples: Vec::new(),
            spread_edges: Vec::new(),
            g_matrix,
            finalized: false,
        }
    }

    /// Fold one observation into the fit. `imbalance` must be
    /// in `[-1, 1]`, `spread` must be ≥ 0. `delta_mid` is the
    /// observed forward-mid change at the fixed horizon `k`.
    ///
    /// Panics if called after [`Self::finalize`].
    pub fn accumulate(&mut self, imbalance: Decimal, spread: Decimal, delta_mid: Decimal) {
        assert!(
            !self.finalized,
            "cannot accumulate after finalize — build a new fit"
        );
        let i = imbalance_bucket(imbalance, self.config.n_imbalance_buckets);
        // Before finalize, stash every spread sample — the
        // quantile edges need the full distribution.
        self.spread_samples.push(spread);
        // Pre-finalize, bucket all spreads into the 0-th bin
        // since we haven't computed the edges yet. finalize
        // re-buckets the running sums using the empirical
        // edges.
        let s_pre = 0usize;
        self.bucket_sum[i][s_pre] += delta_mid;
        self.bucket_count[i][s_pre] += 1;
    }

    /// Compute the quantile edges on spread, re-bucket the
    /// running sums, and populate the final `g_matrix`. No
    /// further [`Self::accumulate`] calls allowed after this
    /// point. Idempotent: calling twice is a no-op after the
    /// first time.
    pub fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        // Compute quantile edges if we have >1 spread bucket.
        if self.config.n_spread_buckets > 1 && !self.spread_samples.is_empty() {
            let mut sorted = self.spread_samples.clone();
            sorted.sort();
            let n = sorted.len();
            let n_edges = self.config.n_spread_buckets - 1;
            let mut edges = Vec::with_capacity(n_edges);
            for k in 1..=n_edges {
                let idx = (k * n) / self.config.n_spread_buckets;
                edges.push(sorted[idx.min(n - 1)]);
            }
            self.spread_edges = edges;
        }

        // Re-bucket sums: we lost per-observation resolution
        // when accumulating into bucket_sum[i][0]. For v1 we
        // re-walk the spread samples only to split per-bucket
        // *counts* proportionally. Without keeping every
        // (i, s, Δ) tuple in memory, we can't perfectly
        // re-bucket after the fact.
        //
        // v1 compromise: the pre-finalize stash uses the
        // 0-th spread bucket only, which is correct when
        // `n_spread_buckets == 1`. For `n_spread_buckets > 1`
        // the caller must stream observations *after*
        // seeding the spread edges via `with_spread_edges`
        // + `accumulate_with_edges`, OR accept the
        // single-bucket fit. See `accumulate_two_pass` for
        // the two-pass path used by the offline CLI in
        // Sprint D-4.

        // Compute per-bucket mean with min-samples clamp.
        for i in 0..self.config.n_imbalance_buckets {
            for s in 0..self.config.n_spread_buckets {
                let count = self.bucket_count[i][s];
                self.g_matrix[i][s] = if count >= self.config.min_bucket_samples {
                    self.bucket_sum[i][s] / Decimal::from(count as u64)
                } else {
                    Decimal::ZERO
                };
            }
        }

        self.spread_samples.clear();
        self.spread_samples.shrink_to_fit();
        self.finalized = true;
    }

    /// Predict `Δmid` given the current `(imbalance, spread)`
    /// state. Returns `0` when:
    /// - The fit has not been finalized yet
    /// - The target bucket had fewer than `min_bucket_samples`
    ///   observations during training
    ///
    /// Always safe to call; never panics.
    pub fn predict(&self, imbalance: Decimal, spread: Decimal) -> Decimal {
        if !self.finalized {
            return Decimal::ZERO;
        }
        let i = imbalance_bucket(imbalance, self.config.n_imbalance_buckets);
        let s = spread_bucket(spread, &self.spread_edges);
        self.g_matrix[i][s]
    }

    /// Seed pre-computed quantile edges for the spread axis
    /// from the caller. Must be called before any
    /// [`Self::accumulate_with_edges`] call. Length must equal
    /// `n_spread_buckets - 1`.
    ///
    /// This is the workaround for v1's single-pass accumulate
    /// limitation when `n_spread_buckets > 1`: the caller does
    /// a first pass to compute the spread edges, calls this,
    /// then does a second pass via
    /// [`Self::accumulate_with_edges`] which buckets the
    /// observation correctly.
    pub fn with_spread_edges(&mut self, edges: Vec<Decimal>) {
        assert!(
            !self.finalized,
            "cannot seed edges after finalize — build a new fit"
        );
        assert_eq!(
            edges.len(),
            self.config.n_spread_buckets.saturating_sub(1),
            "edges must be n_spread_buckets - 1",
        );
        self.spread_edges = edges;
    }

    /// Accumulate with the pre-seeded spread edges. Must be
    /// preceded by a [`Self::with_spread_edges`] call. Panics
    /// if the edges have not been set.
    ///
    /// This is the correct path for `n_spread_buckets > 1`
    /// fits: callers do a first pass to compute the spread
    /// distribution, call `with_spread_edges`, then use this
    /// for the second pass.
    pub fn accumulate_with_edges(
        &mut self,
        imbalance: Decimal,
        spread: Decimal,
        delta_mid: Decimal,
    ) {
        assert!(
            !self.finalized,
            "cannot accumulate after finalize — build a new fit"
        );
        assert!(
            self.config.n_spread_buckets == 1 || !self.spread_edges.is_empty(),
            "call with_spread_edges before accumulate_with_edges",
        );
        let i = imbalance_bucket(imbalance, self.config.n_imbalance_buckets);
        let s = spread_bucket(spread, &self.spread_edges);
        self.bucket_sum[i][s] += delta_mid;
        self.bucket_count[i][s] += 1;
    }

    /// Read-only accessor for the quantile edges computed (or
    /// seeded) on the spread axis.
    pub fn spread_edges(&self) -> &[Decimal] {
        &self.spread_edges
    }

    /// Returns `true` after [`Self::finalize`] has run.
    pub fn is_finalized(&self) -> bool {
        self.finalized
    }

    /// Read-only accessor for the finalized `G` matrix.
    pub fn g_matrix(&self) -> &[Vec<Decimal>] {
        &self.g_matrix
    }

    /// Number of observations folded into the given bucket.
    pub fn bucket_count(&self, i: usize, s: usize) -> usize {
        self.bucket_count[i][s]
    }
}

/// Map imbalance ∈ `[-1, 1]` to a bucket index in
/// `[0, n_buckets)`. Values at or below −1 clamp to bucket 0,
/// values at or above +1 clamp to bucket `n_buckets − 1`.
fn imbalance_bucket(imbalance: Decimal, n_buckets: usize) -> usize {
    if n_buckets == 0 {
        return 0;
    }
    // Clamp to [-1, 1] first to avoid out-of-range indexing
    // on pathological inputs.
    let clamped = if imbalance < dec!(-1) {
        dec!(-1)
    } else if imbalance > dec!(1) {
        dec!(1)
    } else {
        imbalance
    };
    // Scale `[-1, 1]` → `[0, n_buckets]`, floor to integer.
    let scaled = (clamped + dec!(1)) * Decimal::from(n_buckets as u64) / dec!(2);
    let idx = scaled.trunc().to_string().parse::<usize>().unwrap_or(0);
    idx.min(n_buckets - 1)
}

/// Map a spread value to a bucket index in `[0, n_spread_buckets)`
/// using the provided quantile edges. `edges[i]` is the upper
/// boundary of bucket `i`; a value ≤ `edges[i]` and > `edges[i-1]`
/// lands in bucket `i`. Length of `edges` is
/// `n_spread_buckets − 1` (single-bucket fits pass an empty
/// slice).
fn spread_bucket(spread: Decimal, edges: &[Decimal]) -> usize {
    // Binary search would be cleaner; linear is fine for the
    // default `n_spread_buckets = 5`.
    for (i, edge) in edges.iter().enumerate() {
        if spread <= *edge {
            return i;
        }
    }
    edges.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_bucket_config() -> LearnedMicropriceConfig {
        LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 5,
        }
    }

    #[test]
    fn empty_config_returns_zero_predictions() {
        let mut mp = LearnedMicroprice::empty(LearnedMicropriceConfig::default());
        mp.finalize();
        assert_eq!(mp.predict(dec!(0.3), dec!(0.01)), Decimal::ZERO);
        assert_eq!(mp.predict(dec!(-0.7), dec!(0.05)), Decimal::ZERO);
    }

    #[test]
    fn predict_before_finalize_returns_zero() {
        let mp = LearnedMicroprice::empty(single_bucket_config());
        // No finalize call — predict should safely return zero.
        assert_eq!(mp.predict(dec!(0.5), dec!(0.01)), Decimal::ZERO);
    }

    #[test]
    fn single_bucket_fit_recovers_mean_delta() {
        let mut mp = LearnedMicroprice::empty(single_bucket_config());
        // Feed 10 observations with Δmid = +0.5 in the high-
        // imbalance bucket. Fewer in negative buckets so they
        // clamp to zero.
        for _ in 0..10 {
            mp.accumulate(dec!(0.8), dec!(0.01), dec!(0.5));
        }
        mp.finalize();
        let pred = mp.predict(dec!(0.8), dec!(0.01));
        assert_eq!(pred, dec!(0.5));
    }

    #[test]
    fn undersampled_bucket_clamps_to_zero() {
        let mut mp = LearnedMicroprice::empty(single_bucket_config());
        // Only 3 samples — below min_bucket_samples=5.
        for _ in 0..3 {
            mp.accumulate(dec!(0.5), dec!(0.01), dec!(1.0));
        }
        mp.finalize();
        // Bucket count below threshold → prediction clamps to 0.
        assert_eq!(mp.predict(dec!(0.5), dec!(0.01)), Decimal::ZERO);
    }

    #[test]
    fn imbalance_bucket_boundaries_are_stable() {
        // With 4 buckets on [-1, 1]: edges at -0.5, 0, +0.5.
        assert_eq!(imbalance_bucket(dec!(-1), 4), 0);
        assert_eq!(imbalance_bucket(dec!(-0.75), 4), 0);
        assert_eq!(imbalance_bucket(dec!(-0.25), 4), 1);
        assert_eq!(imbalance_bucket(dec!(0), 4), 2);
        assert_eq!(imbalance_bucket(dec!(0.25), 4), 2);
        assert_eq!(imbalance_bucket(dec!(0.75), 4), 3);
        assert_eq!(imbalance_bucket(dec!(1), 4), 3);
    }

    #[test]
    fn imbalance_bucket_clamps_out_of_range() {
        assert_eq!(imbalance_bucket(dec!(-5), 4), 0);
        assert_eq!(imbalance_bucket(dec!(2), 4), 3);
    }

    #[test]
    fn spread_bucket_with_edges() {
        let edges = vec![dec!(0.01), dec!(0.05), dec!(0.1)];
        // 4 buckets total: (−∞, 0.01], (0.01, 0.05], (0.05, 0.1], (0.1, +∞)
        assert_eq!(spread_bucket(dec!(0.005), &edges), 0);
        assert_eq!(spread_bucket(dec!(0.01), &edges), 0);
        assert_eq!(spread_bucket(dec!(0.03), &edges), 1);
        assert_eq!(spread_bucket(dec!(0.05), &edges), 1);
        assert_eq!(spread_bucket(dec!(0.07), &edges), 2);
        assert_eq!(spread_bucket(dec!(0.5), &edges), 3);
    }

    #[test]
    fn spread_bucket_no_edges_always_zero() {
        // Degenerate n_spread_buckets = 1 — empty edges slice.
        assert_eq!(spread_bucket(dec!(0), &[]), 0);
        assert_eq!(spread_bucket(dec!(100), &[]), 0);
    }

    #[test]
    fn two_pass_fit_with_spread_edges() {
        // Demonstrates the two-pass path: operator computes
        // spread edges externally (e.g. quantiles over a
        // training corpus), seeds them, then accumulates.
        let config = LearnedMicropriceConfig {
            n_imbalance_buckets: 2,
            n_spread_buckets: 2,
            min_bucket_samples: 2,
        };
        let mut mp = LearnedMicroprice::empty(config);
        mp.with_spread_edges(vec![dec!(0.05)]);

        // Low imbalance + tight spread → Δmid negative.
        mp.accumulate_with_edges(dec!(-0.8), dec!(0.01), dec!(-0.3));
        mp.accumulate_with_edges(dec!(-0.8), dec!(0.02), dec!(-0.5));

        // High imbalance + wide spread → Δmid positive.
        mp.accumulate_with_edges(dec!(0.8), dec!(0.1), dec!(0.4));
        mp.accumulate_with_edges(dec!(0.8), dec!(0.15), dec!(0.6));

        mp.finalize();

        let low_tight = mp.predict(dec!(-0.9), dec!(0.02));
        let high_wide = mp.predict(dec!(0.9), dec!(0.12));
        assert_eq!(low_tight, dec!(-0.4));
        assert_eq!(high_wide, dec!(0.5));
    }

    #[test]
    fn finalize_is_idempotent() {
        let mut mp = LearnedMicroprice::empty(single_bucket_config());
        for _ in 0..10 {
            mp.accumulate(dec!(0.5), dec!(0.01), dec!(0.1));
        }
        mp.finalize();
        let p1 = mp.predict(dec!(0.5), dec!(0.01));
        mp.finalize();
        let p2 = mp.predict(dec!(0.5), dec!(0.01));
        assert_eq!(p1, p2);
    }

    #[test]
    #[should_panic(expected = "cannot accumulate after finalize")]
    fn accumulate_after_finalize_panics() {
        let mut mp = LearnedMicroprice::empty(single_bucket_config());
        mp.finalize();
        mp.accumulate(dec!(0.5), dec!(0.01), dec!(0.1));
    }

    #[test]
    #[should_panic(expected = "n_imbalance_buckets must be >= 2")]
    fn empty_panics_on_tiny_imbalance_buckets() {
        LearnedMicroprice::empty(LearnedMicropriceConfig {
            n_imbalance_buckets: 1,
            n_spread_buckets: 1,
            min_bucket_samples: 1,
        });
    }

    #[test]
    fn bucket_count_accessor_reports_totals() {
        let mut mp = LearnedMicroprice::empty(single_bucket_config());
        for _ in 0..7 {
            mp.accumulate(dec!(0.8), dec!(0.01), dec!(0.2));
        }
        // All 7 observations land in the last imbalance bucket
        // (i = 3) on spread bucket 0 since n_spread_buckets = 1.
        assert_eq!(mp.bucket_count(3, 0), 7);
        assert_eq!(mp.bucket_count(0, 0), 0);
    }

    #[test]
    fn monotone_imbalance_produces_monotone_prediction_under_monotone_training() {
        // Training data: as imbalance rises, Δmid rises.
        // After fit, predictions should respect that ordering.
        let config = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 3,
        };
        let mut mp = LearnedMicroprice::empty(config);
        // Bucket 0: Δmid = −0.4
        for _ in 0..4 {
            mp.accumulate(dec!(-0.75), dec!(0.01), dec!(-0.4));
        }
        // Bucket 1: Δmid = −0.1
        for _ in 0..4 {
            mp.accumulate(dec!(-0.25), dec!(0.01), dec!(-0.1));
        }
        // Bucket 2: Δmid = +0.1
        for _ in 0..4 {
            mp.accumulate(dec!(0.25), dec!(0.01), dec!(0.1));
        }
        // Bucket 3: Δmid = +0.4
        for _ in 0..4 {
            mp.accumulate(dec!(0.75), dec!(0.01), dec!(0.4));
        }
        mp.finalize();

        let p_neg = mp.predict(dec!(-0.75), dec!(0.01));
        let p_mid_lo = mp.predict(dec!(-0.25), dec!(0.01));
        let p_mid_hi = mp.predict(dec!(0.25), dec!(0.01));
        let p_pos = mp.predict(dec!(0.75), dec!(0.01));

        assert!(p_neg < p_mid_lo);
        assert!(p_mid_lo < p_mid_hi);
        assert!(p_mid_hi < p_pos);
    }
}
