use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use mm_common::types::OrderId;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::Path;
use subtle::ConstantTimeEq;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

/// Persistent state checkpoint for crash recovery.
///
/// On every state change (fill, order placed/cancelled), we write a checkpoint.
/// On restart, we load the last checkpoint and reconcile with exchange state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub timestamp: DateTime<Utc>,
    /// Per-symbol state.
    pub symbols: HashMap<String, SymbolCheckpoint>,
    /// Global PnL tracking.
    pub daily_pnl: Decimal,
    pub total_realized_pnl: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolCheckpoint {
    pub symbol: String,
    /// Net inventory in base asset.
    pub inventory: Decimal,
    /// Average entry price.
    pub avg_entry_price: Decimal,
    /// Known open order IDs (to reconcile on restart).
    pub open_order_ids: Vec<OrderId>,
    /// Realized PnL for this symbol.
    pub realized_pnl: Decimal,
    /// Total volume traded.
    pub total_volume: Decimal,
    /// Total fills count.
    pub total_fills: u64,
    /// S2.1 — atomic bundles that were in flight when the
    /// checkpoint was written. Serialised as opaque JSON so
    /// `mm-persistence` stays engine-type-free; the
    /// `mm-engine` side (re)parses into its
    /// `InflightAtomicBundle` struct on load. On restart the
    /// engine walks this list, force-cancels every expired
    /// bundle's legs via the real connectors, and re-enters
    /// the quoting loop. Without this a crash mid-dispatch
    /// left operators with a spot order placed + perp hedge
    /// pending and no record of the pair.
    #[serde(default)]
    pub inflight_atomic_bundles: Vec<serde_json::Value>,
    /// 22B-0 — strategy-owned calibration / FSM / window state.
    /// `None` for stateless strategies. The engine reads this
    /// into `Strategy::restore_state` on the next boot. The
    /// schema is opaque to this crate — each strategy defines
    /// its own shape and schema version. Missing key after a
    /// crate upgrade is fine (serde_default = None), lets
    /// legacy checkpoints keep loading.
    #[serde(default)]
    pub strategy_state: Option<serde_json::Value>,
}

impl Checkpoint {
    pub fn new() -> Self {
        Self {
            timestamp: Utc::now(),
            symbols: HashMap::new(),
            daily_pnl: Decimal::ZERO,
            total_realized_pnl: Decimal::ZERO,
        }
    }

    /// Validate checkpoint sanity (Epic 7 item 7.2).
    /// Returns a list of issues found; empty = valid.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if self.timestamp > Utc::now() {
            issues.push("checkpoint timestamp is in the future".into());
        }
        for (symbol, sc) in &self.symbols {
            if !sc.inventory.is_zero() && sc.avg_entry_price.is_zero() {
                issues.push(format!(
                    "{symbol}: non-zero inventory ({}) with zero avg_entry_price",
                    sc.inventory
                ));
            }
            if sc.avg_entry_price < Decimal::ZERO {
                issues.push(format!(
                    "{symbol}: negative avg_entry_price ({})",
                    sc.avg_entry_price
                ));
            }
            if sc.total_fills > 0 && sc.total_volume.is_zero() {
                issues.push(format!(
                    "{symbol}: {} fills but zero volume",
                    sc.total_fills
                ));
            }
        }
        issues
    }
}

impl Default for Checkpoint {
    fn default() -> Self {
        Self::new()
    }
}

/// Signed checkpoint envelope written to disk. The `signature`
/// is HMAC-SHA256 of `payload` under a secret loaded from
/// `MM_CHECKPOINT_SECRET` (falling back to `MM_AUTH_SECRET` so
/// single-secret deployments keep working). The payload is the
/// raw JSON of the `Checkpoint` struct — encoded once so the
/// signer and verifier hash identical bytes.
#[derive(Serialize, Deserialize)]
struct SignedCheckpoint {
    /// Envelope format version. `1` = raw JSON + hex HMAC-SHA256.
    version: u32,
    payload: String,
    signature: String,
}

/// Pick the HMAC secret for checkpoint integrity. Prefers
/// `MM_CHECKPOINT_SECRET`, then `MM_AUTH_SECRET`. Returns `None`
/// when neither is set — the manager then writes unsigned
/// checkpoints and logs a warning so an unattended deployment
/// cannot silently skip tamper detection.
fn checkpoint_secret() -> Option<String> {
    if let Ok(s) = std::env::var("MM_CHECKPOINT_SECRET") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    if let Ok(s) = std::env::var("MM_AUTH_SECRET") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn sign(secret: &str, payload: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Manages checkpoint persistence to disk.
pub struct CheckpointManager {
    path: std::path::PathBuf,
    current: Checkpoint,
    /// Write counter — flush every N updates.
    write_count: u64,
    flush_every: u64,
    /// HMAC secret; `None` skips signing/verification with a
    /// warning — intentional for tests and fresh local dev only.
    secret: Option<String>,
}

impl CheckpointManager {
    /// Create a new checkpoint manager. Loads existing checkpoint
    /// from disk, verifying its HMAC signature. Tampered, unsigned
    /// (when a secret is configured), or malformed files are
    /// rejected and the manager starts fresh — the operator sees a
    /// loud warning so a silent reset cannot hide a downgrade.
    pub fn new(path: &Path, flush_every: u64) -> Self {
        let secret = checkpoint_secret();
        if secret.is_none() {
            warn!(
                path = %path.display(),
                "MM_CHECKPOINT_SECRET / MM_AUTH_SECRET not set — checkpoint \
                 integrity verification DISABLED (a tampered file would be \
                 accepted). Set a secret in production."
            );
        }
        Self::new_with_secret(path, flush_every, secret)
    }

    /// Like `new()` but takes the HMAC secret directly. Useful for
    /// test code that wants to avoid mutating process-wide env vars
    /// (which races against parallel tests).
    pub fn new_with_secret(path: &Path, flush_every: u64, secret: Option<String>) -> Self {
        let current = Self::load_from_disk(path, secret.as_deref()).unwrap_or_else(|e| {
            warn!(error = %e, "starting with fresh checkpoint");
            Checkpoint::new()
        });

        Self {
            path: path.to_path_buf(),
            current,
            write_count: 0,
            flush_every,
            secret,
        }
    }

    /// Get current checkpoint state.
    pub fn current(&self) -> &Checkpoint {
        &self.current
    }

    /// Update symbol state and optionally flush.
    pub fn update_symbol(&mut self, state: SymbolCheckpoint) {
        self.current.symbols.insert(state.symbol.clone(), state);
        self.current.timestamp = Utc::now();
        self.write_count += 1;

        if self.write_count >= self.flush_every {
            if let Err(e) = self.flush() {
                warn!(error = %e, "failed to flush checkpoint");
            }
            self.write_count = 0;
        }
    }

    /// Update global PnL.
    pub fn update_pnl(&mut self, daily: Decimal, total_realized: Decimal) {
        self.current.daily_pnl = daily;
        self.current.total_realized_pnl = total_realized;
    }

    /// Force flush to disk. Writes a signed envelope atomically
    /// via temp-file + rename so a crash mid-write cannot produce
    /// a truncated checkpoint that still verifies.
    pub fn flush(&self) -> anyhow::Result<()> {
        let payload = serde_json::to_string(&self.current)?;
        let signature = match &self.secret {
            Some(s) => sign(s, &payload),
            None => String::new(),
        };
        let envelope = SignedCheckpoint {
            version: 1,
            payload,
            signature,
        };
        let json = serde_json::to_string_pretty(&envelope)?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &json)?;
        // Set file permissions to 0600 on Unix — checkpoint
        // contains positions and PnL, shouldn't be world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&tmp, perms);
        }
        std::fs::rename(&tmp, &self.path)?;
        info!(
            symbols = self.current.symbols.len(),
            signed = self.secret.is_some(),
            "checkpoint flushed to disk"
        );
        Ok(())
    }

    /// Load checkpoint from disk. Accepts both the signed envelope
    /// (v1) and a legacy bare-`Checkpoint` JSON for backward
    /// compatibility with pre-HMAC deployments — the legacy path
    /// logs a warning so the operator knows to let the next flush
    /// re-sign the file.
    fn load_from_disk(path: &Path, secret: Option<&str>) -> anyhow::Result<Checkpoint> {
        let content = std::fs::read_to_string(path)?;

        // Try the signed envelope first.
        if let Ok(env) = serde_json::from_str::<SignedCheckpoint>(&content) {
            if env.version != 1 {
                anyhow::bail!("unsupported checkpoint envelope version {}", env.version);
            }
            match (secret, env.signature.is_empty()) {
                (Some(s), false) => {
                    let expected = sign(s, &env.payload);
                    if env
                        .signature
                        .as_bytes()
                        .ct_eq(expected.as_bytes())
                        .unwrap_u8()
                        != 1
                    {
                        anyhow::bail!(
                            "checkpoint HMAC mismatch — file was tampered with or signed \
                             with a different secret; refusing to load"
                        );
                    }
                }
                (Some(_), true) => {
                    warn!(
                        path = %path.display(),
                        "checkpoint is unsigned but a secret is configured — \
                         accepting for migration; next flush will sign it"
                    );
                }
                (None, false) => {
                    warn!(
                        path = %path.display(),
                        "checkpoint carries a signature but no secret is configured — \
                         skipping verification (set MM_CHECKPOINT_SECRET to enforce it)"
                    );
                }
                (None, true) => {
                    // No secret, no signature — unsigned path, fine.
                }
            }
            let checkpoint: Checkpoint = serde_json::from_str(&env.payload)?;
            info!(
                timestamp = %checkpoint.timestamp,
                symbols = checkpoint.symbols.len(),
                "loaded checkpoint from disk"
            );
            return Ok(checkpoint);
        }

        // Legacy unsigned checkpoint — accept with warning.
        let checkpoint: Checkpoint = serde_json::from_str(&content)?;
        warn!(
            path = %path.display(),
            "loaded legacy unsigned checkpoint — next flush will write a signed envelope"
        );
        Ok(checkpoint)
    }

    /// Get symbol state for reconciliation on restart.
    pub fn get_symbol(&self, symbol: &str) -> Option<&SymbolCheckpoint> {
        self.current.symbols.get(symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn fresh_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        // Uniqueify per test run to avoid cross-test contamination
        // when tests run in parallel.
        p.push(format!(
            "mm_ckpt_{}_{}.json",
            name,
            uuid::Uuid::new_v4().simple()
        ));
        p
    }

    /// 22B-0 — strategy_state round-trips verbatim through the
    /// signed envelope. A strategy writes an opaque JSON blob;
    /// loader returns it intact for the restore path to feed
    /// back into `Strategy::restore_state`.
    #[test]
    fn strategy_state_roundtrips() {
        let path = fresh_path("strat-state");
        let secret = Some("test-secret-stratstate".to_string());
        let mut mgr = CheckpointManager::new_with_secret(&path, 1, secret.clone());
        let state = serde_json::json!({
            "schema_version": 1,
            "glft": {"a": "1.23", "k": "4.56", "samples": 72},
        });
        mgr.update_symbol(SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0),
            avg_entry_price: dec!(0),
            open_order_ids: vec![],
            realized_pnl: dec!(0),
            total_volume: dec!(0),
            total_fills: 0,
            inflight_atomic_bundles: Vec::new(),
            strategy_state: Some(state.clone()),
        });

        let loaded = CheckpointManager::new_with_secret(&path, 10, secret);
        let got = loaded.get_symbol("BTCUSDT").unwrap();
        assert_eq!(got.strategy_state, Some(state));
        let _ = std::fs::remove_file(&path);
    }

    /// 22B-0 — legacy checkpoints (pre-strategy_state) load with
    /// `strategy_state = None` via `serde(default)`. Critical:
    /// operators upgrading from pre-22B-0 must not hit a
    /// deserialisation error on first boot.
    #[test]
    fn legacy_checkpoint_without_strategy_state_loads() {
        let path = fresh_path("strat-legacy");
        // Raw JSON without the `strategy_state` key.
        let legacy = r#"{"timestamp":"2026-04-19T00:00:00Z","symbols":{"BTCUSDT":{"symbol":"BTCUSDT","inventory":"0","avg_entry_price":"0","open_order_ids":[],"realized_pnl":"0","total_volume":"0","total_fills":0}},"daily_pnl":"0","total_realized_pnl":"0"}"#;
        std::fs::write(&path, legacy).unwrap();

        let loaded = CheckpointManager::new_with_secret(&path, 10, None);
        let got = loaded.get_symbol("BTCUSDT").unwrap();
        assert!(got.strategy_state.is_none());
        assert!(got.inflight_atomic_bundles.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_checkpoint_roundtrip() {
        let path = fresh_path("roundtrip");
        let secret = Some("test-secret-roundtrip".to_string());

        let mut mgr = CheckpointManager::new_with_secret(&path, 1, secret.clone());
        mgr.update_symbol(SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0.05),
            avg_entry_price: dec!(50000),
            open_order_ids: vec![],
            realized_pnl: dec!(42.5),
            total_volume: dec!(10000),
            total_fills: 100,
            inflight_atomic_bundles: Vec::new(),
            strategy_state: None,
        });

        let loaded = CheckpointManager::new_with_secret(&path, 10, secret);
        let sym = loaded.get_symbol("BTCUSDT").unwrap();
        assert_eq!(sym.inventory, dec!(0.05));
        assert_eq!(sym.realized_pnl, dec!(42.5));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_checkpoint_rejects_tampered_payload() {
        let path = fresh_path("tamper");
        let secret = Some("test-secret-tamper".to_string());

        let mut mgr = CheckpointManager::new_with_secret(&path, 1, secret.clone());
        mgr.update_symbol(SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(1.0),
            avg_entry_price: dec!(50000),
            open_order_ids: vec![],
            realized_pnl: dec!(0),
            total_volume: dec!(50000),
            total_fills: 1,
            inflight_atomic_bundles: Vec::new(),
            strategy_state: None,
        });

        // Tamper with the file — bump inventory directly in the
        // escaped JSON payload without re-signing. The payload is
        // an escaped JSON string inside the envelope, so the
        // tamper pattern uses the escaped form.
        let content = std::fs::read_to_string(&path).unwrap();
        let tampered = content.replace(r#"\"inventory\":\"1.0\""#, r#"\"inventory\":\"100.0\""#);
        assert_ne!(content, tampered, "tamper pattern must match");
        std::fs::write(&path, &tampered).unwrap();

        let loaded = CheckpointManager::new_with_secret(&path, 10, secret);
        // Tampered file is rejected → fresh state, not the
        // attacker-inflated inventory.
        assert!(loaded.get_symbol("BTCUSDT").is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_checkpoint_rejects_wrong_secret() {
        let path = fresh_path("wrong-secret");

        let mut mgr =
            CheckpointManager::new_with_secret(&path, 1, Some("write-secret".to_string()));
        mgr.update_symbol(SymbolCheckpoint {
            symbol: "BTCUSDT".into(),
            inventory: dec!(0.5),
            avg_entry_price: dec!(50000),
            open_order_ids: vec![],
            realized_pnl: dec!(0),
            total_volume: dec!(25000),
            total_fills: 1,
            inflight_atomic_bundles: Vec::new(),
            strategy_state: None,
        });

        let loaded =
            CheckpointManager::new_with_secret(&path, 10, Some("different-secret".to_string()));
        assert!(loaded.get_symbol("BTCUSDT").is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_checkpoint_accepts_legacy_unsigned() {
        // Tests that pre-HMAC checkpoints still load.
        let path = fresh_path("legacy");

        let legacy = Checkpoint {
            timestamp: Utc::now(),
            symbols: {
                let mut m = HashMap::new();
                m.insert(
                    "ETHUSDT".to_string(),
                    SymbolCheckpoint {
                        symbol: "ETHUSDT".into(),
                        inventory: dec!(0.2),
                        avg_entry_price: dec!(3000),
                        open_order_ids: vec![],
                        realized_pnl: dec!(0),
                        total_volume: dec!(600),
                        total_fills: 1,
                        inflight_atomic_bundles: Vec::new(),
                        strategy_state: None,
                    },
                );
                m
            },
            daily_pnl: Decimal::ZERO,
            total_realized_pnl: Decimal::ZERO,
        };
        std::fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();

        // Pass None explicitly to avoid racing the process env.
        let loaded = CheckpointManager::new_with_secret(&path, 10, None);
        assert_eq!(loaded.get_symbol("ETHUSDT").unwrap().inventory, dec!(0.2));

        let _ = std::fs::remove_file(&path);
    }

    // ── Property-based tests (Epic 10) ───────────────────────
    //
    // HMAC-signed checkpoint invariants: roundtrip, tamper
    // detection, wrong-secret rejection. The handwritten tests
    // above cover representative cases; these widen the net with
    // random symbol / qty / pnl inputs to catch encoding edge
    // cases (scientific notation on small Decimals, empty symbol
    // strings, etc.) before they hit prod.

    use proptest::prelude::*;

    prop_compose! {
        fn dec_strat()(cents in -1_000_000_000i64..1_000_000_000i64) -> Decimal {
            Decimal::new(cents, 4)
        }
    }
    prop_compose! {
        fn symbol_strat()(
            s in "[A-Z]{3,6}(USDT|USDC|BTC|ETH)",
        ) -> String {
            s
        }
    }
    prop_compose! {
        fn sym_ckpt_strat()(
            symbol in symbol_strat(),
            inventory in dec_strat(),
            avg_entry_price in dec_strat(),
            realized_pnl in dec_strat(),
            total_volume in dec_strat(),
            total_fills in 0u64..1_000_000u64,
        ) -> SymbolCheckpoint {
            SymbolCheckpoint {
                symbol,
                inventory,
                avg_entry_price,
                open_order_ids: vec![],
                realized_pnl,
                total_volume,
                total_fills,
                inflight_atomic_bundles: Vec::new(),
                strategy_state: None,
            }
        }
    }

    fn fresh_path_prop(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mm_ckpt_prop_{}_{}.json",
            tag,
            uuid::Uuid::new_v4().simple()
        ));
        p
    }

    proptest! {
        /// Sign → flush → load round-trips every field verbatim.
        #[test]
        fn signed_roundtrip_preserves_fields(
            sym in sym_ckpt_strat(),
            secret in "[a-zA-Z0-9]{8,64}",
        ) {
            let path = fresh_path_prop("rt");
            let mut mgr = CheckpointManager::new_with_secret(
                &path, 1, Some(secret.clone())
            );
            mgr.update_symbol(sym.clone());

            let loaded = CheckpointManager::new_with_secret(&path, 10, Some(secret));
            let got = loaded.get_symbol(&sym.symbol);
            prop_assert!(got.is_some(), "symbol missing after roundtrip");
            let got = got.unwrap();
            prop_assert_eq!(&got.symbol, &sym.symbol);
            prop_assert_eq!(got.inventory, sym.inventory);
            prop_assert_eq!(got.avg_entry_price, sym.avg_entry_price);
            prop_assert_eq!(got.realized_pnl, sym.realized_pnl);
            prop_assert_eq!(got.total_volume, sym.total_volume);
            prop_assert_eq!(got.total_fills, sym.total_fills);

            let _ = std::fs::remove_file(&path);
        }

        /// Any non-trivial tampering of the on-disk payload must
        /// be rejected. We flip a random byte in the payload
        /// substring of the envelope and verify the loader
        /// discards the file rather than silently accepting a
        /// forged inventory.
        #[test]
        fn tamper_is_always_detected(
            sym in sym_ckpt_strat(),
            secret in "[a-zA-Z0-9]{16,32}",
            tamper_byte in 0u8..255u8,
        ) {
            let path = fresh_path_prop("tamper");
            let mut mgr = CheckpointManager::new_with_secret(
                &path, 1, Some(secret.clone())
            );
            mgr.update_symbol(sym.clone());

            // Read the envelope and flip exactly one byte in the
            // payload region (between the payload quote marks).
            let content = std::fs::read_to_string(&path).unwrap();
            let payload_start = content.find(r#""payload""#).unwrap_or(0);
            if payload_start == 0 {
                // Shouldn't happen, but skip rather than panic.
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            // Pick a position inside the payload region — bump
            // by an offset that lands us inside the JSON string
            // content. Actual tamper: XOR byte to guarantee it
            // changes.
            let target_pos = payload_start + 20 + (tamper_byte as usize % 30);
            let bytes = content.as_bytes();
            if target_pos >= bytes.len() {
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            let orig_ch = bytes[target_pos];
            // Replace with a different ASCII byte. HARD-4 —
            // rebuild through `Vec<u8>` so we don't need the
            // `String::as_bytes_mut` unsafe: tampering with a
            // byte inside a UTF-8 string could invalidate the
            // invariant, but we immediately re-decode under
            // `String::from_utf8_lossy` so the test reads a
            // legal string either way.
            let new_ch = if orig_ch == b'A' { b'Z' } else { b'A' };
            let mut tampered_bytes = bytes.to_vec();
            tampered_bytes[target_pos] = new_ch;
            let tampered = String::from_utf8_lossy(&tampered_bytes).into_owned();
            if tampered == content {
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            std::fs::write(&path, &tampered).unwrap();

            let loaded = CheckpointManager::new_with_secret(&path, 10, Some(secret));
            // Tampered → either HMAC mismatch OR the mutation
            // corrupted the JSON and load returned empty. Either
            // way, the attacker-controlled symbol MUST NOT be
            // loaded verbatim.
            match loaded.get_symbol(&sym.symbol) {
                None => {}
                Some(got) => {
                    // If loader happened to accept (shouldn't,
                    // but let's be thorough), make sure the
                    // payload still decoded correctly — i.e.
                    // byte flip landed outside the tampered
                    // fields. Inventory / pnl must match or
                    // the loader was fooled.
                    prop_assert_eq!(got.inventory, sym.inventory,
                        "tamper accepted without detection");
                }
            }

            let _ = std::fs::remove_file(&path);
        }

        /// Wrong secret never loads the checkpoint regardless of
        /// payload contents. Uses different secrets of >= 8
        /// bytes so the filter tests real secrets not empty
        /// strings.
        #[test]
        fn wrong_secret_always_rejects(
            sym in sym_ckpt_strat(),
            write_secret in "[a-zA-Z0-9]{16,32}",
            read_secret in "[a-zA-Z0-9]{16,32}",
        ) {
            prop_assume!(write_secret != read_secret);
            let path = fresh_path_prop("wrong");
            let mut mgr = CheckpointManager::new_with_secret(
                &path, 1, Some(write_secret)
            );
            mgr.update_symbol(sym.clone());

            let loaded = CheckpointManager::new_with_secret(&path, 10, Some(read_secret));
            prop_assert!(loaded.get_symbol(&sym.symbol).is_none(),
                "wrong-secret load must not return the checkpoint");

            let _ = std::fs::remove_file(&path);
        }

        /// Checkpoint::validate() returns no issues on a
        /// consistent state: zero-inventory symbols or those with
        /// matching price/volume/fill counts.
        #[test]
        fn validate_clean_on_consistent_state(
            inv_cents in 1i64..1_000_000,
            price_cents in 1i64..1_000_000_000,
            vol_cents in 1i64..1_000_000_000,
            fills in 1u64..1000,
        ) {
            let mut cp = Checkpoint::new();
            cp.symbols.insert("BTCUSDT".into(), SymbolCheckpoint {
                symbol: "BTCUSDT".into(),
                inventory: Decimal::new(inv_cents, 4),
                avg_entry_price: Decimal::new(price_cents, 2),
                open_order_ids: vec![],
                realized_pnl: dec!(0),
                total_volume: Decimal::new(vol_cents, 2),
                total_fills: fills,
                inflight_atomic_bundles: Vec::new(),
                strategy_state: None,
            });
            prop_assert!(cp.validate().is_empty(),
                "clean state flagged: {:?}", cp.validate());
        }

        /// validate() flags negative avg_entry_price — an
        /// invariant we rely on to catch bit-flip / migration
        /// corruption.
        #[test]
        fn validate_flags_negative_entry_price(
            neg_cents in 1i64..1_000_000,
        ) {
            let mut cp = Checkpoint::new();
            cp.symbols.insert("BTCUSDT".into(), SymbolCheckpoint {
                symbol: "BTCUSDT".into(),
                inventory: dec!(0),
                avg_entry_price: -Decimal::new(neg_cents, 2),
                open_order_ids: vec![],
                realized_pnl: dec!(0),
                total_volume: dec!(0),
                total_fills: 0,
                inflight_atomic_bundles: Vec::new(),
                strategy_state: None,
            });
            prop_assert!(!cp.validate().is_empty(),
                "negative entry price not flagged");
        }

        /// validate() flags non-zero inventory with zero entry
        /// price — an impossible state that indicates a corrupted
        /// checkpoint.
        #[test]
        fn validate_flags_inventory_without_entry_price(
            inv_cents in 1i64..1_000_000,
        ) {
            let mut cp = Checkpoint::new();
            cp.symbols.insert("BTCUSDT".into(), SymbolCheckpoint {
                symbol: "BTCUSDT".into(),
                inventory: Decimal::new(inv_cents, 4),
                avg_entry_price: dec!(0),
                open_order_ids: vec![],
                realized_pnl: dec!(0),
                total_volume: dec!(0),
                total_fills: 0,
                inflight_atomic_bundles: Vec::new(),
                strategy_state: None,
            });
            prop_assert!(!cp.validate().is_empty(),
                "inventory without entry price not flagged");
        }

        /// update_pnl is idempotent on equal inputs and always
        /// last-write-wins. Catches a regression where the
        /// manager batches or smooths PnL updates.
        #[test]
        fn update_pnl_is_last_write_wins(
            a_daily in -1_000_000i64..1_000_000,
            a_total in -1_000_000i64..1_000_000,
            b_daily in -1_000_000i64..1_000_000,
            b_total in -1_000_000i64..1_000_000,
        ) {
            let path = fresh_path_prop("pnl");
            let mut mgr = CheckpointManager::new_with_secret(&path, 100, None);
            mgr.update_pnl(Decimal::new(a_daily, 2), Decimal::new(a_total, 2));
            mgr.update_pnl(Decimal::new(b_daily, 2), Decimal::new(b_total, 2));
            prop_assert_eq!(mgr.current().daily_pnl, Decimal::new(b_daily, 2));
            prop_assert_eq!(mgr.current().total_realized_pnl, Decimal::new(b_total, 2));
            let _ = std::fs::remove_file(&path);
        }
    }
}
