//! Credential catalog — the read-only view of the agent's local
//! settings that the reconcile loop consumes to resolve
//! deployment bindings.
//!
//! The catalog wraps a [`SettingsFile`] so consumers don't need
//! to know about TOML or IO. It exists because the reconcile
//! loop and the engine factory both need "give me the resolved
//! credential for id X" and neither should have to thread the
//! whole settings blob around.
//!
//! PR-2b keeps the catalog lookup-only. PR-2c extends it with
//! the [`mm_common::settings::ResolvedCredential`] cache so we
//! don't re-read env on every reconcile pass.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use mm_common::config::{ExchangeType, ProductType};
use mm_common::settings::{ResolveError, ResolvedCredential, SettingsFile};
use mm_control::messages::PushedCredential;

/// In-memory credential catalog — populated by controller-pushed
/// PushCredential commands, read by the RealEngineFactory at
/// reconcile time. Cloneable (Arc-backed) so multiple paths
/// share the same live map.
///
/// PR-2k pivot: credentials no longer come from the agent's
/// settings.toml. Controller owns the credential material, pushes it
/// over the authenticated TLS channel, and the agent keeps the
/// resolved secrets in memory only. Disk persistence would
/// defeat the "compromise agent → leak ONE region's keys"
/// blast-radius limit this architecture buys us.
#[derive(Debug, Clone)]
pub struct CredentialCatalog {
    /// Feature flags + rails + agent identity, loaded from the
    /// agent's on-disk settings at startup. Does NOT contain
    /// credentials — those come over the wire.
    settings: Arc<SettingsFile>,
    inner: Arc<RwLock<HashMap<String, ResolvedCredential>>>,
}

impl CredentialCatalog {
    pub fn from_settings(settings: SettingsFile) -> Self {
        Self {
            settings: Arc::new(settings),
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Borrow the underlying settings — useful for read-only
    /// access to feature flags / rails from places that already
    /// hold the catalog.
    pub fn settings(&self) -> &SettingsFile {
        &self.settings
    }

    /// Install (or replace) a credential the controller just pushed.
    /// Called from the LeaseClient's PushCredential handler.
    pub fn insert(&self, pushed: PushedCredential) -> Result<(), CredentialError> {
        let exchange = parse_exchange(&pushed.exchange)
            .ok_or_else(|| CredentialError::BadExchange(pushed.exchange.clone()))?;
        let product = parse_product(&pushed.product)
            .ok_or_else(|| CredentialError::BadProduct(pushed.product.clone()))?;
        let max_notional_quote = pushed
            .max_notional_quote
            .as_deref()
            .map(|s| s.parse::<rust_decimal::Decimal>())
            .transpose()
            .map_err(|e| CredentialError::BadDecimal(e.to_string()))?;
        let resolved = ResolvedCredential {
            id: pushed.id.clone(),
            exchange,
            product,
            api_key: pushed.api_key,
            api_secret: pushed.api_secret,
            max_notional_quote,
            default_symbol: pushed.default_symbol,
        };
        if let Ok(mut guard) = self.inner.write() {
            guard.insert(pushed.id, resolved);
        }
        Ok(())
    }

    /// Forget a credential — controller pushes a zero-credential
    /// payload or an explicit delete when rotating / revoking.
    /// Not wired to a command variant yet; included for future
    /// PR-2k-d rotation flow.
    #[allow(dead_code)]
    pub fn remove(&self, id: &str) {
        if let Ok(mut guard) = self.inner.write() {
            guard.remove(id);
        }
    }

    pub fn resolve(&self, id: &str) -> Result<ResolvedCredential, ResolveError> {
        self.inner
            .read()
            .ok()
            .and_then(|g| g.get(id).cloned())
            .ok_or_else(|| ResolveError::UnknownId(id.to_string()))
    }

    /// Wave 2b — per-deployment allow-list enforcement at the
    /// catalog boundary. Refuses to surface a credential that is
    /// NOT in the deployment's `credentials` slice, even if the
    /// agent happens to hold it for another deployment.
    ///
    /// Rationale: the controller already gates pushes by
    /// `allowed_agents`, but once a credential lands on the
    /// agent the catalog is agent-global. A bug that lets
    /// `variables.primary_credential` name an id outside the
    /// deployment's declared allow-list would otherwise silently
    /// resolve — leaking tenant A's key into tenant B's runtime
    /// if both tenants happened to share this agent.
    ///
    /// Returns [`ResolveError::UnknownId`] on either cause
    /// (unknown OR outside allow-list) so callers treat the two
    /// the same: refuse the deployment as no-op.
    pub fn resolve_for(
        &self,
        id: &str,
        allowlist: &[String],
    ) -> Result<ResolvedCredential, ResolveError> {
        if !allowlist.iter().any(|a| a == id) {
            return Err(ResolveError::UnknownId(id.to_string()));
        }
        self.resolve(id)
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Normalise common variants of exchange names into the enum.
/// Matches the serde rename on ExchangeType so TOML values
/// people already know (binance / binance_testnet / bybit ...)
/// just work.
fn parse_exchange(raw: &str) -> Option<ExchangeType> {
    match raw.to_ascii_lowercase().as_str() {
        "binance" => Some(ExchangeType::Binance),
        "binance_testnet" | "binance-testnet" => Some(ExchangeType::BinanceTestnet),
        "bybit" => Some(ExchangeType::Bybit),
        "bybit_testnet" | "bybit-testnet" => Some(ExchangeType::BybitTestnet),
        "hyperliquid" => Some(ExchangeType::HyperLiquid),
        "hyperliquid_testnet" | "hyperliquid-testnet" => Some(ExchangeType::HyperLiquidTestnet),
        "custom" => Some(ExchangeType::Custom),
        _ => None,
    }
}

fn parse_product(raw: &str) -> Option<ProductType> {
    match raw.to_ascii_lowercase().as_str() {
        "spot" => Some(ProductType::Spot),
        "linear_perp" | "linear-perp" | "linearperp" => Some(ProductType::LinearPerp),
        "inverse_perp" | "inverse-perp" | "inverseperp" => Some(ProductType::InversePerp),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("unknown exchange: {0}")]
    BadExchange(String),
    #[error("unknown product: {0}")]
    BadProduct(String),
    #[error("bad decimal on credential payload: {0}")]
    BadDecimal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_settings() -> SettingsFile {
        SettingsFile::from_str(
            r#"
            [agent]
            id = "a"
            "#,
        )
        .unwrap()
    }

    fn sample_pushed() -> PushedCredential {
        PushedCredential {
            id: "binance_spot_main".into(),
            exchange: "binance".into(),
            product: "spot".into(),
            api_key: "live-key".into(),
            api_secret: "live-secret".into(),
            max_notional_quote: Some("5000".into()),
            default_symbol: Some("BTCUSDT".into()),
        }
    }

    #[test]
    fn fresh_catalog_is_empty() {
        let cat = CredentialCatalog::from_settings(sample_settings());
        assert!(cat.is_empty());
        assert_eq!(cat.settings().agent.id, "a");
    }

    #[test]
    fn insert_then_resolve_roundtrips() {
        let cat = CredentialCatalog::from_settings(sample_settings());
        cat.insert(sample_pushed()).unwrap();
        let r = cat.resolve("binance_spot_main").unwrap();
        assert_eq!(r.api_key, "live-key");
        assert_eq!(r.api_secret, "live-secret");
        assert_eq!(r.max_notional_quote, Some("5000".parse().unwrap()));
    }

    #[test]
    fn resolve_unknown_is_typed_error() {
        let cat = CredentialCatalog::from_settings(sample_settings());
        assert!(matches!(
            cat.resolve("absent"),
            Err(ResolveError::UnknownId(_))
        ));
    }

    #[test]
    fn insert_rejects_unknown_exchange_type() {
        let cat = CredentialCatalog::from_settings(sample_settings());
        let mut bad = sample_pushed();
        bad.exchange = "nonesuch".into();
        assert!(matches!(cat.insert(bad), Err(CredentialError::BadExchange(_))));
    }

    #[test]
    fn resolve_for_enforces_allowlist() {
        let cat = CredentialCatalog::from_settings(sample_settings());
        cat.insert(sample_pushed()).unwrap();
        let allow: Vec<String> = vec!["binance_spot_main".into()];
        // In allowlist → resolves.
        let r = cat.resolve_for("binance_spot_main", &allow).unwrap();
        assert_eq!(r.api_key, "live-key");
        // Not in allowlist → UnknownId even though the cred is
        // present. This is the Wave 2b cross-tenant guard.
        let empty: Vec<String> = Vec::new();
        assert!(matches!(
            cat.resolve_for("binance_spot_main", &empty),
            Err(ResolveError::UnknownId(_))
        ));
        // In allowlist but physically absent from the catalog →
        // still UnknownId, so callers can't distinguish the two
        // reasons and the no-op path covers both.
        let allow_other = vec!["not_pushed".to_string()];
        assert!(matches!(
            cat.resolve_for("not_pushed", &allow_other),
            Err(ResolveError::UnknownId(_))
        ));
    }

    #[test]
    fn insert_is_idempotent_on_same_id() {
        let cat = CredentialCatalog::from_settings(sample_settings());
        cat.insert(sample_pushed()).unwrap();
        let mut updated = sample_pushed();
        updated.api_key = "rotated".into();
        cat.insert(updated).unwrap();
        assert_eq!(cat.resolve("binance_spot_main").unwrap().api_key, "rotated");
    }

    #[test]
    fn remove_drops_entry() {
        let cat = CredentialCatalog::from_settings(sample_settings());
        cat.insert(sample_pushed()).unwrap();
        cat.remove("binance_spot_main");
        assert!(cat.is_empty());
    }
}
