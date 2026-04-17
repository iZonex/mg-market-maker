use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::warn;

/// Tracks trade volume over sliding windows and enforces daily/hourly caps.
pub struct VolumeLimitTracker {
    /// Max daily volume in quote asset (0 = unlimited).
    max_daily_quote: Decimal,
    /// Max hourly volume in quote asset (0 = unlimited).
    max_hourly_quote: Decimal,
    /// Recent trades: (timestamp, volume_quote).
    trades: VecDeque<(DateTime<Utc>, Decimal)>,
    /// Cached daily total (recomputed on prune).
    daily_total: Decimal,
    /// Cached hourly total (recomputed on prune).
    hourly_total: Decimal,
}

impl VolumeLimitTracker {
    pub fn new(max_daily_quote: Decimal, max_hourly_quote: Decimal) -> Self {
        Self {
            max_daily_quote,
            max_hourly_quote,
            trades: VecDeque::new(),
            daily_total: dec!(0),
            hourly_total: dec!(0),
        }
    }

    /// Record a trade volume (in quote asset).
    pub fn on_trade(&mut self, volume_quote: Decimal) {
        let now = Utc::now();
        self.trades.push_back((now, volume_quote));
        self.daily_total += volume_quote;
        self.hourly_total += volume_quote;
        self.prune(now);
    }

    /// Prune old entries and recompute cached totals.
    fn prune(&mut self, now: DateTime<Utc>) {
        let day_ago = now - chrono::Duration::hours(24);
        let hour_ago = now - chrono::Duration::hours(1);

        // Remove entries older than 24h.
        while let Some(&(ts, vol)) = self.trades.front() {
            if ts < day_ago {
                self.trades.pop_front();
                self.daily_total -= vol;
                // hourly_total doesn't include entries older than 1h.
            } else {
                break;
            }
        }

        // Recompute hourly from remaining entries.
        self.hourly_total = self
            .trades
            .iter()
            .filter(|(ts, _)| *ts >= hour_ago)
            .map(|(_, vol)| *vol)
            .sum();
    }

    /// Check if we can place a trade of the given quote volume.
    pub fn can_trade(&self, volume_quote: Decimal) -> bool {
        if !self.max_daily_quote.is_zero() && self.daily_total + volume_quote > self.max_daily_quote
        {
            warn!(
                daily_total = %self.daily_total,
                limit = %self.max_daily_quote,
                "daily volume limit would be exceeded"
            );
            return false;
        }
        if !self.max_hourly_quote.is_zero()
            && self.hourly_total + volume_quote > self.max_hourly_quote
        {
            warn!(
                hourly_total = %self.hourly_total,
                limit = %self.max_hourly_quote,
                "hourly volume limit would be exceeded"
            );
            return false;
        }
        true
    }

    /// Current daily volume.
    pub fn daily_volume(&self) -> Decimal {
        self.daily_total
    }

    /// Current hourly volume.
    pub fn hourly_volume(&self) -> Decimal {
        self.hourly_total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_limit_blocks_excess() {
        let mut tracker = VolumeLimitTracker::new(dec!(10000), dec!(5000));

        // Should allow initial trade.
        assert!(tracker.can_trade(dec!(3000)));
        tracker.on_trade(dec!(3000));

        // Should allow another trade within limits.
        assert!(tracker.can_trade(dec!(1500)));
        tracker.on_trade(dec!(1500));

        // Hourly total = 4500. Next 600 would exceed hourly limit.
        assert!(!tracker.can_trade(dec!(600)));

        // But 400 is fine.
        assert!(tracker.can_trade(dec!(400)));
    }

    #[test]
    fn test_unlimited_when_zero() {
        let tracker = VolumeLimitTracker::new(dec!(0), dec!(0));
        assert!(tracker.can_trade(dec!(999999999)));
    }

    #[test]
    fn test_daily_limit_independent_of_hourly() {
        let mut tracker = VolumeLimitTracker::new(dec!(5000), dec!(0)); // No hourly limit.
        tracker.on_trade(dec!(4000));
        assert!(tracker.can_trade(dec!(900)));
        assert!(!tracker.can_trade(dec!(1100)));
    }

    // ── Property-based tests (Epic 15) ───────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn vol_strat()(raw in 1i64..10_000_000i64) -> Decimal {
            Decimal::new(raw, 2)
        }
    }

    proptest! {
        /// hourly_volume() ≤ daily_volume() after any fill
        /// sequence — hourly is a strict subset of daily.
        #[test]
        fn hourly_is_subset_of_daily(
            fills in proptest::collection::vec(vol_strat(), 0..50),
        ) {
            let mut t = VolumeLimitTracker::new(dec!(0), dec!(0));
            for v in &fills {
                t.on_trade(*v);
            }
            prop_assert!(t.hourly_volume() <= t.daily_volume(),
                "hourly {} > daily {}", t.hourly_volume(), t.daily_volume());
        }

        /// Zero limits → unlimited. Any volume request passes
        /// regardless of how much has been traded.
        #[test]
        fn zero_limits_are_unlimited(
            fills in proptest::collection::vec(vol_strat(), 0..30),
            ask in vol_strat(),
        ) {
            let mut t = VolumeLimitTracker::new(dec!(0), dec!(0));
            for v in &fills {
                t.on_trade(*v);
            }
            prop_assert!(t.can_trade(ask));
        }

        /// can_trade(v) is true iff adding v would keep each
        /// configured limit ≥ current_total + v. Catches an
        /// off-by-one in the limit check.
        #[test]
        fn can_trade_matches_limit_arithmetic(
            daily_limit_raw in 1i64..10_000_000i64,
            hourly_limit_raw in 1i64..10_000_000i64,
            existing in proptest::collection::vec(vol_strat(), 0..10),
            ask in vol_strat(),
        ) {
            let daily_limit = Decimal::new(daily_limit_raw, 2);
            let hourly_limit = Decimal::new(hourly_limit_raw, 2);
            let mut t = VolumeLimitTracker::new(daily_limit, hourly_limit);
            for v in &existing {
                t.on_trade(*v);
            }
            let expected = (t.daily_volume() + ask <= daily_limit)
                && (t.hourly_volume() + ask <= hourly_limit);
            prop_assert_eq!(t.can_trade(ask), expected);
        }

        /// daily_volume() equals the sum of all recorded fills
        /// within the window (here all are within because the
        /// test takes microseconds).
        #[test]
        fn daily_total_equals_sum_of_fills(
            fills in proptest::collection::vec(vol_strat(), 0..30),
        ) {
            let mut t = VolumeLimitTracker::new(dec!(0), dec!(0));
            let mut sum = dec!(0);
            for v in &fills {
                t.on_trade(*v);
                sum += *v;
            }
            prop_assert_eq!(t.daily_volume(), sum);
        }
    }
}
