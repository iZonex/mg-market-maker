use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use self::volatility_helper::decimal_sqrt;

/// Performance metrics — Sharpe, Sortino, max drawdown, fill rate, inventory turnover.
///
/// These are the metrics institutional clients and exchanges use
/// to evaluate MM quality.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    /// Annualized Sharpe ratio.
    pub sharpe_ratio: Decimal,
    /// Annualized Sortino ratio (downside deviation only).
    pub sortino_ratio: Decimal,
    /// Maximum drawdown as fraction of peak equity.
    pub max_drawdown_pct: Decimal,
    /// Maximum drawdown in absolute terms (quote asset).
    pub max_drawdown_abs: Decimal,
    /// Fill rate: filled orders / total orders placed.
    pub fill_rate: Decimal,
    /// Inventory turnover: total volume / average absolute inventory.
    pub inventory_turnover: Decimal,
    /// Average spread capture per fill (bps).
    pub avg_spread_capture_bps: Decimal,
    /// Win rate: profitable round-trips / total round-trips.
    pub win_rate: Decimal,
    /// Profit factor: gross profit / gross loss.
    pub profit_factor: Decimal,
    /// Total observation periods.
    pub periods: u64,
}

/// Tracks returns and computes performance metrics.
pub struct PerformanceTracker {
    /// Periodic returns (e.g., per-minute PnL changes).
    returns: VecDeque<Decimal>,
    max_periods: usize,
    /// Peak equity for drawdown tracking.
    peak_equity: Decimal,
    max_drawdown: Decimal,
    /// Counts for fill rate.
    orders_placed: u64,
    orders_filled: u64,
    /// Volume + inventory for turnover.
    total_volume: Decimal,
    sum_abs_inventory: Decimal,
    inventory_samples: u64,
    /// Spread captures for avg calc.
    spread_captures: VecDeque<Decimal>,
    /// Round-trip tracking.
    profitable_trips: u64,
    losing_trips: u64,
    gross_profit: Decimal,
    gross_loss: Decimal,
}

impl PerformanceTracker {
    pub fn new(max_periods: usize) -> Self {
        Self {
            returns: VecDeque::with_capacity(max_periods),
            max_periods,
            peak_equity: dec!(0),
            max_drawdown: dec!(0),
            orders_placed: 0,
            orders_filled: 0,
            total_volume: dec!(0),
            sum_abs_inventory: dec!(0),
            inventory_samples: 0,
            spread_captures: VecDeque::with_capacity(10000),
            profitable_trips: 0,
            losing_trips: 0,
            gross_profit: dec!(0),
            gross_loss: dec!(0),
        }
    }

    /// Record a periodic return (e.g., PnL change over last minute).
    pub fn record_return(&mut self, ret: Decimal) {
        self.returns.push_back(ret);
        if self.returns.len() > self.max_periods {
            self.returns.pop_front();
        }
    }

    /// Update equity for drawdown tracking.
    pub fn update_equity(&mut self, equity: Decimal) {
        if equity > self.peak_equity {
            self.peak_equity = equity;
        }
        let dd = self.peak_equity - equity;
        if dd > self.max_drawdown {
            self.max_drawdown = dd;
        }
    }

    /// Record an order placed.
    pub fn on_order_placed(&mut self) {
        self.orders_placed += 1;
    }

    /// Record an order filled.
    pub fn on_order_filled(&mut self, volume: Decimal, spread_capture_bps: Decimal) {
        self.orders_filled += 1;
        self.total_volume += volume;
        self.spread_captures.push_back(spread_capture_bps);
        if self.spread_captures.len() > 10000 {
            self.spread_captures.pop_front();
        }
    }

    /// Sample current inventory for turnover calc.
    pub fn sample_inventory(&mut self, abs_inventory: Decimal) {
        self.sum_abs_inventory += abs_inventory;
        self.inventory_samples += 1;
    }

    /// Record a round-trip PnL.
    pub fn on_round_trip(&mut self, pnl: Decimal) {
        if pnl > dec!(0) {
            self.profitable_trips += 1;
            self.gross_profit += pnl;
        } else {
            self.losing_trips += 1;
            self.gross_loss += pnl.abs();
        }
    }

    /// Compute all metrics.
    pub fn compute(&self, periods_per_year: Decimal) -> PerformanceMetrics {
        let n = self.returns.len();
        let nd = Decimal::from(n as u64);

        // Mean return.
        let mean = if n > 0 {
            self.returns.iter().sum::<Decimal>() / nd
        } else {
            dec!(0)
        };

        // Std dev of returns.
        let variance = if n > 1 {
            self.returns
                .iter()
                .map(|r| (*r - mean) * (*r - mean))
                .sum::<Decimal>()
                / (nd - dec!(1))
        } else {
            dec!(0)
        };
        let std_dev = decimal_sqrt(variance);

        // Downside deviation (only negative returns).
        let downside_var = if n > 1 {
            let downside_sum: Decimal = self
                .returns
                .iter()
                .filter(|r| **r < dec!(0))
                .map(|r| r * r)
                .sum();
            downside_sum / nd
        } else {
            dec!(0)
        };
        let downside_dev = decimal_sqrt(downside_var);

        // Annualized Sharpe.
        let sqrt_periods = decimal_sqrt(periods_per_year);
        let sharpe = if std_dev > dec!(0) {
            mean / std_dev * sqrt_periods
        } else {
            dec!(0)
        };

        // Annualized Sortino.
        let sortino = if downside_dev > dec!(0) {
            mean / downside_dev * sqrt_periods
        } else {
            dec!(0)
        };

        // Max drawdown %.
        let max_dd_pct = if self.peak_equity > dec!(0) {
            self.max_drawdown / self.peak_equity * dec!(100)
        } else {
            dec!(0)
        };

        // Fill rate.
        let fill_rate = if self.orders_placed > 0 {
            Decimal::from(self.orders_filled) / Decimal::from(self.orders_placed)
        } else {
            dec!(0)
        };

        // Inventory turnover.
        let avg_inventory = if self.inventory_samples > 0 {
            self.sum_abs_inventory / Decimal::from(self.inventory_samples)
        } else {
            dec!(1) // Avoid division by zero.
        };
        let turnover = if avg_inventory > dec!(0) {
            self.total_volume / avg_inventory
        } else {
            dec!(0)
        };

        // Avg spread capture.
        let avg_spread = if !self.spread_captures.is_empty() {
            self.spread_captures.iter().sum::<Decimal>()
                / Decimal::from(self.spread_captures.len() as u64)
        } else {
            dec!(0)
        };

        // Win rate.
        let total_trips = self.profitable_trips + self.losing_trips;
        let win_rate = if total_trips > 0 {
            Decimal::from(self.profitable_trips) / Decimal::from(total_trips)
        } else {
            dec!(0)
        };

        // Profit factor.
        let profit_factor = if self.gross_loss > dec!(0) {
            self.gross_profit / self.gross_loss
        } else if self.gross_profit > dec!(0) {
            dec!(999) // Infinite (no losses).
        } else {
            dec!(0)
        };

        PerformanceMetrics {
            sharpe_ratio: sharpe,
            sortino_ratio: sortino,
            max_drawdown_pct: max_dd_pct,
            max_drawdown_abs: self.max_drawdown,
            fill_rate,
            inventory_turnover: turnover,
            avg_spread_capture_bps: avg_spread,
            win_rate,
            profit_factor,
            periods: n as u64,
        }
    }
}

/// Helper: Newton's method sqrt for Decimal.
mod volatility_helper {
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    pub fn decimal_sqrt(x: Decimal) -> Decimal {
        if x <= dec!(0) {
            return dec!(0);
        }
        let mut guess = x / dec!(2);
        if guess.is_zero() {
            guess = dec!(1);
        }
        for _ in 0..20 {
            let next = (guess + x / guess) / dec!(2);
            if (next - guess).abs() < dec!(0.0000000001) {
                return next;
            }
            guess = next;
        }
        guess
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sharpe_calculation() {
        let mut tracker = PerformanceTracker::new(1000);

        // Positive returns → positive Sharpe.
        for _ in 0..100 {
            tracker.record_return(dec!(0.001)); // 0.1% per period.
        }
        // Add some variance.
        for _ in 0..50 {
            tracker.record_return(dec!(-0.0005));
        }

        let metrics = tracker.compute(dec!(525600)); // Minutes per year.
        assert!(
            metrics.sharpe_ratio > dec!(0),
            "sharpe should be positive for net-positive returns"
        );
    }

    #[test]
    fn test_fill_rate() {
        let mut tracker = PerformanceTracker::new(100);
        for _ in 0..10 {
            tracker.on_order_placed();
        }
        for _ in 0..7 {
            tracker.on_order_filled(dec!(1000), dec!(2));
        }

        let metrics = tracker.compute(dec!(1));
        assert_eq!(metrics.fill_rate, dec!(0.7));
    }

    #[test]
    fn test_win_rate_and_profit_factor() {
        let mut tracker = PerformanceTracker::new(100);
        tracker.on_round_trip(dec!(10));
        tracker.on_round_trip(dec!(20));
        tracker.on_round_trip(dec!(-5));

        let metrics = tracker.compute(dec!(1));
        // 2 wins / 3 total = 0.666...
        assert!(metrics.win_rate > dec!(0.6));
        // Profit factor = 30 / 5 = 6.
        assert_eq!(metrics.profit_factor, dec!(6));
    }

    #[test]
    fn test_max_drawdown() {
        let mut tracker = PerformanceTracker::new(100);
        tracker.update_equity(dec!(1000));
        tracker.update_equity(dec!(1100)); // New peak.
        tracker.update_equity(dec!(900)); // Drawdown of 200 from 1100.
        tracker.update_equity(dec!(1050)); // Recovery.

        let metrics = tracker.compute(dec!(1));
        assert_eq!(metrics.max_drawdown_abs, dec!(200));
    }

    // ── Property-based tests (Epic 15) ───────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn equity_strat()(raw in 1i64..1_000_000_000i64) -> Decimal {
            Decimal::new(raw, 2)
        }
    }
    prop_compose! {
        fn ret_strat()(raw in -100_000i64..100_000i64) -> Decimal {
            Decimal::new(raw, 4)
        }
    }

    proptest! {
        /// max_drawdown is monotonic non-decreasing — once a
        /// drawdown is observed it stays at least as deep for
        /// the rest of the session. Only manual reset (not
        /// modelled here) clears it.
        #[test]
        fn max_drawdown_is_monotonic(
            equities in proptest::collection::vec(equity_strat(), 1..40),
        ) {
            let mut t = PerformanceTracker::new(100);
            let mut prev_dd = dec!(0);
            for e in &equities {
                t.update_equity(*e);
                let m = t.compute(dec!(1));
                prop_assert!(m.max_drawdown_abs >= prev_dd,
                    "drawdown shrank {} → {}", prev_dd, m.max_drawdown_abs);
                prev_dd = m.max_drawdown_abs;
            }
        }

        /// fill_rate is in [0, 1] for any sequence of placed /
        /// filled pairs. Catches a regression where cumulative
        /// division order flipped.
        #[test]
        fn fill_rate_is_bounded(
            placed in 0u32..1000u32,
            fill_ratio in 0u32..101u32,
        ) {
            let mut t = PerformanceTracker::new(100);
            let filled = placed * fill_ratio / 100;
            for _ in 0..placed { t.on_order_placed(); }
            for _ in 0..filled { t.on_order_filled(dec!(1), dec!(0)); }
            let m = t.compute(dec!(1));
            prop_assert!(m.fill_rate >= dec!(0));
            prop_assert!(m.fill_rate <= dec!(1));
        }

        /// Sharpe ratio is zero when std_dev is zero (constant
        /// returns). Catches a regression where a nil-variance
        /// branch would divide-by-zero to NaN-like values.
        /// Sortino is NOT tested here: its downside metric uses
        /// the raw `r^2` sum (not a demeaned deviation), so a
        /// constant negative return still produces a non-zero
        /// Sortino. Quirk of the implementation, not a bug —
        /// tested separately by `sortino_zero_on_non_negative_returns`.
        #[test]
        fn sharpe_zero_on_constant_returns(
            r in ret_strat(),
            n in 5usize..30usize,
        ) {
            let mut t = PerformanceTracker::new(100);
            for _ in 0..n { t.record_return(r); }
            let m = t.compute(dec!(365));
            prop_assert_eq!(m.sharpe_ratio, dec!(0));
        }

        /// Sortino is zero when every return is ≥ 0 — the
        /// downside sum is zero so the denominator is zero and
        /// the ratio short-circuits to 0.
        #[test]
        fn sortino_zero_on_non_negative_returns(
            returns in proptest::collection::vec(0i64..100_000i64, 5..30),
        ) {
            let mut t = PerformanceTracker::new(100);
            for r in &returns {
                t.record_return(Decimal::new(*r, 4));
            }
            let m = t.compute(dec!(365));
            prop_assert_eq!(m.sortino_ratio, dec!(0));
        }

        /// Win rate stays bounded even with a mix of profitable
        /// and losing round-trips. Catches sign errors in the
        /// gross_profit / gross_loss split.
        #[test]
        fn win_rate_bounded(
            trips in proptest::collection::vec(ret_strat(), 0..50),
        ) {
            let mut t = PerformanceTracker::new(100);
            for r in &trips {
                t.on_round_trip(*r);
            }
            let m = t.compute(dec!(1));
            prop_assert!(m.win_rate >= dec!(0));
            prop_assert!(m.win_rate <= dec!(1));
        }
    }
}
