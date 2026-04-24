//! `mm-agent` binary entry point.
//!
//! Loads settings, resolves the controller address (env override →
//! settings field → hard error), and drives
//! [`run_with_reconnect`] which owns the control-plane session
//! lifecycle including transparent WS-RPC reconnect. Ctrl-C
//! triggers graceful shutdown via a `watch` channel — the
//! reconnect loop exits at the top of its next iteration.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use mm_agent::{default_registry_builder, run_with_reconnect, CredentialCatalog, ReconnectConfig};
use mm_common::settings::SettingsFile;
use mm_control::identity::IdentityKey;
use mm_control::messages::AgentId;
use tracing_subscriber::EnvFilter;

const DEFAULT_SETTINGS_PATH: &str = "settings.toml";
const SETTINGS_ENV: &str = "MM_SETTINGS";
const BRAIN_ADDR_ENV: &str = "MM_BRAIN_WS_ADDR";
/// Path to the agent's Ed25519 identity seed. Defaults to
/// `agent-identity.key` in the CWD; operators override via env.
/// First start: the file is generated with 0600 perms. Copy
/// the advertised fingerprint to the controller's approval flow
/// to accept the new agent.
const IDENTITY_ENV: &str = "MM_AGENT_IDENTITY";
const DEFAULT_IDENTITY_PATH: &str = "agent-identity.key";

#[tokio::main]
async fn main() -> Result<()> {
    // rustls 0.23 needs an explicit CryptoProvider when more
    // than one provider is in the dep graph (aws-lc-rs from
    // aws-smithy + ring from reqwest). Without this the first
    // WSS handshake panics with "could not automatically
    // determine the process-level CryptoProvider". Same install
    // the server already does — agent needs it too for venue
    // WSS (testnet.binance.vision, api.bybit.com, …).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let settings_path: PathBuf = std::env::var(SETTINGS_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_SETTINGS_PATH));

    let settings = SettingsFile::load_from_path(&settings_path)
        .with_context(|| format!("loading settings from {}", settings_path.display()))?;

    let agent_id = AgentId::new(settings.agent.id.clone());
    let controller_addr = std::env::var(BRAIN_ADDR_ENV)
        .ok()
        .or_else(|| settings.agent.controller_addr.clone())
        .ok_or_else(|| {
            anyhow!(
                "no controller address configured — set {BRAIN_ADDR_ENV} env \
                 or [agent].controller_addr in settings"
            )
        })?;

    // Trading authority = lease + credentials + deployed
    // strategies, all of which flow from the controller's
    // admission-control surface. The agent has no separate
    // "subscribe-only" mode — if nothing is deployed, nothing
    // trades; if strategies are deployed with paper / live
    // modes, the strategy config decides. No env flag needed.

    let identity_path: PathBuf = std::env::var(IDENTITY_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_IDENTITY_PATH));
    let identity = load_or_generate_identity(&identity_path)
        .with_context(|| format!("agent identity @ {}", identity_path.display()))?;
    let fingerprint = identity.public().fingerprint();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        protocol = mm_control::PROTOCOL_VERSION,
        agent = %settings.agent.id,
        credentials = settings.credentials.len(),
        controller_addr = %controller_addr,
        identity = %identity_path.display(),
        fingerprint = %fingerprint,
        "mm-agent starting — operator accepts the fingerprint above on the controller's Fleet UI"
    );

    let catalog = Arc::new(CredentialCatalog::from_settings(settings));
    // Shared in-memory DashboardState — engines write their
    // operator-facing panel state (atomic bundles, funding-arb,
    // SOR decisions, rebalance advisories) into it; the
    // FetchDetails handler reads back via agent-held handle.
    let dashboard = mm_dashboard::state::DashboardState::new();
    let build_registry = default_registry_builder(Arc::clone(&catalog), dashboard.clone());

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_handle = shutdown_tx.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("SIGINT received — initiating graceful shutdown");
            let _ = shutdown_handle.send(true);
        }
    });

    let cfg = ReconnectConfig::new(
        controller_addr,
        agent_id,
        build_registry,
        Arc::clone(&catalog),
    )
    .with_identity(identity)
    .with_dashboard(dashboard);
    run_with_reconnect(cfg, shutdown_rx).await?;
    Ok(())
}

fn load_or_generate_identity(path: &PathBuf) -> Result<IdentityKey> {
    if path.exists() {
        return IdentityKey::load_from_file(path).map_err(|e| anyhow!("load identity: {e}"));
    }
    let key = IdentityKey::generate();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create identity parent dir {}", parent.display()))?;
        }
    }
    key.save_to_file(path)
        .map_err(|e| anyhow!("save identity: {e}"))?;
    // Best-effort 0600 on Unix — non-fatal if it fails (Windows
    // + weird filesystems are out of scope for this path).
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
        "generated fresh agent identity key — persist this file, losing it requires operator re-accept"
    );
    Ok(key)
}
