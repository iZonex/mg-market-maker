//! Hull Moving Average (HMA) and its supporting Weighted
//! Moving Average (WMA).
//!
//! The Hull Moving Average was proposed by Alan Hull in 2005
//! as a moving average that is **both smoother and more
//! responsive** than EMA or SMA. The trick is a two-stage
//! linear combination of weighted moving averages:
//!
//! ```text
//! HMA(n) = WMA( 2 · WMA(n/2) − WMA(n),  √n )
//! ```
//!
//! The short WMA tracks the signal, the long WMA smooths it,
//! `2·short − long` removes half of the lag introduced by the
//! long, and the final `WMA(√n)` smooths the jagged output.
//! Net result: lag ≈ `n/2 − √n/2 + 1` samples, noticeably
//! lower than `n/2` for an SMA of the same window.
//!
//! Ported from `beatzxbt/mm-toolbox`'s `moving_average/hma.py`
//! (MIT). The Python version uses Numba-JIT'd numpy arrays and
//! pre-allocated ring buffers; the Rust port uses a plain
//! `VecDeque<Decimal>` window per WMA. No loss of numerical
//! fidelity — Decimal is strictly more precise than the
//! upstream f64 computation.

use std::collections::VecDeque;

use rust_decimal::Decimal;

/// Weighted Moving Average over a fixed window.
///
/// Linear weights: the most recent sample gets weight `n`, the
/// oldest gets weight `1`, and the denominator is
/// `n·(n+1)/2`. Also known as a "linearly-weighted moving
/// average" — a standard building block for Alan Hull's HMA.
///
/// ```text
/// WMA(n) = Σ[i=1..n] (i · x_{t-n+i}) / (n·(n+1)/2)
/// ```
///
/// With fewer than `n` samples the WMA returns `None`.
#[derive(Debug, Clone)]
pub struct Wma {
    window: usize,
    samples: VecDeque<Decimal>,
    weight_sum: Decimal,
    value: Option<Decimal>,
}

impl Wma {
    pub fn new(window: usize) -> Self {
        assert!(window > 0, "Wma window must be > 0");
        let n = Decimal::from(window as i64);
        let weight_sum = n * (n + Decimal::ONE) / Decimal::from(2);
        Self {
            window,
            samples: VecDeque::with_capacity(window),
            weight_sum,
            value: None,
        }
    }

    pub fn window(&self) -> usize {
        self.window
    }

    /// Feed a new sample and recompute the value once warmed up.
    pub fn update(&mut self, sample: Decimal) {
        if self.samples.len() == self.window {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        if self.samples.len() < self.window {
            self.value = None;
            return;
        }
        // Linear-weighted sum: oldest sample gets weight 1,
        // newest gets weight `window`.
        let mut acc = Decimal::ZERO;
        for (i, v) in self.samples.iter().enumerate() {
            let w = Decimal::from((i + 1) as i64);
            acc += w * v;
        }
        self.value = Some(acc / self.weight_sum);
    }

    pub fn value(&self) -> Option<Decimal> {
        self.value
    }

    pub fn is_ready(&self) -> bool {
        self.samples.len() >= self.window
    }
}

/// Hull Moving Average: smoother and lower-lag than EMA/SMA
/// over the same window.
///
/// The HMA is built from three WMAs:
/// - `short = WMA(n/2)` — fast tracker
/// - `long  = WMA(n)`   — slow smoother
/// - `smooth = WMA(√n)` — final de-jitter on `2·short − long`
///
/// `value()` returns `Some` once enough samples have been
/// pushed for all three WMAs to be warmed up. Before that, it
/// returns `None` — callers must not read the raw internal
/// state, only the exposed method.
#[derive(Debug, Clone)]
pub struct Hma {
    window: usize,
    short_wma: Wma,
    long_wma: Wma,
    smooth_wma: Wma,
    value: Option<Decimal>,
}

impl Hma {
    /// Create a new HMA over `window` samples. Panics if
    /// `window < 4` — smaller windows degenerate (`√window` and
    /// `window/2` collapse to 1 or 2).
    pub fn new(window: usize) -> Self {
        assert!(window >= 4, "Hma window must be >= 4");
        let short_w = (window / 2).max(1);
        let smooth_w = ((window as f64).sqrt().round() as usize).max(1);
        Self {
            window,
            short_wma: Wma::new(short_w),
            long_wma: Wma::new(window),
            smooth_wma: Wma::new(smooth_w),
            value: None,
        }
    }

    pub fn window(&self) -> usize {
        self.window
    }

    /// Feed a new sample. Updates both feeder WMAs and, once
    /// they're warmed up, pushes the combined value into the
    /// smoothing WMA whose output is the HMA's value.
    pub fn update(&mut self, sample: Decimal) {
        self.short_wma.update(sample);
        self.long_wma.update(sample);
        let (Some(s), Some(l)) = (self.short_wma.value(), self.long_wma.value()) else {
            self.value = None;
            return;
        };
        // Hull's anti-lag combination: 2·short − long.
        let diff = Decimal::from(2) * s - l;
        self.smooth_wma.update(diff);
        self.value = self.smooth_wma.value();
    }

    pub fn value(&self) -> Option<Decimal> {
        self.value
    }

    pub fn is_ready(&self) -> bool {
        self.value.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // ----- WMA tests -----

    #[test]
    fn wma_warmup_returns_none() {
        let mut w = Wma::new(4);
        w.update(dec!(1));
        w.update(dec!(2));
        w.update(dec!(3));
        assert_eq!(w.value(), None);
        assert!(!w.is_ready());
    }

    /// Hand-computed: samples `[1, 2, 3, 4]`, window 4 →
    /// numerator `1·1 + 2·2 + 3·3 + 4·4 = 30`, denominator
    /// `4·5/2 = 10` → value `3.0`.
    #[test]
    fn wma_hand_computed_value() {
        let mut w = Wma::new(4);
        for v in [dec!(1), dec!(2), dec!(3), dec!(4)] {
            w.update(v);
        }
        assert_eq!(w.value(), Some(dec!(3)));
    }

    /// WMA of a constant stream equals the constant once warmed up.
    #[test]
    fn wma_of_constant_stream_is_the_constant() {
        let mut w = Wma::new(5);
        for _ in 0..5 {
            w.update(dec!(42));
        }
        assert_eq!(w.value(), Some(dec!(42)));
    }

    /// WMA reacts faster than SMA on a step change — the
    /// newest sample is weighted the heaviest. Feed `[0]*4`
    /// then a single `10` and expect a meaningful non-zero
    /// value.
    #[test]
    fn wma_reacts_faster_than_sma_to_step() {
        let mut w = Wma::new(5);
        for _ in 0..4 {
            w.update(dec!(0));
        }
        w.update(dec!(10));
        // SMA(5) = (0+0+0+0+10)/5 = 2.0
        // WMA(5) = (1·0 + 2·0 + 3·0 + 4·0 + 5·10)/15 ≈ 3.333
        let v = w.value().unwrap();
        assert!(v > dec!(3) && v < dec!(4), "expected WMA ≈ 3.33, got {v}");
    }

    // ----- HMA tests -----

    #[test]
    fn hma_warmup_returns_none() {
        let mut h = Hma::new(8);
        for v in 0..5 {
            h.update(Decimal::from(v));
        }
        assert_eq!(h.value(), None);
    }

    /// HMA of a constant stream converges to the constant
    /// exactly.
    #[test]
    fn hma_of_constant_stream_converges_to_constant() {
        let mut h = Hma::new(16);
        for _ in 0..40 {
            h.update(dec!(50));
        }
        assert!(h.is_ready());
        assert_eq!(h.value().unwrap(), dec!(50));
    }

    /// HMA of a monotonically increasing stream must also be
    /// monotonically increasing after warmup.
    #[test]
    fn hma_of_monotonic_stream_is_monotonic() {
        let mut h = Hma::new(8);
        // Warmup.
        for i in 0..40 {
            h.update(Decimal::from(i));
        }
        let base = h.value().unwrap();
        for i in 40..80 {
            h.update(Decimal::from(i));
        }
        let after = h.value().unwrap();
        assert!(
            after > base,
            "HMA must rise on increasing input: {base} -> {after}"
        );
    }

    /// HMA has less lag than an EMA of the same window on a
    /// step function: after the step and a number of samples
    /// equal to `window`, the HMA must have travelled *more
    /// than half the way* from the old level to the new one.
    #[test]
    fn hma_lags_less_than_half_window_on_step() {
        let mut h = Hma::new(9);
        // Saturate the HMA at 0.
        for _ in 0..30 {
            h.update(dec!(0));
        }
        // Step to 100, feed `window` more samples and check we
        // are past the midpoint.
        for _ in 0..9 {
            h.update(dec!(100));
        }
        let v = h.value().unwrap();
        assert!(
            v > dec!(50),
            "HMA should overshoot the midpoint within one window after a step, got {v}"
        );
    }

    /// `window < 4` must panic per the constructor contract.
    #[test]
    #[should_panic]
    fn hma_rejects_tiny_window() {
        let _ = Hma::new(3);
    }
}
