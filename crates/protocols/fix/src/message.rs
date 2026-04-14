//! FIX 4.4 message value object and wire codec.

use std::fmt::Write as _;

use anyhow::{anyhow, bail, Result};

use crate::tags;

/// FIX field separator — Start of Header.
pub const SOH: u8 = 0x01;

/// BeginString value for FIX 4.4.
pub const FIX_4_4: &str = "FIX.4.4";

/// Side values per FIX 4.4 (tag 54).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_fix(self) -> &'static str {
        match self {
            Side::Buy => "1",
            Side::Sell => "2",
        }
    }
}

/// OrdType values per FIX 4.4 (tag 40). Only the two we need; others can be
/// set as raw strings via `Message::set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrdType {
    Market,
    Limit,
}

impl OrdType {
    pub fn as_fix(self) -> &'static str {
        match self {
            OrdType::Market => "1",
            OrdType::Limit => "2",
        }
    }
}

/// TimeInForce values per FIX 4.4 (tag 59). PostOnly is venue-specific and
/// not a standard FIX value — set tag 59 directly when you need a custom
/// extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInForce {
    Day,
    Gtc,
    Ioc,
    Fok,
}

impl TimeInForce {
    pub fn as_fix(self) -> &'static str {
        match self {
            TimeInForce::Day => "0",
            TimeInForce::Gtc => "1",
            TimeInForce::Ioc => "3",
            TimeInForce::Fok => "4",
        }
    }
}

/// An ordered FIX message.
///
/// The wire format has three fields the codec owns automatically:
/// `8=BeginString`, `9=BodyLength`, and `10=CheckSum`. Callers only set the
/// body — attempts to set any header-owned tag directly are silently
/// overridden at encode time.
#[derive(Debug, Clone)]
pub struct Message {
    msg_type: String,
    fields: Vec<(u32, String)>,
}

impl Message {
    /// Construct a raw message with the given `MsgType` (tag 35 value).
    pub fn new(msg_type: impl Into<String>) -> Self {
        Self {
            msg_type: msg_type.into(),
            fields: Vec::new(),
        }
    }

    pub fn msg_type(&self) -> &str {
        &self.msg_type
    }

    /// Set `tag` to `value`. If the tag already exists, it is replaced
    /// in-place (preserving order).
    pub fn set(&mut self, tag: u32, value: impl Into<String>) -> &mut Self {
        let value = value.into();
        if let Some(entry) = self.fields.iter_mut().find(|(t, _)| *t == tag) {
            entry.1 = value;
        } else {
            self.fields.push((tag, value));
        }
        self
    }

    pub fn get(&self, tag: u32) -> Option<&str> {
        self.fields
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v)| v.as_str())
    }

    /// Encode to FIX wire bytes. The caller provides session identity and
    /// `sending_time` as a pre-formatted UTCTimestamp string
    /// (`YYYYMMDD-HH:MM:SS.sss`). Keeping the timestamp out of the codec
    /// means the output is deterministic for tests.
    pub fn encode(
        &self,
        sender: &str,
        target: &str,
        seq: u64,
        sending_time: &str,
    ) -> Vec<u8> {
        // Body: MsgType first, then mandatory session headers, then custom
        // fields. We skip any caller-set session tags so the session layer
        // retains ownership of them.
        let mut body = String::new();
        append_field(&mut body, tags::MSG_TYPE, &self.msg_type);
        append_field(&mut body, tags::SENDER_COMP_ID, sender);
        append_field(&mut body, tags::TARGET_COMP_ID, target);
        append_field(&mut body, tags::MSG_SEQ_NUM, &seq.to_string());
        append_field(&mut body, tags::SENDING_TIME, sending_time);
        for (tag, value) in &self.fields {
            if is_session_owned(*tag) {
                continue;
            }
            append_field(&mut body, *tag, value);
        }

        // Prefix with BeginString and BodyLength.
        let mut msg = String::with_capacity(body.len() + 32);
        append_field(&mut msg, tags::BEGIN_STRING, FIX_4_4);
        append_field(&mut msg, tags::BODY_LENGTH, &body.len().to_string());
        msg.push_str(&body);

        // CheckSum over all bytes from BeginString through the SOH that ends
        // the last body field, modulo 256, formatted as 3-digit zero-padded.
        let checksum: u32 = msg.bytes().map(|b| b as u32).sum::<u32>() % 256;
        append_field(&mut msg, tags::CHECKSUM, &format!("{checksum:03}"));

        msg.into_bytes()
    }

    /// Decode raw FIX bytes. Validates the trailing checksum and requires
    /// `MsgType` (35) to be present.
    pub fn decode(raw: &[u8]) -> Result<Self> {
        let s = std::str::from_utf8(raw).map_err(|_| anyhow!("FIX message is not UTF-8"))?;
        let mut fields: Vec<(u32, String)> = Vec::new();
        for part in s.split('\x01') {
            if part.is_empty() {
                continue;
            }
            let (tag_s, val) = part
                .split_once('=')
                .ok_or_else(|| anyhow!("FIX field missing '=': {part}"))?;
            let tag: u32 = tag_s
                .parse()
                .map_err(|_| anyhow!("FIX tag not numeric: {tag_s}"))?;
            fields.push((tag, val.to_string()));
        }

        // Checksum is always the last field.
        let last = fields.last().ok_or_else(|| anyhow!("empty FIX message"))?;
        if last.0 != tags::CHECKSUM {
            bail!("FIX: CheckSum (10) must be the last field, found {}", last.0);
        }
        let expected: u32 = last
            .1
            .parse()
            .map_err(|_| anyhow!("FIX: bad CheckSum value {}", last.1))?;

        // CheckSum is computed over the bytes preceding "10=..." — find the
        // SOH immediately before the trailing "10=" field.
        let tail = b"\x0110=";
        let checksum_sep = raw
            .windows(tail.len())
            .rposition(|w| w == tail)
            .ok_or_else(|| anyhow!("FIX: CheckSum field not found"))?;
        // Include the SOH that terminates the last body field (the byte at
        // `checksum_sep`) in the sum.
        let checksummed = &raw[..=checksum_sep];
        let got: u32 = checksummed.iter().map(|b| *b as u32).sum::<u32>() % 256;
        if got != expected {
            bail!("FIX: checksum mismatch (expected {expected}, got {got})");
        }

        let msg_type = fields
            .iter()
            .find(|(t, _)| *t == tags::MSG_TYPE)
            .ok_or_else(|| anyhow!("FIX: MsgType (35) missing"))?
            .1
            .clone();

        Ok(Message { msg_type, fields })
    }

    // --- Common message constructors ---

    /// Logon (35=A). Caller provides the heartbeat interval in seconds.
    pub fn logon(heartbeat_secs: u32) -> Self {
        let mut m = Self::new("A");
        m.set(tags::ENCRYPT_METHOD, "0");
        m.set(tags::HEART_BT_INT, heartbeat_secs.to_string());
        m
    }

    /// Heartbeat (35=0). If responding to a TestRequest, the caller should
    /// additionally `set(tags::TEST_REQ_ID, ...)`.
    pub fn heartbeat() -> Self {
        Self::new("0")
    }

    /// TestRequest (35=1). The test_req_id is echoed back by the peer in
    /// its Heartbeat response.
    pub fn test_request(test_req_id: &str) -> Self {
        let mut m = Self::new("1");
        m.set(tags::TEST_REQ_ID, test_req_id);
        m
    }

    /// NewOrderSingle (35=D). `price` is required for Limit orders and must
    /// be `None` for Market orders. `transact_time` is the UTCTimestamp the
    /// caller wants stamped — kept out of the codec for determinism.
    ///
    /// Eight positional args is on the edge of readable but matches the
    /// mandatory FIX fields one-for-one, which is clearer than a builder
    /// for such a fixed shape.
    #[allow(clippy::too_many_arguments)]
    pub fn new_order_single(
        cl_ord_id: &str,
        symbol: &str,
        side: Side,
        qty: &str,
        ord_type: OrdType,
        price: Option<&str>,
        tif: Option<TimeInForce>,
        transact_time: &str,
    ) -> Self {
        let mut m = Self::new("D");
        m.set(tags::CL_ORD_ID, cl_ord_id);
        m.set(tags::SYMBOL, symbol);
        m.set(tags::SIDE, side.as_fix());
        m.set(tags::TRANSACT_TIME, transact_time);
        m.set(tags::ORDER_QTY, qty);
        m.set(tags::ORD_TYPE, ord_type.as_fix());
        if let Some(p) = price {
            m.set(tags::PRICE, p);
        }
        if let Some(t) = tif {
            m.set(tags::TIME_IN_FORCE, t.as_fix());
        }
        m
    }

    /// OrderCancelRequest (35=F). Per FIX 4.4, cancel requests must carry a
    /// fresh ClOrdID plus the OrigClOrdID of the order being cancelled.
    pub fn cancel_request(
        cl_ord_id: &str,
        orig_cl_ord_id: &str,
        symbol: &str,
        side: Side,
        transact_time: &str,
    ) -> Self {
        let mut m = Self::new("F");
        m.set(tags::CL_ORD_ID, cl_ord_id);
        m.set(tags::ORIG_CL_ORD_ID, orig_cl_ord_id);
        m.set(tags::SYMBOL, symbol);
        m.set(tags::SIDE, side.as_fix());
        m.set(tags::TRANSACT_TIME, transact_time);
        m
    }
}

fn is_session_owned(tag: u32) -> bool {
    matches!(
        tag,
        tags::BEGIN_STRING
            | tags::BODY_LENGTH
            | tags::CHECKSUM
            | tags::MSG_TYPE
            | tags::SENDER_COMP_ID
            | tags::TARGET_COMP_ID
            | tags::MSG_SEQ_NUM
            | tags::SENDING_TIME
    )
}

fn append_field(buf: &mut String, tag: u32, value: &str) {
    let _ = write!(buf, "{tag}={value}");
    buf.push(SOH as char);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENDING_TIME: &str = "20240101-00:00:00.000";

    #[test]
    fn logon_roundtrip_preserves_fields() {
        let bytes = Message::logon(30).encode("CLIENT", "SERVER", 1, SENDING_TIME);
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded.msg_type(), "A");
        assert_eq!(decoded.get(tags::SENDER_COMP_ID), Some("CLIENT"));
        assert_eq!(decoded.get(tags::TARGET_COMP_ID), Some("SERVER"));
        assert_eq!(decoded.get(tags::MSG_SEQ_NUM), Some("1"));
        assert_eq!(decoded.get(tags::SENDING_TIME), Some(SENDING_TIME));
        assert_eq!(decoded.get(tags::ENCRYPT_METHOD), Some("0"));
        assert_eq!(decoded.get(tags::HEART_BT_INT), Some("30"));
    }

    #[test]
    fn encoded_message_starts_with_begin_string_and_ends_with_checksum_soh() {
        let bytes = Message::logon(30).encode("C", "S", 1, SENDING_TIME);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("8=FIX.4.4\x019="));
        assert_eq!(*bytes.last().unwrap(), SOH);
        // Last field must be "10=NNN" of exactly 6 chars (3-digit checksum).
        let last_soh = bytes[..bytes.len() - 1]
            .iter()
            .rposition(|b| *b == SOH)
            .unwrap();
        let last_field = &s[last_soh + 1..bytes.len() - 1];
        assert!(last_field.starts_with("10="));
        assert_eq!(last_field.len(), 6);
    }

    #[test]
    fn body_length_matches_actual_body() {
        let bytes = Message::logon(30).encode("SENDER", "TARGET", 42, SENDING_TIME);
        let s = std::str::from_utf8(&bytes).unwrap();
        // Parse 9=N.
        let start = s.find("9=").unwrap() + 2;
        let end = start + s[start..].find('\x01').unwrap();
        let declared: usize = s[start..end].parse().unwrap();
        // Actual body: everything between the SOH ending BodyLength and
        // the SOH before "10=".
        let body_start = end + 1;
        let checksum_start = s.rfind("\x0110=").unwrap() + 1;
        let actual = checksum_start - body_start;
        assert_eq!(declared, actual);
    }

    #[test]
    fn bad_checksum_is_rejected() {
        let mut bytes = Message::logon(30).encode("A", "B", 1, SENDING_TIME);
        // Flip a byte inside the body — checksum will no longer match.
        let mid = bytes.len() / 2;
        bytes[mid] = bytes[mid].wrapping_add(1);
        let err = Message::decode(&bytes).unwrap_err().to_string();
        assert!(
            err.contains("checksum") || err.contains("CheckSum") || err.contains("'='"),
            "expected checksum/parse error, got: {err}"
        );
    }

    #[test]
    fn heartbeat_has_only_session_header() {
        let bytes = Message::heartbeat().encode("C", "S", 7, SENDING_TIME);
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded.msg_type(), "0");
        assert_eq!(decoded.get(tags::MSG_SEQ_NUM), Some("7"));
        // No body fields beyond the session header.
        assert_eq!(decoded.get(tags::TEST_REQ_ID), None);
    }

    #[test]
    fn new_order_single_carries_all_required_tags() {
        let m = Message::new_order_single(
            "ORDER-1",
            "BTCUSDT",
            Side::Buy,
            "0.01",
            OrdType::Limit,
            Some("42000.50"),
            Some(TimeInForce::Gtc),
            SENDING_TIME,
        );
        let bytes = m.encode("CLIENT", "VENUE", 100, SENDING_TIME);
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded.msg_type(), "D");
        assert_eq!(decoded.get(tags::CL_ORD_ID), Some("ORDER-1"));
        assert_eq!(decoded.get(tags::SYMBOL), Some("BTCUSDT"));
        assert_eq!(decoded.get(tags::SIDE), Some("1"));
        assert_eq!(decoded.get(tags::ORDER_QTY), Some("0.01"));
        assert_eq!(decoded.get(tags::ORD_TYPE), Some("2"));
        assert_eq!(decoded.get(tags::PRICE), Some("42000.50"));
        assert_eq!(decoded.get(tags::TIME_IN_FORCE), Some("1"));
    }

    #[test]
    fn cancel_request_carries_orig_cl_ord_id() {
        let m = Message::cancel_request("CXL-1", "ORDER-1", "BTCUSDT", Side::Sell, SENDING_TIME);
        let bytes = m.encode("C", "S", 1, SENDING_TIME);
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded.msg_type(), "F");
        assert_eq!(decoded.get(tags::ORIG_CL_ORD_ID), Some("ORDER-1"));
        assert_eq!(decoded.get(tags::SIDE), Some("2"));
    }

    #[test]
    fn encode_is_deterministic_for_same_inputs() {
        // Same inputs → identical bytes (no hidden clock, no RNG).
        let a = Message::logon(30).encode("C", "S", 1, SENDING_TIME);
        let b = Message::logon(30).encode("C", "S", 1, SENDING_TIME);
        assert_eq!(a, b);
    }

    #[test]
    fn session_owned_tags_cannot_be_overridden_by_set() {
        let mut m = Message::new("A");
        // Try to stomp on SenderCompID via the public API.
        m.set(tags::SENDER_COMP_ID, "ATTACKER");
        let bytes = m.encode("REAL_CLIENT", "SERVER", 1, SENDING_TIME);
        let decoded = Message::decode(&bytes).unwrap();
        // The encode path strips the caller-set value and the session layer
        // one wins.
        assert_eq!(decoded.get(tags::SENDER_COMP_ID), Some("REAL_CLIENT"));
    }

    /// Hand-computed checksum: for the exact byte sequence
    /// "8=FIX.4.4\x019=5\x0135=0\x01" (BeginString + BodyLength=5 + body),
    /// checksum is `(sum of all bytes) % 256`. This test pins the
    /// checksum computation against a tiny known input so a regression in
    /// the summing/modulo loop would be caught instantly.
    #[test]
    fn checksum_is_sum_mod_256_of_preceding_bytes() {
        let raw = b"8=FIX.4.4\x019=5\x0135=0\x01";
        let sum: u32 = raw.iter().map(|b| *b as u32).sum::<u32>() % 256;
        // Now produce a complete message with that body and verify decode.
        let mut full = Vec::from(&raw[..]);
        write!(&mut FullAppender(&mut full), "10={sum:03}\x01").unwrap();
        let decoded = Message::decode(&full).unwrap();
        assert_eq!(decoded.msg_type(), "0");
    }

    // Tiny std::fmt::Write adapter for Vec<u8>, used only in the checksum
    // pinning test above.
    struct FullAppender<'a>(&'a mut Vec<u8>);
    impl std::fmt::Write for FullAppender<'_> {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0.extend_from_slice(s.as_bytes());
            Ok(())
        }
    }
}
