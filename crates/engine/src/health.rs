//! Engine health degradation modes (Epic 7 item 7.5).
//!
//! Models the engine's operational health as a state machine:
//! `Normal → Degraded → Critical`. Each mode adjusts the
//! spread multiplier and, at `Critical`, recommends cancel-all.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use std::time::{Duration, Instant};

/// Engine health mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthMode {
    /// All systems operational.
    Normal,
    /// One or more subsystems degraded (high error rate,
    /// stale data). Spreads widened as a precaution.
    Degraded { reason: String },
    /// Critical failure — cannot safely quote. Recommends
    /// cancel-all until the issue is resolved.
    Critical { reason: String },
}

/// Health manager — evaluates engine health from multiple signals.
pub struct HealthManager {
    mode: HealthMode,
    /// Spread multiplier in degraded mode.
    degraded_mult: Decimal,
    /// Consecutive venue errors.
    venue_errors: u32,
    /// Threshold for degraded mode.
    degraded_threshold: u32,
    /// Threshold for critical mode.
    critical_threshold: u32,
    /// Last successful venue interaction.
    last_venue_success: Option<Instant>,
    /// Max staleness before degraded.
    max_staleness: Duration,
}

impl HealthManager {
    pub fn new() -> Self {
        Self {
            mode: HealthMode::Normal,
            degraded_mult: dec!(2),
            venue_errors: 0,
            degraded_threshold: 5,
            critical_threshold: 20,
            last_venue_success: None,
            max_staleness: Duration::from_secs(30),
        }
    }

    /// Record a successful venue interaction.
    pub fn record_success(&mut self) {
        self.venue_errors = 0;
        self.last_venue_success = Some(Instant::now());
    }

    /// Record a venue error.
    pub fn record_error(&mut self) {
        self.venue_errors += 1;
    }

    /// Evaluate health and return the current mode.
    pub fn evaluate(&mut self) -> &HealthMode {
        // Check venue error rate.
        if self.venue_errors >= self.critical_threshold {
            self.mode = HealthMode::Critical {
                reason: format!("{} consecutive venue errors", self.venue_errors),
            };
            return &self.mode;
        }

        // Check data staleness.
        if let Some(last) = self.last_venue_success {
            if last.elapsed() > self.max_staleness * 3 {
                self.mode = HealthMode::Critical {
                    reason: format!("no venue response for {}s", last.elapsed().as_secs()),
                };
                return &self.mode;
            }
            if last.elapsed() > self.max_staleness {
                self.mode = HealthMode::Degraded {
                    reason: format!("venue data stale for {}s", last.elapsed().as_secs()),
                };
                return &self.mode;
            }
        }

        if self.venue_errors >= self.degraded_threshold {
            self.mode = HealthMode::Degraded {
                reason: format!("{} consecutive venue errors", self.venue_errors),
            };
            return &self.mode;
        }

        self.mode = HealthMode::Normal;
        &self.mode
    }

    /// Spread multiplier based on health mode.
    pub fn spread_multiplier(&self) -> Decimal {
        match &self.mode {
            HealthMode::Normal => dec!(1),
            HealthMode::Degraded { .. } => self.degraded_mult,
            HealthMode::Critical { .. } => dec!(0), // signal cancel-all
        }
    }

    /// Current health mode.
    pub fn mode(&self) -> &HealthMode {
        &self.mode
    }

    /// Whether the engine should cancel all orders.
    pub fn should_cancel_all(&self) -> bool {
        matches!(&self.mode, HealthMode::Critical { .. })
    }
}

impl Default for HealthManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_normal() {
        let mut mgr = HealthManager::new();
        mgr.evaluate();
        assert_eq!(*mgr.mode(), HealthMode::Normal);
        assert_eq!(mgr.spread_multiplier(), dec!(1));
    }

    #[test]
    fn degrades_on_errors() {
        let mut mgr = HealthManager::new();
        for _ in 0..5 {
            mgr.record_error();
        }
        mgr.evaluate();
        assert!(matches!(mgr.mode(), HealthMode::Degraded { .. }));
        assert_eq!(mgr.spread_multiplier(), dec!(2));
    }

    #[test]
    fn critical_on_many_errors() {
        let mut mgr = HealthManager::new();
        for _ in 0..20 {
            mgr.record_error();
        }
        mgr.evaluate();
        assert!(matches!(mgr.mode(), HealthMode::Critical { .. }));
        assert!(mgr.should_cancel_all());
        assert_eq!(mgr.spread_multiplier(), dec!(0));
    }

    #[test]
    fn recovers_on_success() {
        let mut mgr = HealthManager::new();
        for _ in 0..10 {
            mgr.record_error();
        }
        mgr.evaluate();
        assert!(matches!(mgr.mode(), HealthMode::Degraded { .. }));

        mgr.record_success();
        mgr.evaluate();
        assert_eq!(*mgr.mode(), HealthMode::Normal);
    }

    // ── Property-based tests (Epic 23) ────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// Error count monotonicity: after N consecutive errors,
        /// the state is Normal when N < 5, Degraded when 5 ≤ N < 20,
        /// Critical when N ≥ 20. Mirrors the thresholds baked into
        /// `HealthManager::new()`.
        #[test]
        fn error_count_thresholds(err_count in 0u32..40) {
            let mut mgr = HealthManager::new();
            for _ in 0..err_count {
                mgr.record_error();
            }
            mgr.evaluate();
            let is_critical = matches!(mgr.mode(), HealthMode::Critical { .. });
            let is_degraded = matches!(mgr.mode(), HealthMode::Degraded { .. });
            if err_count >= 20 {
                prop_assert!(is_critical);
            } else if err_count >= 5 {
                prop_assert!(is_degraded);
            } else {
                prop_assert_eq!(mgr.mode(), &HealthMode::Normal);
            }
        }

        /// `record_success` resets the error counter and returns
        /// the state to Normal on the next evaluate, regardless
        /// of how many errors accumulated (short of Critical by
        /// staleness, which needs a timer).
        #[test]
        fn success_resets_state(err_count in 0u32..40) {
            let mut mgr = HealthManager::new();
            for _ in 0..err_count {
                mgr.record_error();
            }
            mgr.record_success();
            mgr.evaluate();
            prop_assert_eq!(mgr.mode(), &HealthMode::Normal);
        }

        /// spread_multiplier matches the current mode deterministically:
        /// Normal = 1, Degraded = 2, Critical = 0. Catches a
        /// regression where a refactor drops a branch.
        #[test]
        fn spread_multiplier_matches_mode(err_count in 0u32..40) {
            let mut mgr = HealthManager::new();
            for _ in 0..err_count {
                mgr.record_error();
            }
            mgr.evaluate();
            match mgr.mode() {
                HealthMode::Normal => prop_assert_eq!(mgr.spread_multiplier(), dec!(1)),
                HealthMode::Degraded { .. } => prop_assert_eq!(mgr.spread_multiplier(), dec!(2)),
                HealthMode::Critical { .. } => prop_assert_eq!(mgr.spread_multiplier(), dec!(0)),
            }
        }

        /// should_cancel_all iff in Critical. Catches a refactor
        /// that adds a 4th variant without updating the guard.
        #[test]
        fn cancel_all_exactly_when_critical(err_count in 0u32..40) {
            let mut mgr = HealthManager::new();
            for _ in 0..err_count {
                mgr.record_error();
            }
            mgr.evaluate();
            let is_critical = matches!(mgr.mode(), HealthMode::Critical { .. });
            prop_assert_eq!(mgr.should_cancel_all(), is_critical);
        }

        /// Error count is monotonic: calling record_error more
        /// times never reduces severity (absent a success call).
        /// Checks that the state transitions in the expected
        /// order: Normal → Degraded → Critical.
        #[test]
        fn severity_is_non_decreasing(
            step_count in 1u32..25,
        ) {
            let mut mgr = HealthManager::new();
            let mut prev_rank: u8 = 0;
            for _ in 0..step_count {
                mgr.record_error();
                mgr.evaluate();
                let rank: u8 = match mgr.mode() {
                    HealthMode::Normal => 0,
                    HealthMode::Degraded { .. } => 1,
                    HealthMode::Critical { .. } => 2,
                };
                prop_assert!(rank >= prev_rank,
                    "severity dropped {} → {}", prev_rank, rank);
                prev_rank = rank;
            }
        }
    }
}
