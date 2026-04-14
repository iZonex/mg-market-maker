use std::collections::VecDeque;

use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

/// One snapshot of a Bollinger Bands indicator.
#[derive(Debug, Clone, PartialEq)]
pub struct BollingerValue {
    pub middle: Decimal,
    pub upper: Decimal,
    pub lower: Decimal,
    pub stddev: Decimal,
}

/// Bollinger Bands. Middle = SMA(period), upper/lower = middle ±
/// `k_stddev × σ` where σ is the **population** standard deviation of
/// the window (divisor `n`, not `n-1`).
///
/// This matches John Bollinger's original definition in *Bollinger on
/// Bollinger Bands* (2001), §"Calculating Bollinger Bands": "the
/// standard deviation is calculated the same way the mean was
/// calculated … using n in the denominator". Spreadsheet vendors that
/// ship "Bollinger Bands" with a sample-stddev `n-1` denominator are
/// strictly non-canonical; we follow Bollinger.
#[derive(Debug, Clone)]
pub struct BollingerBands {
    period: usize,
    k: Decimal,
    samples: VecDeque<Decimal>,
}

impl BollingerBands {
    pub fn new(period: usize, k_stddev: Decimal) -> Self {
        assert!(period > 1, "Bollinger period must be > 1");
        Self {
            period,
            k: k_stddev,
            samples: VecDeque::with_capacity(period),
        }
    }

    pub fn update(&mut self, sample: Decimal) {
        self.samples.push_back(sample);
        if self.samples.len() > self.period {
            self.samples.pop_front();
        }
    }

    pub fn value(&self) -> Option<BollingerValue> {
        if self.samples.len() < self.period {
            return None;
        }
        let n = Decimal::from(self.period);
        let mean: Decimal = self.samples.iter().copied().sum::<Decimal>() / n;
        let variance: Decimal = self
            .samples
            .iter()
            .map(|s| {
                let d = *s - mean;
                d * d
            })
            .sum::<Decimal>()
            / n;
        // `rust_decimal` has no built-in sqrt unless the `maths`
        // feature is on (adds a dependency we don't want for one
        // call). Bollinger bands are width-oriented, so 16-ish
        // significant digits from an f64 round-trip are plenty.
        let stddev = variance
            .to_f64()
            .map(|v| v.sqrt())
            .and_then(Decimal::from_f64)
            .unwrap_or(Decimal::ZERO);
        let width = self.k * stddev;
        Some(BollingerValue {
            middle: mean,
            upper: mean + width,
            lower: mean - width,
            stddev,
        })
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
        let mut b = BollingerBands::new(10, dec!(2));
        for i in 0..5 {
            b.update(Decimal::from(i));
        }
        assert!(b.value().is_none());
    }

    #[test]
    fn constant_samples_have_zero_stddev() {
        let mut b = BollingerBands::new(5, dec!(2));
        for _ in 0..10 {
            b.update(dec!(100));
        }
        let v = b.value().unwrap();
        assert_eq!(v.middle, dec!(100));
        assert_eq!(v.stddev, dec!(0));
        assert_eq!(v.upper, dec!(100));
        assert_eq!(v.lower, dec!(100));
    }

    #[test]
    fn wider_k_gives_wider_bands() {
        let mut a = BollingerBands::new(5, dec!(1));
        let mut b = BollingerBands::new(5, dec!(3));
        for s in [dec!(100), dec!(102), dec!(98), dec!(101), dec!(99)] {
            a.update(s);
            b.update(s);
        }
        let av = a.value().unwrap();
        let bv = b.value().unwrap();
        assert_eq!(av.middle, bv.middle);
        assert!(bv.upper - bv.lower > av.upper - av.lower);
    }

    #[test]
    fn middle_is_sma() {
        let mut b = BollingerBands::new(3, dec!(2));
        b.update(dec!(10));
        b.update(dec!(20));
        b.update(dec!(30));
        let v = b.value().unwrap();
        assert_eq!(v.middle, dec!(20));
    }

    /// Pin the population stddev computation against the textbook
    /// sample `[2, 4, 4, 4, 5, 5, 7, 9]`, whose mean is 5 and whose
    /// population variance is `32/8 = 4`, giving σ = 2 exactly. This
    /// is the example from Wikipedia's "Standard deviation" article
    /// (Retrieved 2026-04-14), chosen because every intermediate
    /// number is an integer so any rounding regression would be
    /// obvious.
    ///
    /// With `k = 2`, band width = `2 × 2 × σ = 8` → upper = 9, lower
    /// = 1, middle = 5. If the implementation ever silently switches
    /// to sample stddev (`n - 1 = 7` in the denominator) the test
    /// will fail because variance would be `32/7 ≈ 4.571` and σ ≈
    /// 2.138.
    #[test]
    fn population_stddev_matches_hand_computed_textbook_sample() {
        let mut b = BollingerBands::new(8, dec!(2));
        for v in [2, 4, 4, 4, 5, 5, 7, 9] {
            b.update(Decimal::from(v));
        }
        let v = b.value().unwrap();
        assert_eq!(v.middle, dec!(5));
        assert_eq!(v.stddev, dec!(2));
        assert_eq!(v.upper, dec!(9));
        assert_eq!(v.lower, dec!(1));
    }

    #[test]
    fn upper_above_lower() {
        let mut b = BollingerBands::new(5, dec!(2));
        for s in [dec!(100), dec!(105), dec!(95), dec!(110), dec!(90)] {
            b.update(s);
        }
        let v = b.value().unwrap();
        assert!(v.upper > v.middle);
        assert!(v.middle > v.lower);
    }
}
