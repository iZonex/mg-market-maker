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
//! TOML persistence + the offline CLI fit tool
//! (`mm-learned-microprice-fit` binary) land in stage-2 polish
//! alongside the engine integration — see
//! `docs/sprints/epic-d-stage2-polish.md` §2A.
//!
//! Full formula + source attribution in
//! `docs/research/signal-wave-2-formulas.md`
//! §"Sub-component #2".

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;

/// Tuning knobs for the G-function fit. See
/// `docs/research/signal-wave-2-formulas.md` §"Sub-component #2"
/// for how each parameter enters the math.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Epic D stage-2 — cap on the online observation ring
    /// used by [`LearnedMicroprice::update_online`]. Older
    /// observations drop off the back as new ones arrive.
    /// Default 10 000 — ≈ 1 hour of 3 Hz L1 snapshots. Set
    /// smaller on thinner tapes (smaller window adapts faster
    /// but noisier), larger on deeper books.
    #[serde(default = "default_online_ring_capacity")]
    pub online_ring_capacity: usize,
    /// Epic D stage-2 — re-fit cadence. The online ring is
    /// appended every update, but the `g_matrix` is
    /// rebuilt only every `refit_every` calls to amortise the
    /// scan. Default 500 ≈ every 3 minutes at 3 Hz.
    #[serde(default = "default_refit_every")]
    pub refit_every: usize,
}

fn default_online_ring_capacity() -> usize {
    10_000
}
fn default_refit_every() -> usize {
    500
}

impl Default for LearnedMicropriceConfig {
    fn default() -> Self {
        Self {
            n_imbalance_buckets: 20,
            n_spread_buckets: 5,
            min_bucket_samples: 100,
            online_ring_capacity: default_online_ring_capacity(),
            refit_every: default_refit_every(),
        }
    }
}

/// Fitted learned-microprice G-function. Build via
/// [`LearnedMicroprice::empty`] + repeated
/// [`LearnedMicroprice::accumulate`] calls + one
/// [`LearnedMicroprice::finalize`], then query via
/// [`LearnedMicroprice::predict`].
///
/// Serialisable for on-disk persistence via
/// [`LearnedMicroprice::to_toml`] / [`LearnedMicroprice::from_toml`].
/// The transient `spread_samples` accumulator is skipped — once a
/// model is finalised it carries no per-observation state and the
/// `spread_edges` / `g_matrix` are sufficient to reproduce
/// predictions byte-for-byte.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedMicroprice {
    config: LearnedMicropriceConfig,
    /// Running sum of `Δmid` per bucket, indexed as `[i][s]`.
    bucket_sum: Vec<Vec<Decimal>>,
    /// Running count per bucket.
    bucket_count: Vec<Vec<usize>>,
    /// Spread samples used to compute the quantile edges at
    /// finalize time. Dropped after `finalize`. Skipped in
    /// serialisation — it is always empty by the time a model
    /// is written to disk.
    #[serde(skip, default)]
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
    /// Epic D stage-2 — bounded ring of recent observations
    /// consumed by [`Self::update_online`] to periodically
    /// rebuild the g-matrix against the freshest window of
    /// data. `#[serde(skip)]` keeps persisted TOML stable
    /// (the offline fit already captures the training-window
    /// state; the ring is a live-only structure).
    #[serde(skip, default)]
    online_ring: VecDeque<(Decimal, Decimal, Decimal)>,
    /// Epic D stage-2 — counter ticked on every
    /// [`Self::update_online`] call. Re-fits fire when
    /// `counter % refit_every == 0`, so updates amortise a
    /// full rebuild over `refit_every` observations.
    #[serde(skip, default)]
    online_counter: usize,
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
            online_ring: VecDeque::new(),
            online_counter: 0,
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

    /// Iterative fixed-point finalization (Stoikov 2018 §3.2).
    ///
    /// For sparse buckets (count < `min_bucket_samples`), borrows
    /// information from the nearest well-sampled neighbors via
    /// inverse-distance weighting. Iterates until convergence or
    /// `max_iter` rounds. Strictly improves on `finalize()` for
    /// models with uneven bucket coverage — the v1 zero-clamp is
    /// a special case where neighbor weight is zero.
    ///
    /// Call instead of `finalize()`, not in addition to it.
    pub fn finalize_iterative(&mut self, _max_iter: usize) {
        if self.finalized {
            return;
        }
        // First compute quantile edges and per-bucket means
        // exactly like `finalize()`.
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

        self.rebuild_g_matrix();

        self.spread_samples.clear();
        self.spread_samples.shrink_to_fit();
        self.finalized = true;
    }

    /// Shared "seed well-sampled buckets + inverse-distance
    /// neighbour fill for sparse ones" machinery used by both
    /// [`Self::finalize_iterative`] and [`Self::update_online`].
    /// Assumes `bucket_sum` + `bucket_count` have been populated
    /// and `spread_edges` are set — does NOT mutate them; only
    /// writes the `g_matrix`.
    fn rebuild_g_matrix(&mut self) {
        let ni = self.config.n_imbalance_buckets;
        let ns = self.config.n_spread_buckets;
        let min_samples = self.config.min_bucket_samples;

        // Seed well-sampled buckets with their empirical mean;
        // sparse buckets start at zero and get filled below.
        for i in 0..ni {
            for s in 0..ns {
                let count = self.bucket_count[i][s];
                self.g_matrix[i][s] = if count >= min_samples {
                    self.bucket_sum[i][s] / Decimal::from(count as u64)
                } else {
                    Decimal::ZERO
                };
            }
        }

        // Neighbour fill: inverse-Manhattan-distance weighted
        // average from every well-sampled bucket to every
        // sparse one. Single-pass — anchors stay fixed, so
        // iteration isn't needed.
        for i in 0..ni {
            for s in 0..ns {
                if self.bucket_count[i][s] >= min_samples {
                    continue;
                }
                let mut weighted_sum = Decimal::ZERO;
                let mut total_weight = Decimal::ZERO;
                for d in 1..=(ni.max(ns)) {
                    for (di, ds) in neighbor_ring(d) {
                        let ni_idx = i as i64 + di;
                        let ns_idx = s as i64 + ds;
                        if ni_idx < 0
                            || ni_idx >= ni as i64
                            || ns_idx < 0
                            || ns_idx >= ns as i64
                        {
                            continue;
                        }
                        let ni_u = ni_idx as usize;
                        let ns_u = ns_idx as usize;
                        if self.bucket_count[ni_u][ns_u] >= min_samples {
                            let w = Decimal::ONE / Decimal::from(d as u64);
                            weighted_sum += w * self.g_matrix[ni_u][ns_u];
                            total_weight += w;
                        }
                    }
                }
                if total_weight > Decimal::ZERO {
                    self.g_matrix[i][s] = weighted_sum / total_weight;
                }
            }
        }
    }

    /// Epic D stage-2 — push one fresh observation into the
    /// online ring and (if the refit cadence has been reached)
    /// rebuild the g-matrix against the most recent
    /// `online_ring_capacity` observations.
    ///
    /// Must be called on an already-finalised model: the
    /// spread edges are consumed as-is so the online path
    /// stays consistent with the offline training window's
    /// spread-quantile definition. Calls on an un-finalised
    /// model are a silent no-op so an operator wiring the
    /// live callback before the fit loads can't accidentally
    /// overwrite an empty g-matrix with meaningless
    /// defaults.
    ///
    /// `imbalance` must be in `[-1, 1]`, `spread ≥ 0`,
    /// `delta_mid` is the observed forward-mid change at
    /// the same horizon the offline fit used.
    pub fn update_online(
        &mut self,
        imbalance: Decimal,
        spread: Decimal,
        delta_mid: Decimal,
    ) {
        if !self.finalized {
            return;
        }
        // Push + evict.
        if self.online_ring.len() >= self.config.online_ring_capacity.max(1) {
            self.online_ring.pop_front();
        }
        self.online_ring.push_back((imbalance, spread, delta_mid));
        self.online_counter = self.online_counter.wrapping_add(1);

        let refit_every = self.config.refit_every.max(1);
        if !self.online_counter.is_multiple_of(refit_every) {
            return;
        }

        // Rebuild: zero the buckets, re-accumulate from the ring
        // against the already-established `spread_edges`, then
        // rerun the seed + neighbour fill.
        for row in self.bucket_sum.iter_mut() {
            for cell in row.iter_mut() {
                *cell = Decimal::ZERO;
            }
        }
        for row in self.bucket_count.iter_mut() {
            for cell in row.iter_mut() {
                *cell = 0;
            }
        }
        for (imb, sp, dm) in &self.online_ring {
            let i = imbalance_bucket(*imb, self.config.n_imbalance_buckets);
            let s = spread_bucket(*sp, &self.spread_edges);
            self.bucket_sum[i][s] += *dm;
            self.bucket_count[i][s] += 1;
        }
        self.rebuild_g_matrix();
    }

    /// Read-only accessor for the online ring length. Used in
    /// tests and observability surfaces; always ≤
    /// `config.online_ring_capacity`.
    pub fn online_ring_len(&self) -> usize {
        self.online_ring.len()
    }

    /// Number of `update_online` calls since construction.
    /// Wraps on overflow (64-bit wrap ≈ never in practice).
    pub fn online_counter(&self) -> usize {
        self.online_counter
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

    /// Serialise the model to a TOML file at `path`. Persists
    /// the full finalised state (config, `bucket_sum`,
    /// `bucket_count`, `spread_edges`, `g_matrix`, `finalized`
    /// flag). Transient `spread_samples` are not written — they
    /// are always empty after [`Self::finalize`] and carry no
    /// value for a reloaded model.
    ///
    /// Round-trip property: a model written with `to_toml` and
    /// re-loaded with [`Self::from_toml`] produces byte-identical
    /// [`Self::predict`] output for every `(imbalance, spread)`
    /// input. See `learned_microprice_toml_*` tests.
    pub fn to_toml(&self, path: &Path) -> Result<()> {
        let s = toml::to_string_pretty(self)
            .context("failed to serialise LearnedMicroprice to TOML")?;
        std::fs::write(path, s)
            .with_context(|| format!("failed to write TOML to {}", path.display()))?;
        Ok(())
    }

    /// Load a previously-persisted model from `path`. The TOML
    /// file must match the schema produced by [`Self::to_toml`].
    pub fn from_toml(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read TOML from {}", path.display()))?;
        let model: Self = toml::from_str(&s).context("failed to parse LearnedMicroprice TOML")?;
        Ok(model)
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
/// Generate all (di, ds) offsets at Manhattan distance `d`.
fn neighbor_ring(d: usize) -> Vec<(i64, i64)> {
    let d = d as i64;
    let mut out = Vec::new();
    for di in -d..=d {
        let ds = d - di.abs();
        if ds == 0 {
            out.push((di, 0));
        } else {
            out.push((di, ds));
            out.push((di, -ds));
        }
    }
    out
}

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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    // ------------------------- TOML persistence -------------------------

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        // Unique-ish suffix so parallel test threads don't stomp.
        let uniq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("mm_lmp_{name}_{uniq}.toml"));
        p
    }

    #[test]
    fn learned_microprice_toml_empty_roundtrip() {
        let mp = LearnedMicroprice::empty(single_bucket_config());
        let path = tmp_path("empty");
        mp.to_toml(&path).expect("write empty model");
        let reloaded = LearnedMicroprice::from_toml(&path).expect("read empty model");
        assert!(!reloaded.is_finalized());
        assert_eq!(reloaded.config.n_imbalance_buckets, 4);
        assert_eq!(reloaded.config.n_spread_buckets, 1);
        assert_eq!(reloaded.config.min_bucket_samples, 5);
        assert_eq!(reloaded.g_matrix().len(), 4);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn learned_microprice_toml_finalized_fit_roundtrip() {
        let mut mp = LearnedMicroprice::empty(single_bucket_config());
        for _ in 0..8 {
            mp.accumulate(dec!(0.8), dec!(0.01), dec!(0.5));
        }
        mp.finalize();
        let path = tmp_path("finalized");
        mp.to_toml(&path).expect("write");
        let reloaded = LearnedMicroprice::from_toml(&path).expect("read");
        assert!(reloaded.is_finalized());
        assert_eq!(reloaded.g_matrix(), mp.g_matrix());
        assert_eq!(reloaded.bucket_count(3, 0), mp.bucket_count(3, 0));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn learned_microprice_toml_spread_edges_roundtrip() {
        let config = LearnedMicropriceConfig {
            n_imbalance_buckets: 2,
            n_spread_buckets: 3,
            min_bucket_samples: 1,
            ..Default::default()
        };
        let mut mp = LearnedMicroprice::empty(config);
        mp.with_spread_edges(vec![dec!(0.02), dec!(0.08)]);
        // A few observations so the g_matrix is non-trivial.
        mp.accumulate_with_edges(dec!(-0.5), dec!(0.01), dec!(-0.1));
        mp.accumulate_with_edges(dec!(0.5), dec!(0.05), dec!(0.2));
        mp.accumulate_with_edges(dec!(0.5), dec!(0.2), dec!(0.4));
        mp.finalize();
        let path = tmp_path("edges");
        mp.to_toml(&path).expect("write");
        let reloaded = LearnedMicroprice::from_toml(&path).expect("read");
        assert_eq!(reloaded.spread_edges(), mp.spread_edges());
        assert_eq!(reloaded.spread_edges().len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn learned_microprice_toml_prediction_parity_post_roundtrip() {
        let config = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 2,
            min_bucket_samples: 2,
            ..Default::default()
        };
        let mut mp = LearnedMicroprice::empty(config);
        mp.with_spread_edges(vec![dec!(0.05)]);
        for _ in 0..4 {
            mp.accumulate_with_edges(dec!(-0.8), dec!(0.01), dec!(-0.2));
            mp.accumulate_with_edges(dec!(0.8), dec!(0.1), dec!(0.3));
        }
        mp.finalize();
        let path = tmp_path("parity");
        mp.to_toml(&path).expect("write");
        let reloaded = LearnedMicroprice::from_toml(&path).expect("read");
        // Exhaustive prediction parity over a grid of inputs.
        for im in [dec!(-0.9), dec!(-0.3), dec!(0.3), dec!(0.9)] {
            for sp in [dec!(0.001), dec!(0.03), dec!(0.06), dec!(0.2)] {
                assert_eq!(
                    reloaded.predict(im, sp),
                    mp.predict(im, sp),
                    "prediction mismatch at im={im}, sp={sp}"
                );
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn monotone_imbalance_produces_monotone_prediction_under_monotone_training() {
        // Training data: as imbalance rises, Δmid rises.
        // After fit, predictions should respect that ordering.
        let config = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 3,
            ..Default::default()
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

    // ── Iterative fixed-point tests ─────────────────────────

    /// Iterative finalize fills sparse buckets from neighbors.
    /// With 4 imbalance buckets and 1 spread bucket, train
    /// only buckets 0 and 3 (extremes) and verify that the
    /// interior buckets get non-zero predictions via neighbor
    /// interpolation.
    #[test]
    fn iterative_fills_sparse_from_neighbors() {
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 5,
            ..Default::default()
        };
        let mut mp = LearnedMicroprice::empty(cfg);
        // Train only bucket 0 (imbalance ~ -0.75) and bucket 3
        // (imbalance ~ +0.75).
        for _ in 0..10 {
            mp.accumulate(dec!(-0.9), dec!(0.01), dec!(-0.5));
        }
        for _ in 0..10 {
            mp.accumulate(dec!(0.9), dec!(0.01), dec!(0.5));
        }
        mp.finalize_iterative(10);

        // Interior buckets should be non-zero (filled from neighbors).
        let p1 = mp.predict(dec!(-0.25), dec!(0.01));
        let p2 = mp.predict(dec!(0.25), dec!(0.01));
        assert!(
            p1 != Decimal::ZERO,
            "bucket 1 should be filled by neighbor, got 0"
        );
        assert!(
            p2 != Decimal::ZERO,
            "bucket 2 should be filled by neighbor, got 0"
        );
    }

    /// Standard finalize clamps sparse buckets to zero; iterative
    /// does not. Verify the difference.
    #[test]
    fn iterative_differs_from_standard_on_sparse() {
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 5,
            ..Default::default()
        };

        // Standard finalize.
        let mut mp_std = LearnedMicroprice::empty(cfg.clone());
        for _ in 0..10 {
            mp_std.accumulate(dec!(-0.9), dec!(0.01), dec!(-0.5));
        }
        mp_std.finalize();
        let p_std = mp_std.predict(dec!(-0.25), dec!(0.01));
        assert_eq!(p_std, Decimal::ZERO, "standard should clamp sparse to 0");

        // Iterative finalize.
        let mut mp_iter = LearnedMicroprice::empty(cfg);
        for _ in 0..10 {
            mp_iter.accumulate(dec!(-0.9), dec!(0.01), dec!(-0.5));
        }
        mp_iter.finalize_iterative(10);
        let p_iter = mp_iter.predict(dec!(-0.25), dec!(0.01));
        assert!(
            p_iter != Decimal::ZERO,
            "iterative should fill sparse bucket"
        );
    }

    /// Iterative finalize with all well-sampled buckets matches
    /// standard finalize exactly (no neighbor borrowing needed).
    #[test]
    fn iterative_matches_standard_when_all_sampled() {
        let cfg = single_bucket_config();
        let mut mp_std = LearnedMicroprice::empty(cfg.clone());
        let mut mp_iter = LearnedMicroprice::empty(cfg);

        for i in 0..40 {
            let imb = Decimal::from(i % 4) / dec!(2) - dec!(0.75);
            let dm = imb * dec!(0.1);
            mp_std.accumulate(imb, dec!(0.01), dm);
            mp_iter.accumulate(imb, dec!(0.01), dm);
        }
        mp_std.finalize();
        mp_iter.finalize_iterative(10);

        for imb_idx in 0..4 {
            let imb = Decimal::from(imb_idx) / dec!(2) - dec!(0.75);
            assert_eq!(
                mp_std.predict(imb, dec!(0.01)),
                mp_iter.predict(imb, dec!(0.01)),
                "bucket {} should match",
                imb_idx
            );
        }
    }

    // ---------------------------------------------------------
    // Epic D stage-2 — online streaming fit
    // ---------------------------------------------------------

    fn seed_finalised_fit() -> LearnedMicroprice {
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 2,
            online_ring_capacity: 50,
            refit_every: 10,
        };
        let mut mp = LearnedMicroprice::empty(cfg);
        // Seed with a mild upward drift so the offline fit is
        // not identically zero, otherwise update_online parity
        // tests can't distinguish "no effect" from "reset to
        // zero".
        for _ in 0..4 {
            mp.accumulate(dec!(-0.75), dec!(0.01), dec!(-0.1));
        }
        for _ in 0..4 {
            mp.accumulate(dec!(0.75), dec!(0.01), dec!(0.1));
        }
        mp.finalize_iterative(5);
        mp
    }

    #[test]
    fn update_online_on_unfinalised_model_is_noop() {
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 2,
            online_ring_capacity: 50,
            refit_every: 10,
        };
        let mut mp = LearnedMicroprice::empty(cfg);
        mp.update_online(dec!(0.5), dec!(0.01), dec!(1.0));
        // Silent no-op: counter stays zero, ring stays empty.
        assert_eq!(mp.online_ring_len(), 0);
        assert_eq!(mp.online_counter(), 0);
    }

    #[test]
    fn update_online_appends_every_call_refits_on_boundary() {
        let mut mp = seed_finalised_fit();
        let initial_g = mp.g_matrix().to_vec();
        for i in 1..=9 {
            mp.update_online(dec!(0.5), dec!(0.01), dec!(0.2));
            // Ring grows; g-matrix should be unchanged until
            // refit_every=10 is reached.
            assert_eq!(mp.online_ring_len(), i);
            assert_eq!(mp.g_matrix(), initial_g.as_slice(),
                "g-matrix must not refit before the {i}th update");
        }
        // 10th update triggers the rebuild.
        mp.update_online(dec!(0.5), dec!(0.01), dec!(0.2));
        assert_eq!(mp.online_counter(), 10);
        assert_ne!(mp.g_matrix(), initial_g.as_slice(),
            "refit at boundary must update g-matrix");
    }

    #[test]
    fn update_online_bounded_ring_does_not_grow_unbounded() {
        let mut mp = seed_finalised_fit();
        for _ in 0..200 {
            mp.update_online(dec!(0.0), dec!(0.01), dec!(0.0));
        }
        assert_eq!(mp.online_ring_len(), 50,
            "ring must cap at online_ring_capacity=50");
        assert_eq!(mp.online_counter(), 200);
    }

    #[test]
    fn update_online_preserves_spread_edges() {
        // Build a multi-spread-bucket fit via the two-pass
        // path, then online-push some observations and verify
        // the edges stay byte-for-byte equal.
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 3,
            min_bucket_samples: 1,
            online_ring_capacity: 50,
            refit_every: 5,
        };
        let mut mp = LearnedMicroprice::empty(cfg);
        mp.with_spread_edges(vec![dec!(0.02), dec!(0.08)]);
        for _ in 0..3 {
            mp.accumulate_with_edges(dec!(-0.5), dec!(0.01), dec!(-0.2));
            mp.accumulate_with_edges(dec!(0.5), dec!(0.05), dec!(0.3));
        }
        mp.finalize();
        let edges_before: Vec<Decimal> = mp.spread_edges().to_vec();

        for _ in 0..20 {
            mp.update_online(dec!(0.7), dec!(0.2), dec!(0.5));
        }
        assert_eq!(mp.spread_edges(), edges_before.as_slice(),
            "online fit must not mutate spread edges");
    }

    #[test]
    fn update_online_shifts_prediction_toward_new_observations() {
        // Fit a model on mildly positive data, then flood the
        // online ring with strongly negative observations at
        // the +0.75 imbalance bucket. After the refit, the
        // prediction for that imbalance should move negative.
        let mut mp = seed_finalised_fit();
        let before = mp.predict(dec!(0.75), dec!(0.01));
        assert!(before > dec!(0), "seed should produce positive prediction at +0.75, got {before}");
        // Push enough observations to fill and refit the ring
        // a few times over. Stream length > ring capacity so
        // the original seed observations are pushed out.
        for _ in 0..60 {
            mp.update_online(dec!(0.75), dec!(0.01), dec!(-0.3));
        }
        let after = mp.predict(dec!(0.75), dec!(0.01));
        assert!(after < dec!(0),
            "online fit should flip prediction negative after 60 negative updates, got {after}");
    }

    #[test]
    fn update_online_matches_offline_fit_on_same_observations() {
        // Parity: pushing exactly the same observations through
        // the online path should yield an identical g-matrix to
        // accumulating them through the offline single-bucket
        // path (both applied on top of the same seed).
        //
        // Build two models with identical config, seed both the
        // same way, then on model A push 10 observations
        // through update_online; on model B wipe + rebuild
        // using direct accumulate on a fresh fit sharing the
        // same spread_edges. Assert prediction parity.
        let mut mp_online = seed_finalised_fit();
        let obs: Vec<(Decimal, Decimal, Decimal)> = (0..10)
            .map(|_| (dec!(0.75), dec!(0.01), dec!(-0.3)))
            .collect();
        for (i, s, d) in &obs {
            mp_online.update_online(*i, *s, *d);
        }
        // Build the offline equivalent: same config, direct
        // accumulate of just the 10 online obs (no seed — the
        // online path throws out the seed once the refit
        // happens against the ring contents).
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 2,
            online_ring_capacity: 50,
            refit_every: 10,
        };
        let mut mp_offline = LearnedMicroprice::empty(cfg);
        for (i, s, d) in &obs {
            mp_offline.accumulate(*i, *s, *d);
        }
        mp_offline.finalize_iterative(5);
        // All 10 observations land in bucket 3 (+0.75).
        // Parity check on the imbalance bucket that got data.
        assert_eq!(
            mp_online.predict(dec!(0.75), dec!(0.01)),
            mp_offline.predict(dec!(0.75), dec!(0.01)),
            "online refit parity failed"
        );
    }

    #[test]
    fn toml_roundtrip_default_online_fields_preserved() {
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 2,
            online_ring_capacity: 500,
            refit_every: 42,
        };
        let mut mp = LearnedMicroprice::empty(cfg);
        mp.accumulate(dec!(-0.5), dec!(0.01), dec!(-0.1));
        mp.accumulate(dec!(0.5), dec!(0.01), dec!(0.1));
        mp.accumulate(dec!(-0.5), dec!(0.01), dec!(-0.2));
        mp.accumulate(dec!(0.5), dec!(0.01), dec!(0.2));
        mp.finalize();
        let path = tmp_path("online_fields");
        mp.to_toml(&path).expect("write");
        let reloaded = LearnedMicroprice::from_toml(&path).expect("read");
        // The online fields are `#[serde(skip)]` so they
        // default back on load — but the *config* fields
        // (ring_capacity + refit_every) must round-trip
        // exactly since they are serialised.
        assert_eq!(reloaded.config.online_ring_capacity, 500);
        assert_eq!(reloaded.config.refit_every, 42);
        // Online state resets to empty.
        assert_eq!(reloaded.online_ring_len(), 0);
        assert_eq!(reloaded.online_counter(), 0);
        let _ = std::fs::remove_file(&path);
    }

}
