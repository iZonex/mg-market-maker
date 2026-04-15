//! Cartea-Jaimungal-Penalva ch.4 §4.3 closed-form optimal
//! quoted half-spread with an adverse-selection component
//! (Epic D, sub-component #4).
//!
//! # Formula
//!
//! ```text
//! δ*(t) = (1/γ) · ln(1 + γ/κ)  +  (1 − 2ρ) · σ · √(T − t)
//! ```
//!
//! Where:
//! - `γ` — MM risk aversion
//! - `κ` — order-arrival intensity decay constant
//! - `σ` — short-horizon volatility
//! - `T − t` — time to horizon end
//! - `ρ` — adverse-selection probability ∈ [0, 1]
//!
//! When `ρ = 0.5` (uninformed flow), the additive term
//! vanishes and the formula collapses to the classic
//! Avellaneda-Stoikov half-spread. When `ρ > 0.5`, the
//! additive term is *negative* and the quoted spread
//! shrinks — the MM "gets out of the way" of informed
//! flow. Per CJP 2015 figure 4.6.
//!
//! v1 uses the symmetric single-`ρ` variant and clamps the
//! final output at zero so `ρ > 0.5` with large
//! `σ · √(T − t)` never produces a negative quoted spread.
//!
//! Stage-2 adds [`quoted_half_spread_per_side`] — a pure
//! function returning `(δ_b, δ_a)` with independent
//! `ρ_b` / `ρ_a` per side. The engine still plumbs the
//! symmetric single-`ρ` through `StrategyContext::as_prob`;
//! wiring the asymmetric pair through the context is a
//! stage-3 follow-up coordinated with the Track 1 changes on
//! `crates/engine/src/market_maker.rs`. Any non-strategy
//! caller (offline calibration, research notebook) can import
//! this function directly today.
//!
//! # Source attribution
//!
//! Cartea, Á., Jaimungal, S., Penalva, J. — "Algorithmic
//! and High-Frequency Trading," Cambridge University Press,
//! 2015. Chapter 4 §4.3 "Market Making with Adverse
//! Selection," eq. (4.20).
//!
//! Full derivation + sign-convention discussion in
//! `docs/research/signal-wave-2-formulas.md`
//! §"Sub-component #4".

use crate::volatility::decimal_sqrt;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Cartea-Jaimungal-Penalva ch.4 §4.3 closed-form optimal
/// quoted half-spread. See module docs for the formula.
///
/// Output is clamped at `0` — `ρ > 0.5` combined with a large
/// `σ · √(T − t)` would otherwise produce a negative spread
/// and the strategy would quote through itself.
pub fn quoted_half_spread(
    gamma: Decimal,
    kappa: Decimal,
    sigma: Decimal,
    t_minus_t: Decimal,
    as_prob: Decimal,
) -> Decimal {
    if gamma.is_zero() || kappa.is_zero() {
        return Decimal::ZERO;
    }
    let base = decimal_ln(Decimal::ONE + gamma / kappa) / gamma;
    let as_component = (Decimal::ONE - Decimal::TWO * as_prob) * sigma * decimal_sqrt(t_minus_t);
    (base + as_component).max(Decimal::ZERO)
}

/// Per-side asymmetric variant of [`quoted_half_spread`].
/// Returns `(δ_b, δ_a)` where each side gets its own additive
/// Cartea adverse-selection component:
///
/// ```text
/// base = (1/γ) · ln(1 + γ/κ)
/// δ_b  = max(0, base + (1 − 2·ρ_b) · σ · √(T − t))
/// δ_a  = max(0, base + (1 − 2·ρ_a) · σ · √(T − t))
/// ```
///
/// When `ρ_b == ρ_a == ρ`, the output collapses to
/// `(quoted_half_spread(ρ), quoted_half_spread(ρ))` byte-for-byte.
/// This is the invariant exercised by the
/// `per_side_symmetric_collapses` test.
///
/// Each side is independently clamped at zero so a large
/// `σ · √(T − t)` combined with `ρ_side > 0.5` never produces
/// a negative quoted side.
///
/// Stage-2 only exposes this as a pure function — wiring it
/// through `StrategyContext` waits on stage-3 coordination
/// with the engine-side `market_maker.rs` changes owned by
/// Track 1.
pub fn quoted_half_spread_per_side(
    gamma: Decimal,
    kappa: Decimal,
    sigma: Decimal,
    t_minus_t: Decimal,
    rho_bid: Decimal,
    rho_ask: Decimal,
) -> (Decimal, Decimal) {
    if gamma.is_zero() || kappa.is_zero() {
        return (Decimal::ZERO, Decimal::ZERO);
    }
    let base = decimal_ln(Decimal::ONE + gamma / kappa) / gamma;
    let sigma_root_t = sigma * decimal_sqrt(t_minus_t);
    let delta_bid =
        (base + (Decimal::ONE - Decimal::TWO * rho_bid) * sigma_root_t).max(Decimal::ZERO);
    let delta_ask =
        (base + (Decimal::ONE - Decimal::TWO * rho_ask) * sigma_root_t).max(Decimal::ZERO);
    (delta_bid, delta_ask)
}

/// Map an adverse-selection metric in bps to the `ρ` input
/// of [`quoted_half_spread`]. Piecewise-linear: `as_bps = 0`
/// (no signal) → `ρ = 0.5` (neutral, no additive effect),
/// `as_bps = +20` → `ρ = 1.0` (maximal shrinkage),
/// `as_bps = −20` → `ρ = 0.0` (maximal widening).
///
/// The ±20 bps saturation is a config-light default picked
/// to match the typical crypto-pair range from the existing
/// `mm_risk::toxicity::AdverseSelectionTracker`. Operators
/// who want a different scale compute `ρ` themselves before
/// calling [`quoted_half_spread`].
pub fn as_prob_from_bps(as_bps: Decimal) -> Decimal {
    let p = dec!(0.5) + (as_bps / dec!(20));
    p.max(Decimal::ZERO).min(Decimal::ONE)
}

/// Natural logarithm on `Decimal`. Uses range reduction
/// `ln(x) = k · ln 2 + ln(y)` with `y ∈ [0.5, 1.5]`, then
/// a Taylor series on `ln(1 + (y − 1))`.
///
/// Accuracy: ~10 decimal places on `x ∈ [1e-6, 1e6]`. Returns
/// `0` on `x ≤ 0` (undefined in the reals).
///
/// Kept in this module rather than promoted to a shared
/// helper because this is the only v1 caller — stage-2 can
/// promote alongside `volatility::decimal_sqrt` if other
/// transcendental call sites surface.
pub fn decimal_ln(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    // ln(2) to 19 decimals (plenty for Decimal's 28-digit
    // mantissa in practice).
    let ln_2 = dec!(0.6931471805599453094);

    // Range reduction: halve until we're below 1.5, double
    // until we're above 0.5.
    let mut k = 0i32;
    let mut y = x;
    while y > dec!(1.5) {
        y /= dec!(2);
        k += 1;
    }
    while y < dec!(0.5) {
        y *= dec!(2);
        k -= 1;
    }
    // y ∈ [0.5, 1.5] so u = y - 1 ∈ [-0.5, 0.5].
    let u = y - Decimal::ONE;
    // Taylor series for ln(1 + u):
    //   ln(1+u) = u - u²/2 + u³/3 - u⁴/4 + ...
    // Converges for |u| < 1; at |u| = 0.5 we need ~40 terms
    // to hit 1e-10 accuracy.
    let mut sum = Decimal::ZERO;
    let mut power = u;
    let mut sign = Decimal::ONE;
    for n in 1..=60 {
        let term = power / Decimal::from(n);
        sum += sign * term;
        if term.abs() < dec!(0.00000000001) {
            break;
        }
        power *= u;
        sign = -sign;
    }
    sum + Decimal::from(k) * ln_2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Decimal, b: Decimal, eps: Decimal) -> bool {
        (a - b).abs() < eps
    }

    // ------------------------- decimal_ln -------------------------

    #[test]
    fn decimal_ln_of_one_is_zero() {
        assert_eq!(decimal_ln(dec!(1)), Decimal::ZERO);
    }

    #[test]
    fn decimal_ln_of_e_is_one() {
        // e ≈ 2.718281828459045
        let e = dec!(2.718281828459045);
        let result = decimal_ln(e);
        assert!(
            approx(result, dec!(1), dec!(0.0000001)),
            "ln(e)={result}, expected ≈ 1"
        );
    }

    #[test]
    fn decimal_ln_of_two_matches_constant() {
        let result = decimal_ln(dec!(2));
        assert!(
            approx(result, dec!(0.6931471805599453), dec!(0.0000001)),
            "ln(2)={result}"
        );
    }

    #[test]
    fn decimal_ln_of_half_is_negative_ln_two() {
        let result = decimal_ln(dec!(0.5));
        assert!(
            approx(result, dec!(-0.6931471805599453), dec!(0.0000001)),
            "ln(0.5)={result}"
        );
    }

    #[test]
    fn decimal_ln_nonpositive_returns_zero_guard() {
        assert_eq!(decimal_ln(Decimal::ZERO), Decimal::ZERO);
        assert_eq!(decimal_ln(dec!(-5)), Decimal::ZERO);
    }

    #[test]
    fn decimal_ln_large_and_small_values() {
        // ln(100) ≈ 4.605170185988091
        let ln_100 = decimal_ln(dec!(100));
        assert!(
            approx(ln_100, dec!(4.605170185988091), dec!(0.0000001)),
            "ln(100)={ln_100}"
        );
        // ln(0.01) ≈ -4.605170185988091
        let ln_small = decimal_ln(dec!(0.01));
        assert!(
            approx(ln_small, dec!(-4.605170185988091), dec!(0.0000001)),
            "ln(0.01)={ln_small}"
        );
    }

    // ------------------------- as_prob_from_bps -------------------------

    #[test]
    fn as_prob_from_bps_zero_is_neutral() {
        assert_eq!(as_prob_from_bps(Decimal::ZERO), dec!(0.5));
    }

    #[test]
    fn as_prob_from_bps_positive_saturates_at_one() {
        assert_eq!(as_prob_from_bps(dec!(20)), dec!(1));
        assert_eq!(as_prob_from_bps(dec!(100)), dec!(1));
    }

    #[test]
    fn as_prob_from_bps_negative_saturates_at_zero() {
        assert_eq!(as_prob_from_bps(dec!(-20)), Decimal::ZERO);
        assert_eq!(as_prob_from_bps(dec!(-100)), Decimal::ZERO);
    }

    #[test]
    fn as_prob_from_bps_mid_range_is_linear() {
        // +10 bps → 0.5 + 0.5 = 1.0 when scaled over ±20 → actually 0.75
        // (10 / 20) + 0.5 = 0.5 + 0.5 = 1.0. Wait — 10/20 = 0.5 so ρ = 1.0.
        // Correction: the formula is 0.5 + bps/20, so +10 → 1.0.
        // Let me test +5 → 0.75 instead which is unambiguous.
        assert_eq!(as_prob_from_bps(dec!(5)), dec!(0.75));
        assert_eq!(as_prob_from_bps(dec!(-5)), dec!(0.25));
    }

    // ------------------------- quoted_half_spread -------------------------

    #[test]
    fn rho_half_collapses_to_wave1_base() {
        // At ρ = 0.5, the AS component is zero and the
        // output equals (1/γ)·ln(1 + γ/κ).
        let gamma = dec!(0.1);
        let kappa = dec!(1.5);
        let sigma = dec!(0.02);
        let t_minus_t = dec!(60);
        let output = quoted_half_spread(gamma, kappa, sigma, t_minus_t, dec!(0.5));
        let expected = decimal_ln(Decimal::ONE + gamma / kappa) / gamma;
        assert_eq!(output, expected.max(Decimal::ZERO));
    }

    #[test]
    fn rho_one_shrinks_spread() {
        // ρ = 1.0 produces the most negative AS component;
        // for large enough σ·√(T-t), the base is fully
        // cancelled and the output clamps at zero.
        let gamma = dec!(0.1);
        let kappa = dec!(1.5);
        let sigma = dec!(10); // absurdly large to force the clamp
        let t_minus_t = dec!(60);
        let output = quoted_half_spread(gamma, kappa, sigma, t_minus_t, dec!(1));
        assert_eq!(output, Decimal::ZERO, "clamp should fire");
    }

    #[test]
    fn rho_zero_widens_spread_maximally() {
        // ρ = 0 means the AS component is +σ·√(T-t), added
        // to the wave-1 base — max widening.
        let gamma = dec!(0.1);
        let kappa = dec!(1.5);
        let sigma = dec!(0.02);
        let t_minus_t = dec!(60);
        let base = decimal_ln(Decimal::ONE + gamma / kappa) / gamma;
        let expected_delta = sigma * decimal_sqrt(t_minus_t);
        let output = quoted_half_spread(gamma, kappa, sigma, t_minus_t, Decimal::ZERO);
        let expected = base + expected_delta;
        assert!(
            approx(output, expected, dec!(0.0001)),
            "output={output}, expected={expected}"
        );
    }

    #[test]
    fn rho_increase_shrinks_spread_monotonically() {
        // Sweep ρ from 0 to 0.5 and verify the output is
        // monotonically decreasing.
        let gamma = dec!(0.1);
        let kappa = dec!(1.5);
        let sigma = dec!(0.02);
        let t_minus_t = dec!(60);
        let s_0 = quoted_half_spread(gamma, kappa, sigma, t_minus_t, dec!(0));
        let s_25 = quoted_half_spread(gamma, kappa, sigma, t_minus_t, dec!(0.25));
        let s_50 = quoted_half_spread(gamma, kappa, sigma, t_minus_t, dec!(0.5));
        assert!(s_0 > s_25);
        assert!(s_25 > s_50);
    }

    #[test]
    fn zero_gamma_returns_zero_guard() {
        let output = quoted_half_spread(Decimal::ZERO, dec!(1.5), dec!(0.02), dec!(60), dec!(0.5));
        assert_eq!(output, Decimal::ZERO);
    }

    #[test]
    fn zero_kappa_returns_zero_guard() {
        let output = quoted_half_spread(dec!(0.1), Decimal::ZERO, dec!(0.02), dec!(60), dec!(0.5));
        assert_eq!(output, Decimal::ZERO);
    }

    // ------------------ quoted_half_spread_per_side ------------------

    fn per_side_fixtures() -> (Decimal, Decimal, Decimal, Decimal) {
        (dec!(0.1), dec!(1.5), dec!(0.02), dec!(60))
    }

    #[test]
    fn per_side_symmetric_collapses_to_scalar_variant() {
        // When ρ_b == ρ_a, the per-side function must
        // reproduce `quoted_half_spread` on both sides.
        let (g, k, s, t) = per_side_fixtures();
        for rho in [dec!(0), dec!(0.25), dec!(0.5), dec!(0.75), dec!(1)] {
            let (db, da) = quoted_half_spread_per_side(g, k, s, t, rho, rho);
            let scalar = quoted_half_spread(g, k, s, t, rho);
            assert_eq!(db, scalar, "bid mismatch at ρ={rho}");
            assert_eq!(da, scalar, "ask mismatch at ρ={rho}");
        }
    }

    #[test]
    fn per_side_bid_widen_only_leaves_ask_at_neutral() {
        // ρ_b = 0 (maximal bid widen), ρ_a = 0.5 (neutral):
        // the bid side should widen vs the scalar neutral
        // value while the ask remains at the scalar neutral.
        let (g, k, s, t) = per_side_fixtures();
        let neutral = quoted_half_spread(g, k, s, t, dec!(0.5));
        let (db, da) = quoted_half_spread_per_side(g, k, s, t, dec!(0), dec!(0.5));
        assert!(db > neutral, "bid should widen: db={db}, neutral={neutral}");
        assert_eq!(da, neutral, "ask should stay neutral: da={da}");
    }

    #[test]
    fn per_side_ask_widen_only_leaves_bid_at_neutral() {
        // Mirror of the previous test.
        let (g, k, s, t) = per_side_fixtures();
        let neutral = quoted_half_spread(g, k, s, t, dec!(0.5));
        let (db, da) = quoted_half_spread_per_side(g, k, s, t, dec!(0.5), dec!(0));
        assert_eq!(db, neutral);
        assert!(da > neutral);
    }

    #[test]
    fn per_side_mixed_asymmetry_produces_distinct_outputs() {
        // ρ_b and ρ_a on opposite sides of 0.5: bid widens,
        // ask narrows, and the two sides are strictly
        // different.
        let (g, k, s, t) = per_side_fixtures();
        let (db, da) = quoted_half_spread_per_side(g, k, s, t, dec!(0.2), dec!(0.8));
        assert!(db > da, "bid should be wider than ask: db={db}, da={da}");
        let neutral = quoted_half_spread(g, k, s, t, dec!(0.5));
        assert!(db > neutral);
        assert!(da < neutral);
    }

    #[test]
    fn per_side_both_clamped_at_zero_under_extreme_informed_flow() {
        // ρ_b = ρ_a = 1 with σ·√(T − t) large enough to
        // overwhelm the wave-1 base → both sides clamp to 0.
        let g = dec!(0.1);
        let k = dec!(1.5);
        let s = dec!(10);
        let t = dec!(60);
        let (db, da) = quoted_half_spread_per_side(g, k, s, t, dec!(1), dec!(1));
        assert_eq!(db, Decimal::ZERO);
        assert_eq!(da, Decimal::ZERO);
    }

    #[test]
    fn per_side_monotone_in_each_side() {
        // Sweep ρ_b from 0 → 0.5 with ρ_a fixed, the bid side
        // should monotonically shrink; and symmetrically for
        // ρ_a.
        let (g, k, s, t) = per_side_fixtures();
        let mut prev_bid: Option<Decimal> = None;
        for rho_b in [dec!(0), dec!(0.1), dec!(0.25), dec!(0.4), dec!(0.5)] {
            let (db, _da) = quoted_half_spread_per_side(g, k, s, t, rho_b, dec!(0.5));
            if let Some(prev) = prev_bid {
                assert!(
                    db <= prev,
                    "bid non-monotone at ρ_b={rho_b}: db={db}, prev={prev}"
                );
            }
            prev_bid = Some(db);
        }
        let mut prev_ask: Option<Decimal> = None;
        for rho_a in [dec!(0), dec!(0.1), dec!(0.25), dec!(0.4), dec!(0.5)] {
            let (_db, da) = quoted_half_spread_per_side(g, k, s, t, dec!(0.5), rho_a);
            if let Some(prev) = prev_ask {
                assert!(
                    da <= prev,
                    "ask non-monotone at ρ_a={rho_a}: da={da}, prev={prev}"
                );
            }
            prev_ask = Some(da);
        }
    }

    #[test]
    fn per_side_zero_gamma_or_kappa_returns_zero_zero() {
        let (_, k, s, t) = per_side_fixtures();
        let (db, da) = quoted_half_spread_per_side(Decimal::ZERO, k, s, t, dec!(0.3), dec!(0.7));
        assert_eq!(db, Decimal::ZERO);
        assert_eq!(da, Decimal::ZERO);
        let (g, _, s, t) = per_side_fixtures();
        let (db, da) = quoted_half_spread_per_side(g, Decimal::ZERO, s, t, dec!(0.3), dec!(0.7));
        assert_eq!(db, Decimal::ZERO);
        assert_eq!(da, Decimal::ZERO);
    }

    #[test]
    fn as_prob_from_bps_roundtrip_through_quoted_half_spread() {
        // End-to-end: an AS bps measurement of 0 should
        // produce a ρ of 0.5, which should produce the
        // wave-1 base spread.
        let as_prob = as_prob_from_bps(Decimal::ZERO);
        assert_eq!(as_prob, dec!(0.5));
        let output = quoted_half_spread(dec!(0.1), dec!(1.5), dec!(0.02), dec!(60), as_prob);
        let base = decimal_ln(Decimal::ONE + dec!(0.1) / dec!(1.5)) / dec!(0.1);
        assert_eq!(output, base);
    }
}
