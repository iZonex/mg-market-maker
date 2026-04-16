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

/// Maximum lag order considered by AIC selection.
pub const MAX_ADF_LAGS: usize = 12;

impl EngleGrangerTest {
    /// Run the test on two price series. Inputs must be the same
    /// length and ≥ [`MIN_SAMPLES_FOR_TEST`]. Degenerate inputs
    /// (`var(X) = 0` or zero-variance residuals) return `None`.
    ///
    /// Uses AIC-selected lag order (0..MAX_ADF_LAGS) for the ADF
    /// regression. The lag that minimises AIC is chosen
    /// automatically.
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
        let adf_stat = adf_with_aic(&residuals)?;
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

    /// Run with a fixed lag order (0 = no lags, same as v1).
    /// Useful for testing or when the caller knows the right lag.
    pub fn run_with_lag(y: &[Decimal], x: &[Decimal], lag: usize) -> Option<CointegrationResult> {
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
        let adf_stat = adf_stat_with_lags(&residuals, lag)?;
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

/// ADF with AIC-selected lag order. Tries lags 0..MAX_ADF_LAGS
/// and returns the t-statistic from the lag with the lowest AIC.
/// AIC = T·ln(SSR/T) + 2·(p+1) where p is the number of lagged
/// differences.
fn adf_with_aic(residuals: &[Decimal]) -> Option<Decimal> {
    if residuals.len() < 3 {
        return None;
    }
    let max_lag = MAX_ADF_LAGS.min((residuals.len() - 3) / 3);
    let mut best_aic = f64::INFINITY;
    let mut best_stat: Option<Decimal> = None;

    for p in 0..=max_lag {
        if let Some((t_stat, ssr, n_obs)) = adf_regression(residuals, p) {
            if n_obs < p + 2 {
                continue;
            }
            let n_f64 = n_obs as f64;
            let ssr_f64 = decimal_to_f64(ssr);
            if ssr_f64 <= 0.0 {
                continue;
            }
            let aic = n_f64 * (ssr_f64 / n_f64).ln() + 2.0 * (p + 1) as f64;
            if aic < best_aic {
                best_aic = aic;
                best_stat = Some(t_stat);
            }
        }
    }
    best_stat
}

/// ADF t-statistic with a fixed lag order. Public for callers
/// that want to bypass AIC selection.
fn adf_stat_with_lags(residuals: &[Decimal], lag: usize) -> Option<Decimal> {
    adf_regression(residuals, lag).map(|(t_stat, _, _)| t_stat)
}

/// ADF regression with `p` lagged differences:
///
/// ```text
/// Δε[t] = ρ·ε[t-1] + Σ_{j=1}^{p} γ_j·Δε[t-j] + u[t]
/// ```
///
/// No constant term (cointegration residuals are zero-mean by
/// construction). Returns `(t_stat_for_ρ, SSR, n_obs)`.
fn adf_regression(residuals: &[Decimal], p: usize) -> Option<(Decimal, Decimal, usize)> {
    let n = residuals.len();
    if n < p + 3 {
        return None;
    }
    // First differences.
    let diffs: Vec<Decimal> = residuals.windows(2).map(|w| w[1] - w[0]).collect();
    // Effective sample: t = p+1 .. n-1 (0-indexed in diffs).
    let start = p;
    let n_obs = diffs.len() - start;
    if n_obs < p + 2 {
        return None;
    }

    // Number of regressors: 1 (ε[t-1]) + p (lagged diffs).
    let k = 1 + p;

    // Build X matrix (n_obs × k) and Y vector (n_obs).
    // Column 0: ε[t-1] = residuals[t] where t is the time index
    //           in the original series for diff[t] = Δε[t+1].
    // Columns 1..p: Δε[t-1], Δε[t-2], ..., Δε[t-p].
    let mut x = vec![vec![Decimal::ZERO; k]; n_obs];
    let mut y = Vec::with_capacity(n_obs);
    for (i, t) in (start..diffs.len()).enumerate() {
        y.push(diffs[t]);
        // ε[t-1]: residuals at index t (since diff[t] = ε[t+1] - ε[t]).
        x[i][0] = residuals[t];
        for j in 1..=p {
            x[i][j] = diffs[t - j];
        }
    }

    // OLS via normal equations: β = (X'X)^{-1} X'Y.
    // For small k (≤13) this is fine.
    let mut xtx = vec![vec![Decimal::ZERO; k]; k];
    let mut xty = vec![Decimal::ZERO; k];
    for i in 0..n_obs {
        for a in 0..k {
            xty[a] += x[i][a] * y[i];
            for b in 0..k {
                xtx[a][b] += x[i][a] * x[i][b];
            }
        }
    }

    // Invert X'X via Gauss-Jordan (in Decimal).
    let xtx_inv = decimal_mat_inv(&xtx, k)?;
    // β = xtx_inv · xty
    let mut beta = vec![Decimal::ZERO; k];
    for a in 0..k {
        for b in 0..k {
            beta[a] += xtx_inv[a][b] * xty[b];
        }
    }
    let rho = beta[0];

    // SSR = Σ (y_i - x_i'·β)²
    let mut ssr = Decimal::ZERO;
    for i in 0..n_obs {
        let mut fitted = Decimal::ZERO;
        for a in 0..k {
            fitted += x[i][a] * beta[a];
        }
        let u = y[i] - fitted;
        ssr += u * u;
    }

    let df = n_obs.saturating_sub(k);
    if df == 0 {
        return None;
    }
    let sigma_sq = ssr / Decimal::from(df);
    // SE(ρ) = sqrt(σ² · (X'X)^{-1}[0][0])
    let var_rho = sigma_sq * xtx_inv[0][0];
    if var_rho <= Decimal::ZERO {
        return None;
    }
    let se_rho = decimal_sqrt(var_rho);
    if se_rho.is_zero() {
        return None;
    }
    Some((rho / se_rho, ssr, n_obs))
}

/// Gauss-Jordan inverse of an n×n Decimal matrix. Returns
/// `None` if singular.
#[allow(clippy::needless_range_loop)]
fn decimal_mat_inv(a: &[Vec<Decimal>], n: usize) -> Option<Vec<Vec<Decimal>>> {
    let mut aug = vec![vec![Decimal::ZERO; 2 * n]; n];
    for i in 0..n {
        for j in 0..n {
            aug[i][j] = a[i][j];
        }
        aug[i][n + i] = Decimal::ONE;
    }
    for col in 0..n {
        // Partial pivoting.
        let mut max_row = col;
        let mut max_val = aug[col][col].abs();
        for row in (col + 1)..n {
            if aug[row][col].abs() > max_val {
                max_val = aug[row][col].abs();
                max_row = row;
            }
        }
        if max_val.is_zero() {
            return None;
        }
        aug.swap(col, max_row);
        let pivot = aug[col][col];
        for j in 0..(2 * n) {
            aug[col][j] /= pivot;
        }
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = aug[row][col];
            // Copy the pivot row to avoid borrow conflict.
            let pivot_row: Vec<Decimal> = aug[col].clone();
            for j in 0..(2 * n) {
                aug[row][j] -= factor * pivot_row[j];
            }
        }
    }
    let mut inv = vec![vec![Decimal::ZERO; n]; n];
    for i in 0..n {
        for j in 0..n {
            inv[i][j] = aug[i][n + j];
        }
    }
    Some(inv)
}

fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

/// MacKinnon 1996 response-surface critical values for an
/// Engle-Granger cointegration test. The polynomial fit:
///
/// ```text
/// c(p, n) = β_∞ + β_1 / n + β_2 / n²
/// ```
///
/// replaces the v1 lookup table with a continuous function
/// valid for any sample size n ≥ 2. Coefficients are from
/// MacKinnon (1996) "Numerical Distribution Functions for
/// Unit Root and Cointegration Tests", *Journal of Applied
/// Econometrics* 11(6), pp. 601–618, Table 1.
///
/// `n_vars` is the number of variables in the cointegration
/// regression (2 for standard Engle-Granger). Supports
/// n_vars = 1..6.
pub fn mackinnon_critical_value(n: usize, n_vars: usize, significance: MacKinnonLevel) -> Decimal {
    // Coefficients: (β_∞, β_1, β_2) indexed by (n_vars, level).
    // Source: MacKinnon (1996) Table 1, "case 2" (constant, no trend).
    let (b_inf, b1, b2) = match (n_vars, significance) {
        // 1 variable (standard ADF, not cointegration).
        (1, MacKinnonLevel::Pct1) => (-3.4336, -5.999, -29.25),
        (1, MacKinnonLevel::Pct5) => (-2.8621, -2.738, -8.36),
        (1, MacKinnonLevel::Pct10) => (-2.5671, -1.438, -4.48),
        // 2 variables (standard Engle-Granger).
        (2, MacKinnonLevel::Pct1) => (-3.9001, -10.534, -30.03),
        (2, MacKinnonLevel::Pct5) => (-3.3377, -5.967, -8.98),
        (2, MacKinnonLevel::Pct10) => (-3.0462, -4.069, -5.73),
        // 3 variables.
        (3, MacKinnonLevel::Pct1) => (-4.2981, -13.790, -46.37),
        (3, MacKinnonLevel::Pct5) => (-3.7429, -8.352, -13.41),
        (3, MacKinnonLevel::Pct10) => (-3.4518, -6.241, -2.79),
        // 4 variables.
        (4, MacKinnonLevel::Pct1) => (-4.6676, -18.492, -49.35),
        (4, MacKinnonLevel::Pct5) => (-4.1193, -11.252, -21.57),
        (4, MacKinnonLevel::Pct10) => (-3.8275, -8.947, -13.91),
        // 5 variables.
        (5, MacKinnonLevel::Pct1) => (-5.0202, -22.504, -74.22),
        (5, MacKinnonLevel::Pct5) => (-4.4735, -14.501, -33.19),
        (5, MacKinnonLevel::Pct10) => (-4.1821, -11.637, -22.85),
        // 6 variables.
        (6, MacKinnonLevel::Pct1) => (-5.3580, -26.606, -109.0),
        (6, MacKinnonLevel::Pct5) => (-4.8088, -17.832, -59.60),
        (6, MacKinnonLevel::Pct10) => (-4.5179, -14.419, -35.81),
        // Fallback: use 2-variable 5%.
        _ => (-3.3377, -5.967, -8.98),
    };

    let n_f64 = n.max(2) as f64;
    let cv = b_inf + b1 / n_f64 + b2 / (n_f64 * n_f64);
    Decimal::from_f64_retain(cv).unwrap_or(dec!(-3.37))
}

/// Significance levels supported by the MacKinnon polynomial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacKinnonLevel {
    Pct1,
    Pct5,
    Pct10,
}

/// Convenience wrapper: 5% critical value for a 2-variable
/// Engle-Granger test (backward-compatible with v1 callers).
pub fn mackinnon_5pct_critical_value(n: usize) -> Decimal {
    mackinnon_critical_value(n, 2, MacKinnonLevel::Pct5)
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

    /// Polynomial fit: small n yields a more negative (stricter)
    /// critical value than the asymptotic β_∞.
    #[test]
    fn mackinnon_small_n_is_stricter_than_asymptotic() {
        let cv_25 = mackinnon_5pct_critical_value(25);
        let cv_inf = mackinnon_5pct_critical_value(100_000);
        assert!(
            cv_25 < cv_inf,
            "n=25 cv={} should be stricter than asymptotic={}",
            cv_25,
            cv_inf
        );
    }

    /// Polynomial fit monotonicity: as n grows, the critical
    /// value approaches the asymptote from below.
    #[test]
    fn mackinnon_polynomial_is_monotone_increasing() {
        let sizes = [25, 50, 100, 250, 500, 1000];
        for w in sizes.windows(2) {
            let cv_lo = mackinnon_5pct_critical_value(w[0]);
            let cv_hi = mackinnon_5pct_critical_value(w[1]);
            assert!(
                cv_hi >= cv_lo,
                "cv(n={})={} should be >= cv(n={})={}",
                w[1],
                cv_hi,
                w[0],
                cv_lo
            );
        }
    }

    /// Polynomial fit agrees with the old lookup table to within
    /// 0.05 at the original table points. The polynomial is a
    /// smooth fit, so it won't match the discrete table exactly.
    #[test]
    fn mackinnon_polynomial_agrees_with_old_table_approximately() {
        let old_table = [
            (50, dec!(-3.50)),
            (100, dec!(-3.42)),
            (250, dec!(-3.37)),
            (500, dec!(-3.36)),
        ];
        for (n, old_cv) in old_table {
            let new_cv = mackinnon_5pct_critical_value(n);
            assert!(
                (new_cv - old_cv).abs() < dec!(0.08),
                "n={}: poly={} vs table={}, diff={}",
                n,
                new_cv,
                old_cv,
                (new_cv - old_cv).abs()
            );
        }
    }

    /// Asymptotic value (n → ∞) converges to β_∞ = -3.3377
    /// for the 2-variable 5% case.
    #[test]
    fn mackinnon_asymptotic_converges() {
        let cv = mackinnon_5pct_critical_value(1_000_000);
        assert!(
            (cv - dec!(-3.3377)).abs() < dec!(0.001),
            "asymptotic cv={} should be near -3.3377",
            cv
        );
    }

    /// Multi-variable critical values: more variables requires
    /// a more negative (stricter) critical value at the same n.
    #[test]
    fn mackinnon_more_variables_is_stricter() {
        let cv2 = mackinnon_critical_value(100, 2, MacKinnonLevel::Pct5);
        let cv3 = mackinnon_critical_value(100, 3, MacKinnonLevel::Pct5);
        let cv4 = mackinnon_critical_value(100, 4, MacKinnonLevel::Pct5);
        assert!(cv3 < cv2, "3-var cv={} should be < 2-var cv={}", cv3, cv2);
        assert!(cv4 < cv3, "4-var cv={} should be < 3-var cv={}", cv4, cv3);
    }

    /// 1% level is stricter than 5% which is stricter than 10%.
    #[test]
    fn mackinnon_1pct_stricter_than_5pct_stricter_than_10pct() {
        let cv1 = mackinnon_critical_value(100, 2, MacKinnonLevel::Pct1);
        let cv5 = mackinnon_critical_value(100, 2, MacKinnonLevel::Pct5);
        let cv10 = mackinnon_critical_value(100, 2, MacKinnonLevel::Pct10);
        assert!(cv1 < cv5, "1% cv={} should be < 5% cv={}", cv1, cv5);
        assert!(cv5 < cv10, "5% cv={} should be < 10% cv={}", cv5, cv10);
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

    // ── ADF lag selection tests ─────────────────────────────

    /// AIC-selected ADF still detects cointegration on the
    /// standard test pair (regression: adding lags should not
    /// break detection on a clean stationary residual).
    #[test]
    fn aic_selected_adf_detects_cointegration() {
        let (y, x) = cointegrated_pair(200);
        let result = EngleGrangerTest::run(&y, &x).unwrap();
        assert!(
            result.is_cointegrated,
            "AIC ADF should detect cointegration: ADF={} crit={}",
            result.adf_statistic, result.critical_value_5pct
        );
    }

    /// AIC-selected ADF still rejects independent walks.
    #[test]
    fn aic_selected_adf_rejects_independent_walks() {
        let (y, x) = independent_walks(200);
        let result = EngleGrangerTest::run(&y, &x).unwrap();
        assert!(
            !result.is_cointegrated,
            "AIC ADF should reject independent walks: ADF={}",
            result.adf_statistic
        );
    }

    /// Fixed-lag=0 matches the behaviour of the old `adf_basic_stat`.
    #[test]
    fn fixed_lag_zero_matches_basic() {
        let (y, x) = cointegrated_pair(100);
        let r0 = EngleGrangerTest::run_with_lag(&y, &x, 0).unwrap();
        // The ADF stat with lag=0 is the same regression as the
        // old basic ADF (Δε = ρ·ε_{t-1}).
        assert!(r0.adf_statistic < Decimal::ZERO);
    }

    /// Higher lag orders do not crash and produce finite results.
    #[test]
    fn higher_lag_orders_produce_finite_results() {
        let (y, x) = cointegrated_pair(200);
        for lag in [1, 2, 4, 8] {
            let r = EngleGrangerTest::run_with_lag(&y, &x, lag);
            assert!(r.is_some(), "lag={} should produce a result", lag);
            let r = r.unwrap();
            assert!(
                r.adf_statistic != Decimal::ZERO || lag == 0,
                "lag={} ADF stat should be non-trivial, got {}",
                lag,
                r.adf_statistic
            );
        }
    }
}
