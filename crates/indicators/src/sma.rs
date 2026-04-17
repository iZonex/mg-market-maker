use std::collections::VecDeque;

use rust_decimal::Decimal;

/// Simple Moving Average over the last `period` samples.
#[derive(Debug, Clone)]
pub struct Sma {
    period: usize,
    samples: VecDeque<Decimal>,
    sum: Decimal,
}

impl Sma {
    pub fn new(period: usize) -> Self {
        assert!(period > 0, "Sma period must be > 0");
        Self {
            period,
            samples: VecDeque::with_capacity(period),
            sum: Decimal::ZERO,
        }
    }

    pub fn update(&mut self, sample: Decimal) {
        self.samples.push_back(sample);
        self.sum += sample;
        if self.samples.len() > self.period {
            if let Some(popped) = self.samples.pop_front() {
                self.sum -= popped;
            }
        }
    }

    /// Current value. `None` until the buffer has `period` samples.
    pub fn value(&self) -> Option<Decimal> {
        if self.samples.len() < self.period {
            return None;
        }
        Some(self.sum / Decimal::from(self.period))
    }

    pub fn is_ready(&self) -> bool {
        self.samples.len() >= self.period
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn warmup_returns_none() {
        let mut s = Sma::new(3);
        s.update(dec!(10));
        assert!(s.value().is_none());
        s.update(dec!(20));
        assert!(s.value().is_none());
    }

    #[test]
    fn ready_after_period_samples() {
        let mut s = Sma::new(3);
        for v in [10, 20, 30] {
            s.update(Decimal::from(v));
        }
        assert_eq!(s.value(), Some(dec!(20)));
    }

    #[test]
    fn window_slides_correctly() {
        let mut s = Sma::new(3);
        for v in [10, 20, 30, 40] {
            s.update(Decimal::from(v));
        }
        // Window now (20, 30, 40) → 30.
        assert_eq!(s.value(), Some(dec!(30)));
    }

    /// Canonical SMA = `sum(last N)/N` — pinned against the textbook
    /// sequence `[10, 20, 30, 40, 50]` with period 3. Expected output
    /// indexed by sample count: `[None, None, 20, 30, 40]`. Any drift
    /// in the sliding-window bookkeeping (off-by-one on pop, wrong
    /// divisor, sum not updated on pop) fails at least one of the
    /// five checks below.
    #[test]
    fn canonical_sma_over_textbook_sequence() {
        let mut s = Sma::new(3);
        let input = [dec!(10), dec!(20), dec!(30), dec!(40), dec!(50)];
        let expected: [Option<Decimal>; 5] =
            [None, None, Some(dec!(20)), Some(dec!(30)), Some(dec!(40))];
        for (i, v) in input.iter().enumerate() {
            s.update(*v);
            assert_eq!(s.value(), expected[i], "step {i} mismatch");
        }
    }

    #[test]
    fn single_period_equals_last_sample() {
        let mut s = Sma::new(1);
        s.update(dec!(42));
        assert_eq!(s.value(), Some(dec!(42)));
        s.update(dec!(99));
        assert_eq!(s.value(), Some(dec!(99)));
    }

    // ── Property-based tests (Epic 18) ───────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn val_strat()(raw in -1_000_000i64..1_000_000i64) -> Decimal {
            Decimal::new(raw, 2)
        }
    }

    proptest! {
        /// Constant stream → SMA converges to exactly the
        /// constant once warmup completes.
        #[test]
        fn sma_of_constant_is_constant(
            v in val_strat(),
            period in 1usize..30usize,
        ) {
            let mut s = Sma::new(period);
            for _ in 0..period {
                s.update(v);
            }
            prop_assert_eq!(s.value(), Some(v));
        }

        /// Post-warmup, SMA is bounded by min and max of the
        /// window. A moving average can never exceed its own
        /// inputs.
        #[test]
        fn sma_bounded_by_window_extrema(
            vals in proptest::collection::vec(val_strat(), 10..30),
        ) {
            let period = 5usize;
            let mut s = Sma::new(period);
            for (i, v) in vals.iter().enumerate() {
                s.update(*v);
                if i + 1 >= period {
                    let window_min = vals[i + 1 - period..=i].iter().copied().min().unwrap();
                    let window_max = vals[i + 1 - period..=i].iter().copied().max().unwrap();
                    let avg = s.value().unwrap();
                    prop_assert!(avg >= window_min, "avg {} < min {}", avg, window_min);
                    prop_assert!(avg <= window_max, "avg {} > max {}", avg, window_max);
                }
            }
        }

        /// Warmup period returns None; the step when sample
        /// count reaches period is the first Some.
        #[test]
        fn sma_warmup_precisely_period_samples(
            vals in proptest::collection::vec(val_strat(), 1..30),
            period in 1usize..30usize,
        ) {
            let mut s = Sma::new(period);
            for (i, v) in vals.iter().enumerate() {
                s.update(*v);
                let ready = i + 1 >= period;
                prop_assert_eq!(s.is_ready(), ready);
                prop_assert_eq!(s.value().is_some(), ready);
            }
        }
    }
}
