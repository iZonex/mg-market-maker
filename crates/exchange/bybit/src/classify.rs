//! Classify a Bybit V5 error string into the shared
//! [`mm_exchange_core::VenueErrorKind`] taxonomy.
//!
//! Bybit V5 responses carry a `retCode` integer; non-zero ⇒
//! failure. The connector formats these as
//! `"Bybit API error {retCode}: {msg}"`. We match on well-known
//! codes so the engine can branch retry policy on the class
//! instead of substring-matching raw messages.
//!
//! Reference: <https://bybit-exchange.github.io/docs/v5/error>

use mm_exchange_core::{VenueError, VenueErrorKind};

pub fn classify(err: &anyhow::Error) -> VenueError {
    classify_message(&err.to_string())
}

pub fn classify_message(msg: &str) -> VenueError {
    if let Some(code) = parse_ret_code(msg) {
        let kind = match code {
            // Rate limiting
            10006 | 10018 | 10016 => VenueErrorKind::RateLimit,
            // Auth / signature / permission
            10003 | 10004 | 10005 | 10007 | 10009 | 10010 | 10017 => VenueErrorKind::AuthRejected,
            // Timestamp skew / retry class
            10002 => VenueErrorKind::TransientNetwork,
            // Order-not-found / stale state
            110001 | 110008 | 110011 => VenueErrorKind::OutOfSync,
            // Too-small / min-notional / min-qty rejects
            110007 | 110030 | 110031 | 110044 | 33004 => VenueErrorKind::OrderTooSmall,
            // Price out of allowed band
            110003 | 110014 | 110022 | 110094 => VenueErrorKind::PriceOutOfBounds,
            // Insufficient wallet balance / margin
            110004 | 110006 | 110012 | 110025 | 30034 | 30031 => {
                VenueErrorKind::InsufficientBalance
            }
            // Anything else — keep typed but `Other` so the engine
            // logs the class without assuming recoverability.
            _ => VenueErrorKind::Other,
        };
        return VenueError::new(kind, msg.to_string());
    }

    // Fallback: generic network errors still look like
    // anyhow-wrapped reqwest failures.
    let lower = msg.to_ascii_lowercase();
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connect")
        || lower.contains("reset")
        || lower.contains("broken pipe")
    {
        return VenueError::transient(msg.to_string());
    }
    VenueError::other(msg.to_string())
}

fn parse_ret_code(msg: &str) -> Option<i64> {
    // Format: "Bybit API error {retCode}: …"
    let after = msg.strip_prefix("Bybit API error ")?;
    let end = after
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(after.len());
    after[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &str) -> anyhow::Error {
        anyhow::anyhow!("{s}")
    }

    #[test]
    fn rate_limit() {
        assert_eq!(
            classify(&err("Bybit API error 10006: too many visits")).kind,
            VenueErrorKind::RateLimit
        );
    }

    #[test]
    fn auth_rejected_on_bad_sign() {
        assert_eq!(
            classify(&err("Bybit API error 10004: Error sign")).kind,
            VenueErrorKind::AuthRejected
        );
    }

    #[test]
    fn insufficient_balance() {
        assert_eq!(
            classify(&err("Bybit API error 110004: Insufficient wallet balance")).kind,
            VenueErrorKind::InsufficientBalance
        );
    }

    #[test]
    fn order_too_small() {
        assert_eq!(
            classify(&err("Bybit API error 110030: Qty below minimum")).kind,
            VenueErrorKind::OrderTooSmall
        );
    }

    #[test]
    fn price_out_of_bounds() {
        assert_eq!(
            classify(&err(
                "Bybit API error 110003: Price exceeds allowed deviation"
            ))
            .kind,
            VenueErrorKind::PriceOutOfBounds
        );
    }

    #[test]
    fn unknown_order_is_out_of_sync() {
        assert_eq!(
            classify(&err("Bybit API error 110001: Order does not exist")).kind,
            VenueErrorKind::OutOfSync
        );
    }

    #[test]
    fn unknown_code_is_other() {
        assert_eq!(
            classify(&err("Bybit API error 99999: something new")).kind,
            VenueErrorKind::Other
        );
    }

    #[test]
    fn transient_on_network() {
        assert_eq!(
            classify(&err("error sending request: Connection reset by peer")).kind,
            VenueErrorKind::TransientNetwork
        );
    }
}
