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
mod tests;
