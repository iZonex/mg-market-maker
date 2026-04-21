//! Market impact estimator — tracks how fills correlate with
//! subsequent mid-price moves.
//!
//! Token projects and exchanges use this to verify that the MM
//! is providing genuine liquidity (fills should NOT move the
//! market) rather than predatory quoting (fills that
//! systematically move the price in the MM's favor).
//!
//! # Model
//!
//! For each fill, we record the mid at fill time (`mid_t0`) and
//! at `horizon` ticks later (`mid_t1`). The signed impact is:
//!
//! ```text
//! impact_bps = (mid_t1 - mid_t0) / mid_t0 × 10000 × side_sign
//! ```
//!
//! where `side_sign = +1` for buys (adverse impact = price goes
//! up after we buy) and `-1` for sells. A positive mean impact
//! means we're systematically moving the market; a negative
//! mean means we're capturing mean-reverting fills.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Number of mid-price ticks to wait before measuring impact.
const DEFAULT_HORIZON_TICKS: usize = 20;

/// Maximum fill records retained.
const MAX_IMPACT_RECORDS: usize = 1000;

/// Per-fill impact record.
#[derive(Debug, Clone)]
struct ImpactPending {
    mid_at_fill: Decimal,
    side_sign: Decimal,
    ticks_remaining: usize,
}

/// Aggregated market impact statistics for the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketImpactReport {
    /// Number of fills with completed impact measurement.
    pub measured_fills: usize,
    /// Mean impact in bps (positive = adverse).
    pub mean_impact_bps: Decimal,
    /// Median impact in bps.
    pub median_impact_bps: Decimal,
    /// Standard deviation of impact in bps.
    pub std_impact_bps: Decimal,
    /// Fraction of fills with adverse impact (> 0 bps).
    pub adverse_fill_pct: Decimal,
    /// Fills still pending measurement.
    pub pending_fills: usize,
}

/// Market impact estimator. Fed by the engine on every fill
/// and every mid-price update.
#[derive(Debug, Clone)]
pub struct MarketImpactEstimator {
    horizon_ticks: usize,
    pending: Vec<ImpactPending>,
    completed_impacts: VecDeque<Decimal>,
}

impl MarketImpactEstimator {
    pub fn new(horizon_ticks: usize) -> Self {
        Self {
            horizon_ticks: if horizon_ticks == 0 {
                DEFAULT_HORIZON_TICKS
            } else {
                horizon_ticks
            },
            pending: Vec::new(),
            completed_impacts: VecDeque::new(),
        }
    }

    /// Record a new fill. `side_sign` is +1 for Buy, -1 for Sell.
    pub fn on_fill(&mut self, mid_at_fill: Decimal, side_sign: Decimal) {
        self.pending.push(ImpactPending {
            mid_at_fill,
            side_sign,
            ticks_remaining: self.horizon_ticks,
        });
    }

    /// Tick forward with a new mid-price. Decrements pending
    /// fill counters and completes impact measurements.
    pub fn on_mid_update(&mut self, current_mid: Decimal) {
        let mut still_pending = Vec::new();
        for mut p in self.pending.drain(..) {
            p.ticks_remaining = p.ticks_remaining.saturating_sub(1);
            if p.ticks_remaining == 0 {
                // Measure impact.
                if p.mid_at_fill > Decimal::ZERO {
                    let impact_bps =
                        (current_mid - p.mid_at_fill) / p.mid_at_fill * dec!(10_000) * p.side_sign;
                    self.completed_impacts.push_back(impact_bps);
                    if self.completed_impacts.len() > MAX_IMPACT_RECORDS {
                        self.completed_impacts.pop_front();
                    }
                }
            } else {
                still_pending.push(p);
            }
        }
        self.pending = still_pending;
    }

    /// Generate an impact report from completed measurements.
    pub fn report(&self) -> MarketImpactReport {
        let n = self.completed_impacts.len();
        if n == 0 {
            return MarketImpactReport {
                measured_fills: 0,
                mean_impact_bps: Decimal::ZERO,
                median_impact_bps: Decimal::ZERO,
                std_impact_bps: Decimal::ZERO,
                adverse_fill_pct: Decimal::ZERO,
                pending_fills: self.pending.len(),
            };
        }
        let n_dec = Decimal::from(n as u64);
        let sum: Decimal = self.completed_impacts.iter().sum();
        let mean = sum / n_dec;

        let var_sum: Decimal = self
            .completed_impacts
            .iter()
            .map(|x| (*x - mean) * (*x - mean))
            .sum();
        let std = if n > 1 {
            decimal_sqrt(var_sum / Decimal::from((n - 1) as u64))
        } else {
            Decimal::ZERO
        };

        let mut sorted: Vec<Decimal> = self.completed_impacts.iter().copied().collect();
        sorted.sort();
        let median = sorted[n / 2];

        let adverse = self
            .completed_impacts
            .iter()
            .filter(|x| **x > Decimal::ZERO)
            .count();
        let adverse_pct = Decimal::from(adverse as u64) / n_dec * dec!(100);

        MarketImpactReport {
            measured_fills: n,
            mean_impact_bps: mean,
            median_impact_bps: median,
            std_impact_bps: std,
            adverse_fill_pct: adverse_pct,
            pending_fills: self.pending.len(),
        }
    }
}

fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let mut guess = if x > Decimal::ONE { x / dec!(2) } else { x };
    for _ in 0..20 {
        let next = (guess + x / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000000001) {
            return next;
        }
        guess = next;
    }
    guess
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fills_produces_empty_report() {
        let est = MarketImpactEstimator::new(10);
        let r = est.report();
        assert_eq!(r.measured_fills, 0);
        assert_eq!(r.pending_fills, 0);
    }

    #[test]
    fn fill_completes_after_horizon_ticks() {
        let mut est = MarketImpactEstimator::new(3);
        est.on_fill(dec!(100), dec!(1)); // buy
        assert_eq!(est.report().pending_fills, 1);
        est.on_mid_update(dec!(100));
        est.on_mid_update(dec!(100));
        assert_eq!(est.report().pending_fills, 1);
        est.on_mid_update(dec!(101)); // tick 3 — completes
        assert_eq!(est.report().measured_fills, 1);
        assert_eq!(est.report().pending_fills, 0);
    }

    #[test]
    fn adverse_buy_impact_is_positive() {
        let mut est = MarketImpactEstimator::new(1);
        // Buy at mid=100, then mid goes to 102 → adverse (we moved market up).
        est.on_fill(dec!(100), dec!(1));
        est.on_mid_update(dec!(102));
        let r = est.report();
        assert!(r.mean_impact_bps > Decimal::ZERO);
    }

    #[test]
    fn favorable_sell_impact_is_negative() {
        let mut est = MarketImpactEstimator::new(1);
        // Sell at mid=100, then mid goes to 99 → favorable (price dropped, our sell was correct).
        // impact = (99 - 100) / 100 * 10000 * (-1) = 100 bps (adverse, market moved against us).
        // Wait — selling and price dropping is actually adverse FOR us (we sold and it dropped more).
        // Actually: side_sign for sell = -1.
        // impact = (99 - 100) / 100 * 10000 * (-1) = +100 bps (positive = adverse).
        // That's correct — selling then price drops means the buyer got a better deal.
        // For favorable sell: price goes UP after we sell.
        est.on_fill(dec!(100), dec!(-1));
        est.on_mid_update(dec!(101)); // price went UP after sell → favorable
        let r = est.report();
        assert!(
            r.mean_impact_bps < Decimal::ZERO,
            "favorable sell impact should be negative, got {}",
            r.mean_impact_bps
        );
    }

    #[test]
    fn zero_impact_on_unchanged_mid() {
        let mut est = MarketImpactEstimator::new(1);
        est.on_fill(dec!(100), dec!(1));
        est.on_mid_update(dec!(100));
        let r = est.report();
        assert_eq!(r.mean_impact_bps, Decimal::ZERO);
    }

    #[test]
    fn multiple_fills_averaged() {
        let mut est = MarketImpactEstimator::new(1);
        est.on_fill(dec!(100), dec!(1)); // buy
        est.on_mid_update(dec!(102)); // +200 bps
        est.on_fill(dec!(100), dec!(1)); // buy
        est.on_mid_update(dec!(100)); // 0 bps
        let r = est.report();
        assert_eq!(r.measured_fills, 2);
        // Mean = (200 + 0) / 2 = 100 bps.
        assert_eq!(r.mean_impact_bps, dec!(100));
    }

    #[test]
    fn adverse_pct_correct() {
        let mut est = MarketImpactEstimator::new(1);
        // 3 fills: 2 adverse, 1 favorable.
        est.on_fill(dec!(100), dec!(1));
        est.on_mid_update(dec!(101)); // adverse
        est.on_fill(dec!(100), dec!(1));
        est.on_mid_update(dec!(101)); // adverse
        est.on_fill(dec!(100), dec!(1));
        est.on_mid_update(dec!(99)); // favorable
        let r = est.report();
        // 2/3 adverse ≈ 66.67%.
        assert!(r.adverse_fill_pct > dec!(60));
        assert!(r.adverse_fill_pct < dec!(70));
    }

    // ── Property-based tests (Epic 11) ───────────────────────

    use proptest::prelude::*;
    use proptest::sample::select;

    prop_compose! {
        fn mid_strat()(cents in 100i64..10_000_000i64) -> Decimal {
            Decimal::new(cents, 2)
        }
    }
    fn side_sign_strat() -> impl Strategy<Value = Decimal> {
        select(vec![dec!(1), dec!(-1)])
    }

    proptest! {
        /// adverse_fill_pct is always in [0, 100] regardless of
        /// fill mix. No fill sequence should push it outside
        /// this bound — a regression there would break the
        /// dashboard gauge semantics.
        #[test]
        fn adverse_pct_is_bounded(
            fills in proptest::collection::vec(
                (mid_strat(), side_sign_strat(), mid_strat()),
                1..30,
            ),
        ) {
            let mut est = MarketImpactEstimator::new(1);
            for (mid0, side, mid1) in &fills {
                est.on_fill(*mid0, *side);
                est.on_mid_update(*mid1);
            }
            let r = est.report();
            prop_assert!(r.adverse_fill_pct >= Decimal::ZERO);
            prop_assert!(r.adverse_fill_pct <= dec!(100));
        }

        /// measured_fills always matches the completed count —
        /// adverse + favorable + neutral fills == measured.
        #[test]
        fn measured_fills_equals_completed(
            fills in proptest::collection::vec(
                (mid_strat(), side_sign_strat(), mid_strat()),
                0..30,
            ),
        ) {
            let mut est = MarketImpactEstimator::new(1);
            for (mid0, side, mid1) in &fills {
                est.on_fill(*mid0, *side);
                est.on_mid_update(*mid1);
            }
            let r = est.report();
            prop_assert_eq!(r.measured_fills, fills.len());
            prop_assert_eq!(r.pending_fills, 0);
        }

        /// mean impact stays between the min and max per-fill
        /// impact when both extrema are well-defined. Failing
        /// this would mean our aggregator is summing buggily.
        #[test]
        fn mean_bounded_by_extrema(
            fills in proptest::collection::vec(
                (mid_strat(), side_sign_strat(), mid_strat()),
                1..30,
            ),
        ) {
            let mut est = MarketImpactEstimator::new(1);
            let mut impacts: Vec<Decimal> = Vec::new();
            for (mid0, side, mid1) in &fills {
                est.on_fill(*mid0, *side);
                est.on_mid_update(*mid1);
                if *mid0 > dec!(0) {
                    let impact = (*mid1 - *mid0) / *mid0 * dec!(10_000) * *side;
                    impacts.push(impact);
                }
            }
            if impacts.is_empty() {
                return Ok(());
            }
            let min = *impacts.iter().min().unwrap();
            let max = *impacts.iter().max().unwrap();
            let r = est.report();
            prop_assert!(r.mean_impact_bps >= min,
                "mean {} below min {}", r.mean_impact_bps, min);
            prop_assert!(r.mean_impact_bps <= max,
                "mean {} above max {}", r.mean_impact_bps, max);
        }

        /// Horizon semantics: a fill fed N-1 mid updates stays
        /// pending, N-th completes. Catches off-by-one in the
        /// ticks_remaining decrement.
        #[test]
        fn horizon_n_completes_on_nth_tick(
            horizon in 1usize..20usize,
            mid in mid_strat(),
        ) {
            let mut est = MarketImpactEstimator::new(horizon);
            est.on_fill(mid, dec!(1));
            for _ in 1..horizon {
                est.on_mid_update(mid);
                prop_assert_eq!(est.report().pending_fills, 1);
                prop_assert_eq!(est.report().measured_fills, 0);
            }
            est.on_mid_update(mid);
            prop_assert_eq!(est.report().pending_fills, 0);
            prop_assert_eq!(est.report().measured_fills, 1);
        }
    }
}
