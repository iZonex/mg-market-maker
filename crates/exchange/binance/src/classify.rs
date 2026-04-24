//! Classify a Binance-origin error string into the shared
//! [`mm_exchange_core::VenueErrorKind`] taxonomy.
//!
//! Binance REST errors are `HTTP status + JSON body` with a
//! `{"code": -N, "msg": "…"}` shape. The connector currently
//! formats these as `"Binance API error {status}: {body}"` and
//! returns them as `anyhow::Error`. We match on well-known
//! codes + HTTP-status buckets so the engine can decide retry
//! policy without substring heuristics of its own.
//!
//! Reference: <https://binance-docs.github.io/apidocs/spot/en/#error-codes>

use mm_exchange_core::{VenueError, VenueErrorKind};

/// Best-effort classification of a raw Binance error. Pure
/// function over the stringified error — no I/O, no allocation
/// beyond the wrapped message. Unknown codes / shapes map to
/// `VenueErrorKind::Other` so callers always get a typed answer.
pub fn classify(err: &anyhow::Error) -> VenueError {
    let msg = err.to_string();
    classify_message(&msg)
}

pub fn classify_message(msg: &str) -> VenueError {
    // HTTP status bucket — parsed from the `"Binance API error
    // {status}: …"` prefix the connector emits.
    let http_status = parse_http_status(msg);
    let code = parse_binance_code(msg);

    // 1) Binance numeric error codes take precedence — they are
    //    more specific than the enclosing HTTP status.
    if let Some(c) = code {
        let kind = match c {
            -1003 => VenueErrorKind::RateLimit,
            -1021 => VenueErrorKind::TransientNetwork, // timestamp skew, retry
            -1022 | -2014 | -2015 => VenueErrorKind::AuthRejected,
            -1100 | -1102 | -1104 | -1106 => VenueErrorKind::Other, // client-side param bug
            -1121 => VenueErrorKind::Other,                         // invalid symbol
            -1013 => VenueErrorKind::OrderTooSmall, // filter failure (NOTIONAL / LOT_SIZE)
            // -2010 is overloaded: "insufficient balance", "order
            // would match" (LIMIT_MAKER cross), plus a few other
            // pre-trade rejects. Lean on the message body to
            // discriminate — missing "immediately match" phrase
            // falls through to InsufficientBalance as the common
            // case.
            -2010 => {
                let lc = msg.to_ascii_lowercase();
                if lc.contains("immediately match") || lc.contains("would immediately match") {
                    VenueErrorKind::PostOnlyCross
                } else {
                    VenueErrorKind::InsufficientBalance
                }
            }
            -2019 => VenueErrorKind::InsufficientBalance,
            -2011 => VenueErrorKind::OutOfSync, // unknown order → engine state lags venue
            -4000..=-3000 => VenueErrorKind::Other, // order-service rejects
            _ => VenueErrorKind::Other,
        };
        return VenueError::new(kind, msg.to_string());
    }

    // 2) HTTP-status fallback when the body didn't parse.
    if let Some(status) = http_status {
        let kind = match status {
            418 | 429 => VenueErrorKind::RateLimit,
            401 | 403 => VenueErrorKind::AuthRejected,
            500..=599 => VenueErrorKind::TransientNetwork,
            _ => VenueErrorKind::Other,
        };
        return VenueError::new(kind, msg.to_string());
    }

    // 3) reqwest / network errors surface as generic strings.
    let lower = msg.to_ascii_lowercase();
    if lower.contains("connect")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("reset")
        || lower.contains("broken pipe")
        || lower.contains("eof")
    {
        return VenueError::transient(msg.to_string());
    }

    VenueError::other(msg.to_string())
}

fn parse_http_status(msg: &str) -> Option<u16> {
    // Format: "Binance API error 429 Too Many Requests: {...}"
    let after = msg.strip_prefix("Binance API error ")?;
    let status_end = after.find(|c: char| !c.is_ascii_digit())?;
    after[..status_end].parse().ok()
}

fn parse_binance_code(msg: &str) -> Option<i32> {
    // Body is JSON like {"code":-2010,"msg":"…"}. Parse the
    // number after the first `"code":` occurrence.
    let idx = msg.find("\"code\":")?;
    let tail = &msg[idx + "\"code\":".len()..];
    let end = tail
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(tail.len());
    tail[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &str) -> anyhow::Error {
        anyhow::anyhow!("{s}")
    }

    #[test]
    fn rate_limit_from_code() {
        let e = err(r#"Binance API error 429 Too Many Requests: {"code":-1003,"msg":"banned"}"#);
        assert_eq!(classify(&e).kind, VenueErrorKind::RateLimit);
    }

    #[test]
    fn insufficient_balance_from_code() {
        let e = err(
            r#"Binance API error 400 Bad Request: {"code":-2010,"msg":"Account has insufficient balance for requested action."}"#,
        );
        assert_eq!(classify(&e).kind, VenueErrorKind::InsufficientBalance);
    }

    #[test]
    fn post_only_cross_detected_from_2010_msg() {
        let e = err(
            r#"Binance API error 400 Bad Request: {"code":-2010,"msg":"Order would immediately match and take."}"#,
        );
        assert_eq!(classify(&e).kind, VenueErrorKind::PostOnlyCross);
    }

    #[test]
    fn auth_rejected_from_code() {
        let e =
            err(r#"Binance API error 401 Unauthorized: {"code":-2015,"msg":"Invalid API-key"}"#);
        assert_eq!(classify(&e).kind, VenueErrorKind::AuthRejected);
    }

    #[test]
    fn order_too_small_from_code() {
        let e =
            err(r#"Binance API error 400: {"code":-1013,"msg":"Filter failure: MIN_NOTIONAL"}"#);
        assert_eq!(classify(&e).kind, VenueErrorKind::OrderTooSmall);
    }

    #[test]
    fn out_of_sync_on_unknown_order() {
        let e = err(r#"Binance API error 400: {"code":-2011,"msg":"Unknown order sent."}"#);
        assert_eq!(classify(&e).kind, VenueErrorKind::OutOfSync);
    }

    #[test]
    fn rate_limit_from_status_when_body_missing() {
        let e = err("Binance API error 429 Too Many Requests: ");
        assert_eq!(classify(&e).kind, VenueErrorKind::RateLimit);
    }

    #[test]
    fn transient_on_connect_refused() {
        let e = err("error sending request: connect error: Connection refused");
        assert_eq!(classify(&e).kind, VenueErrorKind::TransientNetwork);
    }

    #[test]
    fn other_on_unknown_shape() {
        let e = err("something totally unexpected");
        assert_eq!(classify(&e).kind, VenueErrorKind::Other);
    }

    #[test]
    fn message_is_preserved() {
        let e = err(r#"Binance API error 401: {"code":-2015,"msg":"X"}"#);
        let v = classify(&e);
        assert!(v.message.contains("-2015"));
    }
}
