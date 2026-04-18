use anyhow::Result;
use mm_common::config::{AppConfig, ExchangeType};
use std::path::Path;
use tracing::info;

/// Load config from TOML file, then override secrets from env vars.
///
/// Environment variables (override config file):
///   MM_CONFIG            — path to config file (default: config/default.toml)
///   MM_BINANCE_API_KEY  / MM_BINANCE_API_SECRET
///   MM_BYBIT_API_KEY    / MM_BYBIT_API_SECRET
///   MM_HL_API_SECRET    (HL has no api_key; the address is derived)
///   MM_READ_KEY / MM_READ_SECRET — optional read-only key pair
///   MM_TELEGRAM_TOKEN, MM_TELEGRAM_CHAT
///   MM_MODE              — "live" or "paper"
///
/// The venue-scoped vars let a single `.env` carry keys for all
/// three supported venues simultaneously; the loader picks the
/// right pair based on `config.exchange.exchange_type` at startup.
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

    // Venue-scoped env vars — the only supported path. A single
    // `.env` can carry MM_BINANCE_* / MM_BYBIT_* / MM_HL_*
    // simultaneously; the loader picks the pair that matches
    // `config.exchange.exchange_type` at startup.
    let venue_prefix = match config.exchange.exchange_type {
        ExchangeType::Binance | ExchangeType::BinanceTestnet => Some("BINANCE"),
        ExchangeType::Bybit | ExchangeType::BybitTestnet => Some("BYBIT"),
        ExchangeType::HyperLiquid | ExchangeType::HyperLiquidTestnet => Some("HL"),
        ExchangeType::Custom => None,
    };

    if let Some(prefix) = venue_prefix {
        let key_var = format!("MM_{prefix}_API_KEY");
        let secret_var = format!("MM_{prefix}_API_SECRET");
        if let Ok(key) = std::env::var(&key_var) {
            config.exchange.api_key = Some(key);
            info!(%key_var, "API key loaded from venue-scoped env var");
        }
        if let Ok(secret) = std::env::var(&secret_var) {
            config.exchange.api_secret = Some(secret);
            info!(%secret_var, "API secret loaded from venue-scoped env var");
        }
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
