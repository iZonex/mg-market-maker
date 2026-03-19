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
                self.variance = self.sample_variance();
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

    fn sample_variance(&self) -> Decimal {
        if self.returns.len() < 2 {
            return dec!(0);
        }
        let n = Decimal::from(self.returns.len() as u64);
        let mean: Decimal = self.returns.iter().sum::<Decimal>() / n;
        let sum_sq: Decimal = self.returns.iter().map(|r| (*r - mean) * (*r - mean)).sum();
        sum_sq / (n - dec!(1))
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
}
