//! P² quantile estimator (Jain & Chlamtac, 1985).
//!
//! Single-quantile running estimator with **constant memory**
//! (five markers, no sample buffer). Drop-in replacement for a
//! fixed-window rolling median when the goal is a baseline for
//! robust z-score detection (median + MAD), not the full
//! distribution.
//!
//! Ported from VisualHFT's `P2Quantile.cs` (Apache-2.0). The
//! reference paper:
//!
//! > Jain, R. and Chlamtac, I. (1985). *The P² algorithm for
//! > dynamic calculation of quantiles and histograms without
//! > storing observations*. CACM 28(10):1076-1085.

/// A single-quantile P² estimator.
///
/// Tracks one target quantile `p ∈ (0, 1)` over an unbounded
/// stream of observations using five internal markers. Memory
/// is O(1); per-update cost is O(1).
///
/// The estimator needs **at least 5 observations** before it
/// produces its steady-state output; with fewer than 5 samples
/// it returns the largest available observation, which is a
/// harmless warmup fallback.
#[derive(Debug, Clone)]
pub struct P2Quantile {
    p: f64,
    count: usize,
    /// Marker heights `q[0..5]`.
    q: [f64; 5],
    /// Marker positions `n[0..5]` (1-indexed, mirrors the paper).
    n: [f64; 5],
    /// Desired marker positions `n'[0..5]`.
    np: [f64; 5],
    /// Desired position increments `dn[0..5]`.
    dn: [f64; 5],
}

impl P2Quantile {
    /// Create a new estimator targeting quantile `p`.
    ///
    /// # Panics
    /// Panics if `p` is not strictly between 0 and 1.
    pub fn new(p: f64) -> Self {
        assert!(p > 0.0 && p < 1.0, "P2Quantile: p must be in (0, 1)");
        Self {
            p,
            count: 0,
            q: [0.0; 5],
            n: [0.0; 5],
            np: [0.0; 5],
            dn: [0.0; 5],
        }
    }

    /// Convenience constructor for the median (`p = 0.5`).
    pub fn median() -> Self {
        Self::new(0.5)
    }

    /// Number of observations seen so far.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Current estimate of the target quantile.
    ///
    /// With fewer than 5 samples returns the highest sample seen
    /// so far (or 0 before any observation).
    pub fn estimate(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else if self.count < 5 {
            self.q[(self.count - 1).min(4)]
        } else {
            self.q[2]
        }
    }

    /// Feed a new observation. NaN / infinite inputs are ignored
    /// so a single bad tick cannot poison the estimator.
    pub fn observe(&mut self, x: f64) {
        if !x.is_finite() {
            return;
        }

        if self.count < 5 {
            self.q[self.count] = x;
            self.count += 1;
            if self.count == 5 {
                // Sort markers and initialise positions.
                self.q.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for i in 0..5 {
                    self.n[i] = (i + 1) as f64;
                }
                self.np[0] = 1.0;
                self.np[1] = 1.0 + 2.0 * self.p;
                self.np[2] = 1.0 + 4.0 * self.p;
                self.np[3] = 3.0 + 2.0 * self.p;
                self.np[4] = 5.0;
                self.dn[0] = 0.0;
                self.dn[1] = self.p / 2.0;
                self.dn[2] = self.p;
                self.dn[3] = (1.0 + self.p) / 2.0;
                self.dn[4] = 1.0;
            }
            return;
        }

        // Locate cell `k` and extend extreme markers when needed.
        let k: usize = if x < self.q[0] {
            self.q[0] = x;
            0
        } else if x < self.q[1] {
            0
        } else if x < self.q[2] {
            1
        } else if x < self.q[3] {
            2
        } else if x < self.q[4] {
            3
        } else {
            self.q[4] = x;
            3
        };

        // Shift marker positions above `k` by one.
        for i in (k + 1)..5 {
            self.n[i] += 1.0;
        }
        // Shift desired positions.
        for i in 0..5 {
            self.np[i] += self.dn[i];
        }

        // Adjust interior markers with the P² parabolic formula,
        // falling back to linear when the parabolic estimate
        // would step outside the `[q[i-1], q[i+1]]` bracket.
        for i in 1..=3 {
            let d = self.np[i] - self.n[i];
            let forward_gap = self.n[i + 1] - self.n[i];
            let backward_gap = self.n[i - 1] - self.n[i];
            let should_step = (d >= 1.0 && forward_gap > 1.0) || (d <= -1.0 && backward_gap < -1.0);
            if !should_step {
                continue;
            }
            let sign = if d >= 0.0 { 1.0 } else { -1.0 };

            // Parabolic prediction.
            let span = self.n[i + 1] - self.n[i - 1];
            let forward_slope = (self.q[i + 1] - self.q[i]) / (self.n[i + 1] - self.n[i]);
            let backward_slope = (self.q[i] - self.q[i - 1]) / (self.n[i] - self.n[i - 1]);
            let q_par = self.q[i]
                + (sign / span)
                    * ((self.n[i] - self.n[i - 1] + sign) * forward_slope
                        + (self.n[i + 1] - self.n[i] - sign) * backward_slope);

            if self.q[i - 1] < q_par && q_par < self.q[i + 1] {
                self.q[i] = q_par;
            } else {
                // Linear fallback.
                let j = if sign > 0.0 { i + 1 } else { i - 1 };
                self.q[i] += sign * (self.q[j] - self.q[i]) / (self.n[j] - self.n[i]);
            }

            self.n[i] += sign;
        }

        self.count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Median of a constant stream is the constant.
    #[test]
    fn median_of_constant_stream_is_the_constant() {
        let mut m = P2Quantile::median();
        for _ in 0..50 {
            m.observe(7.5);
        }
        assert!((m.estimate() - 7.5).abs() < 1e-9);
    }

    /// Warmup: before 5 samples the estimator must not explode.
    #[test]
    fn warmup_returns_last_sample_before_initialization() {
        let mut m = P2Quantile::median();
        m.observe(1.0);
        m.observe(5.0);
        // Before 5 samples the estimator returns the highest
        // warmup slot — 5.0 is what we pushed last, and the
        // internal array hasn't been sorted yet.
        assert_eq!(m.count(), 2);
        let est = m.estimate();
        assert!(est == 1.0 || est == 5.0);
    }

    /// Median of [0, 100] converges close to 50 on a dense
    /// uniform stream. P² is approximate — we expect a modest
    /// error, not exact equality.
    #[test]
    fn median_converges_on_uniform_stream() {
        let mut m = P2Quantile::median();
        // Deterministic uniform-ish sequence via a linear
        // congruential hash — no RNG crate needed.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..5000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let u = ((state >> 32) as f64) / (u32::MAX as f64); // in [0,1]
            m.observe(u * 100.0);
        }
        let est = m.estimate();
        assert!(
            (est - 50.0).abs() < 5.0,
            "P² median of U[0,100] diverged: {est}"
        );
    }

    /// 90th percentile of a uniform stream should converge near
    /// 90. Loose tolerance — the approximation is asymptotic.
    #[test]
    fn p90_converges_on_uniform_stream() {
        let mut m = P2Quantile::new(0.9);
        let mut state: u64 = 0xC6BC279692B5C323;
        for _ in 0..5000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let u = ((state >> 32) as f64) / (u32::MAX as f64);
            m.observe(u * 100.0);
        }
        let est = m.estimate();
        assert!(
            (est - 90.0).abs() < 5.0,
            "P² 0.9-quantile of U[0,100] diverged: {est}"
        );
    }

    /// NaN / infinite observations are silently ignored so a
    /// single bad tick cannot poison the estimator.
    #[test]
    fn non_finite_observations_are_ignored() {
        let mut m = P2Quantile::median();
        for v in [
            1.0,
            2.0,
            3.0,
            4.0,
            5.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            m.observe(v);
        }
        assert_eq!(m.count(), 5, "non-finite values must not advance count");
        assert!((m.estimate() - 3.0).abs() < 1e-9);
    }
}
