//! Agent-local infrastructure settings.
//!
//! The `SettingsFile` is the "database of configuration" — the
//! TOML (or future DB-backed) document an operator populates
//! once per agent with credentials, feature flags, safety rails,
//! and dashboard bindings. Strategy parameters live NOT here but
//! in strategy-graph deployments pushed by the controller. That
//! separation is the whole point:
//!
//! - Settings = infrastructure. Rarely changes. Describes the
//!   universe of venues + keys available to strategies.
//! - Strategy graph = trading logic. Often changes. References
//!   settings entries by `credential_id` but never embeds secrets.
//!
//! The controller never receives raw api keys — it only knows
//! credential IDs. The agent resolves IDs to secrets locally via
//! environment variables named in the settings file, so a controller
//! compromise cannot leak venue credentials.
//!
//! Shape stays additive: new fields get `#[serde(default)]` so an
//! older settings file keeps working after an agent upgrade.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{ExchangeType, ProductType};

/// Top-level settings document. One file per agent process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsFile {
    /// The agent's own identity in the control plane fleet. The
    /// controller addresses commands to this `agent_id`, so it must
    /// be unique across the fleet.
    pub agent: AgentIdentity,

    /// Venue credentials available to strategies. Each entry has
    /// a stable `id`; strategies reference that id in their
    /// bindings. Duplicate ids are a file-level validation error
    /// (see [`SettingsFile::validate`]).
    #[serde(default)]
    pub credentials: Vec<CredentialSpec>,

    /// Feature toggles that affect the agent as a whole. Kept
    /// coarse on purpose — per-strategy knobs live in the
    /// strategy graph, not here.
    #[serde(default)]
    pub features: FeatureFlags,

    /// Global safety rails applied across every strategy this
    /// agent runs. Operator-authored limits that should be
    /// enforced even when a strategy misbehaves.
    #[serde(default)]
    pub rails: Rails,

    /// Local dashboard binding. When the agent runs in the
    /// single-process `mm-server` / `mm-local` role it serves
    /// the dashboard here; in split-role deployments this block
    /// is ignored and the controller hosts the dashboard instead.
    #[serde(default)]
    pub dashboard: DashboardSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// Human-recognisable fleet id (`"eu-hft-01"`, `"ap-tokyo-01"`).
    pub id: String,
    /// WS-RPC endpoint of the controller this agent dials. `None` for
    /// single-process roles where controller and agent share an in-
    /// memory transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_addr: Option<String>,
    /// Path to the agent's Ed25519 private key file (PEM or raw).
    /// Skipped in PR-2 — the agent-identity signing story wires
    /// up with the real WS-RPC transport. Field lands now so
    /// operators don't re-write their settings file later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_key_path: Option<String>,
}

/// One venue credential record. Secrets live in env vars named
/// by `api_key_env` / `api_secret_env`; the settings file never
/// embeds the actual key/secret. Makes this file safely
/// commit-able, scp-able, and audit-loggable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialSpec {
    pub id: String,
    pub exchange: ExchangeType,
    pub product: ProductType,
    /// Name of the env var holding the API key.
    pub api_key_env: String,
    /// Name of the env var holding the API secret.
    pub api_secret_env: String,
    /// Optional hard cap on notional this credential may trade.
    /// When set, the agent refuses to start any strategy whose
    /// `max_inventory × mark_price` exceeds this value — belt
    /// on top of the braces the strategy's own risk limits give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_notional_quote: Option<rust_decimal::Decimal>,
    /// Optional default symbol this credential trades. Strategies
    /// may override per-deployment; useful when one credential
    /// is tied to a sub-account that only holds one instrument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_symbol: Option<String>,
    /// Per-agent authorisation whitelist. Empty (the default)
    /// means every connected agent receives this credential on
    /// register. Non-empty restricts the push to the listed
    /// agent ids — useful for region-isolated subaccounts
    /// ("EU agent only sees EU keys") and least-privilege
    /// deployment models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_agents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlags {
    #[serde(default)]
    pub telegram_alerts: bool,
    #[serde(default = "default_true")]
    pub audit_jsonl: bool,
    #[serde(default)]
    pub mica_report: bool,
    #[serde(default)]
    pub paper_fill_simulation: bool,
}

// Manual Default so `audit_jsonl` stays on even when the entire
// [features] block is omitted from settings. The MiCA audit
// trail is not something we want off-by-accident.
impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            telegram_alerts: false,
            audit_jsonl: true,
            mica_report: false,
            paper_fill_simulation: false,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rails {
    /// Fleet-wide daily PnL floor. `None` means no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_loss_limit: Option<rust_decimal::Decimal>,
    /// Fleet-wide drawdown ceiling. `None` means no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_drawdown: Option<rust_decimal::Decimal>,
    /// Max messages per minute to each venue — mirrors the field
    /// already in `KillSwitchCfg`, included here so operators can
    /// set a fleet-level ceiling that strategies cannot raise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_message_rate: Option<u32>,
    /// Venue book freshness requirement in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_book_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DashboardSettings {
    /// 0 means dashboard disabled.
    #[serde(default)]
    pub port: u16,
    /// Env var holding the admin token. Never embed the token in
    /// the settings file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_key_env: Option<String>,
}

/// Credential with its api_key / api_secret already materialised
/// from environment. Lives in memory only, never serialised,
/// never leaves the agent's process. Consumers treat the two
/// secret strings as opaque and pass them to the exchange
/// connector constructor.
#[derive(Debug, Clone)]
pub struct ResolvedCredential {
    pub id: String,
    pub exchange: ExchangeType,
    pub product: ProductType,
    pub api_key: String,
    pub api_secret: String,
    pub max_notional_quote: Option<rust_decimal::Decimal>,
    pub default_symbol: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("credential id {0} not found in settings")]
    UnknownId(String),
    #[error("credential {credential_id}: env var {var} is not set")]
    MissingEnv {
        credential_id: String,
        var: String,
    },
    #[error("credential {credential_id}: env var {var} is empty")]
    EmptyEnv {
        credential_id: String,
        var: String,
    },
}

impl CredentialSpec {
    /// Look the api_key + api_secret up from the environment
    /// and return a [`ResolvedCredential`] ready to hand to a
    /// connector factory. Intentionally strict: missing or empty
    /// env vars are hard errors — we never fall back to an empty
    /// credential that would succeed REST requests with anonymous
    /// rate limits and look "almost OK."
    pub fn resolve_from_env(&self) -> Result<ResolvedCredential, ResolveError> {
        let api_key = read_required_env(&self.api_key_env, &self.id)?;
        let api_secret = read_required_env(&self.api_secret_env, &self.id)?;
        Ok(ResolvedCredential {
            id: self.id.clone(),
            exchange: self.exchange,
            product: self.product,
            api_key,
            api_secret,
            max_notional_quote: self.max_notional_quote,
            default_symbol: self.default_symbol.clone(),
        })
    }
}

fn read_required_env(var: &str, credential_id: &str) -> Result<String, ResolveError> {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(v),
        Ok(_) => Err(ResolveError::EmptyEnv {
            credential_id: credential_id.to_string(),
            var: var.to_string(),
        }),
        Err(_) => Err(ResolveError::MissingEnv {
            credential_id: credential_id.to_string(),
            var: var.to_string(),
        }),
    }
}

impl SettingsFile {
    /// Convenience wrapper that finds a credential by id and
    /// resolves its env vars in one call. `UnknownId` when the
    /// id doesn't exist in this settings file.
    pub fn resolve_credential(&self, id: &str) -> Result<ResolvedCredential, ResolveError> {
        let spec = self
            .credential(id)
            .ok_or_else(|| ResolveError::UnknownId(id.to_string()))?;
        spec.resolve_from_env()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("settings file io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings file parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("duplicate credential id: {0}")]
    DuplicateCredentialId(String),
    #[error("agent.id must not be empty")]
    EmptyAgentId,
    #[error("credential {0} has empty api_key_env or api_secret_env")]
    EmptyCredentialEnv(String),
}

impl SettingsFile {
    /// Load from a TOML file on disk. Validates before returning.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let raw = std::fs::read_to_string(path)?;
        let parsed: SettingsFile = toml::from_str(&raw)?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Parse from an in-memory string — handy for tests and for
    /// the forthcoming settings-as-DB backend where the raw
    /// document is a row payload rather than a file.
    pub fn from_str(raw: &str) -> Result<Self, SettingsError> {
        let parsed: SettingsFile = toml::from_str(raw)?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Enforce invariants the type system cannot: unique
    /// credential ids, non-empty agent id, non-empty env var
    /// names. Runs on every load path.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.agent.id.trim().is_empty() {
            return Err(SettingsError::EmptyAgentId);
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for c in &self.credentials {
            if !seen.insert(c.id.as_str()) {
                return Err(SettingsError::DuplicateCredentialId(c.id.clone()));
            }
            if c.api_key_env.trim().is_empty() || c.api_secret_env.trim().is_empty() {
                return Err(SettingsError::EmptyCredentialEnv(c.id.clone()));
            }
        }
        Ok(())
    }

    /// Look up one credential by id. `None` if not found; the
    /// caller (typically the reconcile loop in mm-agent) is
    /// responsible for logging + refusing the deployment.
    pub fn credential(&self, id: &str) -> Option<&CredentialSpec> {
        self.credentials.iter().find(|c| c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [agent]
        id = "eu-test-01"

        [[credentials]]
        id = "binance_spot_main"
        exchange = "binance"
        product = "spot"
        api_key_env = "BINANCE_MAIN_KEY"
        api_secret_env = "BINANCE_MAIN_SECRET"
    "#;

    #[test]
    fn minimal_file_parses() {
        let s = SettingsFile::from_str(MINIMAL).unwrap();
        assert_eq!(s.agent.id, "eu-test-01");
        assert_eq!(s.credentials.len(), 1);
        assert_eq!(s.credentials[0].id, "binance_spot_main");
        assert_eq!(s.credentials[0].exchange, ExchangeType::Binance);
        assert_eq!(s.credentials[0].product, ProductType::Spot);
    }

    #[test]
    fn feature_flags_default_audit_on() {
        let s = SettingsFile::from_str(MINIMAL).unwrap();
        assert!(s.features.audit_jsonl, "audit_jsonl defaults true for MiCA");
        assert!(!s.features.telegram_alerts);
        assert!(!s.features.mica_report);
    }

    #[test]
    fn duplicate_credential_id_rejected() {
        let raw = r#"
            [agent]
            id = "a"

            [[credentials]]
            id = "dup"
            exchange = "binance"
            product = "spot"
            api_key_env = "K"
            api_secret_env = "S"

            [[credentials]]
            id = "dup"
            exchange = "bybit"
            product = "linear_perp"
            api_key_env = "K2"
            api_secret_env = "S2"
        "#;
        let err = SettingsFile::from_str(raw).unwrap_err();
        assert!(matches!(err, SettingsError::DuplicateCredentialId(_)));
    }

    #[test]
    fn empty_agent_id_rejected() {
        let raw = r#"
            [agent]
            id = "   "
        "#;
        let err = SettingsFile::from_str(raw).unwrap_err();
        assert!(matches!(err, SettingsError::EmptyAgentId));
    }

    #[test]
    fn empty_credential_env_rejected() {
        let raw = r#"
            [agent]
            id = "a"

            [[credentials]]
            id = "c1"
            exchange = "binance"
            product = "spot"
            api_key_env = ""
            api_secret_env = "S"
        "#;
        let err = SettingsFile::from_str(raw).unwrap_err();
        assert!(matches!(err, SettingsError::EmptyCredentialEnv(_)));
    }

    #[test]
    fn credential_lookup_by_id() {
        let s = SettingsFile::from_str(MINIMAL).unwrap();
        assert!(s.credential("binance_spot_main").is_some());
        assert!(s.credential("unknown").is_none());
    }

    #[test]
    fn resolve_credential_reads_env_vars() {
        // Use unique env var names so parallel test runs don't
        // step on each other.
        let k = format!("MM_TEST_KEY_{}", uuid::Uuid::new_v4().simple());
        let s = format!("MM_TEST_SECRET_{}", uuid::Uuid::new_v4().simple());
        std::env::set_var(&k, "live-key-value");
        std::env::set_var(&s, "live-secret-value");

        let raw = format!(
            r#"
            [agent]
            id = "a"

            [[credentials]]
            id = "c1"
            exchange = "binance"
            product = "spot"
            api_key_env = "{k}"
            api_secret_env = "{s}"
        "#
        );
        let cfg = SettingsFile::from_str(&raw).unwrap();
        let resolved = cfg.resolve_credential("c1").unwrap();
        assert_eq!(resolved.api_key, "live-key-value");
        assert_eq!(resolved.api_secret, "live-secret-value");
        assert_eq!(resolved.exchange, ExchangeType::Binance);

        std::env::remove_var(&k);
        std::env::remove_var(&s);
    }

    #[test]
    fn resolve_missing_env_is_hard_error() {
        // Env var is deliberately not set — catch-all fallback
        // MUST NOT swallow this.
        let raw = r#"
            [agent]
            id = "a"

            [[credentials]]
            id = "c1"
            exchange = "binance"
            product = "spot"
            api_key_env = "MM_TEST_ABSENT_KEY_XYZ"
            api_secret_env = "MM_TEST_ABSENT_SECRET_XYZ"
        "#;
        let cfg = SettingsFile::from_str(raw).unwrap();
        let err = cfg.resolve_credential("c1").unwrap_err();
        assert!(matches!(err, ResolveError::MissingEnv { .. }));
    }

    #[test]
    fn resolve_empty_env_is_hard_error() {
        let k = format!("MM_TEST_EMPTY_KEY_{}", uuid::Uuid::new_v4().simple());
        let s = format!("MM_TEST_EMPTY_SECRET_{}", uuid::Uuid::new_v4().simple());
        std::env::set_var(&k, "");
        std::env::set_var(&s, "ok");

        let raw = format!(
            r#"
            [agent]
            id = "a"

            [[credentials]]
            id = "c1"
            exchange = "binance"
            product = "spot"
            api_key_env = "{k}"
            api_secret_env = "{s}"
        "#
        );
        let cfg = SettingsFile::from_str(&raw).unwrap();
        let err = cfg.resolve_credential("c1").unwrap_err();
        assert!(matches!(err, ResolveError::EmptyEnv { .. }));

        std::env::remove_var(&k);
        std::env::remove_var(&s);
    }

    #[test]
    fn resolve_unknown_id_is_typed_error() {
        let cfg = SettingsFile::from_str(MINIMAL).unwrap();
        let err = cfg.resolve_credential("does-not-exist").unwrap_err();
        assert!(matches!(err, ResolveError::UnknownId(_)));
    }

    #[test]
    fn serde_roundtrip_preserves_shape() {
        let s = SettingsFile::from_str(MINIMAL).unwrap();
        let re = toml::to_string(&s).unwrap();
        let back = SettingsFile::from_str(&re).unwrap();
        assert_eq!(back.agent.id, s.agent.id);
        assert_eq!(back.credentials.len(), 1);
        assert_eq!(back.credentials[0].id, s.credentials[0].id);
    }
}
