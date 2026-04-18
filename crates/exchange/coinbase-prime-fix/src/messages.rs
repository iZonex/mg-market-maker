//! Coinbase Prime FIX 4.4 message helpers.
//!
//! Pure-function builders that wrap the generic
//! `mm_protocols_fix::Message` API with venue-specific
//! Coinbase Prime fields (signed Logon, CL Ord ID mapping,
//! TIF mapping). All time inputs are strings so tests pin
//! byte-exact output without a clock dependency.
//!
//! # Venue-specific FIX tag numbers
//!
//! Coinbase Prime uses standard FIX 4.4 tags for order
//! entry. The auth-side uses three additional tags:
//!
//! - **95** `RawDataLength` — signature byte length.
//! - **96** `RawData` — base64-encoded HMAC-SHA256 signature.
//! - **553** `Username` — API key.
//! - **554** `Password` — passphrase.
//!
//! Logon also requires:
//! - **98** `EncryptMethod` = 0 (none; TLS handles transport).
//! - **108** `HeartBtInt` — heartbeat seconds.

use anyhow::Result;
use mm_protocols_fix::{Message, OrdType, Side, TimeInForce};

use crate::auth::sign_logon;
use crate::CoinbasePrimeCredentials;

const TAG_RAW_DATA_LENGTH: u32 = 95;
const TAG_RAW_DATA: u32 = 96;
const TAG_USERNAME: u32 = 553;
const TAG_PASSWORD: u32 = 554;
const TAG_ENCRYPT_METHOD: u32 = 98;
const TAG_HEART_BT_INT: u32 = 108;

/// Build a fully-signed Logon (35=A) message ready for
/// encoding + send. `sending_time` is the pre-formatted
/// UTCTimestamp (`YYYYMMDD-HH:MM:SS.sss`) — supplied by
/// the caller to keep the builder deterministic.
pub fn build_logon(
    creds: &CoinbasePrimeCredentials,
    msg_seq_num: u64,
    sending_time: &str,
) -> Result<Message> {
    let sig = sign_logon(
        &creds.api_secret_b64,
        sending_time,
        "A",
        msg_seq_num,
        &creds.sender_comp_id,
        &creds.target_comp_id,
        &creds.passphrase,
    )?;
    let mut m = Message::new("A");
    m.set(TAG_ENCRYPT_METHOD, "0");
    m.set(TAG_HEART_BT_INT, creds.heartbeat_secs.to_string());
    m.set(TAG_RAW_DATA_LENGTH, sig.len().to_string());
    m.set(TAG_RAW_DATA, sig);
    m.set(TAG_USERNAME, &creds.api_key);
    m.set(TAG_PASSWORD, &creds.passphrase);
    Ok(m)
}

/// Map a client ClOrdID (UUID v4 preferred — Coinbase Prime
/// accepts up to 36 chars) + the order fields into a
/// NewOrderSingle (35=D) FIX message. Reuses the generic
/// helper in `mm_protocols_fix` — no venue-specific tags
/// beyond the stock FIX 4.4 shape.
///
/// Eight positional args is on the edge of readable but
/// matches the mandatory FIX fields one-for-one, which is
/// clearer than a builder for such a fixed shape.
#[allow(clippy::too_many_arguments)]
pub fn build_new_order_single(
    cl_ord_id: &str,
    symbol: &str,
    side: Side,
    qty: &str,
    ord_type: OrdType,
    price: Option<&str>,
    tif: Option<TimeInForce>,
    transact_time: &str,
) -> Message {
    Message::new_order_single(
        cl_ord_id,
        symbol,
        side,
        qty,
        ord_type,
        price,
        tif,
        transact_time,
    )
}

/// Map an in-house cancel request to OrderCancelRequest
/// (35=F). `cl_ord_id` is a fresh UUID the sender picks
/// for the cancel itself; `orig_cl_ord_id` is the original
/// order's client ID.
pub fn build_cancel_request(
    cl_ord_id: &str,
    orig_cl_ord_id: &str,
    symbol: &str,
    side: Side,
    transact_time: &str,
) -> Message {
    Message::cancel_request(cl_ord_id, orig_cl_ord_id, symbol, side, transact_time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_protocols_fix::SOH;

    fn sample_creds() -> CoinbasePrimeCredentials {
        CoinbasePrimeCredentials {
            api_key: "api-key-123".to_string(),
            api_secret_b64: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_string(),
            passphrase: "phrase".to_string(),
            sender_comp_id: "MM-SENDER".to_string(),
            target_comp_id: "COINBASE".to_string(),
            heartbeat_secs: 30,
        }
    }

    fn field_value(bytes: &[u8], tag: u32) -> Option<&str> {
        let prefix = format!("{tag}=");
        let body = std::str::from_utf8(bytes).ok()?;
        for segment in body.split(SOH as char) {
            if let Some(v) = segment.strip_prefix(&prefix) {
                return Some(v);
            }
        }
        None
    }

    #[test]
    fn logon_includes_signature_and_auth_fields() {
        let m = build_logon(&sample_creds(), 1, "20260101-00:00:00.000").unwrap();
        let encoded = m.encode("MM-SENDER", "COINBASE", 1, "20260101-00:00:00.000");
        // 95 (RawDataLength) must equal the length of the
        // signature stored at 96.
        let sig = field_value(&encoded, TAG_RAW_DATA).expect("signature present");
        let sig_len: usize = field_value(&encoded, TAG_RAW_DATA_LENGTH)
            .and_then(|v| v.parse().ok())
            .expect("length present");
        assert_eq!(sig.len(), sig_len);
        // Username + Password carry the credential bundle.
        assert_eq!(field_value(&encoded, TAG_USERNAME), Some("api-key-123"));
        assert_eq!(field_value(&encoded, TAG_PASSWORD), Some("phrase"));
        // EncryptMethod = 0 (TLS handles transport).
        assert_eq!(field_value(&encoded, TAG_ENCRYPT_METHOD), Some("0"));
        // HeartBtInt matches the config knob.
        assert_eq!(field_value(&encoded, TAG_HEART_BT_INT), Some("30"));
    }

    #[test]
    fn logon_signature_changes_with_seq_num() {
        let a = build_logon(&sample_creds(), 1, "20260101-00:00:00.000")
            .unwrap()
            .encode("MM-SENDER", "COINBASE", 1, "20260101-00:00:00.000");
        let b = build_logon(&sample_creds(), 2, "20260101-00:00:00.000")
            .unwrap()
            .encode("MM-SENDER", "COINBASE", 2, "20260101-00:00:00.000");
        let sig_a = field_value(&a, TAG_RAW_DATA).unwrap();
        let sig_b = field_value(&b, TAG_RAW_DATA).unwrap();
        assert_ne!(sig_a, sig_b);
    }

    #[test]
    fn new_order_single_preserves_all_mandatory_fields() {
        let m = build_new_order_single(
            "cl-123",
            "BTC-USDT",
            Side::Buy,
            "1.5",
            OrdType::Limit,
            Some("50000"),
            Some(TimeInForce::Ioc),
            "20260101-00:00:00.000",
        );
        let encoded = m.encode("MM-SENDER", "COINBASE", 1, "20260101-00:00:00.000");
        assert_eq!(field_value(&encoded, 11), Some("cl-123"));
        assert_eq!(field_value(&encoded, 55), Some("BTC-USDT"));
        assert_eq!(field_value(&encoded, 54), Some("1"));
        assert_eq!(field_value(&encoded, 38), Some("1.5"));
        assert_eq!(field_value(&encoded, 40), Some("2"));
        assert_eq!(field_value(&encoded, 44), Some("50000"));
        assert_eq!(field_value(&encoded, 59), Some("3"));
    }

    #[test]
    fn market_order_omits_price() {
        let m = build_new_order_single(
            "cl-1",
            "BTC-USDT",
            Side::Sell,
            "0.5",
            OrdType::Market,
            None,
            None,
            "20260101-00:00:00.000",
        );
        let encoded = m.encode("MM-SENDER", "COINBASE", 1, "20260101-00:00:00.000");
        assert_eq!(field_value(&encoded, 44), None, "market orders omit Price");
        assert_eq!(field_value(&encoded, 59), None, "omits TIF when unset");
    }

    #[test]
    fn cancel_request_ties_orig_and_fresh_clordids() {
        let m = build_cancel_request(
            "cancel-1",
            "orig-123",
            "BTC-USDT",
            Side::Sell,
            "20260101-00:00:01.000",
        );
        let encoded = m.encode("MM-SENDER", "COINBASE", 2, "20260101-00:00:01.000");
        assert_eq!(field_value(&encoded, 11), Some("cancel-1"));
        assert_eq!(field_value(&encoded, 41), Some("orig-123"));
        assert_eq!(field_value(&encoded, 55), Some("BTC-USDT"));
    }
}
