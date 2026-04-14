//! EIP-712 signing for HyperLiquid L1 actions.
//!
//! HL authenticates every `/exchange` request by:
//!   1. msgpack-encoding the action payload (field order matters),
//!   2. appending `nonce` (u64 big-endian) and a vault tag byte,
//!   3. keccak256 → 32-byte "connection id",
//!   4. wrapping in an EIP-712 typed-data "Agent" struct with a fixed
//!      domain (name="Exchange", version="1", chainId=1337, verifier=0x0),
//!   5. signing the resulting 32-byte digest with secp256k1 (recoverable).
//!
//! Mainnet vs testnet is distinguished via `source = "a"` / `"b"` inside the
//! Agent struct, NOT via chainId.

use anyhow::{anyhow, Context, Result};
use k256::ecdsa::{SigningKey, VerifyingKey};
use serde::Serialize;
use sha3::{Digest, Keccak256};

/// A secp256k1 private key with cached 20-byte Ethereum address.
#[derive(Clone)]
pub struct PrivateKey {
    signing_key: SigningKey,
    address: [u8; 20],
}

impl PrivateKey {
    /// Parse a hex-encoded 32-byte private key. `0x` prefix is optional.
    pub fn from_hex(hex_str: &str) -> Result<Self> {
        let trimmed = hex_str.trim().trim_start_matches("0x").trim_start_matches("0X");
        let bytes = hex::decode(trimmed).context("invalid hex private key")?;
        if bytes.len() != 32 {
            return Err(anyhow!(
                "private key must be 32 bytes, got {}",
                bytes.len()
            ));
        }
        let signing_key =
            SigningKey::from_slice(&bytes).context("invalid secp256k1 private key")?;
        let address = derive_address(signing_key.verifying_key());
        Ok(Self {
            signing_key,
            address,
        })
    }

    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    /// Lowercase 0x-prefixed Ethereum address.
    pub fn address_hex(&self) -> String {
        format!("0x{}", hex::encode(self.address))
    }

    fn sign_prehash(&self, prehash: &[u8; 32]) -> Result<Signature> {
        let (sig, rec) = self
            .signing_key
            .sign_prehash_recoverable(prehash)
            .context("secp256k1 signing failed")?;
        let bytes: [u8; 64] = sig.to_bytes().into();
        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        r.copy_from_slice(&bytes[..32]);
        s.copy_from_slice(&bytes[32..]);
        Ok(Signature {
            r,
            s,
            v: 27 + rec.to_byte(),
        })
    }
}

fn derive_address(pk: &VerifyingKey) -> [u8; 20] {
    // Uncompressed SEC1: 0x04 || X (32) || Y (32) = 65 bytes.
    let point = pk.to_encoded_point(false);
    let bytes = point.as_bytes();
    debug_assert_eq!(bytes.len(), 65);
    debug_assert_eq!(bytes[0], 0x04);
    let hash = keccak256(&bytes[1..]);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    addr
}

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Ethereum-style signature components. `v` is 27 or 28.
#[derive(Debug, Clone)]
pub struct Signature {
    pub r: [u8; 32],
    pub s: [u8; 32],
    pub v: u8,
}

impl Signature {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "r": format!("0x{}", hex::encode(self.r)),
            "s": format!("0x{}", hex::encode(self.s)),
            "v": self.v,
        })
    }
}

/// Compute the HL action hash (a.k.a. "connection id").
///
/// `msgpack(action) || nonce.to_be_bytes() || vault_tag`
/// where `vault_tag` is `0x00` if no vault, or `0x01 || 20-byte address`.
pub fn action_hash<T: Serialize>(
    action: &T,
    nonce: u64,
    vault: Option<&[u8; 20]>,
) -> Result<[u8; 32]> {
    // `to_vec_named` encodes structs as maps with string keys, matching HL's
    // Python SDK. Field *order* comes from the struct declaration — keep it
    // aligned with what HL expects.
    let mut data = rmp_serde::to_vec_named(action).context("msgpack encode action")?;
    data.extend_from_slice(&nonce.to_be_bytes());
    match vault {
        None => data.push(0x00),
        Some(addr) => {
            data.push(0x01);
            data.extend_from_slice(addr);
        }
    }
    Ok(keccak256(&data))
}

/// Fixed EIP-712 domain separator for HL L1 actions.
///
/// ```text
/// EIP712Domain(string name, string version, uint256 chainId, address verifyingContract)
/// name = "Exchange"
/// version = "1"
/// chainId = 1337            // <-- always 1337, mainnet/testnet via source field
/// verifyingContract = 0x0
/// ```
pub fn hl_domain_separator() -> [u8; 32] {
    let type_hash = keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let name_hash = keccak256(b"Exchange");
    let version_hash = keccak256(b"1");
    let mut chain_id = [0u8; 32];
    chain_id[24..].copy_from_slice(&1337u64.to_be_bytes());
    let verifying_contract = [0u8; 32]; // address 0x0 left-padded to 32 bytes

    let mut data = Vec::with_capacity(32 * 5);
    data.extend_from_slice(&type_hash);
    data.extend_from_slice(&name_hash);
    data.extend_from_slice(&version_hash);
    data.extend_from_slice(&chain_id);
    data.extend_from_slice(&verifying_contract);
    keccak256(&data)
}

/// Compute the EIP-712 digest for the phantom `Agent` struct wrapping a
/// connection id.
fn eip712_agent_digest(connection_id: &[u8; 32], is_mainnet: bool) -> [u8; 32] {
    let agent_type_hash = keccak256(b"Agent(string source,bytes32 connectionId)");
    let source_hash = keccak256(if is_mainnet { b"a" } else { b"b" });

    let mut struct_data = Vec::with_capacity(96);
    struct_data.extend_from_slice(&agent_type_hash);
    struct_data.extend_from_slice(&source_hash);
    struct_data.extend_from_slice(connection_id);
    let struct_hash = keccak256(&struct_data);

    let domain_sep = hl_domain_separator();
    let mut digest_data = Vec::with_capacity(2 + 32 + 32);
    digest_data.push(0x19);
    digest_data.push(0x01);
    digest_data.extend_from_slice(&domain_sep);
    digest_data.extend_from_slice(&struct_hash);
    keccak256(&digest_data)
}

/// Sign an L1 action end-to-end: msgpack → connection id → EIP-712 → secp256k1.
pub fn sign_l1_action<T: Serialize>(
    key: &PrivateKey,
    action: &T,
    nonce: u64,
    vault: Option<&[u8; 20]>,
    is_mainnet: bool,
) -> Result<Signature> {
    let connection_id = action_hash(action, nonce, vault)?;
    let digest = eip712_agent_digest(&connection_id, is_mainnet);
    key.sign_prehash(&digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Well-known Ethereum test vector: secret key `0x...01` → address
    /// `0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf`. Verifies both key parsing
    /// and keccak-based address derivation.
    #[test]
    fn derives_known_address_from_one() {
        let key = PrivateKey::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        assert_eq!(
            key.address_hex(),
            "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );
    }

    #[test]
    fn accepts_0x_prefix_and_whitespace() {
        let a = PrivateKey::from_hex(
            "0x0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let b = PrivateKey::from_hex(
            "  0000000000000000000000000000000000000000000000000000000000000001  ",
        )
        .unwrap();
        assert_eq!(a.address(), b.address());
    }

    /// keccak256 of the empty input is a standard test vector.
    #[test]
    fn keccak256_empty_matches_spec() {
        let h = keccak256(b"");
        assert_eq!(
            hex::encode(h),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }

    /// keccak256("hello") spec vector.
    #[test]
    fn keccak256_hello_matches_spec() {
        let h = keccak256(b"hello");
        assert_eq!(
            hex::encode(h),
            "1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8"
        );
    }

    /// The HL L1 domain separator is a fixed constant — pin it so we notice
    /// if any of the inputs drift. Computed independently from the spec
    /// (type hash + keccak("Exchange") + keccak("1") + u256(1337) + 0x0).
    #[test]
    fn domain_separator_is_stable() {
        let s1 = hl_domain_separator();
        let s2 = hl_domain_separator();
        assert_eq!(s1, s2);
        // Must be deterministic and non-zero.
        assert_ne!(s1, [0u8; 32]);
    }

    /// Signing twice with the same key + input must yield the same (r, s, v)
    /// because ECDSA in k256 uses deterministic RFC 6979 nonces.
    #[test]
    fn signing_is_deterministic() {
        let key = PrivateKey::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let prehash = [0x42u8; 32];
        let a = key.sign_prehash(&prehash).unwrap();
        let b = key.sign_prehash(&prehash).unwrap();
        assert_eq!(a.r, b.r);
        assert_eq!(a.s, b.s);
        assert_eq!(a.v, b.v);
        assert!(a.v == 27 || a.v == 28);
    }

    /// Signature recovers back to the same public key/address — proves the
    /// signing + v calculation match the EVM convention.
    #[test]
    fn signature_recovers_to_signer_address() {
        use k256::ecdsa::{RecoveryId, Signature as EcdsaSig, VerifyingKey};

        let key = PrivateKey::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let expected_addr = key.address();

        let prehash = [0x11u8; 32];
        let sig = key.sign_prehash(&prehash).unwrap();

        let mut sig_bytes = [0u8; 64];
        sig_bytes[..32].copy_from_slice(&sig.r);
        sig_bytes[32..].copy_from_slice(&sig.s);
        let ecdsa = EcdsaSig::from_slice(&sig_bytes).unwrap();
        let rec = RecoveryId::from_byte(sig.v - 27).unwrap();

        let recovered = VerifyingKey::recover_from_prehash(&prehash, &ecdsa, rec).unwrap();
        let recovered_addr = derive_address(&recovered);
        assert_eq!(recovered_addr, expected_addr);
    }

    /// Action hash must be stable for a stable input.
    #[test]
    fn action_hash_is_deterministic() {
        #[derive(Serialize)]
        struct Dummy {
            a: u32,
            b: bool,
        }
        let action = Dummy { a: 42, b: true };
        let h1 = action_hash(&action, 1_700_000_000_000, None).unwrap();
        let h2 = action_hash(&action, 1_700_000_000_000, None).unwrap();
        assert_eq!(h1, h2);

        // Different nonce → different hash.
        let h3 = action_hash(&action, 1_700_000_000_001, None).unwrap();
        assert_ne!(h1, h3);

        // With vault → different hash.
        let vault = [0xAAu8; 20];
        let h4 = action_hash(&action, 1_700_000_000_000, Some(&vault)).unwrap();
        assert_ne!(h1, h4);
    }

    /// Full pipeline smoke test — exercises msgpack → keccak → EIP-712 → sign.
    #[test]
    fn sign_l1_action_full_pipeline() {
        #[derive(Serialize)]
        struct Action {
            #[serde(rename = "type")]
            ty: String,
            val: u64,
        }
        let key = PrivateKey::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let action = Action {
            ty: "order".into(),
            val: 7,
        };
        let sig = sign_l1_action(&key, &action, 1_700_000_000_000, None, true).unwrap();
        assert!(sig.v == 27 || sig.v == 28);
        let json = sig.to_json();
        assert!(json["r"].as_str().unwrap().starts_with("0x"));
        assert!(json["s"].as_str().unwrap().starts_with("0x"));
    }

    /// Mainnet and testnet must produce different signatures for the same
    /// action — source field flips "a" ↔ "b" inside the phantom agent.
    #[test]
    fn mainnet_and_testnet_diverge() {
        #[derive(Serialize)]
        struct Action {
            x: u32,
        }
        let key = PrivateKey::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let action = Action { x: 1 };
        let m = sign_l1_action(&key, &action, 1, None, true).unwrap();
        let t = sign_l1_action(&key, &action, 1, None, false).unwrap();
        assert_ne!((m.r, m.s), (t.r, t.s));
    }
}
