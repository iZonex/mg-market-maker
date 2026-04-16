//! Per-strategy Value-at-Risk guard — Epic C sub-component #4.
//!
//! Maintains a rolling-window PnL buffer per strategy class,
//! computes a parametric Gaussian VaR, and exposes a
//! `[0, 1]` size multiplier that the engine composes with
//! kill switch / Market Resilience / InventoryGammaPolicy via
//! `min()` (max-restrictive wins).
//!
//! # Formula
//!
//! Standard parametric Gaussian VaR from the RiskMetrics
//! technical document (J.P. Morgan, 1996). For a sample of
//! PnL observations `P_1, ..., P_N`:
//!
//! ```text
//! μ   = (1/N) · Σ P_i
//! σ²  = (1/(N-1)) · Σ (P_i - μ)²
//! σ   = sqrt(σ²)
//! VaR_95 = μ - 1.645·σ
//! VaR_99 = μ - 2.326·σ
//! ```
//!
//! The z-scores `1.645` / `2.326` are the one-sided standard-
//! normal quantiles at 95 % / 99 %. We freeze them as compile-
//! time constants so there is no `erf_inv` at runtime.
//!
//! ## EWMA variance (stage-2)
//!
//! When `ewma_lambda` is set in `VarGuardConfig`, the guard
//! computes an exponentially-weighted variance alongside the
//! rolling sample variance. The EWMA variance adapts faster
//! to regime changes (vol spikes) than the equally-weighted
//! sample variance. The guard uses `max(sample_σ, ewma_σ)`
//! for a conservative VaR estimate.
//!
//! ## CVaR / Expected Shortfall (stage-2)
//!
//! Under the Gaussian assumption:
//!
//! ```text
//! CVaR_α = μ - σ · φ(z_α) / (1 - α)
//! ```
//!
//! where `φ(z)` is the standard normal PDF evaluated at the
//! z-score corresponding to the α quantile. CVaR is exposed
//! via `compute_risk_metrics()` for dashboard / audit but does
//! NOT feed into the throttle tiers — operators monitor it as
//! a tail-risk gauge.
//!
//! # Throttle tiers
//!
//! ```text
//! if VaR_99 < VaR_limit_99:  throttle = 0.0   # hard halt
//! elif VaR_95 < VaR_limit_95: throttle = 0.5   # half size
//! else:                       throttle = 1.0   # no throttle
//! ```
//!
//! The limits are **signed negative numbers** — a VaR floor
//! of `-500` means "throttle if the 95 % worst-case daily PnL
//! is worse than losing 500 USDT". When no limit is configured
//! the guard returns `1.0` unconditionally.
//!
//! # Ring buffer
//!
//! One `VecDeque<Decimal>` per strategy class, capped at
//! `MAX_SAMPLES_PER_CLASS = 1440` — the same size the P2.2
//! presence buckets use. The engine pushes one sample per
//! 60 seconds from the `sla_interval` arm, so 1440 samples
//! cover 24 hours of history.
//!
//! The guard returns `1.0` (no throttle) until at least
//! `MIN_SAMPLES_FOR_VAR = 30` samples have landed for a given
//! strategy — below that the Gaussian estimate is too noisy
//! to throttle on.
//!
//! # Multi-strategy isolation
//!
//! Each strategy class has its own ring buffer keyed by the
//! `Strategy::name()` string. An Avellaneda book in breach
//! does NOT throttle a Basis book, and vice versa. The
//! engine's effective-size composition step is what pulls
//! the per-strategy throttle into the global multiplier.

use std::collections::HashMap;
use std::collections::VecDeque;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// 24 h × 60 s cadence = 1440 samples. Same ring-buffer size
/// the P2.2 presence tracker uses — the symmetry is deliberate
/// so a future operator mental model of "daily granularity
/// means 1440 entries" applies to both.
pub const MAX_SAMPLES_PER_CLASS: usize = 1440;

/// Below this sample count the Gaussian VaR estimate is too
/// noisy to throttle on. Returning `1.0` during warm-up avoids
/// the "first few minutes after start have a random throttle"
/// failure mode.
pub const MIN_SAMPLES_FOR_VAR: usize = 30;

/// One-sided standard-normal quantile at 95 %. Frozen compile-
/// time constant so VaR computation is pure multiplication —
/// no `erf_inv` at runtime. Value from any standard normal
/// table; matches RiskMetrics and Jorion's Value at Risk.
pub const Z_SCORE_95: Decimal = dec!(1.645);

/// One-sided standard-normal quantile at 99 %.
pub const Z_SCORE_99: Decimal = dec!(2.326);

/// Throttle tier at the 95 % VaR breach — half size.
pub const THROTTLE_95_TIER: Decimal = dec!(0.5);

/// Throttle tier at the 99 % VaR breach — full halt.
pub const THROTTLE_99_TIER: Decimal = dec!(0);

/// Standard normal PDF at z: φ(z) = (1/√(2π)) · exp(-z²/2).
/// Pre-computed for the two z-scores we care about.
/// φ(1.645) ≈ 0.10314, φ(2.326) ≈ 0.02656.
pub const PHI_Z95: Decimal = dec!(0.10314);
pub const PHI_Z99: Decimal = dec!(0.02656);

/// Configuration knobs for the per-strategy VaR guard.
#[derive(Debug, Clone, Default)]
pub struct VarGuardConfig {
    /// 95 %-VaR floor in the reporting currency. The guard
    /// throttles to `0.5` size when the computed VaR_95 drops
    /// below this number. Typically configured as a negative
    /// number (e.g. `-500` = "throttle at a $500 worst-case
    /// daily loss"). `None` disables the 95 % tier entirely.
    pub limit_95: Option<Decimal>,
    /// 99 %-VaR floor in the reporting currency. `None`
    /// disables the 99 % tier (but then a strategy can
    /// never be hard-halted by VaR).
    pub limit_99: Option<Decimal>,
    /// EWMA decay factor λ ∈ (0, 1). Higher = slower decay,
    /// more weight on history. RiskMetrics default is 0.94 for
    /// daily data. `None` disables the EWMA path entirely —
    /// the guard uses only the equally-weighted sample variance.
    pub ewma_lambda: Option<Decimal>,
}

/// Full risk metrics snapshot for a strategy class. Exposed
/// for the dashboard / audit trail — richer than the scalar
/// throttle returned by `effective_throttle()`.
#[derive(Debug, Clone)]
pub struct RiskMetrics {
    pub var_95: Decimal,
    pub var_99: Decimal,
    /// Conditional Value-at-Risk (Expected Shortfall) at 95 %.
    /// Under the Gaussian assumption: CVaR_95 = μ − σ·φ(z_95)/0.05.
    pub cvar_95: Decimal,
    /// CVaR at 99 %.
    pub cvar_99: Decimal,
    /// EWMA variance (if enabled). `None` when `ewma_lambda`
    /// is not configured.
    pub ewma_variance: Option<Decimal>,
    /// Historical-simulation VaR at 95 % (empirical quantile,
    /// no Gaussian assumption). Sorts the sample buffer and
    /// picks the `floor(N * 0.05)`-th worst observation.
    pub hist_var_95: Decimal,
    /// Historical-simulation VaR at 99 %.
    pub hist_var_99: Decimal,
    pub mean: Decimal,
    pub sample_variance: Decimal,
    pub sample_count: usize,
}

/// Per-strategy EWMA state. Tracks the running variance
/// estimate without needing the full sample buffer.
#[derive(Debug, Clone)]
struct EwmaState {
    variance: Decimal,
    mean: Decimal,
    initialised: bool,
}

impl EwmaState {
    fn new() -> Self {
        Self {
            variance: Decimal::ZERO,
            mean: Decimal::ZERO,
            initialised: false,
        }
    }

    /// Update with a new PnL sample. Uses the RiskMetrics EWMA
    /// recursion: σ²_t = λ·σ²_{t-1} + (1−λ)·(x_t − μ_{t-1})².
    fn update(&mut self, sample: Decimal, lambda: Decimal) {
        if !self.initialised {
            self.mean = sample;
            self.variance = Decimal::ZERO;
            self.initialised = true;
            return;
        }
        let diff = sample - self.mean;
        self.variance = lambda * self.variance + (Decimal::ONE - lambda) * diff * diff;
        self.mean = lambda * self.mean + (Decimal::ONE - lambda) * sample;
    }
}

/// Per-strategy rolling-window VaR guard.
#[derive(Debug, Clone)]
pub struct VarGuard {
    config: VarGuardConfig,
    /// One ring buffer of PnL samples per strategy class.
    /// Key is the owned `String` tag from `Strategy::name()`
    /// (same key convention as `Portfolio::per_strategy_pnl`).
    buffers: HashMap<String, VecDeque<Decimal>>,
    /// Per-strategy EWMA state. Only populated when
    /// `config.ewma_lambda` is `Some`.
    ewma: HashMap<String, EwmaState>,
}

impl VarGuard {
    pub fn new(config: VarGuardConfig) -> Self {
        Self {
            config,
            buffers: HashMap::new(),
            ewma: HashMap::new(),
        }
    }

    /// Record one PnL sample for `strategy_class`. The engine
    /// calls this from the `sla_interval` arm on a 60-second
    /// cadence (gated by `tick_count % 60 == 0`). The value
    /// should be the **delta** in total PnL since the previous
    /// sample, not the absolute PnL.
    pub fn record_pnl_sample(&mut self, strategy_class: &str, pnl_delta: Decimal) {
        let buf = self.buffers.entry(strategy_class.to_string()).or_default();
        if buf.len() >= MAX_SAMPLES_PER_CLASS {
            buf.pop_front();
        }
        buf.push_back(pnl_delta);
        // Update EWMA state if configured.
        if let Some(lambda) = self.config.ewma_lambda {
            self.ewma
                .entry(strategy_class.to_string())
                .or_insert_with(EwmaState::new)
                .update(pnl_delta, lambda);
        }
    }

    /// Effective size-multiplier for the given strategy class.
    /// Returns `1.0` during warm-up (`< MIN_SAMPLES_FOR_VAR`
    /// samples) and when no VaR limit is configured. The
    /// engine composes this with other multipliers via `min()`.
    ///
    /// When `ewma_lambda` is configured, the guard uses
    /// `max(sample_σ, ewma_σ)` for a conservative VaR estimate
    /// that reacts faster to regime changes.
    pub fn effective_throttle(&self, strategy_class: &str) -> Decimal {
        let Some(buf) = self.buffers.get(strategy_class) else {
            return Decimal::ONE;
        };
        if buf.len() < MIN_SAMPLES_FOR_VAR {
            return Decimal::ONE;
        }
        let metrics = self.compute_risk_metrics_inner(buf, self.ewma.get(strategy_class));
        // Hard halt tier first — `min` semantics mean the
        // tighter of the two wins if both breach simultaneously.
        if let Some(limit_99) = self.config.limit_99 {
            if metrics.var_99 < limit_99 {
                return THROTTLE_99_TIER;
            }
        }
        if let Some(limit_95) = self.config.limit_95 {
            if metrics.var_95 < limit_95 {
                return THROTTLE_95_TIER;
            }
        }
        Decimal::ONE
    }

    /// Full risk metrics snapshot for a strategy class.
    /// Returns `None` if the class has fewer than
    /// `MIN_SAMPLES_FOR_VAR` samples. Exposed for dashboard
    /// and audit trail.
    pub fn risk_metrics(&self, strategy_class: &str) -> Option<RiskMetrics> {
        let buf = self.buffers.get(strategy_class)?;
        if buf.len() < MIN_SAMPLES_FOR_VAR {
            return None;
        }
        Some(self.compute_risk_metrics_inner(buf, self.ewma.get(strategy_class)))
    }

    /// Pure computation of (VaR_95, VaR_99) from a sample
    /// window. Exposed for tests. Legacy interface — prefer
    /// `risk_metrics()` for the full snapshot.
    pub fn compute_var(samples: &VecDeque<Decimal>) -> (Decimal, Decimal) {
        let n = samples.len();
        if n < 2 {
            return (Decimal::ZERO, Decimal::ZERO);
        }
        let (mean, variance) = Self::sample_mean_var(samples);
        let sigma = decimal_sqrt(variance);
        let var_95 = mean - Z_SCORE_95 * sigma;
        let var_99 = mean - Z_SCORE_99 * sigma;
        (var_95, var_99)
    }

    fn sample_mean_var(samples: &VecDeque<Decimal>) -> (Decimal, Decimal) {
        let n = samples.len();
        let n_dec = Decimal::from(n as u32);
        let mean = samples.iter().copied().sum::<Decimal>() / n_dec;
        let var_sum: Decimal = samples
            .iter()
            .map(|p| {
                let diff = *p - mean;
                diff * diff
            })
            .sum();
        let variance = if n > 1 {
            var_sum / Decimal::from((n - 1) as u32)
        } else {
            Decimal::ZERO
        };
        (mean, variance)
    }

    fn compute_risk_metrics_inner(
        &self,
        samples: &VecDeque<Decimal>,
        ewma_state: Option<&EwmaState>,
    ) -> RiskMetrics {
        let n = samples.len();
        if n < 2 {
            return RiskMetrics {
                var_95: Decimal::ZERO,
                var_99: Decimal::ZERO,
                cvar_95: Decimal::ZERO,
                cvar_99: Decimal::ZERO,
                ewma_variance: None,
                hist_var_95: Decimal::ZERO,
                hist_var_99: Decimal::ZERO,
                mean: Decimal::ZERO,
                sample_variance: Decimal::ZERO,
                sample_count: n,
            };
        }
        let (mean, sample_variance) = Self::sample_mean_var(samples);
        let sample_sigma = decimal_sqrt(sample_variance);

        // Conservative σ: max(sample, ewma) when EWMA is available.
        let ewma_var = ewma_state
            .filter(|s| s.initialised)
            .map(|s| s.variance);
        let effective_sigma = match ewma_var {
            Some(ev) => {
                let ewma_sigma = decimal_sqrt(ev);
                if ewma_sigma > sample_sigma {
                    ewma_sigma
                } else {
                    sample_sigma
                }
            }
            None => sample_sigma,
        };

        let var_95 = mean - Z_SCORE_95 * effective_sigma;
        let var_99 = mean - Z_SCORE_99 * effective_sigma;

        // CVaR (Expected Shortfall) under Gaussian assumption:
        // CVaR_α = μ − σ · φ(z_α) / (1 − α)
        let cvar_95 = mean - effective_sigma * PHI_Z95 / dec!(0.05);
        let cvar_99 = mean - effective_sigma * PHI_Z99 / dec!(0.01);

        // Historical-simulation VaR: sort and pick empirical
        // quantile. Non-parametric — no distributional assumption.
        let (hist_var_95, hist_var_99) = Self::historical_var(samples);

        RiskMetrics {
            var_95,
            var_99,
            cvar_95,
            cvar_99,
            ewma_variance: ewma_var,
            hist_var_95,
            hist_var_99,
            mean,
            sample_variance,
            sample_count: n,
        }
    }

    /// Empirical quantile VaR: sort samples ascending, pick the
    /// `floor(N * (1 - α))`-th element as the α-level VaR.
    /// No distributional assumption.
    fn historical_var(samples: &VecDeque<Decimal>) -> (Decimal, Decimal) {
        let n = samples.len();
        if n < 2 {
            return (Decimal::ZERO, Decimal::ZERO);
        }
        let mut sorted: Vec<Decimal> = samples.iter().copied().collect();
        sorted.sort();
        // 5th percentile index (for 95% VaR).
        let idx_95 = (n as f64 * 0.05).floor() as usize;
        // 1st percentile index (for 99% VaR).
        let idx_99 = (n as f64 * 0.01).floor() as usize;
        let hv_95 = sorted[idx_95.min(n - 1)];
        let hv_99 = sorted[idx_99.min(n - 1)];
        (hv_95, hv_99)
    }

    /// Read-only accessor for a strategy class's current sample
    /// count. Used by the dashboard "VaR guard status" panel and
    /// by the tests below to assert the rolling window eviction.
    pub fn sample_count(&self, strategy_class: &str) -> usize {
        self.buffers
            .get(strategy_class)
            .map(|b| b.len())
            .unwrap_or(0)
    }

    /// Read-only iterator-style access to every strategy class
    /// the guard has seen a sample for. Used by the dashboard
    /// to enumerate the per-strategy status rows.
    pub fn strategy_classes(&self) -> Vec<String> {
        let mut v: Vec<String> = self.buffers.keys().cloned().collect();
        v.sort();
        v
    }
}

/// Newton-Raphson square root for `Decimal`. `rust_decimal` does
/// not ship a sqrt of its own; this helper is a small
/// self-contained implementation good enough for the VaR
/// guard's σ computation (Gaussian VaR does not need more than
/// ~8 significant digits of σ).
fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    // Initial guess: x / 2, clamped to at least dec!(1) to
    // avoid a painful first iteration for huge inputs.
    let mut guess = if x > Decimal::ONE { x / dec!(2) } else { x };
    for _ in 0..64 {
        let next = (guess + x / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000001) {
            return next;
        }
        guess = next;
    }
    guess
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(limit_95: Option<Decimal>, limit_99: Option<Decimal>) -> VarGuardConfig {
        VarGuardConfig {
            limit_95,
            limit_99,
            ewma_lambda: None,
        }
    }

    /// Warm-up: fewer than `MIN_SAMPLES_FOR_VAR` samples → the
    /// guard returns 1.0 regardless of how bad the buffer
    /// looks. Regression anchor for the "first few minutes
    /// after engine start random-throttle" failure mode.
    #[test]
    fn warmup_returns_one_below_min_samples() {
        let mut g = VarGuard::new(config(Some(dec!(-10)), Some(dec!(-20))));
        for _ in 0..5 {
            g.record_pnl_sample("basis", dec!(-1000));
        }
        assert_eq!(g.effective_throttle("basis"), Decimal::ONE);
    }

    /// Zero-variance buffer: σ = 0, VaR = μ exactly. When μ
    /// sits above both floors the guard returns 1.0 (no breach).
    #[test]
    fn zero_variance_no_breach_returns_one() {
        let mut g = VarGuard::new(config(Some(dec!(-10)), Some(dec!(-20))));
        for _ in 0..50 {
            g.record_pnl_sample("basis", dec!(1));
        }
        assert_eq!(g.effective_throttle("basis"), Decimal::ONE);
    }

    /// Zero-variance buffer WITH a mean below the VaR_95 floor:
    /// VaR_95 = μ = -15, below -10 → throttle 0.5.
    #[test]
    fn zero_variance_mean_below_95_floor_throttles_half() {
        let mut g = VarGuard::new(config(Some(dec!(-10)), Some(dec!(-100))));
        for _ in 0..50 {
            g.record_pnl_sample("basis", dec!(-15));
        }
        assert_eq!(g.effective_throttle("basis"), THROTTLE_95_TIER);
    }

    /// 99 % tier breach → hard halt (throttle = 0.0).
    /// Samples average around -600 with small oscillation, so
    /// VaR_99 ≈ -600 - tiny·2.326 = still below the -500 floor
    /// → hard halt.
    #[test]
    fn ninety_nine_percent_breach_hard_halts() {
        let mut g = VarGuard::new(config(Some(dec!(-10)), Some(dec!(-500))));
        for i in 0..50 {
            // Alternating -595 / -605 → mean = -600, σ ≈ 5.
            let sample = if i % 2 == 0 { dec!(-595) } else { dec!(-605) };
            g.record_pnl_sample("basis", sample);
        }
        let buf = g.buffers.get("basis").unwrap().clone();
        let (_var_95, var_99) = VarGuard::compute_var(&buf);
        assert!(
            var_99 < dec!(-500),
            "test setup must put VaR_99 below the floor, got {}",
            var_99
        );
        assert_eq!(g.effective_throttle("basis"), THROTTLE_99_TIER);
    }

    /// Multi-strategy isolation: strategy A in breach MUST NOT
    /// throttle strategy B. This is the whole point of a per-
    /// strategy guard.
    #[test]
    fn multi_strategy_isolation() {
        let mut g = VarGuard::new(config(Some(dec!(-5)), Some(dec!(-100))));
        for _ in 0..50 {
            g.record_pnl_sample("bad", dec!(-20));
        }
        for _ in 0..50 {
            g.record_pnl_sample("good", dec!(10));
        }
        assert_eq!(g.effective_throttle("bad"), THROTTLE_95_TIER);
        assert_eq!(g.effective_throttle("good"), Decimal::ONE);
    }

    /// Rolling window eviction: pushing more than
    /// `MAX_SAMPLES_PER_CLASS` samples caps the buffer at that
    /// size (oldest evicted first).
    #[test]
    fn rolling_window_evicts_stale_samples() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        for i in 0..(MAX_SAMPLES_PER_CLASS + 100) {
            g.record_pnl_sample("basis", Decimal::from(i as u64));
        }
        assert_eq!(g.sample_count("basis"), MAX_SAMPLES_PER_CLASS);
        // The first 100 samples must have been evicted — the
        // buffer should now contain samples [100, 100+MAX).
        let buf = g.buffers.get("basis").unwrap();
        assert_eq!(buf.front().copied(), Some(Decimal::from(100u64)));
        assert_eq!(
            buf.back().copied(),
            Some(Decimal::from((MAX_SAMPLES_PER_CLASS + 99) as u64))
        );
    }

    /// `None` limits disable the throttle entirely — even an
    /// obviously-breaching sample set returns 1.0. Models the
    /// "operator opts out of VaR" config state.
    #[test]
    fn no_limits_configured_always_returns_one() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        for _ in 0..100 {
            g.record_pnl_sample("basis", dec!(-1000));
        }
        assert_eq!(g.effective_throttle("basis"), Decimal::ONE);
    }

    /// Only the 99 % tier configured: breach still triggers a
    /// hard halt. The 95 % path is inert (its limit is `None`).
    #[test]
    fn only_99_pct_configured_still_hard_halts_on_breach() {
        let mut g = VarGuard::new(config(None, Some(dec!(-5))));
        for _ in 0..50 {
            g.record_pnl_sample("basis", dec!(-20));
        }
        assert_eq!(g.effective_throttle("basis"), THROTTLE_99_TIER);
    }

    /// Unknown strategy class returns 1.0 — the guard is
    /// permissive by default so a newly-introduced strategy
    /// does not need an operator config change before it can
    /// start trading.
    #[test]
    fn unknown_strategy_class_returns_one() {
        let g = VarGuard::new(config(Some(dec!(-10)), Some(dec!(-100))));
        assert_eq!(g.effective_throttle("brand_new_strategy"), Decimal::ONE);
    }

    /// `strategy_classes()` is sorted so the dashboard renders
    /// the per-strategy VaR panel in a deterministic order.
    #[test]
    fn strategy_classes_are_sorted_alphabetically() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        g.record_pnl_sample("zebra", dec!(1));
        g.record_pnl_sample("alpha", dec!(1));
        g.record_pnl_sample("middle", dec!(1));
        assert_eq!(
            g.strategy_classes(),
            vec![
                "alpha".to_string(),
                "middle".to_string(),
                "zebra".to_string()
            ]
        );
    }

    /// `decimal_sqrt` helper: pin the Newton-Raphson output
    /// against a few known values so a future refactor
    /// cannot silently regress the σ computation.
    #[test]
    fn decimal_sqrt_hits_known_values() {
        assert!((decimal_sqrt(dec!(4)) - dec!(2)).abs() < dec!(0.00001));
        assert!((decimal_sqrt(dec!(100)) - dec!(10)).abs() < dec!(0.00001));
        assert!((decimal_sqrt(dec!(2)) - dec!(1.41421)).abs() < dec!(0.001));
        assert_eq!(decimal_sqrt(dec!(0)), dec!(0));
        assert_eq!(decimal_sqrt(dec!(-5)), dec!(0)); // negative guard
    }

    // ── CVaR / Expected Shortfall tests ─────────────────────

    /// CVaR_95 is strictly more negative than VaR_95 — the
    /// expected shortfall in the tail is always worse than the
    /// threshold itself.
    #[test]
    fn cvar_is_more_negative_than_var() {
        let mut g = VarGuard::new(config(Some(dec!(-1000)), Some(dec!(-2000))));
        for i in 0..100 {
            let sample = Decimal::from(i % 10) - dec!(5);
            g.record_pnl_sample("test", sample);
        }
        let m = g.risk_metrics("test").unwrap();
        assert!(
            m.cvar_95 < m.var_95,
            "CVaR_95={} should be < VaR_95={}",
            m.cvar_95,
            m.var_95
        );
        assert!(
            m.cvar_99 < m.var_99,
            "CVaR_99={} should be < VaR_99={}",
            m.cvar_99,
            m.var_99
        );
    }

    /// Zero-variance samples: CVaR = VaR = μ (all tail mass
    /// concentrates at the single point).
    #[test]
    fn cvar_equals_var_when_zero_variance() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        for _ in 0..50 {
            g.record_pnl_sample("flat", dec!(5));
        }
        let m = g.risk_metrics("flat").unwrap();
        assert_eq!(m.var_95, m.cvar_95);
        assert_eq!(m.var_99, m.cvar_99);
        assert_eq!(m.mean, dec!(5));
    }

    /// risk_metrics returns None for unknown or under-sampled
    /// strategy classes.
    #[test]
    fn risk_metrics_none_when_insufficient_samples() {
        let g = VarGuard::new(VarGuardConfig::default());
        assert!(g.risk_metrics("unknown").is_none());

        let mut g2 = VarGuard::new(VarGuardConfig::default());
        for _ in 0..5 {
            g2.record_pnl_sample("few", dec!(1));
        }
        assert!(g2.risk_metrics("few").is_none());
    }

    // ── EWMA variance tests ────────────────────────────────

    /// EWMA variance reacts faster to a regime change than
    /// sample variance. Inject 100 calm samples followed by
    /// 10 volatile ones — the EWMA σ should be higher than
    /// the sample σ (which is dragged by the calm history).
    #[test]
    fn ewma_reacts_faster_to_regime_change() {
        let mut g = VarGuard::new(VarGuardConfig {
            limit_95: Some(dec!(-1000)),
            limit_99: Some(dec!(-2000)),
            ewma_lambda: Some(dec!(0.94)),
        });
        // 100 calm samples.
        for _ in 0..100 {
            g.record_pnl_sample("test", dec!(1));
        }
        // 10 volatile samples.
        for i in 0..10 {
            let v = if i % 2 == 0 { dec!(100) } else { dec!(-100) };
            g.record_pnl_sample("test", v);
        }
        let m = g.risk_metrics("test").unwrap();
        let ewma_var = m.ewma_variance.unwrap();
        // EWMA variance should be much higher than sample
        // variance because the last 10 explosive samples dominate.
        assert!(
            ewma_var > m.sample_variance,
            "EWMA var={} should exceed sample var={} after regime shift",
            ewma_var,
            m.sample_variance
        );
    }

    /// When EWMA is enabled and its σ exceeds sample σ, the
    /// VaR uses the EWMA σ (conservative) — so VaR is more
    /// negative than it would be with sample σ alone.
    #[test]
    fn ewma_makes_var_more_conservative() {
        // Guard WITHOUT EWMA.
        let mut g_no_ewma = VarGuard::new(VarGuardConfig {
            limit_95: Some(dec!(-1000)),
            limit_99: None,
            ewma_lambda: None,
        });
        // Guard WITH EWMA.
        let mut g_ewma = VarGuard::new(VarGuardConfig {
            limit_95: Some(dec!(-1000)),
            limit_99: None,
            ewma_lambda: Some(dec!(0.94)),
        });
        // Same data to both: calm then volatile.
        for _ in 0..80 {
            g_no_ewma.record_pnl_sample("s", dec!(1));
            g_ewma.record_pnl_sample("s", dec!(1));
        }
        for i in 0..20 {
            let v = if i % 2 == 0 { dec!(50) } else { dec!(-50) };
            g_no_ewma.record_pnl_sample("s", v);
            g_ewma.record_pnl_sample("s", v);
        }
        let m_no = g_no_ewma.risk_metrics("s").unwrap();
        let m_ew = g_ewma.risk_metrics("s").unwrap();
        // EWMA guard should produce a more negative (worse) VaR.
        assert!(
            m_ew.var_95 <= m_no.var_95,
            "EWMA VaR_95={} should be ≤ sample-only VaR_95={}",
            m_ew.var_95,
            m_no.var_95
        );
    }

    /// EWMA state is not populated when ewma_lambda is None.
    #[test]
    fn ewma_not_populated_without_config() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        for _ in 0..50 {
            g.record_pnl_sample("test", dec!(1));
        }
        let m = g.risk_metrics("test").unwrap();
        assert!(m.ewma_variance.is_none());
    }

    // ── Historical-simulation VaR tests ─────────────────────

    /// Historical VaR on a known sorted sequence: 100 samples
    /// from 0..99. The 5th percentile is at index 5 (value 5),
    /// 1st percentile at index 1 (value 1).
    #[test]
    fn hist_var_on_known_sequence() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        for i in 0..100 {
            g.record_pnl_sample("test", Decimal::from(i));
        }
        let m = g.risk_metrics("test").unwrap();
        assert_eq!(m.hist_var_95, dec!(5));
        assert_eq!(m.hist_var_99, dec!(1));
    }

    /// Historical VaR with negative samples: 100 samples from
    /// -50..49. The 5th percentile should be around -45.
    #[test]
    fn hist_var_with_negative_samples() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        for i in 0..100 {
            g.record_pnl_sample("test", Decimal::from(i as i64 - 50));
        }
        let m = g.risk_metrics("test").unwrap();
        assert_eq!(m.hist_var_95, dec!(-45));
        assert_eq!(m.hist_var_99, dec!(-49));
    }

    /// Historical VaR is always ≥ parametric VaR for skewed data
    /// (heavy left tail makes parametric overestimate risk).
    /// For symmetric data they should be close.
    #[test]
    fn hist_var_and_parametric_var_are_comparable() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        // Symmetric-ish data: alternating ±1.
        for i in 0..200 {
            let v = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
            g.record_pnl_sample("sym", v);
        }
        let m = g.risk_metrics("sym").unwrap();
        // Both should be negative and roughly similar magnitude.
        assert!(m.hist_var_95 < Decimal::ZERO);
        assert!(m.var_95 < Decimal::ZERO);
        // Within a factor of 3 of each other.
        let ratio = if m.var_95.abs() > Decimal::ZERO {
            m.hist_var_95 / m.var_95
        } else {
            Decimal::ONE
        };
        assert!(
            ratio > dec!(0.3) && ratio < dec!(3),
            "hist_var_95={} and var_95={} should be comparable (ratio={})",
            m.hist_var_95,
            m.var_95,
            ratio
        );
    }

    /// Zero-variance samples: historical VaR equals the constant.
    #[test]
    fn hist_var_constant_samples() {
        let mut g = VarGuard::new(VarGuardConfig::default());
        for _ in 0..50 {
            g.record_pnl_sample("flat", dec!(7));
        }
        let m = g.risk_metrics("flat").unwrap();
        assert_eq!(m.hist_var_95, dec!(7));
        assert_eq!(m.hist_var_99, dec!(7));
    }
}
