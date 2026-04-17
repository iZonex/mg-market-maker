use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Wilder's Relative Strength Index.
///
/// Follows the canonical definition from J. Welles Wilder Jr., *New
/// Concepts in Technical Trading Systems* (1978), chapter on RSI.
/// Computation:
///
/// 1. Seed (computed once, after the first `period` price changes):
///    `avg_gain_0 = mean(first N gains)` and
///    `avg_loss_0 = mean(first N losses)`.
/// 2. Wilder smoothing on every subsequent change:
///    `avg_gain_t = (avg_gain_{t-1} × (N-1) + gain_t) / N` and
///    `avg_loss_t = (avg_loss_{t-1} × (N-1) + loss_t) / N`.
/// 3. `RS  = avg_gain / avg_loss`.
/// 4. `RSI = 100 - 100 / (1 + RS)`.
///
/// When `avg_loss == 0` the ratio is undefined; we return `100` per
/// the widely-used "avoid divide-by-zero, report maximum strength"
/// convention.
#[derive(Debug, Clone)]
pub struct Rsi {
    period: usize,
    prev_close: Option<Decimal>,
    /// Running sum of gains/losses during the seed window. Once the
    /// seed fires (after `period` changes), these are replaced by the
    /// Wilder-smoothed averages and the accumulators are not used
    /// again.
    sum_gain: Decimal,
    sum_loss: Decimal,
    avg_gain: Option<Decimal>,
    avg_loss: Option<Decimal>,
    changes_seen: usize,
}

impl Rsi {
    pub fn new(period: usize) -> Self {
        assert!(period > 1, "Rsi period must be > 1");
        Self {
            period,
            prev_close: None,
            sum_gain: Decimal::ZERO,
            sum_loss: Decimal::ZERO,
            avg_gain: None,
            avg_loss: None,
            changes_seen: 0,
        }
    }

    pub fn update(&mut self, close: Decimal) {
        let Some(prev) = self.prev_close else {
            self.prev_close = Some(close);
            return;
        };
        let change = close - prev;
        let gain = if change > Decimal::ZERO {
            change
        } else {
            Decimal::ZERO
        };
        let loss = if change < Decimal::ZERO {
            -change
        } else {
            Decimal::ZERO
        };

        self.changes_seen += 1;
        let period_dec = Decimal::from(self.period);

        if self.changes_seen < self.period {
            // Accumulate during the seed window.
            self.sum_gain += gain;
            self.sum_loss += loss;
        } else if self.changes_seen == self.period {
            // Seed: divide the accumulated sum by N to get the first
            // averages.
            self.sum_gain += gain;
            self.sum_loss += loss;
            self.avg_gain = Some(self.sum_gain / period_dec);
            self.avg_loss = Some(self.sum_loss / period_dec);
        } else {
            // Wilder smoothing: `new = (prev × (N-1) + current) / N`.
            let prev_g = self.avg_gain.expect("avg_gain seeded");
            let prev_l = self.avg_loss.expect("avg_loss seeded");
            let n_minus_1 = Decimal::from(self.period - 1);
            self.avg_gain = Some((prev_g * n_minus_1 + gain) / period_dec);
            self.avg_loss = Some((prev_l * n_minus_1 + loss) / period_dec);
        }

        self.prev_close = Some(close);
    }

    /// Current RSI in `[0, 100]`, or `None` during warmup.
    pub fn value(&self) -> Option<Decimal> {
        if self.changes_seen < self.period {
            return None;
        }
        let avg_gain = self.avg_gain?;
        let avg_loss = self.avg_loss?;
        if avg_loss.is_zero() {
            return Some(dec!(100));
        }
        let rs = avg_gain / avg_loss;
        Some(dec!(100) - dec!(100) / (Decimal::ONE + rs))
    }

    pub fn is_ready(&self) -> bool {
        self.changes_seen >= self.period
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_returns_none() {
        let mut r = Rsi::new(14);
        for i in 0..5 {
            r.update(Decimal::from(100 + i));
        }
        assert!(r.value().is_none());
    }

    #[test]
    fn monotonic_rising_gives_high_rsi() {
        let mut r = Rsi::new(14);
        for i in 0..30 {
            r.update(Decimal::from(100 + i));
        }
        let v = r.value().unwrap();
        assert!(
            v >= dec!(90),
            "expected RSI >= 90 on monotonic rise, got {v}"
        );
    }

    #[test]
    fn monotonic_falling_gives_low_rsi() {
        let mut r = Rsi::new(14);
        for i in 0..30 {
            r.update(Decimal::from(200 - i));
        }
        let v = r.value().unwrap();
        assert!(
            v <= dec!(10),
            "expected RSI <= 10 on monotonic fall, got {v}"
        );
    }

    #[test]
    fn flat_price_gives_rsi_100_or_undefined() {
        // When avg_loss == 0 we return 100 by convention.
        let mut r = Rsi::new(14);
        for _ in 0..30 {
            r.update(dec!(100));
        }
        assert_eq!(r.value(), Some(dec!(100)));
    }

    /// Hand-walked canonical Wilder RSI with period = 3. Wilder's
    /// 1978 book *New Concepts in Technical Trading Systems* defines:
    ///
    /// 1. Seed: `avg_gain_0 = mean(first N gains)` and
    ///    `avg_loss_0 = mean(first N losses)`.
    /// 2. Smoothing: `avg_t = (avg_{t-1} × (N-1) + current) / N`.
    /// 3. `RS = avg_gain / avg_loss`.
    /// 4. `RSI = 100 - 100 / (1 + RS)`.
    ///
    /// With closes `[100, 101, 102, 103, 104, 103]` and period 3,
    /// the first four changes are `+1, +1, +1, +1` then `−1`.
    ///
    ///   seed (after change #3, index 3):
    ///     avg_gain = (1+1+1)/3 = 1
    ///     avg_loss = 0 → RSI = 100 by convention
    ///
    ///   update (change #4, index 4, gain = +1):
    ///     avg_gain = (1×2 + 1)/3 = 1
    ///     avg_loss = (0×2 + 0)/3 = 0 → RSI = 100
    ///
    ///   update (change #5, index 5, loss = 1):
    ///     avg_gain = (1×2 + 0)/3 = 2/3
    ///     avg_loss = (0×2 + 1)/3 = 1/3
    ///     RS = 2
    ///     RSI = 100 - 100 / (1 + 2) = 100 - 33.333… = 66.666…
    ///
    /// Pinning the 66.666… value verifies both the seed and the
    /// Wilder-smoothing transition.
    #[test]
    fn canonical_wilder_rsi_hand_walked() {
        let mut r = Rsi::new(3);
        let closes = [
            dec!(100),
            dec!(101),
            dec!(102),
            dec!(103),
            dec!(104),
            dec!(103),
        ];
        for c in closes {
            r.update(c);
        }
        let v = r.value().expect("warmup complete after 5 changes");
        // 100 - 100/3 = 200/3. Allow 6 decimal places of tolerance
        // for Decimal-precision trailing digits.
        let expected = dec!(66.6666666666);
        let diff = (v - expected).abs();
        assert!(
            diff < dec!(0.0001),
            "expected RSI ≈ 66.6666, got {v} (|diff| = {diff})"
        );
    }

    #[test]
    fn value_in_range_for_mixed_moves() {
        let mut r = Rsi::new(14);
        let prices = [
            100, 101, 99, 102, 98, 103, 97, 104, 96, 105, 95, 106, 94, 107, 93, 108,
        ];
        for p in prices {
            r.update(Decimal::from(p));
        }
        let v = r.value().unwrap();
        assert!(v >= Decimal::ZERO && v <= dec!(100));
    }

    // ── Property-based tests (Epic 18) ───────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn price_strat()(raw in 1i64..10_000_000i64) -> Decimal {
            Decimal::new(raw, 2)
        }
    }

    proptest! {
        /// RSI always lies in [0, 100] post-warmup. The core
        /// definition bounds it there via the `100 - 100/(1+RS)`
        /// form — any implementation bug shows up here.
        #[test]
        fn rsi_is_bounded_in_0_100(
            prices in proptest::collection::vec(price_strat(), 16..40),
        ) {
            let mut r = Rsi::new(14);
            for p in &prices {
                r.update(*p);
            }
            if let Some(v) = r.value() {
                prop_assert!(v >= dec!(0));
                prop_assert!(v <= dec!(100));
            }
        }

        /// Flat prices — no gains, no losses — produce RSI=100
        /// by the "no divide-by-zero" convention.
        #[test]
        fn flat_sequence_yields_100(
            price in price_strat(),
            n in 15usize..30usize,
        ) {
            let mut r = Rsi::new(14);
            for _ in 0..n {
                r.update(price);
            }
            prop_assert_eq!(r.value(), Some(dec!(100)));
        }

        /// Warmup is exactly `period` changes (i.e., `period + 1`
        /// prices). is_ready() flips precisely at that point.
        #[test]
        fn rsi_warmup_is_exactly_period_changes(
            prices in proptest::collection::vec(price_strat(), 1..25),
            period in 2usize..15usize,
        ) {
            let mut r = Rsi::new(period);
            let mut changes = 0usize;
            let mut prev_price = None;
            for p in &prices {
                r.update(*p);
                if prev_price.is_some() {
                    changes += 1;
                }
                prev_price = Some(*p);
                let ready = changes >= period;
                prop_assert_eq!(r.is_ready(), ready);
            }
        }
    }
}
