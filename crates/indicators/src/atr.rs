use rust_decimal::Decimal;

/// Average True Range (Wilder-smoothed).
///
/// True Range = max(
///     high - low,
///     |high - prev_close|,
///     |low  - prev_close|
/// )
#[derive(Debug, Clone)]
pub struct Atr {
    period: usize,
    prev_close: Option<Decimal>,
    state: Option<Decimal>,
    samples_seen: usize,
}

impl Atr {
    pub fn new(period: usize) -> Self {
        assert!(period > 0, "Atr period must be > 0");
        Self {
            period,
            prev_close: None,
            state: None,
            samples_seen: 0,
        }
    }

    pub fn update(&mut self, high: Decimal, low: Decimal, close: Decimal) {
        let tr = match self.prev_close {
            None => high - low,
            Some(prev) => {
                let a = high - low;
                let b = (high - prev).abs();
                let c = (low - prev).abs();
                a.max(b).max(c)
            }
        };
        self.samples_seen += 1;
        let period_dec = Decimal::from(self.period);
        self.state = Some(match self.state {
            None => tr,
            Some(prev_atr) if self.samples_seen <= self.period => {
                // Simple average until we reach `period` samples.
                (prev_atr * Decimal::from(self.samples_seen - 1) + tr)
                    / Decimal::from(self.samples_seen)
            }
            Some(prev_atr) => {
                (prev_atr * Decimal::from(self.period - 1) + tr) / period_dec
            }
        });
        self.prev_close = Some(close);
    }

    pub fn value(&self) -> Option<Decimal> {
        if self.samples_seen < self.period {
            return None;
        }
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
        let mut a = Atr::new(14);
        a.update(dec!(100), dec!(90), dec!(95));
        assert!(a.value().is_none());
    }

    #[test]
    fn constant_candle_atr_equals_range() {
        let mut a = Atr::new(3);
        for _ in 0..10 {
            a.update(dec!(100), dec!(90), dec!(95));
        }
        assert_eq!(a.value(), Some(dec!(10)));
    }

    #[test]
    fn gap_up_extends_true_range() {
        let mut a = Atr::new(2);
        a.update(dec!(100), dec!(90), dec!(95)); // TR1 = 10
        // Gap up — close was 95, next candle is [110, 105].
        // TR2 = max(110-105, |110-95|, |105-95|) = max(5, 15, 10) = 15.
        a.update(dec!(110), dec!(105), dec!(108));
        let v = a.value().unwrap();
        // Simple average of [10, 15] = 12.5.
        assert_eq!(v, dec!(12.5));
    }

    /// Pin Wilder-smoothed ATR against a hand-walked period-2 example
    /// from Wilder's 1978 *New Concepts in Technical Trading Systems*:
    ///
    ///   Bar 1: H=10, L=8,  C=9   → TR1 = H-L = 2  (no prev close)
    ///   Bar 2: H=11, L=9,  C=10  → TR2 = max(11-9, |11-9|, |9-9|)
    ///                                  = max(2, 2, 0) = 2
    ///   Seed (simple average over first `period` TRs, here 2):
    ///     ATR = (2 + 2) / 2 = 2
    ///   Bar 3: H=13, L=10, C=12 → TR3 = max(13-10, |13-10|, |10-10|)
    ///                                  = max(3, 3, 0) = 3
    ///   Wilder-smoothed:
    ///     ATR = (prev_ATR × (period-1) + TR3) / period
    ///         = (2 × 1 + 3) / 2 = 2.5
    ///
    /// Pinning `2.5` verifies the seed-to-smoothing transition exactly.
    /// If the impl uses an EMA-style α = 2/(N+1) the answer would be
    /// `(2 - 2/3 × 2) + 2/3 × 3 = 2.333…` and the test would fail.
    #[test]
    fn canonical_wilder_atr_seed_and_smoothing_transition() {
        let mut a = Atr::new(2);
        a.update(dec!(10), dec!(8), dec!(9));
        a.update(dec!(11), dec!(9), dec!(10));
        assert_eq!(a.value(), Some(dec!(2)), "seed (simple avg) mismatch");
        a.update(dec!(13), dec!(10), dec!(12));
        assert_eq!(
            a.value(),
            Some(dec!(2.5)),
            "Wilder smoothing transition mismatch"
        );
    }

    #[test]
    fn positive_after_warmup() {
        let mut a = Atr::new(5);
        for i in 0..20 {
            let i = Decimal::from(i);
            a.update(dec!(100) + i, dec!(90) + i, dec!(95) + i);
        }
        assert!(a.value().unwrap() > Decimal::ZERO);
    }
}
