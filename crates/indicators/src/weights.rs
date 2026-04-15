//! Weight generators for kernel-style smoothing.
//!
//! Small, pure helpers — no state, no clocks, no RNG. Used by
//! alpha-signal code that needs a hand-shaped weight vector
//! without owning a full moving-average state machine.
//!
//! Ported from `beatzxbt/mm-toolbox`'s `weights` module (MIT).
//! The upstream implementation is a Numba-JIT'd numpy helper;
//! the Rust port uses `Vec<Decimal>` because these weights
//! feed `Decimal` math downstream (no float round-tripping).

use rust_decimal::Decimal;

/// Generate `num` geometric-decay weights summing to `1.0`.
///
/// Weight `i` is `r^i` (`0 ≤ i < num`), then the whole vector
/// is L1-normalised. Smaller `r` means a heavier tail at index
/// 0 — i.e. the most recent observation dominates. The
/// upstream default is `r = 0.75`.
///
/// # Panics
/// Panics if `num < 2` or `r` is not in `(0, 1)`.
pub fn geometric_weights(num: usize, r: Decimal) -> Vec<Decimal> {
    assert!(num >= 2, "geometric_weights: num must be >= 2");
    assert!(
        r > Decimal::ZERO && r < Decimal::ONE,
        "geometric_weights: r must be in (0, 1)"
    );
    let mut raw: Vec<Decimal> = Vec::with_capacity(num);
    let mut acc = Decimal::ONE;
    for _ in 0..num {
        raw.push(acc);
        acc *= r;
    }
    let total: Decimal = raw.iter().copied().sum();
    raw.into_iter().map(|w| w / total).collect()
}

/// Generate `window` EMA-equivalent weights. If `alpha` is
/// omitted, the upstream heuristic `α = 3 / (window + 1)` is
/// used — i.e. a slightly flatter weighting than the more
/// common `α = 2 / (window + 1)` used by the `Ema` indicator,
/// chosen by Hull and beatzxbt for shorter effective lag.
///
/// The returned vector is **ordered oldest → newest** with the
/// newest element carrying the largest weight. Weights are
/// L1-normalised so the vector sums to `1.0`.
///
/// # Panics
/// Panics if `window < 2` or `alpha` (when supplied) is not in
/// `(0, 1]`.
pub fn ema_weights(window: usize, alpha: Option<Decimal>) -> Vec<Decimal> {
    assert!(window >= 2, "ema_weights: window must be >= 2");
    let alpha = alpha.unwrap_or_else(|| Decimal::from(3) / Decimal::from(window as i64 + 1));
    assert!(
        alpha > Decimal::ZERO && alpha <= Decimal::ONE,
        "ema_weights: alpha must be in (0, 1]"
    );
    let one_minus_alpha = Decimal::ONE - alpha;
    // Oldest first: `alpha * (1 - alpha)^{window-1-i}` for
    // `i = 0..window`. This matches the upstream Python
    // implementation: `[alpha * (1 - alpha)^i for i in
    // range(window-1, -1, -1)]`.
    let mut raw: Vec<Decimal> = Vec::with_capacity(window);
    for i in (0..window).rev() {
        // Integer exponent via repeated multiplication —
        // window is small (tens, not thousands) so this is
        // cheap and avoids a `powf` round-trip.
        let mut pow = Decimal::ONE;
        for _ in 0..i {
            pow *= one_minus_alpha;
        }
        raw.push(alpha * pow);
    }
    let total: Decimal = raw.iter().copied().sum();
    if total.is_zero() {
        // Pathological: `alpha = 1` and `window = 1` would
        // land here. Return a uniform vector as a fallback so
        // callers never divide by zero.
        let uniform = Decimal::ONE / Decimal::from(window as i64);
        return vec![uniform; window];
    }
    raw.into_iter().map(|w| w / total).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn approx_eq(a: Decimal, b: Decimal, eps: Decimal) {
        let diff = (a - b).abs();
        assert!(diff <= eps, "expected {a} ≈ {b} (eps {eps}), diff = {diff}");
    }

    /// Geometric weights sum to exactly 1 after normalisation.
    #[test]
    fn geometric_weights_sum_to_one() {
        let w = geometric_weights(8, dec!(0.75));
        let total: Decimal = w.iter().copied().sum();
        approx_eq(total, Decimal::ONE, dec!(0.000000001));
    }

    /// Smaller `r` means a heavier tail on the newest side:
    /// the last element is larger relative to the first.
    #[test]
    fn geometric_weights_heavier_at_index_zero_for_small_r() {
        let w = geometric_weights(5, dec!(0.5));
        // Upstream convention: index 0 has the largest weight
        // and subsequent indices decay geometrically. Confirm
        // that ordering.
        assert!(w[0] > w[1] && w[1] > w[2] && w[2] > w[3] && w[3] > w[4]);
    }

    /// EMA weights default to `alpha = 3/(window+1)` and sum
    /// to 1.
    #[test]
    fn ema_weights_default_alpha_sums_to_one() {
        let w = ema_weights(10, None);
        let total: Decimal = w.iter().copied().sum();
        approx_eq(total, Decimal::ONE, dec!(0.000000001));
    }

    /// EMA weights with a custom alpha still normalise cleanly.
    #[test]
    fn ema_weights_explicit_alpha_sums_to_one() {
        let w = ema_weights(6, Some(dec!(0.4)));
        let total: Decimal = w.iter().copied().sum();
        approx_eq(total, Decimal::ONE, dec!(0.000000001));
        // Newest weight (last element) must be the largest —
        // matches the upstream "oldest → newest" ordering.
        assert!(w[5] > w[0]);
    }

    /// Invalid inputs must panic per the contract.
    #[test]
    #[should_panic]
    fn geometric_weights_panics_on_tiny_num() {
        let _ = geometric_weights(1, dec!(0.5));
    }

    #[test]
    #[should_panic]
    fn geometric_weights_panics_on_out_of_range_r() {
        let _ = geometric_weights(4, dec!(1.5));
    }

    #[test]
    #[should_panic]
    fn ema_weights_panics_on_tiny_window() {
        let _ = ema_weights(1, None);
    }
}
