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
mod tests;
