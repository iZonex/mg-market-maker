//! Johansen multivariate cointegration test (Epic B, stage-3).
//!
//! Generalises Engle-Granger to N ≥ 2 assets. The procedure:
//!
//! 1. Form ΔY[t] and Y[t-1] from the N-variate price matrix.
//! 2. Concentrate out a constant by OLS-residualising both ΔY
//!    and Y_{t-1} against a vector of ones.
//! 3. Build moment matrices S00, S01, S10, S11 from residuals.
//! 4. Solve the generalised eigenvalue problem
//!    `det(λ·S11 − S10·S00⁻¹·S01) = 0`.
//! 5. Trace and max-eigenvalue statistics vs Osterwald-Lenum
//!    1992 critical values at 5%.
//!
//! Pure function — no IO, no async, no allocation beyond working
//! matrices. Uses f64 internally for eigenvalue decomposition,
//! converts results to Decimal for the public API.
//!
//! Reference: Johansen (1991) "Estimation and Hypothesis Testing
//! of Cointegration Vectors in Gaussian Vector Autoregressive
//! Models", *Econometrica* 59(6), pp. 1551–1580.

use rust_decimal::Decimal;

pub const MIN_SAMPLES_JOHANSEN: usize = 30;
pub const MAX_DIMENSION: usize = 6;

#[derive(Debug, Clone)]
pub struct JohansenResult {
    /// Number of cointegrating relations at 5% significance
    /// (sequential trace test).
    pub rank: usize,
    /// Dimension of the system (number of price series).
    pub n_vars: usize,
    /// Sorted eigenvalues (descending). Length = n_vars.
    pub eigenvalues: Vec<Decimal>,
    /// Cointegrating vectors (column-major, n_vars × n_vars).
    /// The first `rank` columns are the cointegrating vectors.
    pub eigenvectors: Vec<Vec<Decimal>>,
    /// Trace statistics for H0: rank ≤ r, r = 0..n_vars-1.
    pub trace_stats: Vec<Decimal>,
    /// Max-eigenvalue statistics for H0: rank = r vs r+1.
    pub max_eigen_stats: Vec<Decimal>,
    /// 5% critical values for the trace test.
    pub critical_values_trace_5pct: Vec<Decimal>,
    /// 5% critical values for the max-eigenvalue test.
    pub critical_values_max_5pct: Vec<Decimal>,
    /// Sample size used (T − 1 after differencing).
    pub effective_sample_size: usize,
}

pub struct JohansenTest;

impl JohansenTest {
    /// Run the Johansen trace test on N price series.
    ///
    /// `series[i]` is the i-th asset's price history; all must
    /// have the same length ≥ [`MIN_SAMPLES_JOHANSEN`] and
    /// 2 ≤ N ≤ [`MAX_DIMENSION`].
    pub fn run(series: &[&[Decimal]]) -> Option<JohansenResult> {
        let n = series.len();
        if n < 2 || n > MAX_DIMENSION {
            return None;
        }
        let t = series[0].len();
        if t < MIN_SAMPLES_JOHANSEN {
            return None;
        }
        for s in series {
            if s.len() != t {
                return None;
            }
        }

        let t_eff = t - 1; // after differencing

        // Convert to f64 matrix (t × n).
        let mut data = vec![vec![0.0f64; n]; t];
        for j in 0..n {
            for i in 0..t {
                data[i][j] = decimal_to_f64(series[j][i]);
            }
        }

        // ΔY[t] = Y[t] - Y[t-1], t = 1..T-1  →  (t_eff × n)
        let mut dy = vec![vec![0.0; n]; t_eff];
        // Y_{t-1}, t = 1..T-1  →  (t_eff × n)
        let mut y_lag = vec![vec![0.0; n]; t_eff];
        for i in 0..t_eff {
            for j in 0..n {
                dy[i][j] = data[i + 1][j] - data[i][j];
                y_lag[i][j] = data[i][j];
            }
        }

        // Concentrate out constant: residualise against ones.
        let r0 = residualise_against_constant(&dy, t_eff, n);
        let r1 = residualise_against_constant(&y_lag, t_eff, n);

        // Moment matrices: Sij = (1/T) · Ri' · Rj
        let s00 = moment_matrix(&r0, &r0, t_eff, n, n);
        let s01 = moment_matrix(&r0, &r1, t_eff, n, n);
        let s10 = moment_matrix(&r1, &r0, t_eff, n, n);
        let s11 = moment_matrix(&r1, &r1, t_eff, n, n);

        // Solve: eigenvalues of inv(S11) · S10 · inv(S00) · S01
        let s00_inv = mat_inv(&s00, n)?;
        let s11_inv = mat_inv(&s11, n)?;

        // M = S11^{-1} · S10 · S00^{-1} · S01
        let tmp1 = mat_mul(&s00_inv, &s01, n); // S00^{-1} · S01
        let tmp2 = mat_mul(&s10, &tmp1, n); // S10 · S00^{-1} · S01
        let m = mat_mul(&s11_inv, &tmp2, n); // S11^{-1} · ...

        // Eigendecomposition of M (may not be symmetric, but for
        // well-conditioned cointegration data the eigenvalues are
        // real and in [0, 1)).
        let (mut eigenvalues, mut eigenvectors) = eigen_decomp(&m, n)?;

        // Sort descending by eigenvalue.
        let mut indices: Vec<usize> = (0..n).collect();
        indices.sort_by(|&a, &b| eigenvalues[b].partial_cmp(&eigenvalues[a]).unwrap());
        let sorted_evals: Vec<f64> = indices.iter().map(|&i| eigenvalues[i]).collect();
        let sorted_evecs: Vec<Vec<f64>> =
            indices.iter().map(|&i| eigenvectors[i].clone()).collect();
        eigenvalues = sorted_evals;
        eigenvectors = sorted_evecs;

        // Clamp eigenvalues to [0, 1) for numerical safety.
        for ev in &mut eigenvalues {
            if *ev < 0.0 {
                *ev = 0.0;
            }
            if *ev >= 1.0 {
                *ev = 1.0 - 1e-15;
            }
        }

        // Trace statistics: -T · Σ_{i=r+1}^{n-1} ln(1 - λ_i)
        let mut trace_stats = vec![0.0f64; n];
        let mut max_eigen_stats = vec![0.0f64; n];
        for r in 0..n {
            let mut sum = 0.0;
            for i in r..n {
                sum += (1.0 - eigenvalues[i]).ln();
            }
            trace_stats[r] = -(t_eff as f64) * sum;
            max_eigen_stats[r] = -(t_eff as f64) * (1.0 - eigenvalues[r]).ln();
        }

        // Critical values.
        let cv_trace = osterwald_lenum_trace_5pct(n);
        let cv_max = osterwald_lenum_max_5pct(n);

        // Determine rank: sequential trace test.
        let mut rank = 0;
        for r in 0..n {
            if r < cv_trace.len() && trace_stats[r] > cv_trace[r] {
                rank = r + 1;
            } else {
                break;
            }
        }

        Some(JohansenResult {
            rank,
            n_vars: n,
            eigenvalues: eigenvalues.iter().map(|&v| f64_to_decimal(v)).collect(),
            eigenvectors: eigenvectors
                .iter()
                .map(|col| col.iter().map(|&v| f64_to_decimal(v)).collect())
                .collect(),
            trace_stats: trace_stats.iter().map(|&v| f64_to_decimal(v)).collect(),
            max_eigen_stats: max_eigen_stats.iter().map(|&v| f64_to_decimal(v)).collect(),
            critical_values_trace_5pct: cv_trace.iter().map(|&v| f64_to_decimal(v)).collect(),
            critical_values_max_5pct: cv_max.iter().map(|&v| f64_to_decimal(v)).collect(),
            effective_sample_size: t_eff,
        })
    }
}

// ── helpers ──────────────────────────────────────────────────

fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

fn f64_to_decimal(v: f64) -> Decimal {
    Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO)
}

/// Residualise columns of `z` (t_eff × n) against a constant
/// (subtract column means).
fn residualise_against_constant(z: &[Vec<f64>], rows: usize, cols: usize) -> Vec<Vec<f64>> {
    let mut means = vec![0.0; cols];
    for row in z.iter().take(rows) {
        for j in 0..cols {
            means[j] += row[j];
        }
    }
    for m in &mut means {
        *m /= rows as f64;
    }
    let mut out = vec![vec![0.0; cols]; rows];
    for i in 0..rows {
        for j in 0..cols {
            out[i][j] = z[i][j] - means[j];
        }
    }
    out
}

/// (1/T) · A' · B where A is (T × p) and B is (T × q).
fn moment_matrix(a: &[Vec<f64>], b: &[Vec<f64>], rows: usize, p: usize, q: usize) -> Vec<Vec<f64>> {
    let _ = q; // both are n×n in our usage
    let mut out = vec![vec![0.0; p]; p];
    for i in 0..p {
        for j in 0..p {
            let mut s = 0.0;
            for t in 0..rows {
                s += a[t][i] * b[t][j];
            }
            out[i][j] = s / rows as f64;
        }
    }
    out
}

/// Matrix inverse via Gauss-Jordan elimination. Returns `None`
/// if singular.
fn mat_inv(a: &[Vec<f64>], n: usize) -> Option<Vec<Vec<f64>>> {
    let mut aug = vec![vec![0.0; 2 * n]; n];
    for i in 0..n {
        for j in 0..n {
            aug[i][j] = a[i][j];
        }
        aug[i][n + i] = 1.0;
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
        if max_val < 1e-14 {
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
            for j in 0..(2 * n) {
                aug[row][j] -= factor * aug[col][j];
            }
        }
    }
    let mut inv = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..n {
            inv[i][j] = aug[i][n + j];
        }
    }
    Some(inv)
}

/// Matrix multiply C = A · B (n × n).
fn mat_mul(a: &[Vec<f64>], b: &[Vec<f64>], n: usize) -> Vec<Vec<f64>> {
    let mut c = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..n {
            let mut s = 0.0;
            for k in 0..n {
                s += a[i][k] * b[k][j];
            }
            c[i][j] = s;
        }
    }
    c
}

/// Eigendecomposition of a real n×n matrix via QR iteration.
/// Returns (eigenvalues, eigenvectors) where eigenvectors[i] is
/// the i-th eigenvector (column). Returns `None` if iteration
/// fails to converge.
fn eigen_decomp(m: &[Vec<f64>], n: usize) -> Option<(Vec<f64>, Vec<Vec<f64>>)> {
    // For small n (≤6), real Schur form via implicit QR shifts
    // is reliable. We accumulate the orthogonal transformations
    // to recover eigenvectors.
    let mut a = m.to_vec();
    let mut q = eye(n);
    let max_iter = 200 * n;

    for _ in 0..max_iter {
        // Wilkinson shift: use a[n-1][n-1] as shift.
        let shift = a[n - 1][n - 1];
        for i in 0..n {
            a[i][i] -= shift;
        }
        let (q_step, r) = qr_factor(&a, n);
        a = mat_mul(&r, &q_step, n);
        for i in 0..n {
            a[i][i] += shift;
        }
        q = mat_mul(&q, &q_step, n);

        // Check convergence: sub-diagonal elements near zero.
        let mut converged = true;
        for i in 1..n {
            if a[i][i - 1].abs() > 1e-10 {
                converged = false;
                break;
            }
        }
        if converged {
            break;
        }
    }

    let eigenvalues: Vec<f64> = (0..n).map(|i| a[i][i]).collect();
    let eigenvectors: Vec<Vec<f64>> = (0..n).map(|j| (0..n).map(|i| q[i][j]).collect()).collect();

    // Verify all eigenvalues are real (imaginary part would show
    // as large off-diagonal entries that didn't converge).
    for i in 1..n {
        if a[i][i - 1].abs() > 1e-6 {
            return None;
        }
    }

    Some((eigenvalues, eigenvectors))
}

fn eye(n: usize) -> Vec<Vec<f64>> {
    let mut m = vec![vec![0.0; n]; n];
    for i in 0..n {
        m[i][i] = 1.0;
    }
    m
}

/// QR factorisation via Householder reflections. Returns (Q, R).
fn qr_factor(a: &[Vec<f64>], n: usize) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
    let mut r = a.to_vec();
    let mut q = eye(n);
    for col in 0..n.saturating_sub(1) {
        // Build Householder vector for column `col`.
        let mut x = vec![0.0; n];
        for i in col..n {
            x[i] = r[i][col];
        }
        let mut norm = 0.0;
        for i in col..n {
            norm += x[i] * x[i];
        }
        norm = norm.sqrt();
        if norm < 1e-15 {
            continue;
        }
        let sign = if x[col] >= 0.0 { 1.0 } else { -1.0 };
        x[col] += sign * norm;
        // Normalise.
        let mut vnorm = 0.0;
        for i in col..n {
            vnorm += x[i] * x[i];
        }
        if vnorm < 1e-30 {
            continue;
        }
        // H = I - 2·v·v'/||v||²
        // Apply H to R from left: R = H·R
        for j in col..n {
            let mut dot = 0.0;
            for i in col..n {
                dot += x[i] * r[i][j];
            }
            let coeff = 2.0 * dot / vnorm;
            for i in col..n {
                r[i][j] -= coeff * x[i];
            }
        }
        // Accumulate Q = Q·H
        for j in 0..n {
            let mut dot = 0.0;
            for i in col..n {
                dot += q[j][i] * x[i];
            }
            let coeff = 2.0 * dot / vnorm;
            for i in col..n {
                q[j][i] -= coeff * x[i];
            }
        }
    }
    // Q is stored as Q^T accumulation, transpose to get Q.
    // Actually we accumulated Q·H₁·H₂·... which is Q directly.
    (q, r)
}

// ── critical value tables ───────────────────────────────────

/// Osterwald-Lenum 1992 trace test critical values at 5%
/// significance (with constant, no trend). Indexed by N-r-1
/// where N is the system dimension.
///
/// Table rows are for H0: rank ≤ r, r = 0, 1, ..., N-1.
fn osterwald_lenum_trace_5pct(n: usize) -> Vec<f64> {
    match n {
        2 => vec![15.41, 3.76],
        3 => vec![29.68, 15.41, 3.76],
        4 => vec![47.21, 29.68, 15.41, 3.76],
        5 => vec![68.52, 47.21, 29.68, 15.41, 3.76],
        6 => vec![94.15, 68.52, 47.21, 29.68, 15.41, 3.76],
        _ => vec![],
    }
}

/// Osterwald-Lenum 1992 max-eigenvalue critical values at 5%
/// significance (with constant, no trend).
fn osterwald_lenum_max_5pct(n: usize) -> Vec<f64> {
    match n {
        2 => vec![14.07, 3.76],
        3 => vec![20.97, 14.07, 3.76],
        4 => vec![27.07, 20.97, 14.07, 3.76],
        5 => vec![33.46, 27.07, 20.97, 14.07, 3.76],
        6 => vec![39.37, 33.46, 27.07, 20.97, 14.07, 3.76],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

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

    fn cointegrated_triple(n: usize) -> (Vec<Decimal>, Vec<Decimal>, Vec<Decimal>) {
        let innov_x = lcg_innovations(111_111, n, 3);
        let innov_z = lcg_innovations(333_333, n, 3);
        let eps1 = lcg_innovations(555_555, n, 5);
        let eps2 = lcg_innovations(777_777, n, 5);

        let mut x = Vec::with_capacity(n);
        let mut y = Vec::with_capacity(n);
        let mut z = Vec::with_capacity(n);
        let mut x_val = dec!(100);
        let mut z_val = dec!(200);
        for i in 0..n {
            x_val += innov_x[i];
            z_val += innov_z[i];
            x.push(x_val);
            z.push(z_val);
            // Y = 2·X + 0.5·Z + ε  →  two cointegrating vectors
            // But actually this means Y, X, Z share a common
            // stochastic trend, so rank should be ≥ 1.
            y.push(dec!(2) * x_val + z_val / dec!(2) + eps1[i] / dec!(10) + eps2[i] / dec!(10));
        }
        (x, y, z)
    }

    fn independent_walks_3(n: usize) -> (Vec<Decimal>, Vec<Decimal>, Vec<Decimal>) {
        let i1 = lcg_innovations(111, n, 3);
        let i2 = lcg_innovations(222, n, 3);
        let i3 = lcg_innovations(333, n, 3);
        let mut a = Vec::with_capacity(n);
        let mut b = Vec::with_capacity(n);
        let mut c = Vec::with_capacity(n);
        let (mut va, mut vb, mut vc) = (dec!(100), dec!(200), dec!(300));
        for i in 0..n {
            va += i1[i];
            vb += i2[i];
            vc += i3[i];
            a.push(va);
            b.push(vb);
            c.push(vc);
        }
        (a, b, c)
    }

    #[test]
    fn too_few_series_returns_none() {
        let s = vec![dec!(1); 50];
        assert!(JohansenTest::run(&[&s]).is_none());
    }

    #[test]
    fn too_many_series_returns_none() {
        let s = vec![dec!(1); 50];
        let refs: Vec<&[Decimal]> = (0..7).map(|_| s.as_slice()).collect();
        assert!(JohansenTest::run(&refs).is_none());
    }

    #[test]
    fn mismatched_lengths_returns_none() {
        let a = vec![dec!(1); 50];
        let b = vec![dec!(1); 49];
        assert!(JohansenTest::run(&[&a, &b]).is_none());
    }

    #[test]
    fn too_few_samples_returns_none() {
        let a = vec![dec!(1); 20];
        let b = vec![dec!(1); 20];
        assert!(JohansenTest::run(&[&a, &b]).is_none());
    }

    #[test]
    fn cointegrated_pair_detected() {
        let innov_x = lcg_innovations(1_234_567, 200, 3);
        let eps = lcg_innovations(9_876_543, 200, 5);
        let mut x = Vec::with_capacity(200);
        let mut y = Vec::with_capacity(200);
        let mut x_val = dec!(100);
        for i in 0..200 {
            x_val += innov_x[i];
            x.push(x_val);
            y.push(dec!(2) * x_val + eps[i] / dec!(10));
        }
        let result = JohansenTest::run(&[&y, &x]).expect("should return result");
        assert_eq!(result.n_vars, 2);
        assert!(
            result.rank >= 1,
            "expected rank ≥ 1, got {}, trace={:?}, crit={:?}",
            result.rank,
            result.trace_stats,
            result.critical_values_trace_5pct
        );
    }

    #[test]
    fn independent_walks_rank_zero() {
        let (a, b) = {
            let i1 = lcg_innovations(111_111, 200, 3);
            let i2 = lcg_innovations(222_222, 200, 3);
            let mut va = dec!(100);
            let mut vb = dec!(50);
            let mut a = Vec::with_capacity(200);
            let mut b = Vec::with_capacity(200);
            for i in 0..200 {
                va += i1[i];
                vb += i2[i];
                a.push(va);
                b.push(vb);
            }
            (a, b)
        };
        let result = JohansenTest::run(&[&a, &b]).expect("should return result");
        assert_eq!(
            result.rank, 0,
            "independent walks should have rank 0, trace={:?}",
            result.trace_stats
        );
    }

    #[test]
    fn trivariate_cointegrated_has_positive_rank() {
        let (x, y, z) = cointegrated_triple(300);
        let result = JohansenTest::run(&[&y, &x, &z]).expect("should return result");
        assert_eq!(result.n_vars, 3);
        assert!(
            result.rank >= 1,
            "expected rank ≥ 1 for cointegrated triple, got {}, trace={:?}",
            result.rank,
            result.trace_stats
        );
    }

    #[test]
    fn trivariate_independent_rank_zero() {
        let (a, b, c) = independent_walks_3(200);
        let result = JohansenTest::run(&[&a, &b, &c]).expect("should return result");
        assert_eq!(result.rank, 0, "three independent walks should have rank 0");
    }

    #[test]
    fn eigenvalues_in_unit_interval() {
        let innov_x = lcg_innovations(1_234_567, 100, 3);
        let eps = lcg_innovations(9_876_543, 100, 5);
        let mut x = Vec::with_capacity(100);
        let mut y = Vec::with_capacity(100);
        let mut x_val = dec!(100);
        for i in 0..100 {
            x_val += innov_x[i];
            x.push(x_val);
            y.push(dec!(2) * x_val + eps[i] / dec!(10));
        }
        let result = JohansenTest::run(&[&y, &x]).unwrap();
        for ev in &result.eigenvalues {
            assert!(*ev >= Decimal::ZERO, "eigenvalue {} < 0", ev);
            assert!(*ev <= Decimal::ONE, "eigenvalue {} > 1", ev);
        }
    }

    #[test]
    fn eigenvectors_have_correct_dimension() {
        let (x, y, z) = cointegrated_triple(100);
        let result = JohansenTest::run(&[&y, &x, &z]).unwrap();
        assert_eq!(result.eigenvectors.len(), 3);
        for ev in &result.eigenvectors {
            assert_eq!(ev.len(), 3);
        }
    }

    #[test]
    fn trace_stats_descending() {
        let innov_x = lcg_innovations(1_234_567, 150, 3);
        let eps = lcg_innovations(9_876_543, 150, 5);
        let mut x = Vec::with_capacity(150);
        let mut y = Vec::with_capacity(150);
        let mut x_val = dec!(100);
        for i in 0..150 {
            x_val += innov_x[i];
            x.push(x_val);
            y.push(dec!(2) * x_val + eps[i] / dec!(10));
        }
        let result = JohansenTest::run(&[&y, &x]).unwrap();
        for w in result.trace_stats.windows(2) {
            assert!(
                w[0] >= w[1],
                "trace stats not descending: {} < {}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn result_is_deterministic() {
        let innov_x = lcg_innovations(42, 100, 3);
        let eps = lcg_innovations(99, 100, 5);
        let mut x = Vec::with_capacity(100);
        let mut y = Vec::with_capacity(100);
        let mut x_val = dec!(100);
        for i in 0..100 {
            x_val += innov_x[i];
            x.push(x_val);
            y.push(dec!(3) * x_val + eps[i] / dec!(10));
        }
        let a = JohansenTest::run(&[&y, &x]).unwrap();
        let b = JohansenTest::run(&[&y, &x]).unwrap();
        assert_eq!(a.rank, b.rank);
        assert_eq!(a.eigenvalues, b.eigenvalues);
        assert_eq!(a.trace_stats, b.trace_stats);
    }

    #[test]
    fn effective_sample_size_correct() {
        let innov_x = lcg_innovations(42, 80, 3);
        let eps = lcg_innovations(99, 80, 5);
        let mut x = Vec::with_capacity(80);
        let mut y = Vec::with_capacity(80);
        let mut x_val = dec!(100);
        for i in 0..80 {
            x_val += innov_x[i];
            x.push(x_val);
            y.push(dec!(2) * x_val + eps[i] / dec!(10));
        }
        let result = JohansenTest::run(&[&y, &x]).unwrap();
        assert_eq!(result.effective_sample_size, 79);
    }

    #[test]
    fn critical_value_tables_correct_length() {
        for n in 2..=6 {
            let cv = osterwald_lenum_trace_5pct(n);
            assert_eq!(cv.len(), n, "trace CV table wrong length for n={}", n);
            let cv = osterwald_lenum_max_5pct(n);
            assert_eq!(cv.len(), n, "max CV table wrong length for n={}", n);
        }
    }

    #[test]
    fn mat_inv_identity() {
        let id = eye(3);
        let inv = mat_inv(&id, 3).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                let expected = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (inv[i][j] - expected).abs() < 1e-10,
                    "inv[{}][{}] = {} expected {}",
                    i,
                    j,
                    inv[i][j],
                    expected
                );
            }
        }
    }

    #[test]
    fn mat_inv_singular_returns_none() {
        let singular = vec![
            vec![1.0, 2.0, 3.0],
            vec![4.0, 5.0, 6.0],
            vec![5.0, 7.0, 9.0], // row3 = row1 + row2
        ];
        assert!(mat_inv(&singular, 3).is_none());
    }
}
