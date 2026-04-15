//! Kalman filter for an adaptive hedge ratio (Epic B, sub-component #2).
//!
//! State-space:
//!
//! - state:        `β[t]` (scalar, the hedge ratio)
//! - transition:   `β[t] = β[t-1] + w[t]`, `w ~ N(0, Q)`
//! - observation:  `Y[t] = β[t] · X[t] + v[t]`, `v ~ N(0, R)`
//!
//! Per observation the filter runs predict + update:
//!
//! ```text
//! p_pred = P + Q
//! e      = Y - β · X
//! s      = X² · p_pred + R
//! K      = X · p_pred / s
//! β_new  = β + K · e
//! P_new  = (1 - K · X) · p_pred
//! ```
//!
//! Full derivation and default Q/R picks in
//! `docs/research/stat-arb-pairs-formulas.md` §"Sub-component #2".

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Recursive-least-squares Kalman filter for a single scalar
/// hedge ratio. All math is `Decimal` — no `f64`.
#[derive(Debug, Clone)]
pub struct KalmanHedgeRatio {
    beta: Decimal,
    variance: Decimal,
    transition_var: Decimal,
    observation_var: Decimal,
}

impl KalmanHedgeRatio {
    /// Construct with a neutral prior (`β = 1`, `P = 1`) and the
    /// caller-supplied transition / observation noise variances.
    ///
    /// Typical crypto-pair defaults: `Q = 1e-6`, `R = 1e-3`.
    pub fn new(transition_var: Decimal, observation_var: Decimal) -> Self {
        Self {
            beta: dec!(1),
            variance: dec!(1),
            transition_var,
            observation_var,
        }
    }

    /// Construct with a caller-supplied initial β — typically the
    /// Engle-Granger OLS β from the most recent cointegration
    /// check, giving the filter a warm start instead of the
    /// neutral `β = 1`.
    pub fn with_initial_beta(
        beta: Decimal,
        transition_var: Decimal,
        observation_var: Decimal,
    ) -> Self {
        Self {
            beta,
            variance: dec!(1),
            transition_var,
            observation_var,
        }
    }

    /// Fold one new observation `(Y, X)` into the state. Returns
    /// the updated `β`.
    ///
    /// Degenerate guard: if the innovation variance `S = X²·P + R`
    /// is zero (possible only if both `X = 0` AND `R = 0` AND the
    /// prior `P = 0`), the update is a no-op and the current `β`
    /// is returned unchanged.
    pub fn update(&mut self, y: Decimal, x: Decimal) -> Decimal {
        let p_pred = self.variance + self.transition_var;
        let innovation = y - self.beta * x;
        let s = x * x * p_pred + self.observation_var;
        if s.is_zero() {
            return self.beta;
        }
        let k = x * p_pred / s;
        self.beta += k * innovation;
        self.variance = (Decimal::ONE - k * x) * p_pred;
        self.beta
    }

    /// Current β estimate.
    pub fn current_beta(&self) -> Decimal {
        self.beta
    }

    /// Current posterior variance of β. Operators watching this
    /// gauge: high values mean the filter is uncertain and the
    /// driver should widen entry thresholds or hold off.
    pub fn current_variance(&self) -> Decimal {
        self.variance
    }

    /// Transition noise `Q` (read-only accessor for metrics /
    /// snapshotting).
    pub fn transition_var(&self) -> Decimal {
        self.transition_var
    }

    /// Observation noise `R` (read-only accessor).
    pub fn observation_var(&self) -> Decimal {
        self.observation_var
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_filter() -> KalmanHedgeRatio {
        KalmanHedgeRatio::new(dec!(0.000001), dec!(0.001))
    }

    #[test]
    fn new_sets_neutral_prior() {
        let f = default_filter();
        assert_eq!(f.current_beta(), dec!(1));
        assert_eq!(f.current_variance(), dec!(1));
        assert_eq!(f.transition_var(), dec!(0.000001));
        assert_eq!(f.observation_var(), dec!(0.001));
    }

    #[test]
    fn with_initial_beta_seeds_correctly() {
        let f = KalmanHedgeRatio::with_initial_beta(dec!(2.5), dec!(0.000001), dec!(0.001));
        assert_eq!(f.current_beta(), dec!(2.5));
        assert_eq!(f.current_variance(), dec!(1));
    }

    #[test]
    fn stationary_pair_converges_to_true_beta() {
        // Y[t] = 2.0 · X[t] — deterministic, no noise at all.
        let mut f = default_filter();
        for i in 1..=200 {
            let x = Decimal::from(i) / dec!(10);
            let y = dec!(2) * x;
            f.update(y, x);
        }
        let diff = (f.current_beta() - dec!(2)).abs();
        assert!(
            diff < dec!(0.01),
            "expected β≈2.0, got {} (|diff|={})",
            f.current_beta(),
            diff
        );
    }

    #[test]
    fn regime_shift_is_tracked() {
        let mut f = KalmanHedgeRatio::new(dec!(0.01), dec!(0.001));
        // Phase 1: β = 1.5.
        for i in 1..=100 {
            let x = Decimal::from(i);
            let y = dec!(1.5) * x;
            f.update(y, x);
        }
        let phase1_beta = f.current_beta();
        assert!((phase1_beta - dec!(1.5)).abs() < dec!(0.05));

        // Phase 2: β jumps to 3.0.
        for i in 101..=300 {
            let x = Decimal::from(i);
            let y = dec!(3) * x;
            f.update(y, x);
        }
        let phase2_beta = f.current_beta();
        assert!(
            (phase2_beta - dec!(3)).abs() < dec!(0.1),
            "regime shift not tracked: β={} after phase 2",
            phase2_beta
        );
    }

    #[test]
    fn tiny_q_adapts_slower_than_large_q() {
        // Relative invariant: under the SAME regime-shift
        // sequence, a filter with small Q must drift less than
        // one with large Q. This captures the "tiny Q suppresses
        // chasing" intent without baking in an arbitrary
        // absolute threshold.
        fn regime_shift_drift(q: Decimal, r: Decimal) -> Decimal {
            let mut f = KalmanHedgeRatio::with_initial_beta(dec!(1), q, r);
            // Phase 1: stationary β=1, xs in {1,2,3}.
            for i in 0..200 {
                let x = Decimal::from((i % 3) + 1);
                f.update(x, x);
            }
            let baseline = f.current_beta();
            // Phase 2: regime jumps to β=5.
            for i in 0..100 {
                let x = Decimal::from((i % 3) + 1);
                f.update(dec!(5) * x, x);
            }
            (f.current_beta() - baseline).abs()
        }
        let tiny_q = regime_shift_drift(dec!(0.000000001), dec!(0.001));
        let large_q = regime_shift_drift(dec!(0.1), dec!(0.001));
        assert!(
            tiny_q < large_q,
            "tiny-Q drift {tiny_q} should be strictly less than large-Q drift {large_q}",
        );
    }

    #[test]
    fn large_q_chases_noise() {
        // Q = 1.0 means the filter trusts every new observation.
        let mut f = KalmanHedgeRatio::new(dec!(1), dec!(0.001));
        let observations = [
            (dec!(1), dec!(1)),  // β implied = 1
            (dec!(5), dec!(1)),  // β implied = 5
            (dec!(10), dec!(1)), // β implied = 10
            (dec!(-3), dec!(1)), // β implied = -3
        ];
        let mut betas = Vec::new();
        for (y, x) in observations {
            betas.push(f.update(y, x));
        }
        // Consecutive betas should differ meaningfully — the filter
        // is NOT smoothing heavily.
        let spread: Decimal = betas.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        assert!(
            spread > dec!(5),
            "expected large drift, saw spread={spread}"
        );
    }

    #[test]
    fn x_zero_degenerate_guard_holds_beta() {
        // Handcrafted: R = 0 AND initial P = 0 via a cold-start
        // hack so that s = 0·p_pred + 0 = 0 and the guard fires.
        let mut f = KalmanHedgeRatio::new(dec!(0), dec!(0));
        f.variance = dec!(0); // force P = 0
        let before = f.current_beta();
        let after = f.update(dec!(42), dec!(0));
        assert_eq!(before, after, "degenerate s=0 should no-op");
    }

    #[test]
    fn variance_decreases_on_stationary_pair() {
        let mut f = default_filter();
        let mut prev_var = f.current_variance();
        let mut strictly_decreased_at_least_once = false;
        for i in 1..=50 {
            let x = Decimal::from(i);
            let y = dec!(2) * x;
            f.update(y, x);
            let now = f.current_variance();
            if now < prev_var {
                strictly_decreased_at_least_once = true;
            }
            assert!(now <= prev_var + dec!(0.0001), "variance increased");
            prev_var = now;
        }
        assert!(strictly_decreased_at_least_once);
    }

    #[test]
    fn accessors_return_latest_state() {
        let mut f = default_filter();
        f.update(dec!(4), dec!(2));
        assert_eq!(f.current_beta(), f.beta);
        assert_eq!(f.current_variance(), f.variance);
    }

    #[test]
    fn repeated_identical_updates_shrink_variance() {
        let mut f = default_filter();
        let first = f.update(dec!(3), dec!(1.5));
        let var1 = f.current_variance();
        let second = f.update(dec!(3), dec!(1.5));
        let var2 = f.current_variance();
        // Same observation applied twice: β should be very close
        // on the second update and variance should have shrunk.
        assert!((second - first).abs() < dec!(0.1));
        assert!(var2 < var1);
    }

    #[test]
    fn initial_beta_warm_start_matches_ols_seed() {
        // Real use case: Engle-Granger returns β=2.0 as the OLS
        // seed. The Kalman should accept the seed and then refine.
        let mut f = KalmanHedgeRatio::with_initial_beta(dec!(2), dec!(0.000001), dec!(0.001));
        assert_eq!(f.current_beta(), dec!(2));
        // Feed a consistent stream — β should not move meaningfully.
        for i in 1..=100 {
            let x = Decimal::from(i);
            let y = dec!(2) * x;
            f.update(y, x);
        }
        assert!((f.current_beta() - dec!(2)).abs() < dec!(0.01));
    }
}
