use anyhow::Result;
use mm_common::config::AppConfig;
use std::path::Path;
use tracing::info;

/// Load config from TOML file, then override secrets from env vars.
///
/// Environment variables (override config file):
///   MM_CONFIG          — path to config file (default: config/default.toml)
///   MM_API_KEY         — exchange API key
///   MM_API_SECRET      — exchange API secret
///   MM_TELEGRAM_TOKEN  — Telegram bot token for alerts
///   MM_TELEGRAM_CHAT   — Telegram chat ID for alerts
///   MM_MODE            — "live" or "paper"
pub fn load_config() -> Result<AppConfig> {
    let config_path = std::env::var("MM_CONFIG").unwrap_or_else(|_| "config/default.toml".into());
    let path = Path::new(&config_path);

    let mut config = if path.exists() {
        info!(path = %config_path, "loading config from file");
        let contents = std::fs::read_to_string(path)?;
        toml::from_str(&contents)?
    } else {
        info!("no config file found, using defaults");
        AppConfig::default()
    };

    // Override secrets from env — NEVER store secrets in config files.
    if let Ok(key) = std::env::var("MM_API_KEY") {
        config.exchange.api_key = Some(key);
        info!("API key loaded from MM_API_KEY env var");
    }
    if let Ok(secret) = std::env::var("MM_API_SECRET") {
        config.exchange.api_secret = Some(secret);
        info!("API secret loaded from MM_API_SECRET env var");
    }
    // Optional read-only key pair (Epic 3). Lets operators
    // isolate the trading key (write, IP-whitelisted) from a
    // second key used for market data / balance polls / fee
    // tier lookups. Both keys are redacted in Debug output.
    if let Ok(key) = std::env::var("MM_READ_KEY") {
        config.exchange.read_key = Some(key);
        info!("read-only key loaded from MM_READ_KEY env var");
    }
    if let Ok(secret) = std::env::var("MM_READ_SECRET") {
        config.exchange.read_secret = Some(secret);
        info!("read-only secret loaded from MM_READ_SECRET env var");
    }
    if let Ok(mode) = std::env::var("MM_MODE") {
        config.mode = mode;
    }

    // Telegram alerts from env.
    if let Ok(token) = std::env::var("MM_TELEGRAM_TOKEN") {
        config.telegram.bot_token = token;
        info!("Telegram bot token loaded from MM_TELEGRAM_TOKEN");
    }
    if let Ok(chat) = std::env::var("MM_TELEGRAM_CHAT") {
        config.telegram.chat_id = chat;
        info!("Telegram chat ID loaded from MM_TELEGRAM_CHAT");
    }

    Ok(config)
}
