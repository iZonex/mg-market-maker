//! Lead-lag guard (Epic F, sub-component #1).
//!
//! Defensive predictive signal: subscribe to a "leader"
//! venue mid feed (typically Binance Futures perpetual for
//! crypto, since the perpetual leads spot by 200-800 ms per
//! Makarov-Schoar 2020) and emit a soft-widen multiplier
//! when the leader makes a sharp move. The follower-side
//! market maker uses the multiplier to widen its quotes
//! BEFORE the cross-venue arb hits.
//!
//! This is the **defensive** form of latency arbitrage. We
//! cannot race HFTs to update quotes faster, but we can
//! retreat preemptively when the leader signals an incoming
//! move.
//!
//! # Math
//!
//! Standard EWMA mean + variance on the leader's per-update
//! returns, then a piecewise-linear ramp on `|z|`:
//!
//! ```text
//! r[t]   = (mid[t] - mid[t-1]) / mid[t-1]
//! μ[t]   = α · r[t] + (1 − α) · μ[t−1]
//! σ²[t]  = α · (r[t] − μ[t−1])² + (1 − α) · σ²[t−1]
//! z[t]   = (r[t] − μ[t−1]) / σ[t−1]
//!
//!          ⎧  1.0                                if |z| < z_min
//! M[t]   = ⎨  1 + (max_mult − 1) · ramp_pos      if z_min ≤ |z| ≤ z_max
//!          ⎩  max_mult                           if |z| > z_max
//! ```
//!
//! where `ramp_pos = (|z| − z_min) / (z_max − z_min)`.
//!
//! Source attribution + sign-convention discussion in
//! `docs/research/defensive-layer-formulas.md`
//! §"Sub-component #1".

use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Tuning knobs for [`LeadLagGuard::new`].
#[derive(Debug, Clone)]
pub struct LeadLagGuardConfig {
    /// EWMA half-life in observation count. Default 20
    /// events ≈ 5 s at a 250 ms tick. Smaller values react
    /// faster to regime changes; larger values smooth more.
    pub half_life_events: usize,
    /// Lower edge of the ramp. `|z| < z_min` produces
    /// `multiplier = 1.0` (no widening). Default 2.0.
    pub z_min: Decimal,
    /// Upper edge of the ramp. `|z| > z_max` produces
    /// `multiplier = max_mult` (saturated). Default 4.0.
    pub z_max: Decimal,
    /// Saturation multiplier at `|z| ≥ z_max`. Default 3.0.
    pub max_mult: Decimal,
}

impl Default for LeadLagGuardConfig {
    fn default() -> Self {
        Self {
            half_life_events: 20,
            z_min: dec!(2),
            z_max: dec!(4),
            max_mult: dec!(3),
        }
    }
}

/// Cont-Stoikov-style cross-venue leader-mid guard. Holds
/// EWMA state for the leader's return process and exposes a
/// scalar multiplier that the follower-side `AutoTuner`
/// folds into its effective spread.
#[derive(Debug, Clone)]
pub struct LeadLagGuard {
    config: LeadLagGuardConfig,
    /// Pre-computed EWMA decay `α` from the half-life.
    alpha: Decimal,
    /// Most recent leader mid, `None` until the first call.
    last_mid: Option<Decimal>,
    /// EWMA mean of `r`, `None` until the first return is
    /// observed (i.e. after the *second* mid sample).
    ewma_mean: Option<Decimal>,
    /// EWMA variance of `r`, `None` until enough updates
    /// (≥ 2 returns) have landed.
    ewma_var: Option<Decimal>,
    /// Cached output of the most recent `recompute` call.
    last_multiplier: Decimal,
    /// `|z|` from the most recent observation, for metrics.
    last_z_abs: Decimal,
    /// Number of return observations folded in. Used only
    /// for the warmup gate on the variance estimator.
    obs_count: usize,
}

impl LeadLagGuard {
    /// Construct a fresh guard with the given config.
    pub fn new(config: LeadLagGuardConfig) -> Self {
        assert!(config.half_life_events > 0, "half_life_events must be > 0");
        assert!(
            config.z_min < config.z_max,
            "z_min must be strictly less than z_max"
        );
        assert!(config.max_mult >= dec!(1), "max_mult must be >= 1.0");
        let alpha = compute_ewma_alpha(config.half_life_events);
        Self {
            config,
            alpha,
            last_mid: None,
            ewma_mean: None,
            ewma_var: None,
            last_multiplier: Decimal::ONE,
            last_z_abs: Decimal::ZERO,
            obs_count: 0,
        }
    }

    /// Fold one new leader-side mid observation. Updates
    /// the EWMA state and recomputes the cached multiplier.
    /// Safe to call at any cadence — the guard is purely
    /// event-driven, no real-time clock dependency.
    pub fn on_leader_mid(&mut self, mid: Decimal) {
        let Some(prev) = self.last_mid else {
            self.last_mid = Some(mid);
            return;
        };
        if prev.is_zero() {
            self.last_mid = Some(mid);
            return;
        }
        // Approximate log return as (mid − prev) / prev.
        // Accuracy is within 1% for |return| < 5%, fine for
        // the soft-widen use case.
        let r = (mid - prev) / prev;
        // Compute z BEFORE folding the new observation so
        // the test is "is this return surprising relative
        // to what we knew BEFORE seeing it" (the canonical
        // unbiased estimator).
        let z_abs = self.compute_z_abs(r);
        self.fold_ewma(r);
        self.last_mid = Some(mid);
        self.last_z_abs = z_abs;
        self.last_multiplier = ramp(
            z_abs,
            self.config.z_min,
            self.config.z_max,
            self.config.max_mult,
        );
        self.obs_count += 1;
    }

    /// Current soft-widen multiplier in `[1, max_mult]`.
    /// `1.0` = no widening (no signal or `|z| < z_min`).
    pub fn current_multiplier(&self) -> Decimal {
        self.last_multiplier
    }

    /// Most recent `|z|` value, for metrics / dashboards.
    pub fn current_z_abs(&self) -> Decimal {
        self.last_z_abs
    }

    /// `true` when `current_multiplier > 1.0`.
    pub fn is_active(&self) -> bool {
        self.last_multiplier > Decimal::ONE
    }

    /// Drop all cached state. Next `on_leader_mid` call
    /// behaves like the very first one.
    pub fn reset(&mut self) {
        self.last_mid = None;
        self.ewma_mean = None;
        self.ewma_var = None;
        self.last_multiplier = Decimal::ONE;
        self.last_z_abs = Decimal::ZERO;
        self.obs_count = 0;
    }

    /// Number of returns folded into the EWMA so far.
    pub fn obs_count(&self) -> usize {
        self.obs_count
    }

    fn compute_z_abs(&self, r: Decimal) -> Decimal {
        let (Some(mean), Some(var)) = (self.ewma_mean, self.ewma_var) else {
            return Decimal::ZERO;
        };
        if var <= Decimal::ZERO || self.obs_count < 2 {
            return Decimal::ZERO;
        }
        let std = decimal_sqrt(var);
        if std.is_zero() {
            return Decimal::ZERO;
        }
        ((r - mean) / std).abs()
    }

    fn fold_ewma(&mut self, r: Decimal) {
        match self.ewma_mean {
            None => {
                self.ewma_mean = Some(r);
                self.ewma_var = Some(Decimal::ZERO);
            }
            Some(prev_mean) => {
                let centred = r - prev_mean;
                let new_mean = self.alpha * r + (Decimal::ONE - self.alpha) * prev_mean;
                let prev_var = self.ewma_var.unwrap_or(Decimal::ZERO);
                let new_var =
                    self.alpha * centred * centred + (Decimal::ONE - self.alpha) * prev_var;
                self.ewma_mean = Some(new_mean);
                self.ewma_var = Some(new_var);
            }
        }
    }
}

/// Compute the EWMA decay `α` from a half-life in events.
/// Solves `(1 − α)^N = 0.5 → α = 1 − 2^(−1/N)`. Uses `f64`
/// for the `pow` and converts back to `Decimal`.
fn compute_ewma_alpha(half_life: usize) -> Decimal {
    let n = half_life as f64;
    let alpha_f = 1.0 - 0.5_f64.powf(1.0 / n);
    Decimal::from_f64(alpha_f).unwrap_or(dec!(0.05))
}

/// Piecewise-linear ramp: `0` below `z_min`, `max_mult − 1`
/// above `z_max`, linear in between. Returns the multiplier
/// `1 + ramp_value`.
fn ramp(z_abs: Decimal, z_min: Decimal, z_max: Decimal, max_mult: Decimal) -> Decimal {
    if z_abs <= z_min {
        return Decimal::ONE;
    }
    if z_abs >= z_max {
        return max_mult;
    }
    let span = z_max - z_min;
    let frac = (z_abs - z_min) / span;
    Decimal::ONE + (max_mult - Decimal::ONE) * frac
}

/// Newton's-method `√` on `Decimal` — local copy to avoid a
/// cross-module dependency on `mm-strategy::volatility::decimal_sqrt`.
/// Same family as the var_guard / toxicity copies elsewhere
/// in the risk crate.
fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let mut guess = x / dec!(2);
    if guess.is_zero() {
        guess = dec!(1);
    }
    for _ in 0..30 {
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

    fn fixture_config() -> LeadLagGuardConfig {
        LeadLagGuardConfig {
            half_life_events: 10,
            z_min: dec!(2),
            z_max: dec!(4),
            max_mult: dec!(3),
        }
    }

    #[test]
    fn first_mid_returns_neutral_multiplier() {
        let mut g = LeadLagGuard::new(fixture_config());
        g.on_leader_mid(dec!(50000));
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert!(!g.is_active());
        assert_eq!(g.obs_count(), 0);
    }

    #[test]
    fn second_mid_with_no_history_stays_neutral() {
        let mut g = LeadLagGuard::new(fixture_config());
        g.on_leader_mid(dec!(50000));
        // First return seeds the EWMA but variance is still
        // zero — no z-score yet, multiplier stays at 1.
        g.on_leader_mid(dec!(50001));
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert_eq!(g.obs_count(), 1);
    }

    #[test]
    fn quiet_stream_stays_neutral() {
        let mut g = LeadLagGuard::new(fixture_config());
        // Stable mid, all zero returns → variance zero → no
        // signal.
        for _ in 0..50 {
            g.on_leader_mid(dec!(50000));
        }
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert!(!g.is_active());
    }

    #[test]
    fn small_random_noise_stays_below_z_min() {
        // Alternating ±0.001% returns — variance is non-zero
        // but every return is at exactly the same magnitude,
        // so |z| relative to the mean stays modest. The
        // multiplier should NOT trip the ramp.
        let mut g = LeadLagGuard::new(fixture_config());
        let mid = dec!(50000);
        let delta = dec!(0.5); // 1 bps
        for i in 0..50 {
            let m = if i % 2 == 0 { mid + delta } else { mid - delta };
            g.on_leader_mid(m);
        }
        assert_eq!(
            g.current_multiplier(),
            Decimal::ONE,
            "small symmetric noise should not trip ramp, |z|={}",
            g.current_z_abs()
        );
    }

    #[test]
    fn sharp_leader_move_triggers_ramp() {
        // Warmup with stable mid, then a 0.5% jump → very
        // high |z| → multiplier saturates near max_mult.
        let mut g = LeadLagGuard::new(fixture_config());
        let mid = dec!(50000);
        // Build up some non-zero variance from small wiggles.
        for i in 0..30 {
            let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            g.on_leader_mid(mid + delta);
        }
        // Then a sharp jump.
        g.on_leader_mid(dec!(50250)); // 0.5% up
        let mult = g.current_multiplier();
        assert!(
            mult > dec!(1),
            "sharp move should trigger ramp, got {mult} (|z|={})",
            g.current_z_abs()
        );
        assert!(g.is_active());
    }

    #[test]
    fn ramp_saturates_at_max_mult() {
        // Force |z| >> z_max via a massive shock.
        let mut g = LeadLagGuard::new(fixture_config());
        let mid = dec!(50000);
        for i in 0..30 {
            let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            g.on_leader_mid(mid + delta);
        }
        // 5% jump — vastly larger than the EWMA std.
        g.on_leader_mid(dec!(52500));
        assert_eq!(g.current_multiplier(), dec!(3));
    }

    #[test]
    fn negative_shock_triggers_same_as_positive() {
        // Symmetric trigger: a sharp drop produces the same
        // multiplier as a sharp rise.
        let mut g = LeadLagGuard::new(fixture_config());
        let mid = dec!(50000);
        for i in 0..30 {
            let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            g.on_leader_mid(mid + delta);
        }
        g.on_leader_mid(dec!(47500)); // -5%
        assert_eq!(g.current_multiplier(), dec!(3));
    }

    #[test]
    fn decay_back_to_neutral_after_quiet_stream() {
        // Trigger then run a long stable stream — the
        // multiplier should fall back below max as the
        // single shock decays out of the EWMA.
        let mut g = LeadLagGuard::new(fixture_config());
        let mid = dec!(50000);
        for i in 0..30 {
            let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            g.on_leader_mid(mid + delta);
        }
        g.on_leader_mid(dec!(52500)); // shock
        assert!(g.is_active());
        // Long stable stream — every return is zero, |z|
        // collapses, multiplier drops to 1.
        for _ in 0..100 {
            g.on_leader_mid(dec!(52500));
        }
        assert_eq!(
            g.current_multiplier(),
            Decimal::ONE,
            "multiplier should decay to 1 after quiet stream"
        );
    }

    #[test]
    fn reset_drops_all_state() {
        let mut g = LeadLagGuard::new(fixture_config());
        for i in 0..30 {
            g.on_leader_mid(dec!(50000) + Decimal::from(i % 5));
        }
        g.reset();
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert_eq!(g.current_z_abs(), Decimal::ZERO);
        assert_eq!(g.obs_count(), 0);
    }

    #[test]
    fn zero_mid_is_treated_as_seed() {
        // Defensive: a zero leader mid is degenerate but
        // must not panic.
        let mut g = LeadLagGuard::new(fixture_config());
        g.on_leader_mid(Decimal::ZERO);
        g.on_leader_mid(dec!(50000));
        assert_eq!(g.current_multiplier(), Decimal::ONE);
    }

    #[test]
    fn ramp_is_monotone_in_z() {
        // Pin the ramp helper: as |z| increases through the
        // ramp range, the multiplier increases monotonically.
        let m_below = ramp(dec!(1.5), dec!(2), dec!(4), dec!(3));
        let m_min = ramp(dec!(2), dec!(2), dec!(4), dec!(3));
        let m_mid = ramp(dec!(3), dec!(2), dec!(4), dec!(3));
        let m_max = ramp(dec!(4), dec!(2), dec!(4), dec!(3));
        let m_above = ramp(dec!(5), dec!(2), dec!(4), dec!(3));
        assert_eq!(m_below, dec!(1));
        assert_eq!(m_min, dec!(1));
        assert_eq!(m_mid, dec!(2));
        assert_eq!(m_max, dec!(3));
        assert_eq!(m_above, dec!(3));
    }

    #[test]
    fn ewma_alpha_from_half_life_matches_formula() {
        // Half-life 10 → α = 1 - 0.5^(1/10) ≈ 0.0670.
        let alpha = compute_ewma_alpha(10);
        assert!(alpha > dec!(0.066) && alpha < dec!(0.068));
        // Half-life 1 → α = 0.5.
        let alpha1 = compute_ewma_alpha(1);
        assert!((alpha1 - dec!(0.5)).abs() < dec!(0.0001));
    }

    #[test]
    #[should_panic(expected = "half_life_events must be > 0")]
    fn new_panics_on_zero_half_life() {
        LeadLagGuard::new(LeadLagGuardConfig {
            half_life_events: 0,
            z_min: dec!(2),
            z_max: dec!(4),
            max_mult: dec!(3),
        });
    }

    #[test]
    #[should_panic(expected = "z_min must be strictly less than z_max")]
    fn new_panics_on_inverted_ramp() {
        LeadLagGuard::new(LeadLagGuardConfig {
            half_life_events: 10,
            z_min: dec!(4),
            z_max: dec!(2),
            max_mult: dec!(3),
        });
    }
}
