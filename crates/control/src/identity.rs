//! Ed25519 identity for controller + agent.
//!
//! Each side of the control channel holds a 32-byte Ed25519
//! private key on disk (0400 perms in production). Public keys
//! exchange at register time: the agent advertises its pubkey
//! via the Register telemetry, the controller's pubkey is pinned in
//! the agent's settings file (so a rogue peer can't impersonate
//! the controller even if DNS is hijacked).
//!
//! Envelope signing:
//! - Sender serialises the [`crate::envelope::Envelope`] to JSON
//!   canonical bytes.
//! - Signs with its private key; attaches the 64-byte signature
//!   to [`crate::envelope::SignedEnvelope`].
//! - Receiver verifies against the peer's pubkey before routing
//!   the payload. Mismatch → refuse + log.
//!
//! File format: **raw 32-byte seed** (PKCS8 is optional future
//! work; raw seed avoids pulling an ASN.1 parser into the agent
//! for its startup path). Operators generate with
//! `openssl genpkey -algorithm ed25519 -outform DER | tail -c 32`
//! or the provided `IdentityKey::generate` helper.

use std::path::Path;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey, SIGNATURE_LENGTH};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("io error reading / writing key material: {0}")]
    Io(#[from] std::io::Error),
    #[error("key file has unexpected length {0} (expected 32-byte seed)")]
    BadSeedLength(usize),
    #[error("public key hex decode failed: {0}")]
    BadHex(String),
    #[error("public key has wrong length ({got} != 32)")]
    BadPubLength { got: usize },
    #[error("signature has wrong length ({got} != 64)")]
    BadSigLength { got: usize },
    #[error("signature verification failed")]
    VerifyFailed,
}

/// Owned Ed25519 signing key. Wraps `ed25519_dalek::SigningKey`
/// with I/O convenience methods for the agent + controller startup
/// paths.
#[derive(Debug, Clone)]
pub struct IdentityKey {
    signing: SigningKey,
}

impl IdentityKey {
    /// Generate a fresh key pair. Useful in tests + the
    /// `mm-agent --generate-identity` operator flow that the
    /// binary will surface in a follow-up PR.
    pub fn generate() -> Self {
        use rand::rngs::OsRng;
        Self {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    /// Load the 32-byte seed from a file. Rejects any other
    /// length — we explicitly refuse to parse PKCS8 / PEM
    /// here to keep the hot path free of ASN.1 machinery.
    pub fn load_from_file(path: &Path) -> Result<Self, IdentityError> {
        let bytes = std::fs::read(path)?;
        if bytes.len() != 32 {
            return Err(IdentityError::BadSeedLength(bytes.len()));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok(Self {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// Write the seed to a file. Callers are responsible for
    /// setting 0400 perms after — we don't assume a Unix API
    /// here because the library also compiles on Windows for
    /// CI / dev.
    pub fn save_to_file(&self, path: &Path) -> Result<(), IdentityError> {
        std::fs::write(path, self.signing.to_bytes())?;
        Ok(())
    }

    /// Compact 32-byte public key (hex-encoded at the wire
    /// layer). The agent attaches this to its Register frame;
    /// the controller pins its expected copy in settings.
    pub fn public(&self) -> PublicKey {
        PublicKey(self.signing.verifying_key())
    }

    /// Sign `msg` producing a 64-byte Ed25519 signature.
    pub fn sign(&self, msg: &[u8]) -> [u8; SIGNATURE_LENGTH] {
        self.signing.sign(msg).to_bytes()
    }
}

/// Public key counterpart — controller stores one per registered
/// agent, agent stores one for the controller. Round-trips as a
/// hex string on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(pub(crate) VerifyingKey);

impl PublicKey {
    pub fn from_hex(hex_str: &str) -> Result<Self, IdentityError> {
        let bytes = hex::decode(hex_str).map_err(|e| IdentityError::BadHex(e.to_string()))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdentityError> {
        if bytes.len() != 32 {
            return Err(IdentityError::BadPubLength { got: bytes.len() });
        }
        let mut buf = [0u8; 32];
        buf.copy_from_slice(bytes);
        let vk = VerifyingKey::from_bytes(&buf)
            .map_err(|e| IdentityError::BadHex(e.to_string()))?;
        Ok(Self(vk))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }

    /// Stable short identifier derived from the public key — first
    /// 16 hex chars (8 bytes) of `SHA-256(pubkey)`. Used by the
    /// controller's approval store as the admission-control key:
    /// the `agent_id` string can be chosen freely by anyone, the
    /// fingerprint only matches the peer who holds the private
    /// seed. Operators see this on the Fleet UI and approve by
    /// fingerprint, not by self-advertised id.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.0.to_bytes());
        hex::encode(&digest[..8])
    }

    pub fn verify(&self, msg: &[u8], sig: &[u8]) -> Result<(), IdentityError> {
        if sig.len() != SIGNATURE_LENGTH {
            return Err(IdentityError::BadSigLength { got: sig.len() });
        }
        let mut buf = [0u8; SIGNATURE_LENGTH];
        buf.copy_from_slice(sig);
        let parsed = ed25519_dalek::Signature::from_bytes(&buf);
        self.0.verify(msg, &parsed).map_err(|_| IdentityError::VerifyFailed)
    }
}

impl Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_16_hex_chars() {
        let k = IdentityKey::generate();
        let pk = k.public();
        let fp1 = pk.fingerprint();
        let fp2 = pk.fingerprint();
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
        assert!(fp1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fingerprint_differs_between_keys() {
        let a = IdentityKey::generate().public().fingerprint();
        let b = IdentityKey::generate().public().fingerprint();
        assert_ne!(a, b);
    }

    #[test]
    fn generate_sign_verify_roundtrip() {
        let k = IdentityKey::generate();
        let pub_key = k.public();
        let msg = b"hello, control-plane";
        let sig = k.sign(msg);
        assert!(pub_key.verify(msg, &sig).is_ok());
    }

    #[test]
    fn tampered_message_fails_verify() {
        let k = IdentityKey::generate();
        let pub_key = k.public();
        let sig = k.sign(b"original");
        assert!(pub_key.verify(b"different", &sig).is_err());
    }

    #[test]
    fn wrong_signer_fails_verify() {
        let a = IdentityKey::generate();
        let b = IdentityKey::generate();
        let sig = a.sign(b"m");
        assert!(b.public().verify(b"m", &sig).is_err());
    }

    #[test]
    fn seed_file_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.key");
        let k = IdentityKey::generate();
        k.save_to_file(&path).unwrap();
        let loaded = IdentityKey::load_from_file(&path).unwrap();
        // Same pubkey on both sides == same underlying seed.
        assert_eq!(k.public().to_hex(), loaded.public().to_hex());
    }

    #[test]
    fn bad_seed_length_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.key");
        std::fs::write(&path, [0u8; 16]).unwrap();
        let err = IdentityKey::load_from_file(&path).unwrap_err();
        assert!(matches!(err, IdentityError::BadSeedLength(16)));
    }

    #[test]
    fn pubkey_hex_roundtrips_through_serde() {
        let k = IdentityKey::generate();
        let pk = k.public();
        let json = serde_json::to_string(&pk).unwrap();
        let back: PublicKey = serde_json::from_str(&json).unwrap();
        assert_eq!(pk.to_hex(), back.to_hex());
    }
}
