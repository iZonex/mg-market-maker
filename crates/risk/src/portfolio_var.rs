//! Portfolio-level VaR guard (Epic 3: Cross-Symbol Portfolio Risk).
//!
//! Parametric Gaussian VaR on the total portfolio PnL delta stream.
//! Same framework as the per-strategy `VarGuard` but operates on
//! the aggregate PnL across all symbols. Returns a throttle
//! multiplier in `[0, 1]` that composes with existing kill switch
//! and market resilience multipliers.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Configuration for portfolio-level VaR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioVarConfig {
    /// 95%-VaR floor (usually negative). When computed VaR falls
    /// below this, throttle to 0.5.
    #[serde(default)]
    pub var_limit_95: Option<Decimal>,
    /// 99%-VaR floor (usually more negative). On breach, throttle
    /// to 0.0 (hard halt).
    #[serde(default)]
    pub var_limit_99: Option<Decimal>,
    /// Maximum rolling samples. Default 1440 (24h at 60s).
    #[serde(default = "default_max_samples")]
    pub max_samples: usize,
    /// Minimum samples before VaR is computed. Default 30.
    #[serde(default = "default_min_samples")]
    pub min_samples: usize,
}

fn default_max_samples() -> usize {
    1440
}
fn default_min_samples() -> usize {
    30
}

impl Default for PortfolioVarConfig {
    fn default() -> Self {
        Self {
            var_limit_95: None,
            var_limit_99: None,
            max_samples: default_max_samples(),
            min_samples: default_min_samples(),
        }
    }
}

/// Z-scores for parametric Gaussian VaR.
const Z_95: Decimal = dec!(1.645);
const Z_99: Decimal = dec!(2.326);

/// VaR computation result.
#[derive(Debug, Clone, Serialize)]
pub struct PortfolioVarResult {
    /// Parametric VaR at 95% confidence (negative = loss).
    pub var_95: Decimal,
    /// Parametric VaR at 99% confidence (negative = loss).
    pub var_99: Decimal,
    /// Number of samples used.
    pub samples: usize,
    /// Mean of the PnL delta distribution.
    pub mean: Decimal,
    /// Standard deviation of the PnL delta distribution.
    pub std_dev: Decimal,
}

/// Portfolio-level VaR guard.
pub struct PortfolioVarGuard {
    samples: VecDeque<Decimal>,
    config: PortfolioVarConfig,
}

impl PortfolioVarGuard {
    pub fn new(config: PortfolioVarConfig) -> Self {
        Self {
            samples: VecDeque::with_capacity(config.max_samples),
            config,
        }
    }

    /// Record a portfolio PnL delta sample.
    pub fn record_sample(&mut self, pnl_delta: Decimal) {
        if self.samples.len() >= self.config.max_samples {
            self.samples.pop_front();
        }
        self.samples.push_back(pnl_delta);
    }

    /// Compute parametric Gaussian VaR. Returns `None` during
    /// the warm-up period (fewer than `min_samples`).
    pub fn compute_var(&self) -> Option<PortfolioVarResult> {
        if self.samples.len() < self.config.min_samples {
            return None;
        }
        let n = self.samples.len();
        let n_dec = Decimal::from(n as u64);
        let mean: Decimal = self.samples.iter().sum::<Decimal>() / n_dec;
        let variance: Decimal = self
            .samples
            .iter()
            .map(|x| (*x - mean) * (*x - mean))
            .sum::<Decimal>()
            / Decimal::from((n - 1) as u64);
        let std_dev = decimal_sqrt(variance);

        Some(PortfolioVarResult {
            var_95: mean - Z_95 * std_dev,
            var_99: mean - Z_99 * std_dev,
            samples: n,
            mean,
            std_dev,
        })
    }

    /// Returns a throttle multiplier in `[0, 1]`:
    /// - `1.0` — no throttle (normal operation)
    /// - `0.5` — 95% VaR breached (reduce size)
    /// - `0.0` — 99% VaR breached (hard halt)
    pub fn throttle(&self) -> Decimal {
        let Some(var) = self.compute_var() else {
            return dec!(1); // warm-up
        };
        if let Some(limit_99) = self.config.var_limit_99 {
            if var.var_99 < limit_99 {
                return dec!(0);
            }
        }
        if let Some(limit_95) = self.config.var_limit_95 {
            if var.var_95 < limit_95 {
                return dec!(0.5);
            }
        }
        dec!(1)
    }

    /// Number of recorded samples.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }
}

fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let mut guess = if x > Decimal::ONE { x / dec!(2) } else { x };
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

    fn config_with_limits() -> PortfolioVarConfig {
        PortfolioVarConfig {
            var_limit_95: Some(dec!(-100)),
            var_limit_99: Some(dec!(-200)),
            max_samples: 1440,
            min_samples: 10,
        }
    }

    #[test]
    fn warmup_returns_full_throttle() {
        let guard = PortfolioVarGuard::new(config_with_limits());
        assert_eq!(guard.throttle(), dec!(1));
        assert!(guard.compute_var().is_none());
    }

    #[test]
    fn stable_pnl_no_throttle() {
        let mut guard = PortfolioVarGuard::new(config_with_limits());
        for _ in 0..50 {
            guard.record_sample(dec!(1)); // small positive delta
        }
        assert_eq!(guard.throttle(), dec!(1));
    }

    #[test]
    fn large_losses_trigger_95_throttle() {
        // VaR_95 should breach -100 but VaR_99 should NOT breach -200.
        // std ≈ 70, VaR_95 ≈ -115 < -100, VaR_99 ≈ -163 > -200.
        let mut guard = PortfolioVarGuard::new(config_with_limits());
        for i in 0..50 {
            let delta = if i % 2 == 0 { dec!(70) } else { dec!(-70) };
            guard.record_sample(delta);
        }
        assert_eq!(guard.throttle(), dec!(0.5));
    }

    #[test]
    fn extreme_losses_trigger_99_halt() {
        let mut guard = PortfolioVarGuard::new(config_with_limits());
        for i in 0..50 {
            let delta = if i % 2 == 0 { dec!(200) } else { dec!(-200) };
            guard.record_sample(delta);
        }
        // std ≈ 200, VaR_99 ≈ -465 < -200 → throttle 0.0
        assert_eq!(guard.throttle(), dec!(0));
    }

    #[test]
    fn no_limits_configured_never_throttles() {
        let mut guard = PortfolioVarGuard::new(PortfolioVarConfig::default());
        for i in 0..50 {
            guard.record_sample(Decimal::from(i) * dec!(-100));
        }
        assert_eq!(guard.throttle(), dec!(1));
    }

    #[test]
    fn rolling_window_caps_at_max_samples() {
        let config = PortfolioVarConfig {
            max_samples: 20,
            min_samples: 5,
            ..Default::default()
        };
        let mut guard = PortfolioVarGuard::new(config);
        for i in 0..50 {
            guard.record_sample(Decimal::from(i));
        }
        assert_eq!(guard.sample_count(), 20);
    }
}
