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
    ///
    /// R1-CB-1 (2026-04-22) — self-healing. Before this, a
    /// transient WS gap would trip `StaleBook` and the breaker
    /// stayed tripped until `MarketEvent::Connected` fired
    /// (i.e. full reconnect). In practice WS streams have
    /// plenty of 10-15s gaps that don't mean the connection
    /// is dead — especially on low-volume pairs and testnet.
    /// An agent running 6 deployments would see one pair go
    /// permanently dark on the first burst. Now: if the book
    /// is fresh AND the breaker is tripped with the StaleBook
    /// reason, auto-reset. Other trip reasons (MaxDrawdown,
    /// MaxExposure, WideSpread, Manual) still require an
    /// explicit reset — those are real safety halts.
    pub fn check_stale_book(&mut self, last_update_ms: i64, config: &RiskConfig) {
        let now_ms = Utc::now().timestamp_millis();
        let stale_threshold_ms = config.stale_book_timeout_secs as i64 * 1000;
        let is_stale = last_update_ms > 0 && (now_ms - last_update_ms) > stale_threshold_ms;

        if is_stale {
            self.trip(TripReason::StaleBook);
        } else if matches!(self.reason, Some(TripReason::StaleBook)) {
            self.reset();
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

    /// R1-CB-1 regression — self-healing `check_stale_book`.
    /// A StaleBook trip followed by a fresh book update must
    /// auto-reset. This is the "WS gap recovers" happy path;
    /// before the fix the breaker would stay tripped forever
    /// until `MarketEvent::Connected` fired.
    #[test]
    fn stale_book_trip_auto_resets_when_book_fresh_again() {
        use mm_common::config::RiskConfig;
        use rust_decimal_macros::dec;
        let mut cb = CircuitBreaker::new();
        let risk = RiskConfig {
            max_inventory: dec!(1),
            max_exposure_quote: dec!(10000),
            max_drawdown_quote: dec!(500),
            inventory_skew_factor: dec!(1),
            max_spread_bps: dec!(500),
            max_spread_to_quote_bps: None,
            stale_book_timeout_secs: 5,
            max_order_size: dec!(0),
            max_daily_volume_quote: dec!(0),
            max_hourly_volume_quote: dec!(0),
        };

        // Seed a stale update 20s old.
        let stale_ts = Utc::now().timestamp_millis() - 20_000;
        cb.check_stale_book(stale_ts, &risk);
        assert!(cb.is_tripped());
        assert_eq!(cb.reason(), Some(&TripReason::StaleBook));

        // Book resumes: fresh timestamp.
        let fresh_ts = Utc::now().timestamp_millis();
        cb.check_stale_book(fresh_ts, &risk);
        assert!(
            !cb.is_tripped(),
            "StaleBook trip must auto-reset when book is fresh again"
        );
        assert!(cb.reason().is_none());
    }

    /// Non-stale trips (MaxDrawdown, MaxExposure, WideSpread,
    /// Manual) must NOT auto-reset on a fresh book. Those are
    /// real safety halts that need explicit operator action.
    #[test]
    fn fresh_book_does_not_clear_non_stale_trips() {
        use mm_common::config::RiskConfig;
        use rust_decimal_macros::dec;
        let mut cb = CircuitBreaker::new();
        let risk = RiskConfig {
            max_inventory: dec!(1),
            max_exposure_quote: dec!(10000),
            max_drawdown_quote: dec!(500),
            inventory_skew_factor: dec!(1),
            max_spread_bps: dec!(500),
            max_spread_to_quote_bps: None,
            stale_book_timeout_secs: 5,
            max_order_size: dec!(0),
            max_daily_volume_quote: dec!(0),
            max_hourly_volume_quote: dec!(0),
        };

        cb.trip(TripReason::MaxDrawdown);
        let fresh_ts = Utc::now().timestamp_millis();
        cb.check_stale_book(fresh_ts, &risk);
        assert!(cb.is_tripped(), "MaxDrawdown trip must survive fresh book");
        assert_eq!(cb.reason(), Some(&TripReason::MaxDrawdown));
    }
}
