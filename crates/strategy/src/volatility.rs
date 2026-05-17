use crate::cartea_spread::decimal_ln;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;

/// Exponentially weighted moving average volatility estimator.
///
/// Tracks mid-price changes and computes realized volatility
/// using EWMA (like RiskMetrics approach).
pub struct VolatilityEstimator {
    /// Decay factor (λ). Typical: 0.94 for short-term.
    lambda: Decimal,
    /// Current EWMA variance estimate.
    variance: Decimal,
    /// Last observed price.
    last_price: Option<Decimal>,
    /// Recent returns for bootstrapping.
    returns: VecDeque<Decimal>,
    /// Minimum samples before we trust the estimate.
    min_samples: usize,
    /// Annualization factor (sqrt of observations per year).
    /// For 500ms ticks: sqrt(365.25 * 24 * 3600 / 0.5) ≈ 7936.
    annualization: Decimal,
}

impl VolatilityEstimator {
    pub fn new(lambda: Decimal, tick_interval_secs: Decimal) -> Self {
        // Observations per year.
        let secs_per_year = dec!(31_557_600); // 365.25 * 86400
        let obs_per_year = secs_per_year / tick_interval_secs;
        // We need sqrt, approximate with Decimal.
        let annualization = decimal_sqrt(obs_per_year);

        Self {
            lambda,
            variance: dec!(0),
            last_price: None,
            returns: VecDeque::with_capacity(1000),
            min_samples: 20,
            annualization,
        }
    }

    /// Feed a new mid-price observation.
    pub fn update(&mut self, price: Decimal) {
        if price.is_zero() {
            return;
        }

        if let Some(last) = self.last_price {
            if last.is_zero() {
                self.last_price = Some(price);
                return;
            }
            // Log return approximation: (price - last) / last.
            let ret = (price - last) / last;
            self.returns.push_back(ret);
            if self.returns.len() > 1000 {
                self.returns.pop_front();
            }

            let ret_sq = ret * ret;
            if self.variance.is_zero() && self.returns.len() >= self.min_samples {
                // Bootstrap: use sample variance.
                self.variance = sample_variance(self.returns.make_contiguous());
            } else if !self.variance.is_zero() {
                // EWMA update: σ² = λ·σ²_prev + (1-λ)·r².
                self.variance = self.lambda * self.variance + (dec!(1) - self.lambda) * ret_sq;
            }
        }
        self.last_price = Some(price);
    }

    /// Get annualized volatility estimate.
    /// Returns None if not enough data.
    pub fn volatility(&self) -> Option<Decimal> {
        if self.returns.len() < self.min_samples {
            return None;
        }
        if self.variance.is_zero() {
            return None;
        }
        Some(decimal_sqrt(self.variance) * self.annualization)
    }
}

/// GARCH(1,1) conditional-variance estimator.
///
/// Models the one-step-ahead variance as
///
/// ```text
///   σ²ₜ = ω + α·r²ₜ₋₁ + β·σ²ₜ₋₁
/// ```
///
/// [`VolatilityEstimator`]'s EWMA is the special *IGARCH* case
/// `ω = 0, α = 1−λ, β = λ` (so `α + β = 1`): it has no mean
/// reversion — a shock decays geometrically but the process never
/// pulls back toward a long-run level. GARCH(1,1) keeps `ω > 0` and
/// `α + β < 1`, so the conditional variance mean-reverts toward the
/// unconditional variance `ω / (1 − α − β)`. During a transient
/// spike that mean reversion yields a less jumpy, faster-settling σ
/// — a better input for the Avellaneda-Stoikov reservation price
/// than a raw EWMA.
///
/// Parameters are fit by **variance targeting**: the unconditional
/// variance is pinned to the sample variance, which removes `ω` from
/// the search, leaving a coarse-to-fine grid search over `(α, β)`
/// that maximises the Gaussian quasi-likelihood. The fit runs once
/// the buffer crosses `min_samples` and again every `refit_interval`
/// updates thereafter.
pub struct GarchEstimator {
    /// Constant term ω. `0` until the first calibration.
    omega: Decimal,
    /// ARCH coefficient α — reaction to the most recent shock.
    alpha: Decimal,
    /// GARCH coefficient β — persistence of past variance.
    beta: Decimal,
    /// Current one-step-ahead conditional variance σ²ₜ.
    variance: Decimal,
    /// Last observed price.
    last_price: Option<Decimal>,
    /// Recent returns — calibration sample and bootstrap buffer.
    returns: VecDeque<Decimal>,
    /// Minimum returns before the estimator reports a value.
    min_samples: usize,
    /// Hard cap on the retained return history.
    max_samples: usize,
    /// Annualization factor (sqrt of observations per year).
    annualization: Decimal,
    /// Updates since the last calibration — drives `refit_interval`.
    updates_since_fit: usize,
    /// Recalibration cadence, in updates.
    refit_interval: usize,
}

impl GarchEstimator {
    pub fn new(tick_interval_secs: Decimal) -> Self {
        let secs_per_year = dec!(31_557_600); // 365.25 * 86400
        let obs_per_year = if tick_interval_secs > dec!(0) {
            secs_per_year / tick_interval_secs
        } else {
            secs_per_year
        };
        Self {
            omega: dec!(0),
            alpha: dec!(0.1),
            beta: dec!(0.85),
            variance: dec!(0),
            last_price: None,
            returns: VecDeque::with_capacity(512),
            min_samples: 30,
            max_samples: 500,
            annualization: decimal_sqrt(obs_per_year),
            updates_since_fit: 0,
            refit_interval: 200,
        }
    }

    /// Feed a new mid-price observation.
    pub fn update(&mut self, price: Decimal) {
        if price.is_zero() {
            return;
        }
        let last = match self.last_price {
            Some(last) if !last.is_zero() => last,
            _ => {
                self.last_price = Some(price);
                return;
            }
        };
        // Simple return — matches `VolatilityEstimator`.
        let ret = (price - last) / last;
        self.last_price = Some(price);
        self.returns.push_back(ret);
        if self.returns.len() > self.max_samples {
            self.returns.pop_front();
        }

        if self.variance > dec!(0) {
            // One-step-ahead GARCH recursion.
            let ret_sq = ret * ret;
            self.variance =
                (self.omega + self.alpha * ret_sq + self.beta * self.variance).max(MIN_VARIANCE);
        }

        self.updates_since_fit += 1;
        let needs_bootstrap = self.variance <= dec!(0);
        let periodic_due = self.updates_since_fit >= self.refit_interval;
        if self.returns.len() >= self.min_samples && (needs_bootstrap || periodic_due) {
            self.calibrate();
        }
    }

    /// Annualized one-step-ahead volatility. `None` until the
    /// buffer crosses `min_samples` and the first calibration has
    /// produced a positive variance.
    pub fn volatility(&self) -> Option<Decimal> {
        if self.returns.len() < self.min_samples || self.variance <= dec!(0) {
            return None;
        }
        Some(decimal_sqrt(self.variance) * self.annualization)
    }

    /// Annualized long-run volatility `√(ω / (1 − α − β))` the
    /// process mean-reverts toward. `None` before the first
    /// calibration or if the fitted parameters are non-stationary.
    pub fn unconditional_volatility(&self) -> Option<Decimal> {
        let persistence = self.alpha + self.beta;
        if persistence >= dec!(1) {
            return None;
        }
        let uncond_var = self.omega / (dec!(1) - persistence);
        if uncond_var <= dec!(0) {
            return None;
        }
        Some(decimal_sqrt(uncond_var) * self.annualization)
    }

    /// Current `(ω, α, β)` parameters. Before the first
    /// calibration these are the constructor defaults.
    pub fn params(&self) -> (Decimal, Decimal, Decimal) {
        (self.omega, self.alpha, self.beta)
    }

    /// Re-fit `(ω, α, β)` from the retained returns by variance
    /// targeting + a coarse-to-fine `(α, β)` grid search.
    fn calibrate(&mut self) {
        self.updates_since_fit = 0;

        let n = self.returns.len();
        if n < self.min_samples {
            return;
        }
        let window: Vec<Decimal> = self
            .returns
            .iter()
            .skip(n.saturating_sub(CALIB_WINDOW))
            .copied()
            .collect();
        let sample_var = sample_variance(&window);
        if sample_var <= dec!(0) {
            return;
        }

        // (objective, α, β, terminal variance) — lower objective is a better fit.
        let mut best: Option<(Decimal, Decimal, Decimal, Decimal)> = None;
        let consider =
            |best: &mut Option<(Decimal, Decimal, Decimal, Decimal)>, a: Decimal, b: Decimal| {
                if let Some((obj, term_var)) = garch_objective(&window, sample_var, a, b) {
                    let better = match *best {
                        Some((obj0, ..)) => obj < obj0,
                        None => true,
                    };
                    if better {
                        *best = Some((obj, a, b, term_var));
                    }
                }
            };

        // Coarse grid over the (α, β) stationarity region.
        let coarse_alpha = [
            dec!(0.03),
            dec!(0.06),
            dec!(0.10),
            dec!(0.15),
            dec!(0.22),
            dec!(0.30),
        ];
        let coarse_beta = [
            dec!(0.55),
            dec!(0.65),
            dec!(0.74),
            dec!(0.82),
            dec!(0.88),
            dec!(0.93),
            dec!(0.97),
        ];
        for a in coarse_alpha {
            for b in coarse_beta {
                consider(&mut best, a, b);
            }
        }
        let Some((_, coarse_a, coarse_b, _)) = best else {
            return;
        };

        // Refine: a ±2-step neighbourhood around the coarse winner.
        for i in -2..=2_i32 {
            for j in -2..=2_i32 {
                let a = coarse_a + Decimal::from(i) * dec!(0.015);
                let b = coarse_b + Decimal::from(j) * dec!(0.02);
                if a < dec!(0.001) || b < dec!(0.001) {
                    continue;
                }
                consider(&mut best, a, b);
            }
        }

        if let Some((_, alpha, beta, term_var)) = best {
            self.alpha = alpha;
            self.beta = beta;
            self.omega = sample_var * (dec!(1) - alpha - beta);
            self.variance = term_var.max(MIN_VARIANCE);
        }
    }
}

/// Newton's method sqrt for Decimal.
pub fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= dec!(0) {
        return dec!(0);
    }
    let mut guess = x / dec!(2);
    if guess.is_zero() {
        guess = dec!(1);
    }
    for _ in 0..20 {
        let next = (guess + x / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000000001) {
            return next;
        }
        guess = next;
    }
    guess
}

/// Floor applied to the conditional variance — keeps `ln σ²` and
/// `r²/σ²` finite when the recursion drives σ² toward zero.
const MIN_VARIANCE: Decimal = dec!(0.000000000001);

/// Most-recent returns considered by a GARCH calibration pass.
const CALIB_WINDOW: usize = 250;

/// Unbiased sample variance of a return series.
fn sample_variance(returns: &[Decimal]) -> Decimal {
    if returns.len() < 2 {
        return dec!(0);
    }
    let n = Decimal::from(returns.len() as u64);
    let mean: Decimal = returns.iter().sum::<Decimal>() / n;
    let sum_sq: Decimal = returns.iter().map(|r| (*r - mean) * (*r - mean)).sum();
    sum_sq / (n - dec!(1))
}

/// Gaussian quasi-likelihood objective for one GARCH(1,1) candidate
/// under variance targeting.
///
/// With the unconditional variance pinned to `sample_var`, the
/// constant is `ω = sample_var·(1 − α − β)`. The recursion is run
/// over `returns` and the `−2·logL` contributions (dropped
/// constants) `ln σ²ₜ + r²ₜ/σ²ₜ` are summed — so a *lower* return
/// value is a better fit. Returns `None` for a non-stationary
/// candidate (`α + β ≥ 1`), alongside the terminal variance so the
/// caller can seed the live estimate consistently with the fit.
fn garch_objective(
    returns: &[Decimal],
    sample_var: Decimal,
    alpha: Decimal,
    beta: Decimal,
) -> Option<(Decimal, Decimal)> {
    let persistence = alpha + beta;
    if persistence >= dec!(1) {
        return None;
    }
    let omega = sample_var * (dec!(1) - persistence);
    if omega <= dec!(0) {
        return None;
    }

    let mut sigma2 = sample_var; // σ²₁ seeded at the variance target.
    let mut objective = dec!(0);
    for &r in returns {
        let s = sigma2.max(MIN_VARIANCE);
        let r_sq = r * r;
        objective += decimal_ln(s) + r_sq / s;
        sigma2 = omega + alpha * r_sq + beta * s;
    }
    Some((objective, sigma2.max(MIN_VARIANCE)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqrt() {
        let result = decimal_sqrt(dec!(4));
        assert!((result - dec!(2)).abs() < dec!(0.0001));

        let result = decimal_sqrt(dec!(100));
        assert!((result - dec!(10)).abs() < dec!(0.0001));
    }

    #[test]
    fn test_volatility_needs_min_samples() {
        let mut est = VolatilityEstimator::new(dec!(0.94), dec!(1));
        est.update(dec!(100));
        est.update(dec!(101));
        assert!(est.volatility().is_none());
    }

    #[test]
    fn test_volatility_converges() {
        let mut est = VolatilityEstimator::new(dec!(0.94), dec!(1));
        // Feed 50 prices with small fluctuations.
        for i in 0..50 {
            let price = dec!(1000) + Decimal::from(i % 5) * dec!(0.1);
            est.update(price);
        }
        let vol = est.volatility();
        assert!(vol.is_some());
        assert!(vol.unwrap() > dec!(0));
    }

    // -------------------------- GARCH(1,1) --------------------------

    /// Drive `est` from `start`, walking the price by the repeating
    /// `deltas` pattern for `steps` ticks. Returns the final price so
    /// segments can be chained.
    fn feed(est: &mut GarchEstimator, start: Decimal, deltas: &[Decimal], steps: usize) -> Decimal {
        let mut price = start;
        est.update(price);
        for i in 0..steps {
            price *= dec!(1) + deltas[i % deltas.len()];
            est.update(price);
        }
        price
    }

    #[test]
    fn test_garch_needs_min_samples() {
        let mut est = GarchEstimator::new(dec!(1));
        est.update(dec!(100));
        est.update(dec!(101));
        est.update(dec!(102));
        assert!(est.volatility().is_none());
    }

    #[test]
    fn test_garch_produces_positive_vol() {
        let mut est = GarchEstimator::new(dec!(1));
        let pattern = [dec!(0.002), dec!(-0.0015), dec!(0.003), dec!(-0.001)];
        feed(&mut est, dec!(1000), &pattern, 60);
        let vol = est.volatility();
        assert!(vol.is_some());
        assert!(vol.unwrap() > dec!(0));
    }

    #[test]
    fn test_garch_constant_price_has_no_vol() {
        let mut est = GarchEstimator::new(dec!(1));
        for _ in 0..60 {
            est.update(dec!(500));
        }
        assert!(
            est.volatility().is_none(),
            "flat price ⇒ zero variance ⇒ no estimate"
        );
    }

    #[test]
    fn test_garch_calibration_yields_stationary_params() {
        let mut est = GarchEstimator::new(dec!(1));
        let pattern = [
            dec!(0.002),
            dec!(-0.0015),
            dec!(0.003),
            dec!(-0.001),
            dec!(0.0008),
            dec!(-0.0025),
        ];
        feed(&mut est, dec!(1000), &pattern, 80);
        let (omega, alpha, beta) = est.params();
        assert!(omega > dec!(0), "ω must be positive, got {omega}");
        assert!(alpha >= dec!(0), "α must be non-negative, got {alpha}");
        assert!(beta >= dec!(0), "β must be non-negative, got {beta}");
        assert!(
            alpha + beta < dec!(1),
            "α+β must be < 1 for stationarity, got {}",
            alpha + beta
        );
    }

    #[test]
    fn test_garch_reacts_to_volatility_clustering() {
        let mut est = GarchEstimator::new(dec!(1));
        let calm = [dec!(0.0003), dec!(-0.0002), dec!(0.0004), dec!(-0.0003)];
        let storm = [dec!(0.006), dec!(-0.005), dec!(0.007), dec!(-0.006)];
        let p = feed(&mut est, dec!(1000), &calm, 60);
        let vol_calm = est.volatility().expect("calm vol");
        feed(&mut est, p, &storm, 60);
        let vol_storm = est.volatility().expect("storm vol");
        assert!(
            vol_storm > vol_calm,
            "GARCH must lift σ during a volatility cluster: calm={vol_calm}, storm={vol_storm}"
        );
    }

    #[test]
    fn test_garch_mean_reverts_after_shock() {
        let mut est = GarchEstimator::new(dec!(1));
        let calm = [dec!(0.0003), dec!(-0.0002), dec!(0.0004), dec!(-0.0003)];
        let storm = [dec!(0.006), dec!(-0.005), dec!(0.007), dec!(-0.006)];
        let p = feed(&mut est, dec!(1000), &calm, 40);
        let p = feed(&mut est, p, &storm, 30);
        let vol_peak = est.volatility().expect("peak vol");
        feed(&mut est, p, &calm, 80);
        let vol_settled = est.volatility().expect("settled vol");
        assert!(
            vol_settled < vol_peak,
            "GARCH σ must mean-revert once the shock clears: peak={vol_peak}, settled={vol_settled}"
        );
    }

    #[test]
    fn test_garch_unconditional_volatility_available_after_fit() {
        let mut est = GarchEstimator::new(dec!(1));
        let pattern = [dec!(0.002), dec!(-0.0015), dec!(0.003), dec!(-0.001)];
        feed(&mut est, dec!(1000), &pattern, 80);
        let uncond = est.unconditional_volatility();
        assert!(
            uncond.is_some(),
            "long-run σ should be defined for stationary fitted params"
        );
        assert!(uncond.unwrap() > dec!(0));
    }
}
