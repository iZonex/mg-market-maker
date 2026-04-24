//! Message envelope — the outer wrapper every transport carries.
//!
//! An envelope holds a sequence number, a wall-clock timestamp,
//! the payload kind, and (eventually) a signature over the
//! serialized bytes. PR-1 keeps the signature field as an opaque
//! byte blob with a placeholder verify that always accepts — real
//! Ed25519 signing wires in once we introduce the agent identity
//! key. Defining the shape now means later we flip the
//! verification switch without changing any call sites.
//!
//! Separating payload kind (command vs telemetry) at the envelope
//! layer lets the transport demultiplex without peeking into the
//! payload — handy for routing and for metrics ("how many
//! telemetry envelopes did we drop this minute").

use serde::{Deserialize, Serialize};

use crate::messages::{CommandPayload, TelemetryPayload};
use crate::seq::Seq;

/// Direction marker on the envelope. Serialized so bad wiring
/// surfaces immediately (an agent receiving an envelope marked
/// `Telemetry` refuses rather than silently treating its own
/// upstream event as a controller command).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeKind {
    Command,
    Telemetry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Monotonic within its direction. Controller's command seq and
    /// agent's telemetry seq are independent counters.
    pub seq: Seq,
    /// UTC millis at the sending endpoint. Not trusted for
    /// authority decisions — used only for observability.
    pub sent_at_ms: i64,
    pub kind: EnvelopeKind,
    /// Protocol version of the sender. Receivers reject mismatched
    /// majors so a rolling upgrade can never mid-flight-corrupt
    /// the stream.
    pub protocol_version: u16,
    /// Exactly one of these is populated per envelope; the other
    /// is `None`. We flatten both here rather than use an outer
    /// enum so the wire layout stays stable when new payload
    /// families are added in future protocol versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryPayload>,
}

impl Envelope {
    pub fn command(seq: Seq, payload: CommandPayload) -> Self {
        Self {
            seq,
            sent_at_ms: chrono::Utc::now().timestamp_millis(),
            kind: EnvelopeKind::Command,
            protocol_version: crate::PROTOCOL_VERSION,
            command: Some(payload),
            telemetry: None,
        }
    }

    pub fn telemetry(seq: Seq, payload: TelemetryPayload) -> Self {
        Self {
            seq,
            sent_at_ms: chrono::Utc::now().timestamp_millis(),
            kind: EnvelopeKind::Telemetry,
            protocol_version: crate::PROTOCOL_VERSION,
            command: None,
            telemetry: Some(payload),
        }
    }
}

/// Wrapper carrying a detached signature over the serialized
/// [`Envelope`] bytes. PR-1 ships with a no-op signer; when the
/// Ed25519 identity key lands this type gains a real verify()
/// and the transport layer refuses envelopes whose signature
/// does not match the registered peer key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedEnvelope {
    pub envelope: Envelope,
    /// Raw signature bytes. Empty vec in the unsigned placeholder
    /// mode; real Ed25519 signatures are 64 bytes.
    #[serde(with = "sig_hex")]
    pub signature: Vec<u8>,
}

impl SignedEnvelope {
    /// Construct without signing. Used by in-memory transports
    /// + by paths where the crypto identity isn't configured
    /// yet. Real production paths use [`SignedEnvelope::signed`]
    /// and verify with [`SignedEnvelope::verify_with`].
    pub fn unsigned(envelope: Envelope) -> Self {
        Self {
            envelope,
            signature: Vec::new(),
        }
    }

    /// Sign the envelope with `key` producing a real Ed25519
    /// signature over the canonical JSON encoding of the
    /// envelope. The wire format is unchanged — only the
    /// signature bytes differ from the unsigned placeholder.
    pub fn signed(
        envelope: Envelope,
        key: &crate::identity::IdentityKey,
    ) -> Result<Self, VerifyError> {
        let bytes =
            serde_json::to_vec(&envelope).map_err(|e| VerifyError::Encoding(e.to_string()))?;
        let sig = key.sign(&bytes);
        Ok(Self {
            envelope,
            signature: sig.to_vec(),
        })
    }

    /// Verify this envelope's signature against `peer`. Rejects
    /// unsigned envelopes — callers that want to tolerate them
    /// (in-memory tests, bootstrap before identity wire-up)
    /// call [`SignedEnvelope::verify_optional`] instead.
    pub fn verify_with(&self, peer: &crate::identity::PublicKey) -> Result<(), VerifyError> {
        if self.signature.is_empty() {
            return Err(VerifyError::Unsigned);
        }
        let bytes =
            serde_json::to_vec(&self.envelope).map_err(|e| VerifyError::Encoding(e.to_string()))?;
        peer.verify(&bytes, &self.signature)
            .map_err(|_| VerifyError::BadSignature)
    }

    /// Compatibility wrapper — when `peer` is `None`, the path
    /// pre-dates signing wire-up and we accept unsigned
    /// envelopes. When `peer` is `Some`, verification is
    /// mandatory and unsigned envelopes are rejected. This lets
    /// the crate land real signing incrementally per call site
    /// without a big-bang migration.
    pub fn verify_optional(
        &self,
        peer: Option<&crate::identity::PublicKey>,
    ) -> Result<(), VerifyError> {
        match peer {
            Some(k) => self.verify_with(k),
            None => Ok(()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("signature length mismatch (expected 64, got {0})")]
    BadLength(usize),
    #[error("signature does not verify against the registered peer key")]
    BadSignature,
    #[error("envelope is unsigned but caller required a verified signature")]
    Unsigned,
    #[error("envelope serialization failed while signing/verifying: {0}")]
    Encoding(String),
}

mod sig_hex {
    //! Serde helper — hex-encode the signature so on-wire JSON is
    //! human-inspectable. Cheap: sigs are 64 bytes at most.
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex_decode(&s).map_err(serde::de::Error::custom)
    }

    fn hex_encode(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }

    fn hex_decode(s: &str) -> Result<Vec<u8>, &'static str> {
        if !s.len().is_multiple_of(2) {
            return Err("odd-length hex");
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let hi = from_hex_digit(bytes[i])?;
            let lo = from_hex_digit(bytes[i + 1])?;
            out.push((hi << 4) | lo);
            i += 2;
        }
        Ok(out)
    }

    fn from_hex_digit(b: u8) -> Result<u8, &'static str> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err("non-hex character"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_envelope_sets_direction() {
        let e = Envelope::command(Seq(7), CommandPayload::Heartbeat);
        assert!(matches!(e.kind, EnvelopeKind::Command));
        assert!(e.command.is_some());
        assert!(e.telemetry.is_none());
    }

    #[test]
    fn telemetry_envelope_sets_direction() {
        let e = Envelope::telemetry(Seq(1), TelemetryPayload::Heartbeat { agent_clock_ms: 0 });
        assert!(matches!(e.kind, EnvelopeKind::Telemetry));
        assert!(e.telemetry.is_some());
        assert!(e.command.is_none());
    }

    #[test]
    fn unsigned_rejected_when_verify_required() {
        let e = Envelope::command(Seq(1), CommandPayload::Heartbeat);
        let signed = SignedEnvelope::unsigned(e);
        let k = crate::identity::IdentityKey::generate();
        assert!(matches!(
            signed.verify_with(&k.public()),
            Err(VerifyError::Unsigned)
        ));
    }

    #[test]
    fn optional_verify_accepts_unsigned_when_peer_absent() {
        let e = Envelope::command(Seq(1), CommandPayload::Heartbeat);
        let signed = SignedEnvelope::unsigned(e);
        assert!(signed.verify_optional(None).is_ok());
    }

    #[test]
    fn real_signature_verifies_with_matching_pubkey() {
        let k = crate::identity::IdentityKey::generate();
        let e = Envelope::command(Seq(1), CommandPayload::Heartbeat);
        let signed = SignedEnvelope::signed(e, &k).unwrap();
        assert!(signed.verify_with(&k.public()).is_ok());
    }

    #[test]
    fn signature_fails_against_wrong_pubkey() {
        let signer = crate::identity::IdentityKey::generate();
        let rogue = crate::identity::IdentityKey::generate();
        let e = Envelope::command(Seq(1), CommandPayload::Heartbeat);
        let signed = SignedEnvelope::signed(e, &signer).unwrap();
        assert!(matches!(
            signed.verify_with(&rogue.public()),
            Err(VerifyError::BadSignature)
        ));
    }

    #[test]
    fn tampered_envelope_fails_verify() {
        let k = crate::identity::IdentityKey::generate();
        let e = Envelope::command(Seq(1), CommandPayload::Heartbeat);
        let mut signed = SignedEnvelope::signed(e, &k).unwrap();
        // Mutate the envelope after signing — signature now
        // applies to a different payload.
        signed.envelope.seq = Seq(2);
        assert!(matches!(
            signed.verify_with(&k.public()),
            Err(VerifyError::BadSignature)
        ));
    }

    #[test]
    fn signature_hex_roundtrip() {
        let e = Envelope::command(Seq(1), CommandPayload::Heartbeat);
        let signed = SignedEnvelope {
            envelope: e,
            signature: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let json = serde_json::to_string(&signed).unwrap();
        assert!(json.contains("\"deadbeef\""));
        let back: SignedEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.signature, vec![0xde, 0xad, 0xbe, 0xef]);
    }
}
