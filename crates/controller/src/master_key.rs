//! AES-256-GCM master key for the vault.
//!
//! Threat model: a cold-copy snapshot of the vault file alone
//! should not yield plaintext secrets. We achieve that by
//! encrypting every secret in the vault under a 32-byte master
//! key that lives at a different path than the vault, with
//! 0600 permissions. Operators who need a stronger story stage
//! the key file out-of-band (systemd `LoadCredential=`, Kubernetes
//! secret mount, cloud KMS + envelope decryption) and point the
//! binary at it.
//!
//! What we DO NOT do: require an operator to type a passphrase
//! on every restart. That's the HashiCorp-Vault pattern and it's
//! wrong for this class of app — trading systems need to come
//! back up autonomously after crashes / reboots / host rotations
//! at 3am. A stolen disk is a narrow threat; a root-on-the-box
//! attacker reads process memory regardless of how fancy the
//! at-rest story is.
//!
//! Load order:
//!   1. `MM_MASTER_KEY=<64-hex>` env var — explicit operator
//!      supply (preferred for systemd `Credentials=` and
//!      container secret mounts).
//!   2. `MM_MASTER_KEY_FILE=/path/to/master-key` — raw 32-byte
//!      file at operator-chosen path. Auto-generated with 0600
//!      perms on first startup if missing.

use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::Aead;
use aes_gcm::{AeadCore, Aes256Gcm, Key, KeyInit, Nonce};
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;

#[derive(Debug, thiserror::Error)]
pub enum MasterKeyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("master key hex decode failed: {0}")]
    BadHex(String),
    #[error("master key has wrong length ({got} != 32)")]
    BadLength { got: usize },
    #[error("AEAD encrypt failed: {0}")]
    Encrypt(String),
    #[error("AEAD decrypt failed: {0}")]
    Decrypt(String),
    #[error("base64 decode: {0}")]
    B64(#[from] base64::DecodeError),
}

#[derive(Clone)]
pub struct MasterKey {
    bytes: [u8; 32],
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MasterKey")
            .field("bytes", &"<redacted>")
            .finish()
    }
}

impl MasterKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }

    pub fn from_hex(hex: &str) -> Result<Self, MasterKeyError> {
        let raw = hex::decode(hex.trim()).map_err(|e| MasterKeyError::BadHex(e.to_string()))?;
        if raw.len() != 32 {
            return Err(MasterKeyError::BadLength { got: raw.len() });
        }
        let mut b = [0u8; 32];
        b.copy_from_slice(&raw);
        Ok(Self::from_bytes(b))
    }

    /// Load from file (raw 32 bytes) or auto-generate with 0600
    /// perms. The server uses this on every start; missing file
    /// means first-run → generate + persist + warn loud so
    /// operator backs it up.
    pub fn load_or_generate(path: &Path) -> Result<Self, MasterKeyError> {
        if path.exists() {
            let raw = std::fs::read(path)?;
            if raw.len() != 32 {
                return Err(MasterKeyError::BadLength { got: raw.len() });
            }
            let mut b = [0u8; 32];
            b.copy_from_slice(&raw);
            return Ok(Self::from_bytes(b));
        }
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut f = std::fs::File::create(path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(path, perms);
            }
        }
        tracing::warn!(
            path = %path.display(),
            "generated fresh vault master key — back this file up somewhere safe, \
             losing it bricks every encrypted secret in the vault"
        );
        Ok(Self::from_bytes(bytes))
    }

    /// Preferred load path — env var (hex) wins; falls back to
    /// the file. Matches the pattern `MM_USERS` / `MM_AUTH_SECRET`
    /// already use for their own secrets.
    pub fn resolve(env_var: Option<&str>, file_path: PathBuf) -> Result<Self, MasterKeyError> {
        if let Some(hex) = env_var {
            return Self::from_hex(hex);
        }
        Self::load_or_generate(&file_path)
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.bytes))
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<EncryptedBlob, MasterKeyError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ct = self
            .cipher()
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| MasterKeyError::Encrypt(e.to_string()))?;
        let b64 = base64::engine::general_purpose::STANDARD;
        Ok(EncryptedBlob {
            ciphertext_b64: b64.encode(&ct),
            nonce_b64: b64.encode(nonce.as_slice()),
        })
    }

    pub fn decrypt(&self, blob: &EncryptedBlob) -> Result<String, MasterKeyError> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let ct = b64.decode(blob.ciphertext_b64.as_bytes())?;
        let nonce_bytes = b64.decode(blob.nonce_b64.as_bytes())?;
        if nonce_bytes.len() != 12 {
            return Err(MasterKeyError::BadLength {
                got: nonce_bytes.len(),
            });
        }
        let nonce = Nonce::from_slice(&nonce_bytes);
        let pt = self
            .cipher()
            .decrypt(nonce, ct.as_ref())
            .map_err(|e| MasterKeyError::Decrypt(e.to_string()))?;
        String::from_utf8(pt).map_err(|e| MasterKeyError::Decrypt(e.to_string()))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct EncryptedBlob {
    pub ciphertext_b64: String,
    pub nonce_b64: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let k = MasterKey::from_bytes([7u8; 32]);
        let blob = k.encrypt("super-secret-api-key").unwrap();
        assert_eq!(k.decrypt(&blob).unwrap(), "super-secret-api-key");
    }

    #[test]
    fn two_encrypts_produce_different_ciphertexts() {
        let k = MasterKey::from_bytes([3u8; 32]);
        let a = k.encrypt("x").unwrap();
        let b = k.encrypt("x").unwrap();
        assert_ne!(a.nonce_b64, b.nonce_b64);
    }

    #[test]
    fn wrong_key_rejects_ciphertext() {
        let a = MasterKey::from_bytes([1u8; 32]);
        let b = MasterKey::from_bytes([2u8; 32]);
        let blob = a.encrypt("secret").unwrap();
        assert!(b.decrypt(&blob).is_err());
    }

    #[test]
    fn hex_roundtrip() {
        let hex = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let k = MasterKey::from_hex(hex).unwrap();
        let blob = k.encrypt("m").unwrap();
        assert_eq!(k.decrypt(&blob).unwrap(), "m");
    }

    #[test]
    fn load_or_generate_persists_same_key() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("master-key");
        let a = MasterKey::load_or_generate(&p).unwrap();
        let blob = a.encrypt("m").unwrap();
        let b = MasterKey::load_or_generate(&p).unwrap();
        assert_eq!(b.decrypt(&blob).unwrap(), "m");
    }
}
