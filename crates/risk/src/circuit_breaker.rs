use chrono::{DateTime, Utc};
use mm_common::config::RiskConfig;
use rust_decimal::Decimal;
use tracing::{error, info, warn};

/// Reasons the circuit breaker can trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TripReason {
    MaxDrawdown,
    MaxExposure,
    StaleBook,
    WideSpread,
    Manual,
}

/// Circuit breaker — halts all quoting when risk limits are breached.
pub struct CircuitBreaker {
    tripped: bool,
    reason: Option<TripReason>,
    tripped_at: Option<DateTime<Utc>>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self {
            tripped: false,
            reason: None,
            tripped_at: None,
        }
    }

    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    pub fn reason(&self) -> Option<&TripReason> {
        self.reason.as_ref()
    }

    pub fn trip(&mut self, reason: TripReason) {
        if !self.tripped {
            error!(?reason, "CIRCUIT BREAKER TRIPPED — halting all quoting");
            self.tripped = true;
            self.reason = Some(reason);
            self.tripped_at = Some(Utc::now());
        }
    }

    pub fn reset(&mut self) {
        if self.tripped {
            info!("circuit breaker reset");
            self.tripped = false;
            self.reason = None;
            self.tripped_at = None;
        }
    }

    /// Check stale book condition.
    pub fn check_stale_book(&mut self, last_update_ms: i64, config: &RiskConfig) {
        let now_ms = Utc::now().timestamp_millis();
        let stale_threshold_ms = config.stale_book_timeout_secs as i64 * 1000;

        if last_update_ms > 0 && (now_ms - last_update_ms) > stale_threshold_ms {
            self.trip(TripReason::StaleBook);
        }
    }

    /// Check if the spread is abnormally wide (possible manipulation).
    pub fn check_spread(&mut self, spread_bps: Option<Decimal>, config: &RiskConfig) {
        if let Some(bps) = spread_bps {
            if bps > config.max_spread_bps {
                warn!(%bps, max = %config.max_spread_bps, "spread too wide");
                self.trip(TripReason::WideSpread);
            }
        }
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::sample::select;

    fn reason_strat() -> impl Strategy<Value = TripReason> {
        select(vec![
            TripReason::MaxDrawdown,
            TripReason::MaxExposure,
            TripReason::StaleBook,
            TripReason::WideSpread,
            TripReason::Manual,
        ])
    }

    proptest! {
        /// First trip wins — subsequent trips do NOT overwrite
        /// the reason or re-timestamp. Catches a regression
        /// where back-to-back trips during a cascading breach
        /// would rotate the stored reason.
        #[test]
        fn first_trip_wins(
            first in reason_strat(),
            later in reason_strat(),
        ) {
            let mut cb = CircuitBreaker::new();
            cb.trip(first.clone());
            let first_at = cb.tripped_at;
            cb.trip(later);
            prop_assert_eq!(cb.reason(), Some(&first));
            prop_assert_eq!(cb.tripped_at, first_at);
        }

        /// reset() from any state returns to the clean initial
        /// state regardless of how many trips happened.
        #[test]
        fn reset_clears_state(
            trips in proptest::collection::vec(reason_strat(), 0..10),
        ) {
            let mut cb = CircuitBreaker::new();
            for r in &trips {
                cb.trip(r.clone());
            }
            cb.reset();
            prop_assert!(!cb.is_tripped());
            prop_assert!(cb.reason().is_none());
            prop_assert!(cb.tripped_at.is_none());
        }

        /// is_tripped() exactly tracks whether `reason` is Some.
        /// A desync between the two would leak a "tripped
        /// without reason" state.
        #[test]
        fn tripped_flag_matches_reason_presence(
            ops in proptest::collection::vec(
                (proptest::bool::ANY, reason_strat()),
                0..20,
            ),
        ) {
            let mut cb = CircuitBreaker::new();
            for (trip_now, r) in &ops {
                if *trip_now {
                    cb.trip(r.clone());
                } else {
                    cb.reset();
                }
                prop_assert_eq!(cb.is_tripped(), cb.reason().is_some());
            }
        }
    }
}
