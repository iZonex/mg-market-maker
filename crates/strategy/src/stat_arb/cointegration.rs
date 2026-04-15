//! Engle-Granger 2-leg cointegration test (Epic B, sub-component #1).
//!
//! Pipeline:
//!
//! 1. OLS regression `Y ~ α + β · X` on the synchronised price
//!    pair.
//! 2. Residual series `ε[t] = Y[t] − α − β · X[t]`.
//! 3. Basic ADF regression on the residuals without constant or
//!    lag terms: `Δε[t] = ρ · ε[t-1] + u[t]`. The ADF statistic
//!    is `ρ_hat / SE(ρ_hat)`.
//! 4. Compare against MacKinnon 5% critical values for a
//!    two-variable cointegration regression.
//!
//! Pure function — no IO, no async, no mutation of inputs, and
//! no allocation beyond a single `Vec<Decimal>` for the
//! residuals.
//!
//! Full formula derivation + source attribution
//! (Engle-Granger 1987, Cartea-Jaimungal-Penalva ch.11,
//! MacKinnon 1991 table 6.1) lives in
//! `docs/research/stat-arb-pairs-formulas.md`.

use crate::volatility::decimal_sqrt;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Minimum sample size accepted by the test. Below this the
/// MacKinnon table is too coarse and OLS regression degrees of
/// freedom are too thin for a defensible decision.
pub const MIN_SAMPLES_FOR_TEST: usize = 25;

/// Outcome of a single Engle-Granger run. A `None` return from
/// [`EngleGrangerTest::run`] means the inputs were too small /
/// malformed / degenerate — callers treat that as "undecidable,
/// skip the cointegration gate this tick".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CointegrationResult {
    /// `true` if the ADF statistic is strictly less than the 5%
    /// MacKinnon critical value.
    pub is_cointegrated: bool,
    /// OLS hedge ratio β.
    pub beta: Decimal,
    /// OLS intercept α.
    pub alpha: Decimal,
    /// ADF t-statistic on `ρ`. More negative means stronger
    /// rejection of the unit-root null.
    pub adf_statistic: Decimal,
    /// MacKinnon 5% critical value at the tested sample size.
    pub critical_value_5pct: Decimal,
    /// Sample size used.
    pub sample_size: usize,
}

/// Zero-state marker type; the test is a pure function.
pub struct EngleGrangerTest;

impl EngleGrangerTest {
    /// Run the test on two price series. Inputs must be the same
    /// length and ≥ [`MIN_SAMPLES_FOR_TEST`]. Degenerate inputs
    /// (`var(X) = 0` or zero-variance residuals) return `None`.
    pub fn run(y: &[Decimal], x: &[Decimal]) -> Option<CointegrationResult> {
        if y.len() != x.len() || y.len() < MIN_SAMPLES_FOR_TEST {
            return None;
        }
        let n = y.len();
        let (alpha, beta) = ols_2d(y, x)?;
        let residuals: Vec<Decimal> = y
            .iter()
            .zip(x.iter())
            .map(|(yi, xi)| *yi - alpha - beta * *xi)
            .collect();
        let adf_stat = adf_basic_stat(&residuals)?;
        let crit = mackinnon_5pct_critical_value(n);
        Some(CointegrationResult {
            is_cointegrated: adf_stat < crit,
            beta,
            alpha,
            adf_statistic: adf_stat,
            critical_value_5pct: crit,
            sample_size: n,
        })
    }
}

/// OLS for `Y = α + β · X`. Returns `None` if `var(X) = 0`
/// (vertical or constant predictor — β is undefined).
fn ols_2d(y: &[Decimal], x: &[Decimal]) -> Option<(Decimal, Decimal)> {
    let n = Decimal::from(y.len());
    let mean_y: Decimal = y.iter().copied().sum::<Decimal>() / n;
    let mean_x: Decimal = x.iter().copied().sum::<Decimal>() / n;
    let mut cov_xy = Decimal::ZERO;
    let mut var_x = Decimal::ZERO;
    for (yi, xi) in y.iter().zip(x.iter()) {
        let dx = *xi - mean_x;
        let dy = *yi - mean_y;
        cov_xy += dx * dy;
        var_x += dx * dx;
    }
    if var_x.is_zero() {
        return None;
    }
    let beta = cov_xy / var_x;
    let alpha = mean_y - beta * mean_x;
    Some((alpha, beta))
}

/// Basic ADF test statistic — regression `Δε[t] = ρ · ε[t-1] + u[t]`
/// with no constant and no lags. Returns `None` if the residual
/// series is degenerate.
fn adf_basic_stat(residuals: &[Decimal]) -> Option<Decimal> {
    if residuals.len() < 3 {
        return None;
    }
    // Build lagged series and first differences.
    let lagged = &residuals[..residuals.len() - 1]; // ε[t-1]
    let diffs: Vec<Decimal> = residuals
        .windows(2)
        .map(|w| w[1] - w[0]) // Δε[t]
        .collect();
    debug_assert_eq!(lagged.len(), diffs.len());
    let m = lagged.len();

    // OLS of Δε on ε[t-1] without intercept.
    //   ρ_hat = Σ(lag · diff) / Σ(lag²)
    let mut sum_lag_diff = Decimal::ZERO;
    let mut sum_lag_sq = Decimal::ZERO;
    for (l, d) in lagged.iter().zip(diffs.iter()) {
        sum_lag_diff += *l * *d;
        sum_lag_sq += *l * *l;
    }
    if sum_lag_sq.is_zero() {
        return None;
    }
    let rho = sum_lag_diff / sum_lag_sq;

    // Residuals of the ADF regression: u_hat[t] = Δε[t] − ρ · ε[t-1]
    let ssr: Decimal = lagged
        .iter()
        .zip(diffs.iter())
        .map(|(l, d)| {
            let u = *d - rho * *l;
            u * u
        })
        .sum();

    // Residual degrees of freedom: m observations, 1 parameter.
    let df = m.saturating_sub(1);
    if df == 0 {
        return None;
    }
    let sigma_sq = ssr / Decimal::from(df);
    let var_rho = sigma_sq / sum_lag_sq;
    if var_rho <= Decimal::ZERO {
        return None;
    }
    let se_rho = decimal_sqrt(var_rho);
    if se_rho.is_zero() {
        return None;
    }
    Some(rho / se_rho)
}

/// MacKinnon 1991 Table 6.1 — 5% critical values for an
/// Engle-Granger cointegration test on two variables. v1 uses a
/// hard-coded lookup table with linear interpolation between
/// adjacent entries and clamping at the extremes. Stage-2 can
/// refine with MacKinnon's full polynomial fit if operators
/// demand it.
pub fn mackinnon_5pct_critical_value(n: usize) -> Decimal {
    const TABLE: &[(usize, Decimal)] = &[
        (25, dec!(-3.67)),
        (50, dec!(-3.50)),
        (100, dec!(-3.42)),
        (250, dec!(-3.37)),
        (500, dec!(-3.36)),
    ];
    if n <= TABLE[0].0 {
        return TABLE[0].1;
    }
    if n >= TABLE[TABLE.len() - 1].0 {
        return TABLE[TABLE.len() - 1].1;
    }
    // Find the bracketing entries and linearly interpolate.
    for window in TABLE.windows(2) {
        let (lo_n, lo_v) = window[0];
        let (hi_n, hi_v) = window[1];
        if n >= lo_n && n <= hi_n {
            let span = Decimal::from(hi_n - lo_n);
            let frac = Decimal::from(n - lo_n) / span;
            return lo_v + (hi_v - lo_v) * frac;
        }
    }
    TABLE[TABLE.len() - 1].1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal deterministic LCG (`glibc`-style) for test
    /// innovations — avoids pulling `rand` in as a dev-dep.
    fn lcg_innovations(seed: u64, n: usize, range: i64) -> Vec<Decimal> {
        let mut s = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            let v = ((s >> 16) & 0x7fff) as i64;
            out.push(Decimal::from(v % (2 * range + 1) - range));
        }
        out
    }

    /// Deterministic cointegrated pair: `X` is a random-walk
    /// stand-in with pseudo-random innovations, `Y = 2·X + ε`
    /// with a *separately-seeded* mean-reverting residual that is
    /// NOT phase-aligned with `X`. The ε term has enough richness
    /// to keep the ADF regression non-degenerate.
    fn cointegrated_pair(n: usize) -> (Vec<Decimal>, Vec<Decimal>) {
        let x_innov = lcg_innovations(1_234_567, n, 3);
        let eps_raw = lcg_innovations(9_876_543, n, 5);
        let mut x = Vec::with_capacity(n);
        let mut y = Vec::with_capacity(n);
        let mut x_val = dec!(100);
        for i in 0..n {
            x_val += x_innov[i];
            x.push(x_val);
            // ε is mean-reverting by construction: `eps_raw`
            // values are independent draws so the residual
            // behaves like white noise — stationary.
            let eps = eps_raw[i] / dec!(10);
            y.push(dec!(2) * x_val + eps);
        }
        (y, x)
    }

    /// Two independent driftless random walks. Residuals from
    /// `Y = α + β · X` accumulate unbounded — should NOT be
    /// flagged cointegrated.
    fn independent_walks(n: usize) -> (Vec<Decimal>, Vec<Decimal>) {
        let x_innov = lcg_innovations(111_111, n, 3);
        let y_innov = lcg_innovations(222_222, n, 3);
        let mut x = Vec::with_capacity(n);
        let mut y = Vec::with_capacity(n);
        let mut x_val = dec!(100);
        let mut y_val = dec!(50);
        for i in 0..n {
            x_val += x_innov[i];
            y_val += y_innov[i];
            x.push(x_val);
            y.push(y_val);
        }
        (y, x)
    }

    #[test]
    fn mismatched_lengths_return_none() {
        let y = vec![dec!(1); 30];
        let x = vec![dec!(1); 29];
        assert!(EngleGrangerTest::run(&y, &x).is_none());
    }

    #[test]
    fn too_few_samples_returns_none() {
        let y = vec![dec!(1); 10];
        let x = vec![dec!(1); 10];
        assert!(EngleGrangerTest::run(&y, &x).is_none());
    }

    #[test]
    fn constant_x_is_degenerate() {
        let y: Vec<Decimal> = (0..30).map(Decimal::from).collect();
        let x = vec![dec!(42); 30];
        assert!(EngleGrangerTest::run(&y, &x).is_none());
    }

    #[test]
    fn cointegrated_pair_is_flagged_cointegrated() {
        let (y, x) = cointegrated_pair(100);
        let result = EngleGrangerTest::run(&y, &x).expect("should return result");
        assert!(
            result.is_cointegrated,
            "ADF={} crit={} — expected cointegration",
            result.adf_statistic, result.critical_value_5pct
        );
    }

    #[test]
    fn cointegrated_pair_recovers_beta() {
        let (y, x) = cointegrated_pair(200);
        let result = EngleGrangerTest::run(&y, &x).unwrap();
        assert!(
            (result.beta - dec!(2)).abs() < dec!(0.05),
            "β={} vs 2.0",
            result.beta
        );
    }

    #[test]
    fn independent_walks_are_not_cointegrated() {
        let (y, x) = independent_walks(100);
        let result = EngleGrangerTest::run(&y, &x).unwrap();
        assert!(
            !result.is_cointegrated,
            "random walks flagged as cointegrated: ADF={} crit={}",
            result.adf_statistic, result.critical_value_5pct
        );
    }

    #[test]
    fn adf_stat_is_finite_and_negative_on_mean_reverting_residuals() {
        let (y, x) = cointegrated_pair(100);
        let result = EngleGrangerTest::run(&y, &x).unwrap();
        assert!(result.adf_statistic < Decimal::ZERO);
    }

    #[test]
    fn mackinnon_table_clamps_below_25() {
        assert_eq!(mackinnon_5pct_critical_value(10), dec!(-3.67));
        assert_eq!(mackinnon_5pct_critical_value(25), dec!(-3.67));
    }

    #[test]
    fn mackinnon_table_clamps_above_500() {
        assert_eq!(mackinnon_5pct_critical_value(1000), dec!(-3.36));
    }

    #[test]
    fn mackinnon_interpolation_at_75() {
        // Between 50 (-3.50) and 100 (-3.42). At n=75 the
        // interpolated value is -3.46.
        let v = mackinnon_5pct_critical_value(75);
        assert_eq!(v, dec!(-3.46));
    }

    #[test]
    fn mackinnon_interpolation_at_175() {
        // Between 100 (-3.42) and 250 (-3.37). At n=175 the
        // interpolated value is -3.395.
        let v = mackinnon_5pct_critical_value(175);
        assert_eq!(v, dec!(-3.395));
    }

    #[test]
    fn mackinnon_table_exact_entries_match() {
        assert_eq!(mackinnon_5pct_critical_value(50), dec!(-3.50));
        assert_eq!(mackinnon_5pct_critical_value(100), dec!(-3.42));
        assert_eq!(mackinnon_5pct_critical_value(250), dec!(-3.37));
        assert_eq!(mackinnon_5pct_critical_value(500), dec!(-3.36));
    }

    #[test]
    fn result_is_deterministic_across_repeated_calls() {
        let (y, x) = cointegrated_pair(80);
        let a = EngleGrangerTest::run(&y, &x).unwrap();
        let b = EngleGrangerTest::run(&y, &x).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn sample_size_reported_in_result() {
        let (y, x) = cointegrated_pair(60);
        let result = EngleGrangerTest::run(&y, &x).unwrap();
        assert_eq!(result.sample_size, 60);
    }
}
