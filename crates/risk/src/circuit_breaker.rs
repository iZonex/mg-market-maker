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
