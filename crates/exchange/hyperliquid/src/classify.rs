//! Classify a HyperLiquid error string into the shared
//! [`mm_exchange_core::VenueErrorKind`] taxonomy.
//!
//! HL `/exchange` responses come in two failure shapes:
//! - HTTP non-200: `"HL /exchange {status}: {body}"`
//! - HTTP 200 with `{"status":"err", "response":"..."}`
//!   → `"HL /exchange error: {…json…}"`
//!
//! HL does not use numeric error codes — responses carry a
//! human-readable `response` string. Classification is keyword-
//! based on that text, matching the phrases HL actually emits.
//! Reference: <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint>

use mm_exchange_core::{VenueError, VenueErrorKind};

pub fn classify(err: &anyhow::Error) -> VenueError {
    classify_message(&err.to_string())
}

pub fn classify_message(msg: &str) -> VenueError {
    let lower = msg.to_ascii_lowercase();

    // Rate-limit first — the phrase is unambiguous.
    if lower.contains("rate limit") || lower.contains("too many requests") {
        return VenueError::rate_limit(msg.to_string());
    }

    // Auth / signature failures. HL returns "User or API Wallet …
    // does not exist" for a key that isn't registered, "signature"
    // anywhere else.
    if lower.contains("signature")
        || lower.contains("does not exist.")  // unregistered sub-wallet
        || lower.contains("unauthorized")
        || lower.contains("not authorized")
    {
        return VenueError::auth_rejected(msg.to_string());
    }

    // Insufficient collateral / margin. HL's margin engine says
    // "insufficient margin" or "insufficient ... balance".
    if lower.contains("insufficient") && (lower.contains("margin") || lower.contains("balance")) {
        return VenueError::insufficient_balance(msg.to_string());
    }

    // Order-size rejects.
    if lower.contains("below minimum")
        || lower.contains("less than the minimum")
        || lower.contains("too small")
        || lower.contains("min value")
    {
        return VenueError::new(VenueErrorKind::OrderTooSmall, msg.to_string());
    }

    // Price band rejects.
    if lower.contains("price") && (lower.contains("outside") || lower.contains("deviation")) {
        return VenueError::new(VenueErrorKind::PriceOutOfBounds, msg.to_string());
    }

    // Unknown / stale order — HL returns "Order was never placed,
    // already cancelled, or filled".
    if lower.contains("never placed")
        || lower.contains("already cancelled")
        || lower.contains("already filled")
    {
        return VenueError::new(VenueErrorKind::OutOfSync, msg.to_string());
    }

    // HTTP-status fallback pulled from "HL /exchange {status}: …"
    if let Some(status) = parse_http_status(msg) {
        let kind = match status {
            429 => VenueErrorKind::RateLimit,
            401 | 403 => VenueErrorKind::AuthRejected,
            500..=599 => VenueErrorKind::TransientNetwork,
            _ => VenueErrorKind::Other,
        };
        return VenueError::new(kind, msg.to_string());
    }

    // Network errors bubble through `anyhow`-wrapped reqwest.
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connect")
        || lower.contains("reset")
        || lower.contains("eof")
    {
        return VenueError::transient(msg.to_string());
    }

    VenueError::other(msg.to_string())
}

fn parse_http_status(msg: &str) -> Option<u16> {
    // "HL /exchange 429 Too Many Requests: …"
    // or "HL /info 500 Internal Server Error: …"
    let after = msg
        .strip_prefix("HL /exchange ")
        .or_else(|| msg.strip_prefix("HL /info "))?;
    let end = after.find(|c: char| !c.is_ascii_digit())?;
    after[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &str) -> anyhow::Error {
        anyhow::anyhow!("{s}")
    }

    #[test]
    fn rate_limit_detected() {
        assert_eq!(
            classify(&err("HL /exchange 429 Too Many Requests: rate limit")).kind,
            VenueErrorKind::RateLimit
        );
    }

    #[test]
    fn bad_signature() {
        assert_eq!(
            classify(&err(
                "HL /exchange error: {\"status\":\"err\",\"response\":\"Invalid signature\"}"
            ))
            .kind,
            VenueErrorKind::AuthRejected
        );
    }

    #[test]
    fn insufficient_margin() {
        assert_eq!(
            classify(&err(
                "HL /exchange error: {\"response\":\"Insufficient margin to place order\"}"
            ))
            .kind,
            VenueErrorKind::InsufficientBalance
        );
    }

    #[test]
    fn insufficient_balance() {
        assert_eq!(
            classify(&err("HL /exchange error: insufficient USDC balance")).kind,
            VenueErrorKind::InsufficientBalance
        );
    }

    #[test]
    fn order_too_small() {
        assert_eq!(
            classify(&err("HL /exchange error: order size below minimum")).kind,
            VenueErrorKind::OrderTooSmall
        );
    }

    #[test]
    fn order_never_placed_is_out_of_sync() {
        assert_eq!(
            classify(&err(
                "HL /exchange error: Order was never placed, already cancelled, or filled"
            ))
            .kind,
            VenueErrorKind::OutOfSync
        );
    }

    #[test]
    fn http_500_is_transient() {
        assert_eq!(
            classify(&err("HL /exchange 503 Service Unavailable: down")).kind,
            VenueErrorKind::TransientNetwork
        );
    }

    #[test]
    fn unknown_is_other() {
        assert_eq!(
            classify(&err("HL /exchange error: some new thing we haven't seen")).kind,
            VenueErrorKind::Other
        );
    }

    #[test]
    fn transient_on_connect_refused() {
        assert_eq!(
            classify(&err("error sending request: connection timed out")).kind,
            VenueErrorKind::TransientNetwork
        );
    }
}
