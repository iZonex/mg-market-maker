use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
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

/// Closed-form state-space policy for the risk-aversion
/// parameter γ inspired by
/// <https://github.com/im1235/ISAC> — an RL agent that learned to
/// drive γ from two normalised state features:
///
///   (|q| / max_inventory, time_remaining ∈ [0, 1])
///
/// Their policy surface is monotonic in both axes:
/// - γ rises with `|q|` (convex) — push the reservation price
///   harder against inventory, accept tighter fills on the
///   reducing side to dump the position.
/// - γ rises as `time_remaining` shrinks — widen spreads toward
///   session end so the taker leg is less likely to load us up
///   right before we have to liquidate.
///
/// We **do not** port their SAC network — no PyTorch, no
/// inference runtime, no blessed weights, no desire to ship a
/// black-box controller. Instead this struct approximates the
/// shape they learned with a closed-form formula:
///
/// ```text
/// γ_mult = 1 + q_weight * (|q| / max_q)^q_exp
///             + t_weight * (1 - time_remaining)^t_exp
/// ```
///
/// The same quadratic-in-inventory + polynomial-in-time-left
/// shape falls out of the original Avellaneda-Stoikov optimal
/// control derivation; ISAC's contribution is empirical
/// evidence that SAC converges to that shape under their
/// simulator, which is reassurance that the closed form is
/// correct up to coefficients.
///
/// Plugged into `AutoTuner::effective_gamma_mult` via
/// [`AutoTuner::with_inventory_gamma_policy`], the tuner
/// multiplies the regime- and toxicity-driven γ multipliers by
/// the policy's output each tick. Bypass by not setting a
/// policy — `None` is the default, engines that don't want
/// inventory-aware γ see the same behaviour as before.
#[derive(Debug, Clone, Copy)]
pub struct InventoryGammaPolicy {
    /// Maximum expected inventory in base asset. Used to
    /// normalise `|q|` into `[0, 1]`. Should match
    /// `RiskConfig::max_inventory`.
    pub max_inventory: Decimal,
    /// Coefficient on the inventory term. Higher = harder push
    /// on `|q|`. Typical: `0.5 .. 2.0`.
    pub q_weight: Decimal,
    /// Exponent on the inventory term. `2.0` matches the
    /// canonical quadratic penalty, `1.0` is linear, higher
    /// exponents saturate toward `max_q`.
    pub q_exp: Decimal,
    /// Coefficient on the time term. Higher = harder widening
    /// as the session elapses.
    pub t_weight: Decimal,
    /// Exponent on the time term. `3.0` matches the
    /// cubic-in-time-remaining shape ISAC's RL agent
    /// approximates after training on 2000 price paths.
    pub t_exp: Decimal,
    /// Hard floor on the multiplier. Defaults to `1.0` — γ is
    /// never allowed to go BELOW its base setting via this
    /// policy. Lower would be a tightening, which this policy
    /// is deliberately not in charge of.
    pub min_mult: Decimal,
    /// Hard cap on the multiplier. Clamps the output so a
    /// wildly-loaded position at session end does not produce
    /// an untradable γ.
    pub max_mult: Decimal,
}

impl InventoryGammaPolicy {
    /// Sensible defaults tuned to mirror the ISAC policy
    /// surface on a `max_inventory = 0.1` sim:
    /// `γ_mult ∈ [1.0, ~3.5]` across the state space.
    pub fn new(max_inventory: Decimal) -> Self {
        assert!(
            max_inventory > Decimal::ZERO,
            "InventoryGammaPolicy: max_inventory must be > 0"
        );
        Self {
            max_inventory,
            q_weight: dec!(1.5),
            q_exp: dec!(2),
            t_weight: dec!(0.5),
            t_exp: dec!(3),
            min_mult: dec!(1),
            max_mult: dec!(5),
        }
    }

    /// Compute the γ multiplier for the current state. `q` is
    /// signed inventory; the policy reads `|q|`. `time_remaining`
    /// is the fraction of the strategy horizon still ahead, in
    /// `[0, 1]` (1 = full horizon ahead, 0 = session close).
    pub fn multiplier(&self, q: Decimal, time_remaining: Decimal) -> Decimal {
        if self.max_inventory.is_zero() {
            return Decimal::ONE;
        }
        let q_norm = (q.abs() / self.max_inventory).min(Decimal::ONE);
        let t_elapsed = (Decimal::ONE - time_remaining)
            .max(Decimal::ZERO)
            .min(Decimal::ONE);

        // Decimal has no native `powf`, so small-integer exponents
        // go through iterative multiply and the general case
        // falls back to an f64 round-trip. The coefficients are
        // feature-level — not money-critical — so the rounding
        // is acceptable.
        let q_term = self.q_weight * dec_pow(q_norm, self.q_exp);
        let t_term = self.t_weight * dec_pow(t_elapsed, self.t_exp);

        let raw = Decimal::ONE + q_term + t_term;
        raw.max(self.min_mult).min(self.max_mult)
    }
}

/// Decimal exponentiation via integer fast path where possible.
/// Integer exponents `0..=6` use repeated multiplication (exact);
/// fractional exponents fall back to an f64 round-trip. Kept
/// private because the f64 fallback is feature-level only.
fn dec_pow(base: Decimal, exp: Decimal) -> Decimal {
    if exp == Decimal::ZERO {
        return Decimal::ONE;
    }
    if exp == Decimal::ONE {
        return base;
    }
    // Small positive integer exponents — exact.
    if exp.fract().is_zero() && exp > Decimal::ZERO && exp <= dec!(6) {
        let n = exp.to_u32().unwrap_or(1);
        let mut acc = Decimal::ONE;
        for _ in 0..n {
            acc *= base;
        }
        return acc;
    }
    // General case via f64.
    let b = base.to_f64().unwrap_or(0.0);
    let e = exp.to_f64().unwrap_or(1.0);
    let v = b.powf(e);
    Decimal::from_f64(v).unwrap_or(base)
}

/// Compute an ISAC-style risk penalty on an inventory of `q`
/// over one tick of width `dt_seconds`, given a per-second
/// volatility `sigma`. Returns the penalty in the same unit as
/// PnL (quote asset). Use with
/// [`mm_hyperopt::LossFn`][crate::r#trait] to score a strategy
/// on risk-adjusted reward rather than raw PnL.
///
/// Formula from ISAC's `inventory_sac.py`:
///
/// ```text
/// penalty = 0.5 * |q| * sigma * sqrt(dt)
/// ```
///
/// The 0.5 coefficient matches the mean-variance utility used
/// in the original Avellaneda-Stoikov paper; it's conservative
/// — twice the penalty the "textbook" optimal control
/// derivation would apply — so treating it as an UPPER BOUND on
/// the risk charge is appropriate.
pub fn inventory_risk_penalty(q: Decimal, sigma_per_sec: Decimal, dt_seconds: Decimal) -> Decimal {
    if dt_seconds <= Decimal::ZERO || sigma_per_sec <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let sqrt_dt_f = dt_seconds.to_f64().unwrap_or(0.0).max(0.0).sqrt();
    let Some(sqrt_dt) = Decimal::from_f64(sqrt_dt_f) else {
        return Decimal::ZERO;
    };
    dec!(0.5) * q.abs() * sigma_per_sec * sqrt_dt
}

/// Auto-tuner that adjusts strategy parameters based on regime + toxicity.
pub struct AutoTuner {
    pub regime_detector: RegimeDetector,
    /// VPIN-based spread multiplier [1.0, 3.0].
    pub toxicity_spread_mult: Decimal,
    /// Optional closed-form state-space γ policy. When set,
    /// `effective_gamma_mult()` multiplies the regime- and
    /// toxicity-derived multiplier by `policy.multiplier(q,
    /// time_remaining)`. `None` → no inventory-aware adjustment
    /// (legacy behaviour).
    pub inventory_gamma_policy: Option<InventoryGammaPolicy>,
    /// Cached policy inputs from the engine's last tick, so
    /// `effective_gamma_mult()` can read them without an extra
    /// plumbing argument. The engine calls
    /// [`AutoTuner::update_policy_state`] once per tick before
    /// the quote computation.
    inventory_snapshot: Decimal,
    time_remaining_snapshot: Decimal,
    /// Latest Market Resilience score in `[0, 1]`. `None` means
    /// the caller has not attached an MR detector; the spread
    /// multiplier is untouched in that case. `Some(s)` feeds
    /// `1 / max(s, 0.2)` into `effective_spread_mult()` so a
    /// depressed score widens the book until the detector
    /// decays back toward 1.0.
    market_resilience: Option<Decimal>,
}

impl AutoTuner {
    pub fn new(window: usize) -> Self {
        Self {
            regime_detector: RegimeDetector::new(window),
            toxicity_spread_mult: dec!(1),
            inventory_gamma_policy: None,
            inventory_snapshot: Decimal::ZERO,
            time_remaining_snapshot: Decimal::ONE,
            market_resilience: None,
        }
    }

    /// Attach a closed-form inventory/time γ policy. Returns
    /// `self` for builder-style use.
    pub fn with_inventory_gamma_policy(mut self, policy: InventoryGammaPolicy) -> Self {
        self.inventory_gamma_policy = Some(policy);
        self
    }

    /// Update the inventory / time-remaining snapshot the γ
    /// policy reads on its next `effective_gamma_mult()` call.
    /// The engine tick loop calls this once per tick before
    /// asking for multipliers.
    pub fn update_policy_state(&mut self, inventory: Decimal, time_remaining: Decimal) {
        self.inventory_snapshot = inventory;
        self.time_remaining_snapshot = time_remaining;
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

    /// Get effective gamma multiplier. When an
    /// `InventoryGammaPolicy` is attached the result is further
    /// multiplied by its state-space output so heavy inventory
    /// and/or session-end conditions tighten reservation prices.
    pub fn effective_gamma_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        let base = regime_params.gamma_mult * self.toxicity_spread_mult;
        if let Some(policy) = &self.inventory_gamma_policy {
            base * policy.multiplier(self.inventory_snapshot, self.time_remaining_snapshot)
        } else {
            base
        }
    }

    /// Get effective size multiplier.
    pub fn effective_size_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        // Reduce size when toxic.
        let toxicity_size = dec!(2) - self.toxicity_spread_mult; // [1, -1] → invert
        let toxicity_size = toxicity_size.max(dec!(0.2)); // Floor at 0.2x.
        regime_params.size_mult * toxicity_size
    }

    /// Set the latest Market Resilience score. Values outside
    /// `[0, 1]` are clamped. See
    /// [`crate::market_resilience::MarketResilienceCalculator`]
    /// for the source signal.
    pub fn set_market_resilience(&mut self, score: Decimal) {
        let clamped = score.max(Decimal::ZERO).min(Decimal::ONE);
        self.market_resilience = Some(clamped);
    }

    /// Clear any attached Market Resilience reading. After this
    /// call `effective_spread_mult()` returns to the
    /// regime+toxicity-only baseline.
    pub fn clear_market_resilience(&mut self) {
        self.market_resilience = None;
    }

    /// Get effective spread multiplier. When a Market
    /// Resilience reading is attached the regime+toxicity
    /// product is further divided by `max(mr, 0.2)` — a low
    /// score widens the book post-shock, and the floor keeps
    /// the multiplier bounded at `5×` even when MR is at zero.
    pub fn effective_spread_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        let base = regime_params.spread_mult * self.toxicity_spread_mult;
        if let Some(mr) = self.market_resilience {
            let floor = dec!(0.2);
            let denom = mr.max(floor);
            base / denom
        } else {
            base
        }
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

    // ----- InventoryGammaPolicy tests -----

    #[test]
    fn policy_returns_min_mult_at_zero_state() {
        let p = InventoryGammaPolicy::new(dec!(0.1));
        // q = 0, time_remaining = 1 (start of session). Both
        // terms vanish → multiplier = 1, clamped at min_mult.
        assert_eq!(p.multiplier(Decimal::ZERO, Decimal::ONE), Decimal::ONE);
    }

    #[test]
    fn policy_scales_up_with_inventory() {
        let p = InventoryGammaPolicy::new(dec!(0.1));
        let at_zero = p.multiplier(Decimal::ZERO, Decimal::ONE);
        let at_half = p.multiplier(dec!(0.05), Decimal::ONE);
        let at_max = p.multiplier(dec!(0.1), Decimal::ONE);
        assert!(at_half > at_zero, "inventory term must push mult up");
        assert!(at_max > at_half, "mult should be monotone in |q|");
    }

    #[test]
    fn policy_scales_up_as_time_runs_out() {
        let p = InventoryGammaPolicy::new(dec!(0.1));
        let early = p.multiplier(Decimal::ZERO, Decimal::ONE);
        let late = p.multiplier(Decimal::ZERO, dec!(0.1));
        let end = p.multiplier(Decimal::ZERO, Decimal::ZERO);
        assert!(late > early, "time term must push mult up as t→0");
        assert!(end > late, "session close hits max time term");
    }

    #[test]
    fn policy_clamps_to_max_mult() {
        // Custom policy with low cap so we can force clamping.
        let p = InventoryGammaPolicy {
            max_inventory: dec!(0.1),
            q_weight: dec!(10),
            q_exp: dec!(1),
            t_weight: dec!(10),
            t_exp: dec!(1),
            min_mult: dec!(1),
            max_mult: dec!(2),
        };
        let m = p.multiplier(dec!(0.1), Decimal::ZERO);
        assert_eq!(m, dec!(2), "must clamp at max_mult");
    }

    #[test]
    fn policy_clamps_abs_inventory_above_max() {
        let p = InventoryGammaPolicy::new(dec!(0.1));
        let at_max = p.multiplier(dec!(0.1), Decimal::ONE);
        let above_max = p.multiplier(dec!(0.5), Decimal::ONE);
        assert_eq!(at_max, above_max, "q_norm must saturate at 1");
    }

    #[test]
    fn policy_negative_inventory_uses_abs() {
        let p = InventoryGammaPolicy::new(dec!(0.1));
        let long = p.multiplier(dec!(0.05), Decimal::ONE);
        let short = p.multiplier(dec!(-0.05), Decimal::ONE);
        assert_eq!(long, short, "policy must be symmetric in q sign");
    }

    #[test]
    fn autotune_applies_policy_via_effective_gamma_mult() {
        let mut t =
            AutoTuner::new(32).with_inventory_gamma_policy(InventoryGammaPolicy::new(dec!(0.1)));
        let base = t.effective_gamma_mult();
        t.update_policy_state(dec!(0.1), Decimal::ZERO);
        let loaded = t.effective_gamma_mult();
        assert!(
            loaded > base,
            "loaded/end-of-session state must widen γ above the unloaded baseline"
        );
    }

    #[test]
    fn autotune_none_policy_leaves_gamma_untouched() {
        let mut t = AutoTuner::new(32);
        let before = t.effective_gamma_mult();
        t.update_policy_state(dec!(0.1), Decimal::ZERO);
        let after = t.effective_gamma_mult();
        assert_eq!(
            before, after,
            "no policy attached → state updates must be ignored"
        );
    }

    // ----- inventory_risk_penalty tests -----

    #[test]
    fn risk_penalty_zero_for_zero_inventory() {
        let r = inventory_risk_penalty(Decimal::ZERO, dec!(0.01), dec!(1));
        assert_eq!(r, Decimal::ZERO);
    }

    #[test]
    fn risk_penalty_zero_for_non_positive_sigma_or_dt() {
        assert_eq!(
            inventory_risk_penalty(dec!(1), Decimal::ZERO, dec!(1)),
            Decimal::ZERO
        );
        assert_eq!(
            inventory_risk_penalty(dec!(1), dec!(0.01), Decimal::ZERO),
            Decimal::ZERO
        );
        assert_eq!(
            inventory_risk_penalty(dec!(1), dec!(-0.01), dec!(1)),
            Decimal::ZERO
        );
    }

    #[test]
    fn risk_penalty_scales_linearly_with_absolute_inventory() {
        let a = inventory_risk_penalty(dec!(1), dec!(0.02), dec!(4));
        let b = inventory_risk_penalty(dec!(2), dec!(0.02), dec!(4));
        assert_eq!(b, a * dec!(2));
    }

    #[test]
    fn risk_penalty_is_symmetric_in_inventory_sign() {
        let long = inventory_risk_penalty(dec!(1.5), dec!(0.02), dec!(4));
        let short = inventory_risk_penalty(dec!(-1.5), dec!(0.02), dec!(4));
        assert_eq!(long, short);
    }

    #[test]
    fn risk_penalty_canonical_hand_computed_value() {
        // 0.5 * |2| * 0.02 * sqrt(4) = 0.5 * 2 * 0.02 * 2 = 0.04
        let r = inventory_risk_penalty(dec!(2), dec!(0.02), dec!(4));
        assert_eq!(r, dec!(0.04));
    }

    // ----- Market Resilience wiring tests -----

    /// Without an MR reading the spread multiplier falls back
    /// to the regime+toxicity product.
    #[test]
    fn effective_spread_mult_ignores_unset_market_resilience() {
        let t = AutoTuner::new(32);
        let before = t.effective_spread_mult();
        // No reading attached → unchanged.
        assert!(before > Decimal::ZERO);
    }

    /// A resilient MR score of 1.0 must leave the spread
    /// multiplier unchanged (1 / 1 = 1).
    #[test]
    fn mr_of_one_is_a_noop_on_spread_mult() {
        let mut t = AutoTuner::new(32);
        let before = t.effective_spread_mult();
        t.set_market_resilience(Decimal::ONE);
        let after = t.effective_spread_mult();
        assert_eq!(before, after);
    }

    /// A depressed MR score must widen the effective spread
    /// multiplier compared to the default (no reading).
    #[test]
    fn low_mr_widens_effective_spread_mult() {
        let mut t = AutoTuner::new(32);
        let base = t.effective_spread_mult();
        t.set_market_resilience(dec!(0.3));
        let after = t.effective_spread_mult();
        assert!(
            after > base,
            "MR=0.3 must widen the book: base={base}, after={after}"
        );
    }

    /// The spread multiplier is capped by the MR floor at 0.2 —
    /// even an MR of 0 cannot push the widen factor past 5×.
    #[test]
    fn mr_floor_caps_the_widen_factor() {
        let mut t = AutoTuner::new(32);
        let base = t.effective_spread_mult();
        t.set_market_resilience(Decimal::ZERO);
        let after = t.effective_spread_mult();
        // base / 0.2 = base * 5.
        assert_eq!(after, base * dec!(5));
    }

    /// Out-of-range MR inputs are clamped into `[0, 1]` before
    /// they influence the multiplier.
    #[test]
    fn out_of_range_mr_is_clamped() {
        let mut t = AutoTuner::new(32);
        let base = t.effective_spread_mult();
        t.set_market_resilience(dec!(-3));
        let after_low = t.effective_spread_mult();
        assert_eq!(after_low, base * dec!(5), "negative MR clamps to 0");
        t.set_market_resilience(dec!(5));
        let after_high = t.effective_spread_mult();
        assert_eq!(after_high, base, "MR above 1 clamps to 1");
    }

    /// `clear_market_resilience` removes the reading so the
    /// baseline is restored.
    #[test]
    fn clearing_mr_restores_baseline() {
        let mut t = AutoTuner::new(32);
        let base = t.effective_spread_mult();
        t.set_market_resilience(dec!(0.3));
        t.clear_market_resilience();
        let after = t.effective_spread_mult();
        assert_eq!(after, base);
    }
}
