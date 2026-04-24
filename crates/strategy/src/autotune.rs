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

    /// 22B-3 — snapshot the returns ring + current regime so a
    /// restart doesn't drop the detection window (default 100
    /// samples) and default the regime back to `Quiet`. Without
    /// this, a 10-sample minimum is required before the detector
    /// re-classifies anything — up to ~10 minutes of Quiet
    /// mis-labelling before the window refills.
    pub fn snapshot_state(&self) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "window": self.window,
            "current_regime": regime_label(self.current_regime),
            "returns": self.returns.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
        })
    }

    /// 22B-3 — restore the detector from a prior snapshot.
    /// Silently truncates a returns buffer that exceeds the
    /// current `window` cap so operators can shrink the detector
    /// window without a restore failure.
    pub fn restore_state(&mut self, state: &serde_json::Value) -> Result<(), String> {
        let schema = state.get("schema_version").and_then(|v| v.as_u64());
        if schema != Some(1) {
            return Err(format!(
                "autotune checkpoint has unsupported schema_version {schema:?}"
            ));
        }
        let regime_str = state
            .get("current_regime")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "autotune: missing current_regime".to_string())?;
        let current_regime = parse_regime(regime_str)
            .ok_or_else(|| format!("autotune: unknown regime '{regime_str}'"))?;
        let mut returns: VecDeque<Decimal> = state
            .get("returns")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "autotune: missing returns".to_string())?
            .iter()
            .filter_map(|v| v.as_str()?.parse::<Decimal>().ok())
            .collect();
        while returns.len() > self.window {
            returns.pop_front();
        }
        self.returns = returns;
        self.current_regime = current_regime;
        Ok(())
    }
}

fn regime_label(r: MarketRegime) -> &'static str {
    match r {
        MarketRegime::Quiet => "quiet",
        MarketRegime::Trending => "trending",
        MarketRegime::Volatile => "volatile",
        MarketRegime::MeanReverting => "mean_reverting",
    }
}

fn parse_regime(s: &str) -> Option<MarketRegime> {
    match s {
        "quiet" => Some(MarketRegime::Quiet),
        "trending" => Some(MarketRegime::Trending),
        "volatile" => Some(MarketRegime::Volatile),
        "mean_reverting" => Some(MarketRegime::MeanReverting),
        _ => None,
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
    /// Lead-lag soft-widen multiplier (Epic F sub-component
    /// #1). Default `1.0` (no widening). Engine pushes the
    /// latest `LeadLagGuard::current_multiplier()` here on
    /// every leader-side mid update.
    lead_lag_mult: Decimal,
    /// News-retreat soft-widen multiplier (Epic F sub-component
    /// #2). Default `1.0`. Engine pushes the latest
    /// `NewsRetreatStateMachine::current_multiplier()` on
    /// every state-machine read.
    news_retreat_mult: Decimal,
    /// Product-specific widening multiplier (Epic 40.8). Perp
    /// order flow is measurably more informed than spot — VPIN /
    /// Kyle's λ run 1.3–1.5× hotter on the same book depth.
    /// Default `1.0` (spot); engine sets ~`1.4` on perp so the
    /// toxicity response keeps us out of informed-flow fills.
    product_widen_mult: Decimal,
    /// Epic G — spread-widen multiplier from the
    /// `SocialRiskEngine`'s fused sentiment + market signal.
    /// Clamped at `1.0` on the low side (never narrow),
    /// capped at `max_vol_multiplier` (default 3.0) on the
    /// high side inside the risk engine itself.
    social_spread_mult: Decimal,
    /// Epic G — size-shrink multiplier from the
    /// `SocialRiskEngine`. Unlike widening, this CAN be
    /// below 1.0 (it SHRINKS quotes under crowd spikes).
    /// Floored at `min_size_multiplier` by the risk engine.
    social_size_mult: Decimal,
    /// Epic H — spread multiplier produced by a user-
    /// authored strategy graph. Layered on top of all the
    /// hand-wired multipliers; floored at 1.0 same as the
    /// others (external signals cannot *narrow* the spread).
    graph_spread_mult: Decimal,
    /// Epic H — size multiplier from the graph.
    graph_size_mult: Decimal,
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
            lead_lag_mult: dec!(1),
            news_retreat_mult: dec!(1),
            product_widen_mult: dec!(1),
            social_spread_mult: dec!(1),
            social_size_mult: dec!(1),
            graph_spread_mult: dec!(1),
            graph_size_mult: dec!(1),
        }
    }

    /// Set the product-specific toxicity-widen multiplier. Pass
    /// `1.0` for spot (default), `1.4` for perp per Epic-40.8
    /// research (informed-flow density ≈ 1.3–1.5× higher on perp
    /// than spot for the same book depth). Engine calls this
    /// once at startup based on `SymbolConfig::product`.
    pub fn set_product_widen_mult(&mut self, mult: Decimal) {
        self.product_widen_mult = mult.max(dec!(1.0));
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

    /// Get effective size multiplier. Regime × toxicity-
    /// derived shrink × social-risk shrink. The social layer
    /// is multiplicative with the existing toxicity path so
    /// a simultaneously toxic + socially-spiked market
    /// compounds the size cut (exactly the desired
    /// "tighten-up" behaviour from Epic G).
    pub fn effective_size_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        // Reduce size when toxic.
        let toxicity_size = dec!(2) - self.toxicity_spread_mult; // [1, -1] → invert
        let toxicity_size = toxicity_size.max(dec!(0.2)); // Floor at 0.2x.
        regime_params.size_mult * toxicity_size * self.social_size_mult * self.graph_size_mult
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

    /// Push a fresh lead-lag-guard multiplier (Epic F #1).
    /// Engine wires the latest
    /// `LeadLagGuard::current_multiplier()` after every
    /// leader-side mid update. Values below `1.0` clamp to
    /// `1.0` — the lead-lag guard never *narrows* the
    /// spread, only widens.
    pub fn set_lead_lag_mult(&mut self, mult: Decimal) {
        self.lead_lag_mult = mult.max(Decimal::ONE);
    }

    /// Read the cached lead-lag multiplier. Mainly for tests
    /// and operator dashboards.
    pub fn lead_lag_mult(&self) -> Decimal {
        self.lead_lag_mult
    }

    /// Push a fresh news-retreat multiplier (Epic F #2).
    /// Engine wires the latest
    /// `NewsRetreatStateMachine::current_multiplier()` on
    /// every state-machine read. Same clamp-at-1.0 invariant
    /// as [`Self::set_lead_lag_mult`].
    pub fn set_news_retreat_mult(&mut self, mult: Decimal) {
        self.news_retreat_mult = mult.max(Decimal::ONE);
    }

    /// Read the cached news-retreat multiplier.
    pub fn news_retreat_mult(&self) -> Decimal {
        self.news_retreat_mult
    }

    /// Epic G — push the spread widening coming out of the
    /// `SocialRiskEngine`. Floor at 1.0 (same invariant as
    /// lead-lag / news-retreat: external multipliers can
    /// only WIDEN).
    pub fn set_social_spread_mult(&mut self, mult: Decimal) {
        self.social_spread_mult = mult.max(Decimal::ONE);
    }

    /// Read the cached social-risk spread multiplier.
    pub fn social_spread_mult(&self) -> Decimal {
        self.social_spread_mult
    }

    /// Epic G — push the size-shrink coming out of the
    /// `SocialRiskEngine`. Clamped to `(0, 1]`: social can
    /// only SHRINK quotes, never grow them past baseline.
    pub fn set_social_size_mult(&mut self, mult: Decimal) {
        self.social_size_mult = mult.max(dec!(0.1)).min(dec!(1));
    }

    /// Read the cached social-risk size multiplier.
    pub fn social_size_mult(&self) -> Decimal {
        self.social_size_mult
    }

    /// Epic H — push the spread multiplier emitted by a
    /// strategy graph's `Out.SpreadMult` sink. Floored at
    /// 1.0 (no narrowing via graphs).
    pub fn set_graph_spread_mult(&mut self, mult: Decimal) {
        self.graph_spread_mult = mult.max(Decimal::ONE);
    }

    pub fn graph_spread_mult(&self) -> Decimal {
        self.graph_spread_mult
    }

    /// Epic H — push the size multiplier from a strategy
    /// graph's `Out.SizeMult` sink. Clamped to `(0.1, 1]`
    /// (same invariant as social).
    pub fn set_graph_size_mult(&mut self, mult: Decimal) {
        self.graph_size_mult = mult.max(dec!(0.1)).min(dec!(1));
    }

    pub fn graph_size_mult(&self) -> Decimal {
        self.graph_size_mult
    }

    /// Get effective spread multiplier. The product is:
    /// `regime · toxicity · (1 / max(mr, 0.2)) · lead_lag · news_retreat · product`.
    /// When `lead_lag`, `news_retreat`, and `product` are at
    /// their defaults (`1.0`) the formula is byte-identical to
    /// the pre-Epic-F shape. `product` applies on perp only
    /// (Epic 40.8).
    pub fn effective_spread_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        let base = regime_params.spread_mult * self.toxicity_spread_mult;
        let with_mr = if let Some(mr) = self.market_resilience {
            let floor = dec!(0.2);
            base / mr.max(floor)
        } else {
            base
        };
        with_mr
            * self.lead_lag_mult
            * self.news_retreat_mult
            * self.product_widen_mult
            * self.social_spread_mult
            * self.graph_spread_mult
    }

    /// Get effective refresh interval multiplier.
    pub fn effective_refresh_mult(&self) -> Decimal {
        let regime_params = RegimeParams::for_regime(self.regime_detector.regime());
        regime_params.refresh_mult
    }
}

#[cfg(test)]
mod tests;
