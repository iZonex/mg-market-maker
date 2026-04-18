//! Shared venue-error taxonomy.
//!
//! Every connector lowers its native error shape (Binance HTTP
//! status + JSON code, Bybit `retCode`, HL response body) into
//! the same `VenueErrorKind` so the engine can pick a retry
//! policy, emit per-class metrics, and route alerts without
//! substring-matching raw `anyhow` strings.
//!
//! The taxonomy is deliberately small: operators care about the
//! **class** of failure, not the exact venue code. Connectors
//! pass through the original message in `VenueError::message`
//! for debug logs / tickets; the engine only branches on `kind`.

use std::fmt;

/// Coarse class of a venue-side failure. Ordered from "most
/// recoverable" to "operator must look". Mirrors the 3am-triage
/// question: do I retry, back off, or wake someone up?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueErrorKind {
    /// Rate-limited. Back off with jitter and retry; not a bug.
    RateLimit,
    /// Transient network / TLS / WS disconnect. Retry once.
    TransientNetwork,
    /// Venue reports the book/quote is stale or out of sync.
    /// Re-fetch a snapshot before the next action.
    OutOfSync,
    /// Order rejected because notional or qty is below the
    /// venue's min. Caller must resize or skip, not retry.
    OrderTooSmall,
    /// Order rejected because price lies outside the venue's
    /// allowed deviation band. Caller must re-quote with a
    /// tighter price.
    PriceOutOfBounds,
    /// Post-only order would have crossed the book and was
    /// rejected by the venue (Binance `-2010 "Order would
    /// immediately match"`, Bybit `110094`, HL "would take").
    /// Caller should re-price one tick behind best and retry.
    PostOnlyCross,
    /// Account does not have enough free balance to back the
    /// order. Caller should cancel existing orders or stop
    /// quoting until balance refreshes.
    InsufficientBalance,
    /// Authentication rejected (bad signature, expired key,
    /// missing permission). Do NOT retry — keep firing will
    /// burn the venue's rate-limit budget and may trigger
    /// account lockout. Escalate to operator.
    AuthRejected,
    /// Anything else — unmapped venue code, unknown 5xx, etc.
    Other,
}

impl fmt::Display for VenueErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            VenueErrorKind::RateLimit => "rate_limit",
            VenueErrorKind::TransientNetwork => "transient_network",
            VenueErrorKind::OutOfSync => "out_of_sync",
            VenueErrorKind::OrderTooSmall => "order_too_small",
            VenueErrorKind::PriceOutOfBounds => "price_out_of_bounds",
            VenueErrorKind::PostOnlyCross => "post_only_cross",
            VenueErrorKind::InsufficientBalance => "insufficient_balance",
            VenueErrorKind::AuthRejected => "auth_rejected",
            VenueErrorKind::Other => "other",
        };
        f.write_str(s)
    }
}

impl VenueErrorKind {
    /// Should the caller retry this class of failure?
    ///
    /// - `RateLimit` / `TransientNetwork` / `OutOfSync` → yes (with backoff)
    /// - `AuthRejected` → no (burns rate budget, risks lockout)
    /// - `OrderTooSmall` / `PriceOutOfBounds` / `InsufficientBalance` → no (needs parameter change)
    /// - `Other` → no by default; operator decides after reading the message
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            VenueErrorKind::RateLimit
                | VenueErrorKind::TransientNetwork
                | VenueErrorKind::OutOfSync
        )
    }

    /// Is this class an operator-attention alert? Returns true
    /// for anything that a running desk cannot self-heal from.
    pub fn is_operator_alert(self) -> bool {
        matches!(
            self,
            VenueErrorKind::AuthRejected | VenueErrorKind::InsufficientBalance
        )
    }
}

/// A classified venue error. Carries the kind (for branching)
/// and the original message (for logs). Construct via
/// `VenueError::new(kind, "…")` or the helpers below.
#[derive(Debug, Clone)]
pub struct VenueError {
    pub kind: VenueErrorKind,
    pub message: String,
}

impl VenueError {
    pub fn new(kind: VenueErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn rate_limit(message: impl Into<String>) -> Self {
        Self::new(VenueErrorKind::RateLimit, message)
    }

    pub fn auth_rejected(message: impl Into<String>) -> Self {
        Self::new(VenueErrorKind::AuthRejected, message)
    }

    pub fn insufficient_balance(message: impl Into<String>) -> Self {
        Self::new(VenueErrorKind::InsufficientBalance, message)
    }

    pub fn transient(message: impl Into<String>) -> Self {
        Self::new(VenueErrorKind::TransientNetwork, message)
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(VenueErrorKind::Other, message)
    }
}

impl fmt::Display for VenueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.kind, self.message)
    }
}

impl std::error::Error for VenueError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classes_are_limited() {
        assert!(VenueErrorKind::RateLimit.is_retryable());
        assert!(VenueErrorKind::TransientNetwork.is_retryable());
        assert!(VenueErrorKind::OutOfSync.is_retryable());
        assert!(!VenueErrorKind::AuthRejected.is_retryable());
        assert!(!VenueErrorKind::InsufficientBalance.is_retryable());
        assert!(!VenueErrorKind::OrderTooSmall.is_retryable());
        assert!(!VenueErrorKind::PriceOutOfBounds.is_retryable());
        assert!(!VenueErrorKind::Other.is_retryable());
    }

    #[test]
    fn operator_alert_classes() {
        assert!(VenueErrorKind::AuthRejected.is_operator_alert());
        assert!(VenueErrorKind::InsufficientBalance.is_operator_alert());
        assert!(!VenueErrorKind::RateLimit.is_operator_alert());
    }

    #[test]
    fn display_is_stable() {
        assert_eq!(
            format!("{}", VenueError::rate_limit("binance: 429")),
            "[rate_limit] binance: 429"
        );
    }
}
