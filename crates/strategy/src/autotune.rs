use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::{debug, info};

/// Regime detector — identifies current market state.
///
/// Different regimes require different MM parameters:
/// - Quiet: tight spreads, aggressive quoting
/// - Trending: wider spreads, inventory management priority
/// - Volatile: wide spreads, reduced size, fast refresh
/// - Mean-reverting: tighter spreads, larger size
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketRegime {
    Quiet,
    Trending,
    Volatile,
    MeanReverting,
}

/// Detects market regime from recent price data.
pub struct RegimeDetector {
    /// Recent returns for analysis.
    returns: VecDeque<Decimal>,
    window: usize,
    current_regime: MarketRegime,
}

impl RegimeDetector {
    pub fn new(window: usize) -> Self {
        Self {
            returns: VecDeque::with_capacity(window),
            window,
            current_regime: MarketRegime::Quiet,
        }
    }

    /// Feed a new return observation (e.g., per-tick return).
    pub fn update(&mut self, ret: Decimal) {
        self.returns.push_back(ret);
        if self.returns.len() > self.window {
            self.returns.pop_front();
        }
        if self.returns.len() >= self.window / 2 {
            self.current_regime = self.detect();
        }
    }

    pub fn regime(&self) -> MarketRegime {
        self.current_regime
    }

    fn detect(&self) -> MarketRegime {
        if self.returns.len() < 10 {
            return MarketRegime::Quiet;
        }

        let n = Decimal::from(self.returns.len() as u64);
        let mean: Decimal = self.returns.iter().sum::<Decimal>() / n;

        // Variance of returns.
        let variance: Decimal = self
            .returns
            .iter()
            .map(|r| (*r - mean) * (*r - mean))
            .sum::<Decimal>()
            / n;

        // Autocorrelation (lag-1) — positive = trending, negative = mean-reverting.
        let autocorr = self.lag1_autocorrelation();

        // Volatility threshold (annualized rough estimate).
        let vol_threshold_high = dec!(0.0001); // ~50% annualized at 500ms ticks
        let vol_threshold_low = dec!(0.00001); // ~5% annualized

        let regime = if variance > vol_threshold_high {
            if autocorr > dec!(0.1) {
                MarketRegime::Trending
            } else {
                MarketRegime::Volatile
            }
        } else if variance < vol_threshold_low {
            MarketRegime::Quiet
        } else if autocorr < dec!(-0.1) {
            MarketRegime::MeanReverting
        } else {
            MarketRegime::Quiet
        };

        if regime != self.current_regime {
            info!(
                from = ?self.current_regime,
                to = ?regime,
                variance = %variance,
                autocorr = %autocorr,
                "regime change detected"
            );
        }

        regime
    }

    fn lag1_autocorrelation(&self) -> Decimal {
        if self.returns.len() < 3 {
            return dec!(0);
        }
        let n = self.returns.len();
        let nd = Decimal::from(n as u64);
        let mean: Decimal = self.returns.iter().sum::<Decimal>() / nd;

        let mut cov = dec!(0);
        let mut var = dec!(0);

        let rets: Vec<Decimal> = self.returns.iter().copied().collect();
        for i in 1..n {
            let d0 = rets[i - 1] - mean;
            let d1 = rets[i] - mean;
            cov += d0 * d1;
            var += d0 * d0;
        }

        if var.is_zero() {
            return dec!(0);
        }
        cov / var
    }
}

/// Parameter adjustments per regime.
#[derive(Debug, Clone)]
pub struct RegimeParams {
    /// Multiplier for gamma (risk aversion). >1 = wider spread.
    pub gamma_mult: Decimal,
    /// Multiplier for order size. <1 = smaller orders.
    pub size_mult: Decimal,
    /// Multiplier for minimum spread.
    pub spread_mult: Decimal,
    /// Multiplier for refresh interval. >1 = slower refresh.
    pub refresh_mult: Decimal,
}

impl RegimeParams {
    /// Get parameters for a given regime.
    pub fn for_regime(regime: MarketRegime) -> Self {
        match regime {
            MarketRegime::Quiet => Self {
                gamma_mult: dec!(0.8),   // Tighter spread — capture more.
                size_mult: dec!(1.2),    // Bigger size — more volume.
                spread_mult: dec!(0.8),  // Tighter min spread.
                refresh_mult: dec!(1.0), // Normal refresh.
            },
            MarketRegime::Trending => Self {
                gamma_mult: dec!(2.0),   // Wide spread — protect from adverse.
                size_mult: dec!(0.5),    // Smaller size — less inventory risk.
                spread_mult: dec!(2.0),  // Wide min spread.
                refresh_mult: dec!(0.5), // Fast refresh — adapt quickly.
            },
            MarketRegime::Volatile => Self {
                gamma_mult: dec!(3.0),   // Very wide — vol is high.
                size_mult: dec!(0.3),    // Tiny size — survival mode.
                spread_mult: dec!(3.0),  // Very wide min spread.
                refresh_mult: dec!(0.3), // Very fast refresh.
            },
            MarketRegime::MeanReverting => Self {
                gamma_mult: dec!(0.6),   // Tight — mean reversion is our friend.
                size_mult: dec!(1.5),    // Large size — confident.
                spread_mult: dec!(0.6),  // Tight spread.
                refresh_mult: dec!(1.5), // Slower refresh — less churn.
            },
        }
    }
}

/// Auto-tuner that adjusts strategy parameters based on regime + toxicity.
pub struct AutoTuner {
    pub regime_detector: RegimeDetector,
    /// VPIN-based spread multiplier [1.0, 3.0].
    pub toxicity_spread_mult: Decimal,
}

impl AutoTuner {
    pub fn new(window: usize) -> Self {
        Self {
            regime_detector: RegimeDetector::new(window),
            toxicity_spread_mult: dec!(1),
        }
    }

    /// Update with a new mid-price return.
    pub fn on_return(&mut self, ret: Decimal) {
        self.regime_detector.update(ret);
    }

    /// Set toxicity multiplier from VPIN value.
    /// VPIN in [0, 1] → multiplier in [1.0, 3.0].
    pub fn set_toxicity(&mut self, vpin: Decimal) {
        self.toxicity_spread_mult = dec!(1) + vpin * dec!(2);
        debug!(vpin = %vpin, mult = %self.toxicity_spread_mult, "toxicity spread adjustment");
    }

    /// Get effective gamma multiplier.
    pub fn effective_gamma_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        regime_params.gamma_mult * self.toxicity_spread_mult
    }

    /// Get effective size multiplier.
    pub fn effective_size_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        // Reduce size when toxic.
        let toxicity_size = dec!(2) - self.toxicity_spread_mult; // [1, -1] → invert
        let toxicity_size = toxicity_size.max(dec!(0.2)); // Floor at 0.2x.
        regime_params.size_mult * toxicity_size
    }

    /// Get effective spread multiplier.
    pub fn effective_spread_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        regime_params.spread_mult * self.toxicity_spread_mult
    }

    /// Get effective refresh interval multiplier.
    pub fn effective_refresh_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        regime_params.refresh_mult
    }
}
