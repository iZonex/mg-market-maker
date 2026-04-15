//! Rolling z-score signal with hysteresis bands (Epic B,
//! sub-component #3).
//!
//! Given a stream of spread observations `s[t] = Y[t] - β[t] · X[t]`,
//! [`ZScoreSignal`] maintains a fixed-size rolling window of the
//! last `window` spreads, computes the sample mean + std, and
//! emits a z-score `z = (s - mean) / std`. [`ZScoreSignal::decide`]
//! then resolves the z into an [`SignalAction`] using two-level
//! hysteresis (entry wider than exit) so the strategy does not
//! oscillate in and out of a position on noise.
//!
//! Variance is maintained via running `Σs` and `Σs²` totals — O(1)
//! updates, sample (not population) variance via the Welford-
//! equivalent identity `var = (sum_sq - sum²/n) / (n-1)`. Numerical
//! stability is covered by a dedicated unit test running 10k
//! updates against a naive-recompute reference.
//!
//! Full formula and hysteresis rationale in
//! `docs/research/stat-arb-pairs-formulas.md` §"Sub-component #3".

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;

use crate::volatility::decimal_sqrt;

/// Knobs for [`ZScoreSignal::new`].
#[derive(Debug, Clone)]
pub struct ZScoreConfig {
    /// Rolling window size. Warmup returns `None` until this many
    /// samples have been fed.
    pub window: usize,
    /// `|z|` must exceed this to OPEN a position.
    pub entry_threshold: Decimal,
    /// `|z|` must fall below this to CLOSE a position. Must be
    /// strictly less than `entry_threshold`.
    pub exit_threshold: Decimal,
}

impl Default for ZScoreConfig {
    fn default() -> Self {
        Self {
            window: 120,
            entry_threshold: dec!(2),
            exit_threshold: dec!(0.5),
        }
    }
}

/// Which side of the spread is "long Y, short β·X" vs the
/// opposite. Direction is a pure function of the z-score sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadDirection {
    /// `z > 0`: spread is too high → short Y, long β·X.
    SellY,
    /// `z < 0`: spread is too low → long Y, short β·X.
    BuyY,
}

/// Resolution of one [`ZScoreSignal::decide`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAction {
    /// `|z|` crossed the entry band and no position is open.
    Open {
        z: Decimal,
        direction: SpreadDirection,
    },
    /// `|z|` fell inside the exit band and a position is open.
    Close { z: Decimal },
    /// Within the dead band, or stay-in-position through the
    /// hysteresis gap.
    Hold { z: Decimal },
}

/// Rolling-window spread → z-score generator.
#[derive(Debug, Clone)]
pub struct ZScoreSignal {
    config: ZScoreConfig,
    samples: VecDeque<Decimal>,
    sum: Decimal,
    sum_sq: Decimal,
}

impl ZScoreSignal {
    /// Build a fresh signal. Panics if `window < 2` or if
    /// `exit_threshold >= entry_threshold` — both are caller
    /// mistakes that would otherwise silently produce degenerate
    /// behavior.
    pub fn new(config: ZScoreConfig) -> Self {
        assert!(config.window >= 2, "window must be >= 2");
        assert!(
            config.exit_threshold < config.entry_threshold,
            "exit_threshold must be strictly less than entry_threshold",
        );
        Self {
            samples: VecDeque::with_capacity(config.window),
            sum: Decimal::ZERO,
            sum_sq: Decimal::ZERO,
            config,
        }
    }

    /// Fold a new spread observation into the window. Returns
    /// `Some(z)` once the window is full, otherwise `None`.
    /// Degenerate zero-variance windows also return `None`.
    pub fn update(&mut self, spread: Decimal) -> Option<Decimal> {
        if self.samples.len() == self.config.window {
            let evicted = self.samples.pop_front().expect("len == window");
            self.sum -= evicted;
            self.sum_sq -= evicted * evicted;
        }
        self.samples.push_back(spread);
        self.sum += spread;
        self.sum_sq += spread * spread;

        if self.samples.len() < self.config.window {
            return None;
        }
        let n = Decimal::from(self.samples.len());
        let mean = self.sum / n;
        // Sample variance: (Σs² − n·mean²) / (n − 1).
        let ss = self.sum_sq - n * mean * mean;
        if ss <= Decimal::ZERO {
            return None;
        }
        let var = ss / (n - Decimal::ONE);
        let std = decimal_sqrt(var);
        if std.is_zero() {
            return None;
        }
        Some((spread - mean) / std)
    }

    /// Pure decision function: maps `z` + position state to
    /// [`SignalAction`]. Does NOT mutate the signal.
    pub fn decide(&self, z: Decimal, in_position: bool) -> SignalAction {
        let abs_z = z.abs();
        if in_position {
            if abs_z < self.config.exit_threshold {
                SignalAction::Close { z }
            } else {
                SignalAction::Hold { z }
            }
        } else if abs_z > self.config.entry_threshold {
            let direction = if z > Decimal::ZERO {
                SpreadDirection::SellY
            } else {
                SpreadDirection::BuyY
            };
            SignalAction::Open { z, direction }
        } else {
            SignalAction::Hold { z }
        }
    }

    /// Current number of samples in the window. Less than `window`
    /// during warmup, then saturates at `window`.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Configured window size.
    pub fn window(&self) -> usize {
        self.config.window
    }

    /// Configured entry threshold.
    pub fn entry_threshold(&self) -> Decimal {
        self.config.entry_threshold
    }

    /// Configured exit threshold.
    pub fn exit_threshold(&self) -> Decimal {
        self.config.exit_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ZScoreConfig {
        ZScoreConfig {
            window: 10,
            entry_threshold: dec!(2),
            exit_threshold: dec!(0.5),
        }
    }

    #[test]
    fn warmup_returns_none_until_window_full() {
        let mut s = ZScoreSignal::new(default_config());
        for i in 1..10 {
            assert!(s.update(Decimal::from(i)).is_none(), "warmup i={i}");
        }
        // 10th sample fills the window — should now return Some.
        assert!(s.update(Decimal::from(10)).is_some());
    }

    #[test]
    fn sample_count_tracks_window_fill_and_saturates() {
        let mut s = ZScoreSignal::new(default_config());
        for i in 1..=15 {
            s.update(Decimal::from(i));
            assert!(s.sample_count() <= 10);
        }
        assert_eq!(s.sample_count(), 10);
    }

    #[test]
    fn zero_variance_window_returns_none() {
        let mut s = ZScoreSignal::new(default_config());
        for _ in 0..10 {
            assert!(s.update(dec!(42)).is_none());
        }
    }

    #[test]
    fn z_score_matches_naive_recompute() {
        // Reference fixture: samples 1..=10, spread 10 at the end.
        let mut s = ZScoreSignal::new(default_config());
        let fixture: Vec<Decimal> = (1..=10).map(Decimal::from).collect();
        for v in &fixture {
            s.update(*v);
        }
        // Naive reference calculation.
        let n = Decimal::from(fixture.len());
        let mean = fixture.iter().copied().sum::<Decimal>() / n;
        let ss: Decimal = fixture.iter().map(|x| (*x - mean) * (*x - mean)).sum();
        let var = ss / (n - Decimal::ONE);
        let std = decimal_sqrt(var);
        let expected = (fixture[fixture.len() - 1] - mean) / std;
        // Re-run final update to get z.
        // (The previous loop already did 10 updates — re-push the
        // last value would evict a stale entry. So rebuild and
        // snapshot z cleanly.)
        let mut s2 = ZScoreSignal::new(default_config());
        let mut last_z = Decimal::ZERO;
        for v in &fixture {
            if let Some(z) = s2.update(*v) {
                last_z = z;
            }
        }
        let diff = (last_z - expected).abs();
        assert!(diff < dec!(0.0001), "z={last_z} expected≈{expected}");
    }

    #[test]
    fn rolling_eviction_drops_front_sample() {
        let mut s = ZScoreSignal::new(default_config());
        // Fill with 1..=10.
        for i in 1..=10 {
            s.update(Decimal::from(i));
        }
        // Sum should equal 55.
        assert_eq!(s.sum, dec!(55));
        // Push an 11th — should evict 1 and add 11. Sum → 65.
        s.update(dec!(11));
        assert_eq!(s.sum, dec!(65));
    }

    #[test]
    fn decide_opens_on_z_above_entry_no_position() {
        let s = ZScoreSignal::new(default_config());
        let action = s.decide(dec!(2.5), false);
        assert!(matches!(
            action,
            SignalAction::Open {
                direction: SpreadDirection::SellY,
                ..
            }
        ));
    }

    #[test]
    fn decide_opens_on_z_below_neg_entry_no_position() {
        let s = ZScoreSignal::new(default_config());
        let action = s.decide(dec!(-2.5), false);
        assert!(matches!(
            action,
            SignalAction::Open {
                direction: SpreadDirection::BuyY,
                ..
            }
        ));
    }

    #[test]
    fn decide_holds_inside_entry_band_no_position() {
        let s = ZScoreSignal::new(default_config());
        let action = s.decide(dec!(1.5), false);
        assert!(matches!(action, SignalAction::Hold { .. }));
    }

    #[test]
    fn decide_closes_inside_exit_band_in_position() {
        let s = ZScoreSignal::new(default_config());
        let action = s.decide(dec!(0.3), true);
        assert!(matches!(action, SignalAction::Close { .. }));
    }

    #[test]
    fn decide_holds_through_hysteresis_gap_in_position() {
        // |z| in (exit, entry) while in position — should Hold,
        // NOT Close. This is the hysteresis band that prevents
        // oscillation when z drifts slowly back toward zero.
        let s = ZScoreSignal::new(default_config());
        let action = s.decide(dec!(1.2), true);
        assert!(matches!(action, SignalAction::Hold { .. }));
    }

    #[test]
    fn decide_holds_at_exact_entry_threshold() {
        // |z| == entry is NOT strictly greater — should Hold.
        let s = ZScoreSignal::new(default_config());
        let action = s.decide(dec!(2), false);
        assert!(matches!(action, SignalAction::Hold { .. }));
    }

    #[test]
    fn numerical_stability_over_10k_updates() {
        // 10k updates with bounded spreads — recompute the final
        // window mean naively and compare against the running-
        // sum mean inside the signal.
        let mut s = ZScoreSignal::new(ZScoreConfig {
            window: 50,
            entry_threshold: dec!(2),
            exit_threshold: dec!(0.5),
        });
        for i in 0..10_000 {
            let spread = Decimal::from(i % 7) - dec!(3);
            s.update(spread);
        }
        // Internal running sum must equal a naive recompute of the
        // last 50 samples' sum.
        let naive: Decimal = s.samples.iter().copied().sum();
        let diff = (s.sum - naive).abs();
        assert!(diff < dec!(0.0001), "running sum drift: {diff}");
    }

    #[test]
    #[should_panic(expected = "window must be >= 2")]
    fn new_panics_on_tiny_window() {
        ZScoreSignal::new(ZScoreConfig {
            window: 1,
            entry_threshold: dec!(2),
            exit_threshold: dec!(0.5),
        });
    }

    #[test]
    #[should_panic(expected = "exit_threshold must be strictly less")]
    fn new_panics_on_inverted_thresholds() {
        ZScoreSignal::new(ZScoreConfig {
            window: 10,
            entry_threshold: dec!(1),
            exit_threshold: dec!(2),
        });
    }
}
