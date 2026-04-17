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

use std::collections::HashMap;

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
    /// Signed z from the most recent observation. Positive z
    /// means leader moved up (bid side more exposed); negative
    /// z means leader moved down (ask side more exposed).
    last_z_signed: Decimal,
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
            last_z_signed: Decimal::ZERO,
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
        let z_signed = self.compute_z_signed(r);
        let z_abs = z_signed.abs();
        self.fold_ewma(r);
        self.last_mid = Some(mid);
        self.last_z_abs = z_abs;
        self.last_z_signed = z_signed;
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

    /// Per-side asymmetric multiplier for the **bid** side.
    /// When the leader moves UP (positive z), the follower's
    /// bid is the stale side that informed flow hits first →
    /// bid gets the full multiplier. When the leader moves DOWN,
    /// the bid is the safe side → bid gets a reduced widening
    /// (square root of the symmetric multiplier, floored at 1).
    pub fn bid_multiplier(&self) -> Decimal {
        if self.last_multiplier <= Decimal::ONE {
            return Decimal::ONE;
        }
        if self.last_z_signed > Decimal::ZERO {
            // Leader moved up → bid is the stale side.
            self.last_multiplier
        } else {
            // Leader moved down → bid is safer, partial widen.
            let excess = self.last_multiplier - Decimal::ONE;
            Decimal::ONE + excess / dec!(2)
        }
    }

    /// Per-side asymmetric multiplier for the **ask** side.
    /// Mirror of `bid_multiplier()`: when the leader moves
    /// DOWN, the ask is stale → full multiplier; when UP, the
    /// ask is safer → partial widen.
    pub fn ask_multiplier(&self) -> Decimal {
        if self.last_multiplier <= Decimal::ONE {
            return Decimal::ONE;
        }
        if self.last_z_signed < Decimal::ZERO {
            // Leader moved down → ask is the stale side.
            self.last_multiplier
        } else {
            // Leader moved up → ask is safer, partial widen.
            let excess = self.last_multiplier - Decimal::ONE;
            Decimal::ONE + excess / dec!(2)
        }
    }

    /// Drop all cached state. Next `on_leader_mid` call
    /// behaves like the very first one.
    pub fn reset(&mut self) {
        self.last_mid = None;
        self.ewma_mean = None;
        self.ewma_var = None;
        self.last_multiplier = Decimal::ONE;
        self.last_z_abs = Decimal::ZERO;
        self.last_z_signed = Decimal::ZERO;
        self.obs_count = 0;
    }

    /// Number of returns folded into the EWMA so far.
    pub fn obs_count(&self) -> usize {
        self.obs_count
    }

    fn compute_z_signed(&self, r: Decimal) -> Decimal {
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
        (r - mean) / std
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

// ---------------- Multi-leader aggregation (stage-2) ----------------

/// Multi-leader extension of [`LeadLagGuard`]. Operators
/// register one or more leader venues (e.g.
/// `"binance_futures"`, `"bybit_perp"`, `"okx_perp"`) with a
/// per-leader weight, feed each leader's mid into its own
/// sub-guard via [`Self::on_leader_mid`], and read a single
/// aggregated multiplier back via [`Self::current_multiplier`].
///
/// # Aggregation rule — weight-scaled maximum
///
/// The aggregated multiplier is the **weight-scaled maximum**
/// over all registered leaders:
///
/// ```text
/// M_agg = max over L of ( w[L] · (M[L] − 1) + 1 )
/// ```
///
/// Taking the maximum (rather than an average) is deliberate:
/// defensive controls should listen to the LOUDEST leader.
/// Averaging would let N quiet leaders dilute a single shocked
/// leader's widening signal and delay the retreat.
///
/// The `(M − 1)` shift applies the weight to the *additional*
/// widening above the neutral 1.0 baseline. A leader with
/// `weight = 0.5` therefore contributes half the widening
/// headroom (so a per-leader multiplier of `3.0` counts as
/// `1 + 0.5 · (3 − 1) = 2.0`), not half the raw multiplier.
/// Weight `0.0` mutes a leader without removing it. The
/// aggregated multiplier is floored at `1.0`.
///
/// # Single-leader compatibility
///
/// The original [`LeadLagGuard`] is unchanged. Existing
/// callers that read a single leader keep using it directly.
/// `MultiLeaderLeadLagGuard` is an additive sibling for
/// operators who want to watch multiple leaders at once.
#[derive(Debug, Clone)]
pub struct MultiLeaderLeadLagGuard {
    /// Template config applied to every registered leader's
    /// sub-guard.
    config: LeadLagGuardConfig,
    /// Per-leader state. The `String` key is the operator's
    /// chosen leader-id (e.g. `"binance_futures"`); the
    /// tuple stores the per-leader `LeadLagGuard` and its
    /// weight.
    leaders: HashMap<String, (LeadLagGuard, Decimal)>,
    /// Cached aggregated multiplier. Recomputed on each
    /// `on_leader_mid` / `register_leader` / `unregister_leader`
    /// / `reset` call so the hot read path is O(1).
    cached_mult: Decimal,
    /// Cached max of `|z|` across leaders, for metrics.
    cached_max_z_abs: Decimal,
}

impl MultiLeaderLeadLagGuard {
    /// Build an empty multi-leader guard with the given
    /// config template. Leaders are added later via
    /// [`Self::register_leader`]. An empty guard always
    /// returns multiplier `1.0`.
    pub fn new(config: LeadLagGuardConfig) -> Self {
        Self {
            config,
            leaders: HashMap::new(),
            cached_mult: Decimal::ONE,
            cached_max_z_abs: Decimal::ZERO,
        }
    }

    /// Register (or re-register) a leader. If the leader
    /// already exists, its weight is updated **but the
    /// existing EWMA state is preserved** — useful for
    /// operators re-weighting leaders at runtime without
    /// losing warmup. Weights below zero are clamped to 0.
    pub fn register_leader(&mut self, id: impl Into<String>, weight: Decimal) {
        let id = id.into();
        let weight = if weight < Decimal::ZERO {
            Decimal::ZERO
        } else {
            weight
        };
        if let Some(entry) = self.leaders.get_mut(&id) {
            entry.1 = weight;
        } else {
            self.leaders
                .insert(id, (LeadLagGuard::new(self.config.clone()), weight));
        }
        self.recompute_cache();
    }

    /// Remove a leader and its cached state. No-op if the
    /// leader was never registered.
    pub fn unregister_leader(&mut self, id: &str) {
        self.leaders.remove(id);
        self.recompute_cache();
    }

    /// Fold one new mid observation into the named leader's
    /// sub-guard, then refresh the aggregated multiplier.
    /// If `leader_id` is not registered the call is a
    /// silent no-op — operators can stream mids from every
    /// venue and let the guard ignore leaders it doesn't
    /// care about.
    pub fn on_leader_mid(&mut self, leader_id: &str, mid: Decimal) {
        if let Some(entry) = self.leaders.get_mut(leader_id) {
            entry.0.on_leader_mid(mid);
            self.recompute_cache();
        }
    }

    /// Aggregated weight-scaled maximum multiplier. Floored
    /// at `1.0` (no negative widening is possible).
    pub fn current_multiplier(&self) -> Decimal {
        self.cached_mult
    }

    /// Largest `|z|` seen across all registered leaders,
    /// for metrics / dashboards. `0` if no leaders are
    /// registered or none have produced a return yet.
    pub fn current_max_z_abs(&self) -> Decimal {
        self.cached_max_z_abs
    }

    /// `true` if ANY registered leader is currently
    /// producing a multiplier above `1.0`.
    pub fn is_active(&self) -> bool {
        self.cached_mult > Decimal::ONE
    }

    /// Clear every registered leader's internal state
    /// WITHOUT unregistering them. Useful on kill-switch
    /// reset: registrations survive, but warmup restarts
    /// from scratch.
    pub fn reset(&mut self) {
        for (_id, (guard, _w)) in self.leaders.iter_mut() {
            guard.reset();
        }
        self.recompute_cache();
    }

    /// Number of currently registered leaders.
    pub fn leader_count(&self) -> usize {
        self.leaders.len()
    }

    /// Recompute the cached aggregated multiplier + max
    /// `|z|` from the per-leader states. Called whenever
    /// the leader set or any leader's state changes.
    fn recompute_cache(&mut self) {
        let mut best = Decimal::ONE;
        let mut best_z = Decimal::ZERO;
        for (guard, weight) in self.leaders.values() {
            let per_leader = guard.current_multiplier();
            // Weight-scaled widening: w · (M − 1) + 1.
            let scaled = *weight * (per_leader - Decimal::ONE) + Decimal::ONE;
            if scaled > best {
                best = scaled;
            }
            let z = guard.current_z_abs();
            if z > best_z {
                best_z = z;
            }
        }
        // Floor at 1.0 — weight 0 on a quiet guard would
        // otherwise produce 0·(1−1)+1 = 1 which is fine,
        // but defensive floor guards against any future
        // change to the formula.
        if best < Decimal::ONE {
            best = Decimal::ONE;
        }
        self.cached_mult = best;
        self.cached_max_z_abs = best_z;
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

    // ---------- MultiLeaderLeadLagGuard (stage-2) ----------

    /// Drive a guard through the standard warmup pattern:
    /// 30 small alternating ±1 wiggles to build EWMA
    /// variance. Helper shared across multi-leader tests.
    fn warmup_with_wiggles(g: &mut LeadLagGuard, mid: Decimal) {
        for i in 0..30 {
            let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            g.on_leader_mid(mid + delta);
        }
    }

    /// Same warmup pattern but applied to a multi-leader
    /// guard for a specific leader id.
    fn warmup_multi(g: &mut MultiLeaderLeadLagGuard, id: &str, mid: Decimal) {
        for i in 0..30 {
            let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            g.on_leader_mid(id, mid + delta);
        }
    }

    #[test]
    fn multi_empty_guard_returns_neutral_multiplier() {
        let g = MultiLeaderLeadLagGuard::new(fixture_config());
        assert_eq!(g.leader_count(), 0);
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert_eq!(g.current_max_z_abs(), Decimal::ZERO);
        assert!(!g.is_active());
    }

    #[test]
    fn multi_single_leader_matches_single_leader_guard() {
        // A single-leader MultiLeaderLeadLagGuard with
        // weight=1.0 must produce the same multiplier as a
        // direct LeadLagGuard fed the same sequence.
        let mut single = LeadLagGuard::new(fixture_config());
        let mut multi = MultiLeaderLeadLagGuard::new(fixture_config());
        multi.register_leader("leader", dec!(1));

        let mid = dec!(50000);
        for i in 0..30 {
            let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            single.on_leader_mid(mid + delta);
            multi.on_leader_mid("leader", mid + delta);
        }
        // Big shock.
        single.on_leader_mid(dec!(52500));
        multi.on_leader_mid("leader", dec!(52500));

        assert_eq!(single.current_multiplier(), multi.current_multiplier());
        assert_eq!(single.current_z_abs(), multi.current_max_z_abs());
        assert!(multi.is_active());
    }

    #[test]
    fn multi_two_quiet_leaders_stay_neutral() {
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(1));
        g.register_leader("b", dec!(1));
        for _ in 0..40 {
            g.on_leader_mid("a", dec!(50000));
            g.on_leader_mid("b", dec!(30000));
        }
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert!(!g.is_active());
    }

    #[test]
    fn multi_one_shocked_leader_drives_aggregate() {
        // Leader "a" shocks hard, leader "b" stays flat —
        // aggregate reflects "a" (the loudest).
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(1));
        g.register_leader("b", dec!(1));
        warmup_multi(&mut g, "a", dec!(50000));
        for _ in 0..40 {
            g.on_leader_mid("b", dec!(30000));
        }
        // Big 5% shock on "a".
        g.on_leader_mid("a", dec!(52500));
        assert_eq!(g.current_multiplier(), dec!(3));
        assert!(g.is_active());
    }

    #[test]
    fn multi_loudest_of_two_shocked_wins() {
        // Both leaders shock, but leader "a" saturates to
        // max_mult=3 while leader "b" only lifts partway.
        // Aggregate must equal the loudest = 3.
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(1));
        g.register_leader("b", dec!(1));
        warmup_multi(&mut g, "a", dec!(50000));
        warmup_multi(&mut g, "b", dec!(30000));
        g.on_leader_mid("a", dec!(52500)); // huge shock → sat
        g.on_leader_mid("b", dec!(30015)); // tiny bump
        assert_eq!(g.current_multiplier(), dec!(3));
    }

    #[test]
    fn multi_weight_half_halves_additional_widening() {
        // Leader at weight 0.5 with a per-leader multiplier
        // of 3.0 must contribute 1 + 0.5·(3−1) = 2.0.
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(0.5));
        warmup_multi(&mut g, "a", dec!(50000));
        g.on_leader_mid("a", dec!(52500));
        // Cross-check vs a single-leader guard's raw mult.
        let mut single = LeadLagGuard::new(fixture_config());
        warmup_with_wiggles(&mut single, dec!(50000));
        single.on_leader_mid(dec!(52500));
        let raw = single.current_multiplier();
        assert_eq!(raw, dec!(3));
        assert_eq!(g.current_multiplier(), dec!(2));
    }

    #[test]
    fn multi_weight_zero_mutes_leader() {
        // A muted (weight=0) leader cannot drive widening
        // no matter how shocked it gets.
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("muted", dec!(0));
        warmup_multi(&mut g, "muted", dec!(50000));
        g.on_leader_mid("muted", dec!(52500)); // 5% shock
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert!(!g.is_active());
    }

    #[test]
    fn multi_negative_weight_is_clamped_to_zero() {
        // Defensive input handling: negative weights are
        // clamped to 0, not reflected.
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(-1));
        warmup_multi(&mut g, "a", dec!(50000));
        g.on_leader_mid("a", dec!(52500));
        assert_eq!(g.current_multiplier(), Decimal::ONE);
    }

    #[test]
    fn multi_unregister_drops_leader_contribution() {
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("shocked", dec!(1));
        warmup_multi(&mut g, "shocked", dec!(50000));
        g.on_leader_mid("shocked", dec!(52500));
        assert!(g.is_active());
        g.unregister_leader("shocked");
        assert_eq!(g.leader_count(), 0);
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert!(!g.is_active());
    }

    #[test]
    fn multi_reset_clears_state_keeps_registrations() {
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(1));
        g.register_leader("b", dec!(1));
        warmup_multi(&mut g, "a", dec!(50000));
        g.on_leader_mid("a", dec!(52500));
        assert!(g.is_active());
        assert_eq!(g.leader_count(), 2);

        g.reset();

        // Registrations survive, state is cleared.
        assert_eq!(g.leader_count(), 2);
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert_eq!(g.current_max_z_abs(), Decimal::ZERO);
        assert!(!g.is_active());
    }

    #[test]
    fn multi_is_active_reflects_any_leader() {
        // Two leaders, only one shocked — the aggregate is
        // still "active" because at least one leader is.
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(1));
        g.register_leader("b", dec!(1));
        warmup_multi(&mut g, "a", dec!(50000));
        warmup_multi(&mut g, "b", dec!(30000));
        g.on_leader_mid("b", dec!(31500)); // 5% on b
        assert!(g.is_active());
    }

    #[test]
    fn multi_unknown_leader_mid_is_silent_noop() {
        // Feeding a mid for an un-registered leader must
        // not panic and must not change state.
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(1));
        g.on_leader_mid("b", dec!(12345));
        assert_eq!(g.current_multiplier(), Decimal::ONE);
        assert_eq!(g.leader_count(), 1);
    }

    #[test]
    fn multi_reregister_preserves_existing_state() {
        // Re-registering an existing leader updates the
        // weight WITHOUT dropping its EWMA state.
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(1));
        warmup_multi(&mut g, "a", dec!(50000));
        g.on_leader_mid("a", dec!(52500));
        let mult_before = g.current_multiplier();
        assert_eq!(mult_before, dec!(3));
        // Re-register with weight 0.5 — per-leader state
        // must survive, only the aggregation weight changes.
        g.register_leader("a", dec!(0.5));
        assert_eq!(g.current_multiplier(), dec!(2)); // 1 + 0.5·(3-1)
    }

    #[test]
    fn multi_hand_verified_two_leader_fixture() {
        // Hand-computed end-to-end fixture: two leaders,
        // leader "a" weight 1.0 gets a huge shock (5%) →
        // per-leader multiplier 3.0. Leader "b" weight 0.25
        // gets the same shock → per-leader multiplier 3.0
        // → weight-scaled widening 1 + 0.25·(3−1) = 1.5.
        // Aggregate = max(3.0, 1.5) = 3.0.
        let mut g = MultiLeaderLeadLagGuard::new(fixture_config());
        g.register_leader("a", dec!(1));
        g.register_leader("b", dec!(0.25));
        warmup_multi(&mut g, "a", dec!(50000));
        warmup_multi(&mut g, "b", dec!(30000));
        g.on_leader_mid("a", dec!(52500));
        g.on_leader_mid("b", dec!(31500));
        assert_eq!(g.current_multiplier(), dec!(3));
        // If we now unregister "a", the aggregate collapses
        // to just "b"'s scaled contribution: 1.5.
        g.unregister_leader("a");
        assert_eq!(g.current_multiplier(), dec!(1.5));
    }

    // ── Per-side asymmetric multiplier tests ─────────────────

    /// Warm up a single-leader guard with enough observations
    /// to build an EWMA state.
    fn warmup_single(g: &mut LeadLagGuard, mid: Decimal) {
        for i in 0..30 {
            // Small oscillation to build variance.
            let offset = if i % 2 == 0 { dec!(0.01) } else { dec!(-0.01) };
            g.on_leader_mid(mid + mid * offset / dec!(100));
        }
    }

    /// When the leader moves UP, the bid side (stale) gets the
    /// full multiplier and the ask side (safe) gets partial.
    #[test]
    fn per_side_bid_stale_on_up_move() {
        let mut g = LeadLagGuard::new(fixture_config());
        warmup_single(&mut g, dec!(50000));
        // Large upward move → positive z → bid is stale.
        g.on_leader_mid(dec!(52500)); // +5%
        assert!(g.is_active());
        assert_eq!(g.bid_multiplier(), g.current_multiplier());
        assert!(
            g.ask_multiplier() < g.bid_multiplier(),
            "ask={} should be < bid={}",
            g.ask_multiplier(),
            g.bid_multiplier()
        );
        assert!(g.ask_multiplier() > Decimal::ONE);
    }

    /// When the leader moves DOWN, the ask side (stale) gets
    /// the full multiplier and the bid side gets partial.
    #[test]
    fn per_side_ask_stale_on_down_move() {
        let mut g = LeadLagGuard::new(fixture_config());
        warmup_single(&mut g, dec!(50000));
        // Large downward move → negative z → ask is stale.
        g.on_leader_mid(dec!(47500)); // -5%
        assert!(g.is_active());
        assert_eq!(g.ask_multiplier(), g.current_multiplier());
        assert!(
            g.bid_multiplier() < g.ask_multiplier(),
            "bid={} should be < ask={}",
            g.bid_multiplier(),
            g.ask_multiplier()
        );
        assert!(g.bid_multiplier() > Decimal::ONE);
    }

    /// When the guard is not active, both sides return 1.0.
    #[test]
    fn per_side_both_one_when_inactive() {
        let mut g = LeadLagGuard::new(fixture_config());
        warmup_single(&mut g, dec!(50000));
        // Tiny move — under z_min threshold.
        g.on_leader_mid(dec!(50001));
        assert!(!g.is_active());
        assert_eq!(g.bid_multiplier(), Decimal::ONE);
        assert_eq!(g.ask_multiplier(), Decimal::ONE);
    }

    /// The stale-side multiplier equals the symmetric
    /// multiplier — the per-side split is only asymmetric
    /// on the SAFE side.
    #[test]
    fn stale_side_equals_symmetric() {
        let mut g = LeadLagGuard::new(fixture_config());
        warmup_single(&mut g, dec!(50000));
        g.on_leader_mid(dec!(52500)); // up
        assert_eq!(g.bid_multiplier(), g.current_multiplier());

        g.reset();
        warmup_single(&mut g, dec!(50000));
        g.on_leader_mid(dec!(47500)); // down
        assert_eq!(g.ask_multiplier(), g.current_multiplier());
    }

    // ── Property-based tests (Epic 13) ───────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn mid_step_strat()(raw in 1i64..1_000_000i64) -> Decimal {
            Decimal::new(raw, 2)
        }
    }

    proptest! {
        /// current_multiplier() is bounded in [1, max_mult] for
        /// any sequence of leader mids. The guard only widens
        /// spreads — a value below 1 would tighten them under
        /// stress, the opposite of risk reduction. Upper bound
        /// saturates at config.max_mult regardless of |z|.
        #[test]
        fn multiplier_bounded_in_one_to_max(
            mids in proptest::collection::vec(mid_step_strat(), 1..40),
        ) {
            let cfg = fixture_config();
            let max = cfg.max_mult;
            let mut g = LeadLagGuard::new(cfg);
            for m in &mids {
                g.on_leader_mid(*m);
                let mult = g.current_multiplier();
                prop_assert!(mult >= dec!(1), "mult {} < 1", mult);
                prop_assert!(mult <= max, "mult {} > max {}", mult, max);
            }
        }

        /// bid_multiplier and ask_multiplier both stay in the
        /// same bounded range. Catches a regression where one
        /// side's helper would leak a negative scalar from the
        /// signed-z path.
        #[test]
        fn side_multipliers_also_bounded(
            mids in proptest::collection::vec(mid_step_strat(), 1..40),
        ) {
            let cfg = fixture_config();
            let max = cfg.max_mult;
            let mut g = LeadLagGuard::new(cfg);
            for m in &mids {
                g.on_leader_mid(*m);
                let bid = g.bid_multiplier();
                let ask = g.ask_multiplier();
                prop_assert!(bid >= dec!(1));
                prop_assert!(bid <= max);
                prop_assert!(ask >= dec!(1));
                prop_assert!(ask <= max);
            }
        }

        /// A constant leader mid produces zero z-score on every
        /// tick, so the multiplier stays at 1.0 — no spurious
        /// widening on a static feed.
        #[test]
        fn constant_mid_never_widens(
            mid in mid_step_strat(),
            n in 5usize..40usize,
        ) {
            let mut g = LeadLagGuard::new(fixture_config());
            for _ in 0..n {
                g.on_leader_mid(mid);
            }
            prop_assert_eq!(g.current_multiplier(), dec!(1));
            prop_assert!(!g.is_active());
        }

        /// reset() wipes the guard to its initial state from
        /// any configuration of observed events.
        #[test]
        fn reset_restores_initial_state(
            mids in proptest::collection::vec(mid_step_strat(), 1..30),
        ) {
            let mut g = LeadLagGuard::new(fixture_config());
            for m in &mids {
                g.on_leader_mid(*m);
            }
            g.reset();
            prop_assert_eq!(g.current_multiplier(), dec!(1));
            prop_assert!(!g.is_active());
            prop_assert_eq!(g.obs_count(), 0);
        }
    }
}
