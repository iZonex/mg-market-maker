//! Unified vault — one encrypted store for every secret the
//! controller holds, whether it's a venue API keypair pushed to
//! agents or a Telegram bot token consumed locally.
//!
//! Prior design split this into two modules (`credentials.rs` +
//! `secrets.rs`) because the consumption patterns differ: exchange
//! credentials are push-to-agent with per-agent ACLs, while generic
//! secrets stay server-side. From an operator's perspective that
//! was an artificial distinction — one vault of encrypted things,
//! tagged by kind, is simpler. The distinction is now a runtime
//! filter (`kind == "exchange"`) rather than a separate store.
//!
//! Shape:
//!   - `VaultEntry` carries a flat string keyed bag of secret
//!     `values` (plaintext in memory), a `metadata` bag for
//!     non-secret labels (exchange, product, …), and an optional
//!     `allowed_agents` whitelist that only matters for exchange
//!     entries.
//!   - On disk, each secret in `values` is encrypted separately
//!     under its own AES-256-GCM nonce. `metadata` stays plaintext
//!     — it describes the entry, it is not the secret itself.
//!
//! Kinds (server doesn't enforce — UI groups):
//!   `"exchange"`    — venue API keypair. Required values:
//!                     `api_key`, `api_secret`. Required metadata:
//!                     `exchange`, `product`. Optional metadata:
//!                     `default_symbol`, `max_notional_quote`.
//!                     Honours `allowed_agents`. Pushed to agents.
//!   `"telegram"`    — bot token. Value: `token`. Metadata may
//!                     carry `chat_id`.
//!   `"sentry"`      — Sentry DSN. Value: `dsn`.
//!   `"webhook"`     — outbound URL. Value: `url`.
//!   `"smtp"`        — SMTP creds. Values: `username`, `password`.
//!                     Metadata: `host`, `port`.
//!   `"rpc"`         — on-chain RPC provider key. Value: `api_key`.
//!                     Metadata: `url`, `chain`.
//!   `"generic"`     — anything else. Shape is free — operator
//!                     decides which keys to store.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use chrono::Utc;
use mm_control::messages::PushedCredential;
use serde::{Deserialize, Serialize};

use crate::master_key::{EncryptedBlob, MasterKey, MasterKeyError};

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("duplicate entry name: {0}")]
    Duplicate(String),
    #[error("entry not found: {0}")]
    NotFound(String),
    #[error("encryption: {0}")]
    Crypto(#[from] MasterKeyError),
    #[error("entry validation: {0}")]
    Invalid(String),
}

/// Entry kinds the server recognises for validation. Unknown
/// strings are accepted and stored — but `Kind::Exchange` gates
/// the push-to-agent path. Kept as a runtime tag so adding new
/// service kinds to the UI doesn't require a server rebuild.
pub mod kinds {
    pub const EXCHANGE: &str = "exchange";
    pub const TELEGRAM: &str = "telegram";
    pub const SENTRY: &str = "sentry";
    pub const WEBHOOK: &str = "webhook";
    pub const SMTP: &str = "smtp";
    pub const RPC: &str = "rpc";
    pub const GENERIC: &str = "generic";
}

#[derive(Debug, Clone)]
pub struct VaultEntry {
    pub name: String,
    pub kind: String,
    pub description: Option<String>,
    /// Secret values, keyed by field name. `api_key`, `api_secret`
    /// for exchange; `token` for telegram; operator-defined for
    /// generic. Encrypted individually on disk.
    pub values: BTreeMap<String, String>,
    /// Non-secret labels describing the entry (exchange, product,
    /// chat_id, url, …). Plaintext on disk — the UI needs these
    /// to render the entry without decrypting secrets.
    pub metadata: BTreeMap<String, String>,
    /// Agent whitelist — only meaningful for `kind = "exchange"`.
    /// Empty list = pushable to every Accepted agent; non-empty =
    /// only listed agent ids receive the credential on register.
    pub allowed_agents: Vec<String>,
    /// Tenant this entry belongs to. `None` = shared infra (the
    /// old default — Telegram bots, Sentry DSNs shared across
    /// tenants, agents with no `client_id` on their profile).
    /// `Some(x)` = scoped to tenant `x`; the controller's deploy
    /// gate refuses to route such an entry to an agent whose
    /// profile `client_id` doesn't match, and refuses any
    /// deployment whose selected credentials mix two tenants.
    /// Coarser than `allowed_agents` — both gates compose.
    pub client_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Wave C5 — epoch millis of the last rotation. `None` if
    /// the secret has never been rotated since creation (same
    /// as `created_at_ms`). Updated whenever `upsert` replaces
    /// at least one existing secret value; pure metadata / ACL
    /// edits don't bump it.
    pub rotated_at_ms: Option<i64>,
    /// Wave C6 — operator-supplied expiry (epoch millis).
    /// `None` = never expires (default). UI renders a red
    /// warning chip when `expires_at_ms - now < 7 days`.
    pub expires_at_ms: Option<i64>,
}

/// Public-facing view of a vault entry — everything EXCEPT the
/// secret values. The list endpoint returns this shape so an
/// operator can see what's in the vault without revealing
/// plaintexts.
#[derive(Debug, Clone, Serialize)]
pub struct VaultSummary {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// The set of VALUE KEYS (not their plaintexts) the entry
    /// has — `["api_key", "api_secret"]` for exchange,
    /// `["token"]` for telegram. Lets the UI show "2 secret
    /// fields" without exposing anything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Wave C5 — last rotation timestamp. See `VaultEntry::rotated_at_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at_ms: Option<i64>,
    /// Wave C6 — operator-supplied expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OnDiskEntry {
    name: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default)]
    values_enc: BTreeMap<String, EncryptedBlob>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed_agents: Vec<String>,
    /// Wave 2b tenant tag. Missing in older on-disk files — serde
    /// `default` gives `None`, which means "shared infra" and
    /// preserves existing behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rotated_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct VaultFile {
    #[serde(default)]
    entries: Vec<OnDiskEntry>,
}

#[derive(Clone)]
pub struct VaultStore {
    inner: Arc<RwLock<HashMap<String, VaultEntry>>>,
    master_key: Option<Arc<MasterKey>>,
    path: Option<Arc<PathBuf>>,
}

impl std::fmt::Debug for VaultStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultStore")
            .field("len", &self.len())
            .field("has_master_key", &self.master_key.is_some())
            .field("path", &self.path)
            .finish()
    }
}

impl Default for VaultStore {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            master_key: None,
            path: None,
        }
    }
}

impl VaultStore {
    pub fn in_memory_with_key(master_key: MasterKey) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            master_key: Some(Arc::new(master_key)),
            path: None,
        }
    }

    pub fn load_from_path(
        path: impl AsRef<Path>,
        master_key: MasterKey,
    ) -> Result<Self, VaultError> {
        let p = path.as_ref().to_path_buf();
        let mut map: HashMap<String, VaultEntry> = HashMap::new();
        if p.exists() {
            let raw = std::fs::read_to_string(&p)?;
            if !raw.trim().is_empty() {
                let parsed: VaultFile = serde_json::from_str(&raw)?;
                for rec in parsed.entries {
                    let mut values = BTreeMap::new();
                    for (k, blob) in rec.values_enc {
                        values.insert(k, master_key.decrypt(&blob)?);
                    }
                    map.insert(
                        rec.name.clone(),
                        VaultEntry {
                            name: rec.name,
                            kind: rec.kind,
                            description: rec.description,
                            values,
                            metadata: rec.metadata,
                            allowed_agents: rec.allowed_agents,
                            client_id: rec.client_id,
                            created_at_ms: rec.created_at_ms,
                            updated_at_ms: rec.updated_at_ms,
                            rotated_at_ms: rec.rotated_at_ms,
                            expires_at_ms: rec.expires_at_ms,
                        },
                    );
                }
            }
        }
        Ok(Self {
            inner: Arc::new(RwLock::new(map)),
            master_key: Some(Arc::new(master_key)),
            path: Some(Arc::new(p)),
        })
    }

    /// Create a new entry. Validates kind-specific required
    /// fields (exchange demands api_key + api_secret + exchange +
    /// product; telegram demands token; …). Empty name rejected.
    pub fn insert(&self, entry: VaultEntry) -> Result<VaultSummary, VaultError> {
        validate(&entry)?;
        let now = Utc::now().timestamp_millis();
        let mut entry = entry;
        if entry.created_at_ms == 0 {
            entry.created_at_ms = now;
        }
        entry.updated_at_ms = now;
        {
            let mut guard = self
                .inner
                .write()
                .map_err(|_| VaultError::Io(std::io::Error::other("vault poisoned")))?;
            if guard.contains_key(&entry.name) {
                return Err(VaultError::Duplicate(entry.name));
            }
            guard.insert(entry.name.clone(), entry.clone());
        }
        self.persist()?;
        Ok(summary(&entry))
    }

    pub fn upsert(&self, entry: VaultEntry) -> Result<VaultSummary, VaultError> {
        validate(&entry)?;
        let now = Utc::now().timestamp_millis();
        let mut entry = entry;
        let updated = {
            let mut guard = self
                .inner
                .write()
                .map_err(|_| VaultError::Io(std::io::Error::other("vault poisoned")))?;
            let prev = guard.get(&entry.name).cloned();
            let created_at = prev.as_ref().map(|e| e.created_at_ms).unwrap_or(now);
            entry.created_at_ms = created_at;
            entry.updated_at_ms = now;

            // Wave C5 — rotation detection. A rotation happens
            // when upsert replaces at least one existing secret
            // value with a different plaintext, or when the set
            // of secret keys itself changes (e.g. adding an
            // `api_passphrase`). Pure metadata / ACL / expiry
            // edits leave `rotated_at_ms` untouched so the UI
            // can show a faithful "last rotated" age.
            let is_rotation = match prev.as_ref() {
                None => false,
                Some(prev) => prev.values != entry.values,
            };
            if is_rotation {
                entry.rotated_at_ms = Some(now);
            } else if let Some(prev) = prev.as_ref() {
                // Preserve the previous rotation stamp on
                // non-secret edits.
                entry.rotated_at_ms = prev.rotated_at_ms;
            }
            guard.insert(entry.name.clone(), entry.clone());
            entry
        };
        self.persist()?;
        Ok(summary(&updated))
    }

    pub fn remove(&self, name: &str) -> Result<(), VaultError> {
        let removed = {
            let mut guard = self
                .inner
                .write()
                .map_err(|_| VaultError::Io(std::io::Error::other("vault poisoned")))?;
            guard.remove(name).is_some()
        };
        if removed {
            self.persist()?;
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<VaultEntry> {
        self.inner.read().ok().and_then(|g| g.get(name).cloned())
    }

    pub fn get_value(&self, name: &str, key: &str) -> Option<String> {
        self.get(name).and_then(|e| e.values.get(key).cloned())
    }

    pub fn list_summaries(&self) -> Vec<VaultSummary> {
        let mut out: Vec<VaultSummary> = self
            .inner
            .read()
            .map(|g| g.values().map(summary).collect())
            .unwrap_or_default();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ────────────────────────── Exchange-specific helpers ──────────────────────────
    //
    // These filter to `kind == "exchange"` entries. The session
    // push pipeline + deploy validation + UI dropdowns all call
    // into these methods; non-exchange entries are invisible to
    // the agent-facing paths.

    pub fn pushable_exchange_for_agent(&self, agent_id: &str) -> Vec<PushedCredential> {
        let mut out = Vec::new();
        let Ok(guard) = self.inner.read() else {
            return out;
        };
        for entry in guard.values() {
            if entry.kind != kinds::EXCHANGE {
                continue;
            }
            if !agent_is_authorised(entry, agent_id) {
                tracing::debug!(
                    credential = %entry.name,
                    agent = agent_id,
                    "exchange credential not authorised for agent — skipping push"
                );
                continue;
            }
            let (Some(api_key), Some(api_secret)) = (
                entry.values.get("api_key").cloned(),
                entry.values.get("api_secret").cloned(),
            ) else {
                tracing::warn!(
                    credential = %entry.name,
                    "exchange entry missing api_key or api_secret — skipping push"
                );
                continue;
            };
            out.push(PushedCredential {
                id: entry.name.clone(),
                exchange: entry.metadata.get("exchange").cloned().unwrap_or_default(),
                product: entry.metadata.get("product").cloned().unwrap_or_default(),
                api_key,
                api_secret,
                max_notional_quote: entry.metadata.get("max_notional_quote").cloned(),
                default_symbol: entry.metadata.get("default_symbol").cloned(),
            });
        }
        out
    }

    pub fn exchange_descriptors_for_agent(&self, agent_id: &str) -> Vec<CredentialDescriptor> {
        let mut out = Vec::new();
        let Ok(guard) = self.inner.read() else {
            return out;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        for entry in guard.values() {
            if entry.kind != kinds::EXCHANGE {
                continue;
            }
            if !agent_is_authorised(entry, agent_id) {
                continue;
            }
            // Fix #4 — expired creds are excluded from the push
            // list entirely. Agents on reconnect then won't
            // receive them; the deploy gate also refuses them
            // so there's no path through which an expired
            // credential reaches a live engine.
            if let Some(exp) = entry.expires_at_ms {
                if now_ms >= exp {
                    continue;
                }
            }
            out.push(CredentialDescriptor {
                id: entry.name.clone(),
                exchange: entry.metadata.get("exchange").cloned().unwrap_or_default(),
                product: entry.metadata.get("product").cloned().unwrap_or_default(),
                default_symbol: entry.metadata.get("default_symbol").cloned(),
                client_id: entry.client_id.clone(),
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Access-control check used by the controller's pre-deploy
    /// validation path.
    ///
    /// Three gates compose in order (earliest rejection wins):
    ///   1. Entry kind — only `exchange` entries are deployable.
    ///   2. Tenant — if both the credential and the agent's
    ///      profile have a `client_id`, they must match. `None`
    ///      on either side is "shared infra", which bypasses
    ///      the gate. This is the Wave 2b coarse tenant fence.
    ///   3. `allowed_agents` whitelist — fine-grained, "even
    ///      within my tenant, only these specific agents".
    ///
    /// Wrong kind / unknown credential collapse before tenant —
    /// operator needs to fix those first regardless of tenancy.
    pub fn can_exchange_access(
        &self,
        credential_id: &str,
        agent_id: &str,
        agent_tenant: Option<&str>,
    ) -> CredentialCheck {
        let Some(entry) = self.get(credential_id) else {
            return CredentialCheck::Unknown;
        };
        if entry.kind != kinds::EXCHANGE {
            return CredentialCheck::WrongKind { actual: entry.kind };
        }
        // Fix #4 — block expired credentials BEFORE tenant
        // gate. Expiry is operator-facing sanity (force
        // rotation cadence), not a security boundary; the
        // tenant gate remains the real isolation.
        if let Some(exp_ms) = entry.expires_at_ms {
            let now_ms = chrono::Utc::now().timestamp_millis();
            if now_ms >= exp_ms {
                return CredentialCheck::Expired {
                    expired_at_ms: exp_ms,
                };
            }
        }
        if let Some(cred_tenant) = entry.client_id.as_deref() {
            if let Some(agent_tenant) = agent_tenant {
                if cred_tenant != agent_tenant {
                    return CredentialCheck::TenantMismatch {
                        cred_tenant: cred_tenant.to_string(),
                        agent_tenant: agent_tenant.to_string(),
                    };
                }
            }
            // Credential is tenant-scoped but agent profile has
            // no tenant — refuse. Shared-infra escape goes only
            // in the opposite direction (shared credential on a
            // tenant agent is fine).
            else {
                return CredentialCheck::TenantMismatch {
                    cred_tenant: cred_tenant.to_string(),
                    agent_tenant: String::new(),
                };
            }
        }
        if agent_is_authorised(&entry, agent_id) {
            CredentialCheck::Ok {
                exchange: entry.metadata.get("exchange").cloned().unwrap_or_default(),
                product: entry.metadata.get("product").cloned().unwrap_or_default(),
            }
        } else {
            CredentialCheck::NotAuthorised {
                whitelist: entry.allowed_agents.clone(),
            }
        }
    }

    fn persist(&self) -> Result<(), VaultError> {
        let (Some(path), Some(key)) = (self.path.as_ref(), self.master_key.as_ref()) else {
            return Ok(());
        };
        let snapshot: Vec<VaultEntry> = self
            .inner
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default();
        let mut records = Vec::with_capacity(snapshot.len());
        for entry in snapshot {
            let mut values_enc = BTreeMap::new();
            for (k, v) in entry.values {
                values_enc.insert(k, key.encrypt(&v)?);
            }
            records.push(OnDiskEntry {
                name: entry.name,
                kind: entry.kind,
                description: entry.description,
                values_enc,
                metadata: entry.metadata,
                allowed_agents: entry.allowed_agents,
                client_id: entry.client_id,
                created_at_ms: entry.created_at_ms,
                updated_at_ms: entry.updated_at_ms,
                rotated_at_ms: entry.rotated_at_ms,
                expires_at_ms: entry.expires_at_ms,
            });
        }
        let file = VaultFile { entries: records };
        let json = serde_json::to_string_pretty(&file)?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".{}.tmp-{}",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("vault"),
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path.as_path())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path.as_path()) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(path.as_path(), perms);
            }
        }
        Ok(())
    }
}

fn validate(entry: &VaultEntry) -> Result<(), VaultError> {
    if entry.name.trim().is_empty() {
        return Err(VaultError::Invalid("name must not be empty".into()));
    }
    if entry.kind.trim().is_empty() {
        return Err(VaultError::Invalid("kind must not be empty".into()));
    }
    match entry.kind.as_str() {
        kinds::EXCHANGE => {
            for k in ["api_key", "api_secret"] {
                if entry.values.get(k).map(|v| v.is_empty()).unwrap_or(true) {
                    return Err(VaultError::Invalid(format!(
                        "exchange entry requires value '{k}'"
                    )));
                }
            }
            for k in ["exchange", "product"] {
                if entry.metadata.get(k).map(|v| v.is_empty()).unwrap_or(true) {
                    return Err(VaultError::Invalid(format!(
                        "exchange entry requires metadata '{k}'"
                    )));
                }
            }
            let allowed = [
                "binance",
                "binance_testnet",
                "bybit",
                "bybit_testnet",
                "hyperliquid",
                "hyperliquid_testnet",
            ];
            let ex = entry
                .metadata
                .get("exchange")
                .map(String::as_str)
                .unwrap_or("");
            if !allowed.contains(&ex) {
                return Err(VaultError::Invalid(format!(
                    "unknown exchange '{ex}' — allowed: {allowed:?}"
                )));
            }
            let allowed_products = ["spot", "linear_perp", "inverse_perp"];
            let pr = entry
                .metadata
                .get("product")
                .map(String::as_str)
                .unwrap_or("");
            if !allowed_products.contains(&pr) {
                return Err(VaultError::Invalid(format!(
                    "unknown product '{pr}' — allowed: {allowed_products:?}"
                )));
            }
        }
        _ => {
            if entry.values.is_empty() {
                return Err(VaultError::Invalid("entry has no values".into()));
            }
            if entry.values.values().any(|v| v.is_empty()) {
                return Err(VaultError::Invalid("values must not be empty".into()));
            }
        }
    }
    Ok(())
}

fn agent_is_authorised(entry: &VaultEntry, agent_id: &str) -> bool {
    entry.allowed_agents.is_empty() || entry.allowed_agents.iter().any(|a| a == agent_id)
}

fn summary(r: &VaultEntry) -> VaultSummary {
    VaultSummary {
        name: r.name.clone(),
        kind: r.kind.clone(),
        description: r.description.clone(),
        metadata: r.metadata.clone(),
        value_keys: r.values.keys().cloned().collect(),
        allowed_agents: r.allowed_agents.clone(),
        client_id: r.client_id.clone(),
        created_at_ms: r.created_at_ms,
        updated_at_ms: r.updated_at_ms,
        rotated_at_ms: r.rotated_at_ms,
        expires_at_ms: r.expires_at_ms,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialCheck {
    Ok {
        exchange: String,
        product: String,
    },
    Unknown,
    WrongKind {
        actual: String,
    },
    NotAuthorised {
        whitelist: Vec<String>,
    },
    /// Wave 2b — credential's tenant (`client_id`) doesn't match
    /// the target agent's profile `client_id`. `cred_tenant` and
    /// `agent_tenant` surface the clash for the UI error banner.
    TenantMismatch {
        cred_tenant: String,
        agent_tenant: String,
    },
    /// Fix #4 — credential carried `expires_at_ms` and that
    /// deadline is already past. Agents receiving an expired
    /// credential would hit auth failures on every exchange
    /// call, so we refuse the gate before push. `expired_at_ms`
    /// surfaces the exact timestamp for the UI banner.
    Expired {
        expired_at_ms: i64,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CredentialDescriptor {
    pub id: String,
    pub exchange: String,
    pub product: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_symbol: Option<String>,
    /// Wave 2b tenant tag — `None` = shared infra.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> MasterKey {
        MasterKey::from_bytes([99u8; 32])
    }

    fn exchange_entry(name: &str) -> VaultEntry {
        let mut values = BTreeMap::new();
        values.insert("api_key".into(), "live-key".into());
        values.insert("api_secret".into(), "live-secret".into());
        let mut metadata = BTreeMap::new();
        metadata.insert("exchange".into(), "binance".into());
        metadata.insert("product".into(), "spot".into());
        VaultEntry {
            name: name.into(),
            kind: kinds::EXCHANGE.into(),
            description: None,
            values,
            metadata,
            allowed_agents: Vec::new(),
            client_id: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            rotated_at_ms: None,
            expires_at_ms: None,
        }
    }

    fn telegram_entry(name: &str) -> VaultEntry {
        let mut values = BTreeMap::new();
        values.insert("token".into(), "BOT_TOKEN".into());
        let mut metadata = BTreeMap::new();
        metadata.insert("chat_id".into(), "-123".into());
        VaultEntry {
            name: name.into(),
            kind: kinds::TELEGRAM.into(),
            description: Some("ops alerts".into()),
            values,
            metadata,
            allowed_agents: Vec::new(),
            client_id: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            rotated_at_ms: None,
            expires_at_ms: None,
        }
    }

    #[test]
    fn insert_exchange_and_list() {
        let v = VaultStore::in_memory_with_key(key());
        v.insert(exchange_entry("binance_spot_main")).unwrap();
        let rows = v.list_summaries();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "exchange");
        assert_eq!(rows[0].metadata.get("exchange").unwrap(), "binance");
        assert_eq!(rows[0].value_keys, vec!["api_key", "api_secret"]);
    }

    #[test]
    fn exchange_missing_value_rejected() {
        let v = VaultStore::in_memory_with_key(key());
        let mut e = exchange_entry("x");
        e.values.remove("api_secret");
        let err = v.insert(e).unwrap_err();
        assert!(matches!(err, VaultError::Invalid(_)));
    }

    #[test]
    fn exchange_bad_exchange_rejected() {
        let v = VaultStore::in_memory_with_key(key());
        let mut e = exchange_entry("x");
        e.metadata.insert("exchange".into(), "nopeMex".into());
        let err = v.insert(e).unwrap_err();
        assert!(matches!(err, VaultError::Invalid(_)));
    }

    #[test]
    fn telegram_entry_accepted() {
        let v = VaultStore::in_memory_with_key(key());
        v.insert(telegram_entry("telegram_ops")).unwrap();
        assert_eq!(v.get_value("telegram_ops", "token").unwrap(), "BOT_TOKEN");
    }

    #[test]
    fn list_never_surfaces_plaintext_values() {
        let v = VaultStore::in_memory_with_key(key());
        v.insert(telegram_entry("t")).unwrap();
        v.insert(exchange_entry("e")).unwrap();
        let raw = serde_json::to_string(&v.list_summaries()).unwrap();
        assert!(!raw.contains("BOT_TOKEN"));
        assert!(!raw.contains("live-secret"));
    }

    #[test]
    fn upsert_rotates_secret() {
        let v = VaultStore::in_memory_with_key(key());
        v.insert(telegram_entry("t")).unwrap();
        let mut rot = telegram_entry("t");
        rot.values.insert("token".into(), "ROTATED".into());
        v.upsert(rot).unwrap();
        assert_eq!(v.get_value("t", "token").unwrap(), "ROTATED");
    }

    #[test]
    fn upsert_preserves_created_at() {
        let v = VaultStore::in_memory_with_key(key());
        v.insert(telegram_entry("t")).unwrap();
        let created = v.get("t").unwrap().created_at_ms;
        std::thread::sleep(std::time::Duration::from_millis(3));
        v.upsert(telegram_entry("t")).unwrap();
        let entry = v.get("t").unwrap();
        assert_eq!(entry.created_at_ms, created);
        assert!(entry.updated_at_ms >= entry.created_at_ms);
    }

    #[test]
    fn pushable_only_returns_exchange_kind() {
        let v = VaultStore::in_memory_with_key(key());
        v.insert(exchange_entry("e")).unwrap();
        v.insert(telegram_entry("t")).unwrap();
        let pushable = v.pushable_exchange_for_agent("agent-01");
        assert_eq!(pushable.len(), 1);
        assert_eq!(pushable[0].id, "e");
        assert_eq!(pushable[0].api_key, "live-key");
    }

    #[test]
    fn exchange_descriptors_filter_by_kind_and_whitelist() {
        let v = VaultStore::in_memory_with_key(key());
        v.insert(exchange_entry("e1")).unwrap();
        let mut e2 = exchange_entry("e2");
        e2.allowed_agents = vec!["eu-01".into()];
        v.insert(e2).unwrap();
        v.insert(telegram_entry("t")).unwrap();
        let descs = v.exchange_descriptors_for_agent("eu-01");
        assert_eq!(descs.len(), 2);
        let descs_ap = v.exchange_descriptors_for_agent("ap-01");
        assert_eq!(descs_ap.len(), 1);
        assert_eq!(descs_ap[0].id, "e1");
    }

    #[test]
    fn can_exchange_access_rejects_expired_credentials() {
        let v = VaultStore::in_memory_with_key(key());
        let mut e = exchange_entry("stale");
        // Expired one hour ago.
        e.expires_at_ms = Some(Utc::now().timestamp_millis() - 3_600_000);
        v.insert(e).unwrap();
        match v.can_exchange_access("stale", "eu-01", None) {
            CredentialCheck::Expired { expired_at_ms } => {
                assert!(expired_at_ms < Utc::now().timestamp_millis());
            }
            other => panic!("expected Expired, got: {other:?}"),
        }
        // And the push list excludes it so agents on reconnect
        // won't even see the credential in their catalog.
        let descs = v.exchange_descriptors_for_agent("eu-01");
        assert!(descs.iter().all(|d| d.id != "stale"));
    }

    #[test]
    fn can_exchange_access_ok_on_future_expiry() {
        let v = VaultStore::in_memory_with_key(key());
        let mut e = exchange_entry("fresh");
        // Expires in an hour — still valid.
        e.expires_at_ms = Some(Utc::now().timestamp_millis() + 3_600_000);
        v.insert(e).unwrap();
        assert!(matches!(
            v.can_exchange_access("fresh", "eu-01", None),
            CredentialCheck::Ok { .. }
        ));
    }

    #[test]
    fn can_exchange_access_flags_wrong_kind() {
        let v = VaultStore::in_memory_with_key(key());
        v.insert(telegram_entry("t")).unwrap();
        match v.can_exchange_access("t", "eu-01", None) {
            CredentialCheck::WrongKind { actual } => assert_eq!(actual, "telegram"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn can_exchange_access_shared_infra_matches_any_tenant() {
        let v = VaultStore::in_memory_with_key(key());
        // cred with no client_id = shared infra.
        v.insert(exchange_entry("shared")).unwrap();
        // Either agent tenant shape is fine.
        assert!(matches!(
            v.can_exchange_access("shared", "eu-01", None),
            CredentialCheck::Ok { .. }
        ));
        assert!(matches!(
            v.can_exchange_access("shared", "eu-01", Some("alice")),
            CredentialCheck::Ok { .. }
        ));
    }

    #[test]
    fn can_exchange_access_blocks_cross_tenant() {
        let v = VaultStore::in_memory_with_key(key());
        let mut e = exchange_entry("alice_cred");
        e.client_id = Some("alice".into());
        v.insert(e).unwrap();
        // Agent belongs to tenant "bob" — tenant mismatch fires.
        match v.can_exchange_access("alice_cred", "eu-01", Some("bob")) {
            CredentialCheck::TenantMismatch {
                cred_tenant,
                agent_tenant,
            } => {
                assert_eq!(cred_tenant, "alice");
                assert_eq!(agent_tenant, "bob");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn can_exchange_access_blocks_tenant_cred_on_untagged_agent() {
        let v = VaultStore::in_memory_with_key(key());
        let mut e = exchange_entry("alice_cred");
        e.client_id = Some("alice".into());
        v.insert(e).unwrap();
        // Agent profile has no client_id — also refused; the
        // shared-infra escape only runs in the opposite direction.
        match v.can_exchange_access("alice_cred", "eu-01", None) {
            CredentialCheck::TenantMismatch {
                cred_tenant,
                agent_tenant,
            } => {
                assert_eq!(cred_tenant, "alice");
                assert_eq!(agent_tenant, "");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn can_exchange_access_tenant_match_then_whitelist() {
        let v = VaultStore::in_memory_with_key(key());
        let mut e = exchange_entry("alice_cred");
        e.client_id = Some("alice".into());
        e.allowed_agents = vec!["eu-01".into()];
        v.insert(e).unwrap();
        // Matching tenant + in whitelist → Ok.
        assert!(matches!(
            v.can_exchange_access("alice_cred", "eu-01", Some("alice")),
            CredentialCheck::Ok { .. }
        ));
        // Matching tenant, out of whitelist → NotAuthorised.
        assert!(matches!(
            v.can_exchange_access("alice_cred", "ap-01", Some("alice")),
            CredentialCheck::NotAuthorised { .. }
        ));
    }

    #[test]
    fn disk_roundtrip_decrypts() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("vault.json");
        {
            let v = VaultStore::load_from_path(&path, key()).unwrap();
            v.insert(exchange_entry("e")).unwrap();
            v.insert(telegram_entry("t")).unwrap();
        }
        let reloaded = VaultStore::load_from_path(&path, key()).unwrap();
        assert_eq!(reloaded.len(), 2);
        let e = reloaded.get("e").unwrap();
        assert_eq!(e.values.get("api_key").unwrap(), "live-key");
    }

    #[test]
    fn on_disk_has_no_plaintext() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("vault.json");
        let v = VaultStore::load_from_path(&path, key()).unwrap();
        v.insert(exchange_entry("e")).unwrap();
        v.insert(telegram_entry("t")).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("live-key"));
        assert!(!raw.contains("BOT_TOKEN"));
        assert!(raw.contains("binance"));
    }
}
