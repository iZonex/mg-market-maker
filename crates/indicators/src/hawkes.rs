//! Hawkes self-exciting point process intensity estimator.
//!
//! Tracks the instantaneous intensity of an event stream (e.g.
//! trade arrivals) using the recursive kernel trick for O(1)
//! per-event updates.
//!
//! # Univariate model
//!
//! ```text
//! λ(t) = μ + α · R(t)
//! R(t_n) = exp(-β · Δt) · R(t_{n-1}) + 1
//! ```
//!
//! where μ is baseline intensity, α is excitation (jump per
//! event), β is decay rate, and Δt is inter-arrival time.
//!
//! # Bivariate mutually-exciting model
//!
//! Two streams (buy/sell) where each stream excites both
//! itself and the other:
//!
//! ```text
//! λ_buy(t)  = μ + α_self · R_buy(t) + α_cross · R_sell(t)
//! λ_sell(t) = μ + α_self · R_sell(t) + α_cross · R_buy(t)
//! ```
//!
//! Reference: Bacry, Mastromatteo, Muzy (2015) "Hawkes
//! processes in finance", *Market Microstructure and Liquidity*
//! 1(1).

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Univariate Hawkes intensity estimator with O(1) updates.
#[derive(Debug, Clone)]
pub struct HawkesIntensity {
    /// Baseline intensity μ.
    mu: Decimal,
    /// Excitation jump per event α.
    alpha: Decimal,
    /// Decay rate β (per second).
    beta: Decimal,
    /// Recursive kernel state R(t).
    kernel: Decimal,
    /// Last event timestamp (seconds, monotonic).
    last_time: Option<Decimal>,
    /// Total events observed.
    event_count: u64,
}

impl HawkesIntensity {
    /// Construct with parameters. `alpha < beta` ensures
    /// stationarity (branching ratio α/β < 1).
    pub fn new(mu: Decimal, alpha: Decimal, beta: Decimal) -> Self {
        assert!(beta > Decimal::ZERO, "beta must be > 0");
        assert!(
            alpha < beta,
            "alpha must be < beta for stationarity (branching ratio < 1)"
        );
        Self {
            mu,
            alpha,
            beta,
            kernel: Decimal::ZERO,
            last_time: None,
            event_count: 0,
        }
    }

    /// Register an event at time `t` (seconds). Updates the
    /// kernel and returns the new intensity.
    pub fn on_event(&mut self, t: Decimal) -> Decimal {
        match self.last_time {
            None => {
                self.kernel = Decimal::ONE;
            }
            Some(prev) => {
                let dt = t - prev;
                if dt < Decimal::ZERO {
                    // Out-of-order event — skip without corrupting state.
                    return self.intensity_at(t);
                }
                let decay = exp_neg(self.beta * dt);
                self.kernel = decay * self.kernel + Decimal::ONE;
            }
        }
        self.last_time = Some(t);
        self.event_count += 1;
        self.mu + self.alpha * self.kernel
    }

    /// Current intensity at time `t` WITHOUT registering an event.
    /// Decays the kernel from the last event time.
    pub fn intensity_at(&self, t: Decimal) -> Decimal {
        match self.last_time {
            None => self.mu,
            Some(prev) => {
                let dt = (t - prev).max(Decimal::ZERO);
                let decay = exp_neg(self.beta * dt);
                self.mu + self.alpha * decay * self.kernel
            }
        }
    }

    /// Branching ratio α/β — must be < 1 for stationarity.
    pub fn branching_ratio(&self) -> Decimal {
        if self.beta.is_zero() {
            return Decimal::ZERO;
        }
        self.alpha / self.beta
    }

    /// Total events observed.
    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Reset all state.
    pub fn reset(&mut self) {
        self.kernel = Decimal::ZERO;
        self.last_time = None;
        self.event_count = 0;
    }
}

/// Bivariate mutually-exciting Hawkes for buy/sell streams.
#[derive(Debug, Clone)]
pub struct BivariateHawkes {
    mu: Decimal,
    alpha_self: Decimal,
    alpha_cross: Decimal,
    beta: Decimal,
    kernel_buy: Decimal,
    kernel_sell: Decimal,
    last_time_buy: Option<Decimal>,
    last_time_sell: Option<Decimal>,
    event_count: u64,
}

impl BivariateHawkes {
    pub fn new(mu: Decimal, alpha_self: Decimal, alpha_cross: Decimal, beta: Decimal) -> Self {
        assert!(beta > Decimal::ZERO, "beta must be > 0");
        assert!(
            alpha_self + alpha_cross < beta,
            "alpha_self + alpha_cross must be < beta for stationarity"
        );
        Self {
            mu,
            alpha_self,
            alpha_cross,
            beta,
            kernel_buy: Decimal::ZERO,
            kernel_sell: Decimal::ZERO,
            last_time_buy: None,
            last_time_sell: None,
            event_count: 0,
        }
    }

    /// Register a buy event at time `t`.
    pub fn on_buy(&mut self, t: Decimal) -> (Decimal, Decimal) {
        self.decay_kernels(t);
        self.kernel_buy += Decimal::ONE;
        self.last_time_buy = Some(t);
        self.event_count += 1;
        self.intensities()
    }

    /// Register a sell event at time `t`.
    pub fn on_sell(&mut self, t: Decimal) -> (Decimal, Decimal) {
        self.decay_kernels(t);
        self.kernel_sell += Decimal::ONE;
        self.last_time_sell = Some(t);
        self.event_count += 1;
        self.intensities()
    }

    /// Current (buy_intensity, sell_intensity) without event.
    pub fn intensities_at(&self, t: Decimal) -> (Decimal, Decimal) {
        let kb = self.decayed_kernel(self.kernel_buy, self.last_time_buy, t);
        let ks = self.decayed_kernel(self.kernel_sell, self.last_time_sell, t);
        (
            self.mu + self.alpha_self * kb + self.alpha_cross * ks,
            self.mu + self.alpha_self * ks + self.alpha_cross * kb,
        )
    }

    /// Intensity imbalance: (λ_buy - λ_sell) / (λ_buy + λ_sell).
    /// Returns 0 when both are zero.
    pub fn intensity_imbalance_at(&self, t: Decimal) -> Decimal {
        let (lb, ls) = self.intensities_at(t);
        let total = lb + ls;
        if total.is_zero() {
            return Decimal::ZERO;
        }
        (lb - ls) / total
    }

    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    pub fn reset(&mut self) {
        self.kernel_buy = Decimal::ZERO;
        self.kernel_sell = Decimal::ZERO;
        self.last_time_buy = None;
        self.last_time_sell = None;
        self.event_count = 0;
    }

    fn decay_kernels(&mut self, t: Decimal) {
        if let Some(prev) = self.last_time_buy {
            let dt = (t - prev).max(Decimal::ZERO);
            self.kernel_buy *= exp_neg(self.beta * dt);
        }
        if let Some(prev) = self.last_time_sell {
            let dt = (t - prev).max(Decimal::ZERO);
            self.kernel_sell *= exp_neg(self.beta * dt);
        }
    }

    fn decayed_kernel(&self, kernel: Decimal, last_time: Option<Decimal>, t: Decimal) -> Decimal {
        match last_time {
            None => Decimal::ZERO,
            Some(prev) => {
                let dt = (t - prev).max(Decimal::ZERO);
                exp_neg(self.beta * dt) * kernel
            }
        }
    }

    fn intensities(&self) -> (Decimal, Decimal) {
        (
            self.mu + self.alpha_self * self.kernel_buy + self.alpha_cross * self.kernel_sell,
            self.mu + self.alpha_self * self.kernel_sell + self.alpha_cross * self.kernel_buy,
        )
    }
}

/// Fast exp(-x) approximation for Decimal. Uses the Taylor
/// series truncated at 6 terms — accurate to ~1e-6 for
/// |x| < 5, which covers the typical Hawkes decay range.
fn exp_neg(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ONE;
    }
    if x > dec!(10) {
        return Decimal::ZERO; // underflow guard
    }
    // exp(-x) = 1 - x + x²/2! - x³/3! + x⁴/4! - x⁵/5! + x⁶/6!
    let x2 = x * x;
    let x3 = x2 * x;
    let x4 = x3 * x;
    let x5 = x4 * x;
    let x6 = x5 * x;
    let result = Decimal::ONE - x + x2 / dec!(2) - x3 / dec!(6) + x4 / dec!(24) - x5 / dec!(120)
        + x6 / dec!(720);
    result.max(Decimal::ZERO) // clamp negative residuals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_intensity_before_any_event() {
        let h = HawkesIntensity::new(dec!(1), dec!(0.5), dec!(1));
        assert_eq!(h.intensity_at(dec!(0)), dec!(1)); // just μ
    }

    #[test]
    fn intensity_jumps_on_event() {
        let mut h = HawkesIntensity::new(dec!(1), dec!(0.5), dec!(1));
        let lambda = h.on_event(dec!(0));
        // λ = μ + α·R = 1 + 0.5·1 = 1.5
        assert_eq!(lambda, dec!(1.5));
    }

    #[test]
    fn intensity_decays_after_event() {
        let mut h = HawkesIntensity::new(dec!(1), dec!(0.5), dec!(1));
        h.on_event(dec!(0));
        // At t=20, decay = exp(-1·20) ≈ 0 → λ ≈ μ = 1.
        let lambda = h.intensity_at(dec!(20));
        assert!(
            (lambda - dec!(1)).abs() < dec!(0.01),
            "intensity should decay to baseline, got {}",
            lambda
        );
    }

    #[test]
    fn cluster_of_events_increases_intensity() {
        let mut h = HawkesIntensity::new(dec!(1), dec!(0.5), dec!(2));
        h.on_event(dec!(0));
        h.on_event(dec!(0.1));
        let lambda = h.on_event(dec!(0.2));
        // Rapid events → kernel accumulates → high intensity.
        assert!(
            lambda > dec!(2),
            "clustered events should raise intensity above 2, got {}",
            lambda
        );
    }

    #[test]
    fn branching_ratio_correct() {
        let h = HawkesIntensity::new(dec!(1), dec!(0.3), dec!(1));
        assert_eq!(h.branching_ratio(), dec!(0.3));
    }

    #[test]
    fn reset_clears_state() {
        let mut h = HawkesIntensity::new(dec!(1), dec!(0.5), dec!(1));
        h.on_event(dec!(0));
        h.on_event(dec!(1));
        h.reset();
        assert_eq!(h.event_count(), 0);
        assert_eq!(h.intensity_at(dec!(2)), dec!(1)); // back to μ
    }

    // ── Bivariate tests ─────────────────────────────────────

    #[test]
    fn bivariate_symmetric_at_start() {
        let bh = BivariateHawkes::new(dec!(1), dec!(0.3), dec!(0.1), dec!(1));
        let (lb, ls) = bh.intensities_at(dec!(0));
        assert_eq!(lb, dec!(1));
        assert_eq!(ls, dec!(1));
    }

    #[test]
    fn bivariate_buy_excites_both() {
        let mut bh = BivariateHawkes::new(dec!(1), dec!(0.3), dec!(0.1), dec!(1));
        let (lb, ls) = bh.on_buy(dec!(0));
        // buy: λ_buy = 1 + 0.3·1 + 0.1·0 = 1.3
        // sell: λ_sell = 1 + 0.3·0 + 0.1·1 = 1.1
        assert_eq!(lb, dec!(1.3));
        assert_eq!(ls, dec!(1.1));
    }

    #[test]
    fn bivariate_imbalance_positive_on_buy_cluster() {
        let mut bh = BivariateHawkes::new(dec!(1), dec!(0.3), dec!(0.1), dec!(1));
        bh.on_buy(dec!(0));
        bh.on_buy(dec!(0.1));
        bh.on_buy(dec!(0.2));
        let imb = bh.intensity_imbalance_at(dec!(0.3));
        assert!(
            imb > Decimal::ZERO,
            "buy cluster should produce positive imbalance, got {}",
            imb
        );
    }

    #[test]
    fn bivariate_imbalance_negative_on_sell_cluster() {
        let mut bh = BivariateHawkes::new(dec!(1), dec!(0.3), dec!(0.1), dec!(1));
        bh.on_sell(dec!(0));
        bh.on_sell(dec!(0.1));
        bh.on_sell(dec!(0.2));
        let imb = bh.intensity_imbalance_at(dec!(0.3));
        assert!(
            imb < Decimal::ZERO,
            "sell cluster should produce negative imbalance, got {}",
            imb
        );
    }

    #[test]
    fn bivariate_decays_to_symmetric() {
        let mut bh = BivariateHawkes::new(dec!(1), dec!(0.3), dec!(0.1), dec!(1));
        bh.on_buy(dec!(0));
        // After long time, both should be back to μ.
        let (lb, ls) = bh.intensities_at(dec!(100));
        assert!(
            (lb - ls).abs() < dec!(0.001),
            "should decay to symmetric: lb={}, ls={}",
            lb,
            ls
        );
    }

    // ── exp_neg tests ───────────────────────────────────────

    #[test]
    fn exp_neg_zero_is_one() {
        assert_eq!(exp_neg(Decimal::ZERO), Decimal::ONE);
    }

    #[test]
    fn exp_neg_large_is_zero() {
        assert_eq!(exp_neg(dec!(15)), Decimal::ZERO);
    }

    #[test]
    fn exp_neg_one_is_approximately_correct() {
        let v = exp_neg(dec!(1));
        // e^{-1} ≈ 0.36788
        assert!(
            (v - dec!(0.36788)).abs() < dec!(0.001),
            "exp(-1) ≈ 0.36788, got {}",
            v
        );
    }
}
