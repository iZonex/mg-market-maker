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
}

/// Per-strategy rolling-window VaR guard.
#[derive(Debug, Clone)]
pub struct VarGuard {
    config: VarGuardConfig,
    /// One ring buffer of PnL samples per strategy class.
    /// Key is the owned `String` tag from `Strategy::name()`
    /// (same key convention as `Portfolio::per_strategy_pnl`).
    buffers: HashMap<String, VecDeque<Decimal>>,
}

impl VarGuard {
    pub fn new(config: VarGuardConfig) -> Self {
        Self {
            config,
            buffers: HashMap::new(),
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
    }

    /// Effective size-multiplier for the given strategy class.
    /// Returns `1.0` during warm-up (`< MIN_SAMPLES_FOR_VAR`
    /// samples) and when no VaR limit is configured. The
    /// engine composes this with other multipliers via `min()`.
    pub fn effective_throttle(&self, strategy_class: &str) -> Decimal {
        let Some(buf) = self.buffers.get(strategy_class) else {
            return Decimal::ONE;
        };
        if buf.len() < MIN_SAMPLES_FOR_VAR {
            return Decimal::ONE;
        }
        let (var_95, var_99) = Self::compute_var(buf);
        // Hard halt tier first — `min` semantics mean the
        // tighter of the two wins if both breach simultaneously.
        if let Some(limit_99) = self.config.limit_99 {
            if var_99 < limit_99 {
                return THROTTLE_99_TIER;
            }
        }
        if let Some(limit_95) = self.config.limit_95 {
            if var_95 < limit_95 {
                return THROTTLE_95_TIER;
            }
        }
        Decimal::ONE
    }

    /// Pure computation of (VaR_95, VaR_99) from a sample
    /// window. Exposed for tests.
    pub fn compute_var(samples: &VecDeque<Decimal>) -> (Decimal, Decimal) {
        let n = samples.len();
        if n < 2 {
            return (Decimal::ZERO, Decimal::ZERO);
        }
        let n_dec = Decimal::from(n as u32);
        let mean = samples.iter().copied().sum::<Decimal>() / n_dec;
        let var_sum: Decimal = samples
            .iter()
            .map(|p| {
                let diff = *p - mean;
                diff * diff
            })
            .sum();
        let variance = var_sum / Decimal::from((n - 1) as u32);
        let sigma = decimal_sqrt(variance);
        let var_95 = mean - Z_SCORE_95 * sigma;
        let var_99 = mean - Z_SCORE_99 * sigma;
        (var_95, var_99)
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
        VarGuardConfig { limit_95, limit_99 }
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
}
