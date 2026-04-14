use rust_decimal::Decimal;

/// Exponential Moving Average. Smoothing factor α = 2/(period+1).
#[derive(Debug, Clone)]
pub struct Ema {
    period: usize,
    alpha: Decimal,
    state: Option<Decimal>,
    samples_seen: usize,
}

impl Ema {
    pub fn new(period: usize) -> Self {
        assert!(period > 0, "Ema period must be > 0");
        let alpha = Decimal::from(2) / Decimal::from(period + 1);
        Self {
            period,
            alpha,
            state: None,
            samples_seen: 0,
        }
    }

    pub fn update(&mut self, sample: Decimal) {
        self.samples_seen += 1;
        self.state = Some(match self.state {
            None => sample,
            Some(prev) => self.alpha * sample + (Decimal::ONE - self.alpha) * prev,
        });
    }

    /// Value returned once we've seen at least `period` samples.
    /// Early values are available via `value_raw` but tests should
    /// wait for `is_ready()`.
    pub fn value(&self) -> Option<Decimal> {
        if self.samples_seen < self.period {
            return None;
        }
        self.state
    }

    pub fn value_raw(&self) -> Option<Decimal> {
        self.state
    }

    pub fn is_ready(&self) -> bool {
        self.samples_seen >= self.period
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn warmup_returns_none() {
        let mut e = Ema::new(5);
        e.update(dec!(10));
        assert!(e.value().is_none());
    }

    #[test]
    fn constant_samples_converge_to_sample() {
        let mut e = Ema::new(10);
        for _ in 0..50 {
            e.update(dec!(42));
        }
        assert_eq!(e.value(), Some(dec!(42)));
    }

    #[test]
    fn initial_value_equals_first_sample() {
        let mut e = Ema::new(1);
        e.update(dec!(100));
        assert_eq!(e.value(), Some(dec!(100)));
    }

    /// Pin EMA against a hand-walked sequence that a reader can
    /// reproduce on paper. The seed convention in this impl is
    /// "first-sample seed" (not "SMA-of-first-N"), matching the
    /// streaming-friendly variant used by most trading libraries and
    /// documented as one of the two acceptable seeds in Investopedia's
    /// EMA article (Retrieved 2026-04-14). With `period = 3`, α =
    /// `2/(3+1) = 0.5`, and input prices `[22.27, 22.19, 22.08, 22.17,
    /// 22.18]` the walk is:
    ///
    ///   step 1: 22.27 (seed)
    ///   step 2: 0.5 × 22.19 + 0.5 × 22.27 = 22.23
    ///   step 3: 0.5 × 22.08 + 0.5 × 22.23 = 22.155
    ///   step 4: 0.5 × 22.17 + 0.5 × 22.155 = 22.1625
    ///   step 5: 0.5 × 22.18 + 0.5 × 22.1625 = 22.17125
    ///
    /// If α drifts to `2/(period+0) = 0.666…` or the seed changes,
    /// the step-5 value will not match.
    #[test]
    fn canonical_ema_walk_pinned_against_hand_computed_values() {
        let mut e = Ema::new(3);
        let input = [
            dec!(22.27),
            dec!(22.19),
            dec!(22.08),
            dec!(22.17),
            dec!(22.18),
        ];
        let expected: [Decimal; 5] = [
            dec!(22.27),
            dec!(22.23),
            dec!(22.155),
            dec!(22.1625),
            dec!(22.17125),
        ];
        for (i, v) in input.iter().enumerate() {
            e.update(*v);
            assert_eq!(
                e.value_raw().unwrap(),
                expected[i],
                "step {i} mismatch"
            );
        }
    }

    #[test]
    fn ema_reacts_to_step() {
        let mut e = Ema::new(3);
        // Warmup at 100.
        for _ in 0..10 {
            e.update(dec!(100));
        }
        let before = e.value().unwrap();
        assert_eq!(before, dec!(100));
        e.update(dec!(200));
        let after = e.value().unwrap();
        // EMA(3) α=0.5, so new value = 0.5 * 200 + 0.5 * 100 = 150.
        assert_eq!(after, dec!(150));
    }
}
