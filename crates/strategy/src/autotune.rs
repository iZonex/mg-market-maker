use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::{debug, info};

use crate::features::hurst_exponent;

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

        let heuristic = if variance > vol_threshold_high {
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

        // Hurst cross-check: if the rescaled-range R/S estimator
        // on the returns window gives a confident reading that
        // **disagrees** with the heuristic label, downgrade to
        // `Quiet`. Reason: the heuristic uses a 2-knob (variance
        // + lag-1 autocorrelation) classifier that is noisy on
        // short windows; Hurst is an orthogonal statistical
        // measure of persistence that tends to be right when
        // it is confident. When the two disagree we trust
        // neither and step back — `Quiet`'s parameters are
        // the mildest, so a disagreement never causes a harder
        // setting than the heuristic alone.
        let hurst = self.hurst_label();
        let regime = match (heuristic, hurst) {
            // Both agree, or Hurst is inconclusive → keep the
            // heuristic's answer.
            (h, None) => h,
            (h, Some(hz)) if regimes_agree(h, hz) => h,
            // Disagreement → downgrade to Quiet.
            (h, Some(hz)) => {
                debug!(
                    heuristic = ?h,
                    hurst = ?hz,
                    "regime disagreement — downgrading to Quiet"
                );
                MarketRegime::Quiet
            }
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

    /// Classify the returns window via the Hurst exponent.
    /// Returns `None` when the window is too short or the
    /// Hurst estimate is too uncertain (its 95 % confidence
    /// interval straddles `0.5` widely) to be useful as a
    /// cross-check.
    fn hurst_label(&self) -> Option<MarketRegime> {
        // Hurst needs at least 20 samples per the module
        // guard — reject smaller windows early so the
        // conversion to f64 doesn't fire on a degenerate series.
        if self.returns.len() < 20 {
            return None;
        }
        let series: Vec<f64> = self
            .returns
            .iter()
            .map(|r| r.to_f64().unwrap_or(0.0))
            .collect();
        let result = hurst_exponent(&series)?;

        // The full 95 % CI spans 4 × se_slope. Demand that at
        // least one of the bounds sit clearly on one side of
        // 0.5 before trusting the reading; an estimate whose
        // CI covers both mean-reversion and trending regimes
        // is useless as a tiebreaker.
        let (lo, hi) = result.ci_95;
        if hi <= 0.45 {
            // Clearly mean-reverting: the full CI is below
            // the random-walk line.
            Some(MarketRegime::MeanReverting)
        } else if lo >= 0.55 {
            // Clearly trending: the full CI is above the
            // random-walk line.
            Some(MarketRegime::Trending)
        } else if lo >= 0.45 && hi <= 0.55 {
            // Tightly centred around 0.5 → random walk = quiet
            // from a persistence standpoint.
            Some(MarketRegime::Quiet)
        } else {
            // CI is too wide to commit — leave the heuristic
            // in charge.
            None
        }
    }

    /// Test-only accessor for the Hurst cross-check so unit
    /// tests can pin the classifier without having to force
    /// the heuristic into a particular branch.
    #[cfg(test)]
    pub(crate) fn hurst_label_for_test(&self) -> Option<MarketRegime> {
        self.hurst_label()
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

/// Heuristic and Hurst agree iff they point at the same
/// "direction" — persistent / anti-persistent / flat. We
/// deliberately tolerate small mis-labelings (Volatile is
/// neither, Trending and Quiet are both on the persistent /
/// non-reverting side in our taxonomy) and focus on rejecting
/// the sharp contradictions (heuristic says trending, Hurst
/// says mean-reverting, or vice versa).
fn regimes_agree(heuristic: MarketRegime, hurst: MarketRegime) -> bool {
    use MarketRegime::*;
    match (heuristic, hurst) {
        // Trivial match.
        (a, b) if a == b => true,
        // Volatile and Quiet are both "no persistent
        // direction" → compatible with Hurst's Quiet.
        (Volatile, Quiet) | (Quiet, Volatile) => true,
        // Trending vs Quiet: heuristic sees momentum, Hurst
        // does not. Not a direct contradiction, let it pass —
        // momentum can live inside a random-walk shell.
        (Trending, Quiet) | (Quiet, Trending) => true,
        // MeanReverting vs Quiet similarly.
        (MeanReverting, Quiet) | (Quiet, MeanReverting) => true,
        // The real contradictions: Trending ↔ MeanReverting,
        // Volatile ↔ MeanReverting (on a mean-reverting series
        // "volatile" is a misclassification because variance is
        // on its own), Trending ↔ Volatile with clashing
        // persistence calls.
        _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- regimes_agree partition ----

    #[test]
    fn regimes_agree_on_identity() {
        for r in [
            MarketRegime::Quiet,
            MarketRegime::Trending,
            MarketRegime::Volatile,
            MarketRegime::MeanReverting,
        ] {
            assert!(regimes_agree(r, r));
        }
    }

    #[test]
    fn regimes_agree_tolerates_quiet_pairings() {
        // Quiet is compatible with every other label in our
        // "disagreement = downgrade" policy.
        use MarketRegime::*;
        assert!(regimes_agree(Volatile, Quiet));
        assert!(regimes_agree(Trending, Quiet));
        assert!(regimes_agree(MeanReverting, Quiet));
    }

    #[test]
    fn regimes_disagree_on_trending_vs_mean_reverting() {
        // Trending vs MeanReverting is the sharpest
        // contradiction — heuristic says the series is going
        // one way, Hurst says the other. Must reject.
        use MarketRegime::*;
        assert!(!regimes_agree(Trending, MeanReverting));
        assert!(!regimes_agree(MeanReverting, Trending));
    }

    #[test]
    fn regimes_disagree_on_volatile_vs_mean_reverting() {
        use MarketRegime::*;
        assert!(!regimes_agree(Volatile, MeanReverting));
        assert!(!regimes_agree(MeanReverting, Volatile));
    }

    // ---- Hurst-driven downgrade in RegimeDetector::detect ----

    // ---- hurst_label direct unit tests ----

    fn push(det: &mut RegimeDetector, r: Decimal, n: usize) {
        for _ in 0..n {
            det.update(r);
        }
    }

    #[test]
    fn hurst_label_none_on_tiny_window() {
        let mut det = RegimeDetector::new(200);
        // Fewer than 20 samples — the Hurst helper returns
        // `None` by construction (the inner `hurst_exponent`
        // guard rejects short series).
        for i in 0..15 {
            det.update(Decimal::from(i) / dec!(10000));
        }
        assert!(det.hurst_label_for_test().is_none());
    }

    #[test]
    fn hurst_label_trending_on_monotonic_returns() {
        // A monotonically increasing return series yields
        // `H ≈ 1` under R/S analysis — the label must be
        // Trending when the 95 % CI's lower bound sits
        // above 0.55.
        let mut det = RegimeDetector::new(500);
        for i in 0..500 {
            det.update(Decimal::from(i));
        }
        let label = det.hurst_label_for_test();
        assert!(
            matches!(label, Some(MarketRegime::Trending)),
            "expected Trending on monotone returns, got {label:?}"
        );
    }

    #[test]
    fn hurst_label_iid_white_noise_is_usually_quiet_or_none() {
        // iid ±1 via xorshift popcount parity. Hurst lands
        // near 0.5 and the CI either tightly brackets 0.5
        // (→ Quiet) or is wide enough that the classifier
        // returns `None`. Either is acceptable — the contract
        // is "do NOT label iid white noise as Trending or
        // MeanReverting".
        let mut det = RegimeDetector::new(2000);
        let mut state: u64 = 0x1234_5678_9abc_def0;
        for _ in 0..2000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let parity = state.count_ones() & 1;
            det.update(if parity == 0 { dec!(-1) } else { dec!(1) });
        }
        let label = det.hurst_label_for_test();
        assert!(
            !matches!(
                label,
                Some(MarketRegime::Trending) | Some(MarketRegime::MeanReverting)
            ),
            "iid white noise should not classify as trending / mean-reverting, got {label:?}"
        );
    }

    /// End-to-end detect(): a mean-reverting alternating
    /// stream with high variance. Heuristic sees MeanReverting
    /// directly (autocorr < -0.1). Hurst on the same stream
    /// either agrees → final is MeanReverting, or is noisy
    /// and downgrades → final is Quiet. Either way the result
    /// must NOT be Trending or Volatile — that's the safety
    /// contract the Hurst cross-check enforces.
    #[test]
    fn detect_never_labels_mean_reverting_returns_as_trending() {
        let mut det = RegimeDetector::new(200);
        for i in 0..200 {
            let r = if i % 2 == 0 { dec!(0.03) } else { dec!(-0.03) };
            det.update(r);
        }
        let regime = det.regime();
        assert!(
            !matches!(regime, MarketRegime::Trending | MarketRegime::Volatile),
            "detector must not label an alternating stream as Trending/Volatile, got {regime:?}"
        );
    }

    /// Feed an obviously quiet series (all returns very
    /// small). Heuristic lands in `Quiet` via the low-variance
    /// branch. The detector must not promote this to a more
    /// aggressive regime.
    #[test]
    fn quiet_returns_stay_quiet_after_hurst_check() {
        let mut det = RegimeDetector::new(200);
        push(&mut det, dec!(0.0000001), 200);
        assert_eq!(det.regime(), MarketRegime::Quiet);
    }

    /// Iid white-noise returns → heuristic likely lands on
    /// Quiet or Volatile. Hurst on white noise sits near 0.5
    /// with a narrow CI → label is `Quiet`. Agreement path —
    /// no downgrade. Simply verifies the detector does not
    /// break under the common "nothing to see" input.
    #[test]
    fn white_noise_returns_do_not_produce_panic() {
        let mut det = RegimeDetector::new(200);
        // Deterministic ±0.00001 iid via xorshift + popcount.
        let mut state: u64 = 0x5555_aaaa_1234_5678;
        for _ in 0..200 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let parity = state.count_ones() & 1;
            let r = if parity == 0 {
                dec!(-0.00001)
            } else {
                dec!(0.00001)
            };
            det.update(r);
        }
        // Just assert `regime()` returns something; the
        // specific label depends on the variance threshold.
        let _ = det.regime();
    }
}
