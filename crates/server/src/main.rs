use std::sync::Arc;

use anyhow::Result;
use mm_common::config::{AppConfig, ExchangeType, StrategyType};
use mm_common::types::ProductSpec;
use mm_dashboard::alerts::{AlertManager, TelegramConfig};
use mm_dashboard::auth::{ApiUser, AuthState, Role};
use mm_dashboard::state::DashboardState;
use mm_dashboard::websocket::WsBroadcast;
use mm_engine::MarketMakerEngine;
use mm_exchange_core::connector::ExchangeConnector;
use mm_persistence::checkpoint::CheckpointManager;
use mm_strategy::{AvellanedaStoikov, GlftStrategy, GridStrategy, Strategy};
use rust_decimal_macros::dec;
use tracing::{error, info, warn};

mod config;
mod validate;

#[tokio::main]
async fn main() -> Result<()> {
    // Load config first (needed for log_file).
    let config = config::load_config()?;

    // Initialize logging.
    init_logging(&config);

    info!("Market Maker v0.2.0 starting...");
    info!(
        symbols = ?config.symbols,
        strategy = ?config.market_maker.strategy,
        mode = %config.mode,
        "config loaded"
    );

    // Validate config.
    validate::validate_config(&config)?;
    info!("config validation passed");

    // Initialize checkpoint manager.
    let checkpoint_path = std::path::Path::new(&config.checkpoint_path);
    if let Some(parent) = checkpoint_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let checkpoint = Arc::new(std::sync::Mutex::new(CheckpointManager::new(
        checkpoint_path,
        10,
    )));

    // Check mode.
    if config.mode == "paper" {
        info!("PAPER TRADING MODE — no real orders will be sent");
    }

    // Build exchange connector.
    let connector: Arc<dyn ExchangeConnector> = create_connector(&config)?;
    info!(
        exchange = ?config.exchange.exchange_type,
        "exchange connector created"
    );

    // Health check.
    match connector.health_check().await {
        Ok(true) => info!("exchange health check OK"),
        Ok(false) => error!("exchange health check failed"),
        Err(e) => {
            if config.mode == "live" {
                error!(error = %e, "cannot reach exchange — aborting");
                anyhow::bail!("exchange unreachable");
            } else {
                warn!(error = %e, "exchange unreachable (paper mode, continuing)");
            }
        }
    }

    // Auth: create state and load users from config.
    let auth_secret =
        std::env::var("MM_AUTH_SECRET").unwrap_or_else(|_| "change-me-in-production".to_string());
    let auth_state = AuthState::new(&auth_secret);

    // Load pre-configured users.
    for u in &config.users {
        let role = match u.role.as_str() {
            "admin" => Role::Admin,
            "operator" => Role::Operator,
            _ => Role::Viewer,
        };
        auth_state.add_user(ApiUser {
            id: u.id.clone(),
            name: u.name.clone(),
            role,
            api_key: u.api_key.clone(),
            allowed_symbols: if u.allowed_symbols.is_empty() {
                None
            } else {
                Some(u.allowed_symbols.clone())
            },
        });
        info!(name = %u.name, role = %u.role, "user loaded");
    }

    // If no users configured, create a default admin.
    if config.users.is_empty() {
        let default_key =
            std::env::var("MM_ADMIN_KEY").unwrap_or_else(|_| "admin-key-change-me".to_string());
        auth_state.add_user(ApiUser {
            id: "default-admin".to_string(),
            name: "Admin".to_string(),
            role: Role::Admin,
            api_key: default_key.clone(),
            allowed_symbols: None,
        });
        info!(key_hint = %&default_key[..8], "default admin user created (set MM_ADMIN_KEY to customize)");
    }

    // Telegram alerts.
    let telegram_config = if config.telegram.is_configured() {
        info!("Telegram alerts enabled");
        Some(TelegramConfig {
            bot_token: config.telegram.bot_token.clone(),
            chat_id: config.telegram.chat_id.clone(),
        })
    } else {
        info!("Telegram alerts disabled (set MM_TELEGRAM_TOKEN + MM_TELEGRAM_CHAT to enable)");
        None
    };
    let alert_manager = AlertManager::new(telegram_config);

    // Start dashboard.
    let dashboard_state = DashboardState::new();
    dashboard_state.set_loans(config.loans.clone());
    let ws_broadcast = Arc::new(WsBroadcast::new(1024));
    if config.dashboard_port > 0 {
        let ds = dashboard_state.clone();
        let wsb = ws_broadcast.clone();
        let auth = auth_state.clone();
        let port = config.dashboard_port;
        tokio::spawn(async move {
            if let Err(e) = mm_dashboard::server::start(ds, wsb, auth, port).await {
                error!(error = %e, "dashboard server failed");
            }
        });
        info!(
            port = config.dashboard_port,
            "dashboard + WebSocket + auth started"
        );
    }

    // Shutdown signal.
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let mut handles = Vec::new();

    for symbol in &config.symbols {
        let symbol = symbol.clone();
        let config = config.clone();
        let connector = connector.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        let checkpoint = checkpoint.clone();
        let dashboard_state = dashboard_state.clone();
        let alerts = alert_manager.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_symbol(
                symbol.clone(),
                config,
                connector,
                shutdown_rx,
                checkpoint,
                dashboard_state,
                alerts,
            )
            .await
            {
                error!(symbol = %symbol, error = %e, "market maker crashed");
            }
        });
        handles.push(handle);
    }

    // Wait for Ctrl+C.
    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received — cancelling all orders");
    let _ = shutdown_tx.send(true);

    for handle in handles {
        let _ = handle.await;
    }

    // Final checkpoint flush.
    if let Ok(cp) = checkpoint.lock() {
        let _ = cp.flush();
    }

    info!("all engines shut down cleanly");
    Ok(())
}

async fn run_symbol(
    symbol: String,
    config: AppConfig,
    connector: Arc<dyn ExchangeConnector>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    _checkpoint: Arc<std::sync::Mutex<CheckpointManager>>,
    dashboard_state: DashboardState,
    alert_manager: AlertManager,
) -> Result<()> {
    let product = product_for_symbol(&symbol);

    let strategy: Box<dyn Strategy> = match config.market_maker.strategy {
        StrategyType::AvellanedaStoikov => {
            info!(symbol = %symbol, "using Avellaneda-Stoikov strategy");
            Box::new(AvellanedaStoikov)
        }
        StrategyType::Glft => {
            info!(symbol = %symbol, "using GLFT strategy");
            Box::new(GlftStrategy::new())
        }
        StrategyType::Grid => {
            info!(symbol = %symbol, "using Grid strategy");
            Box::new(GridStrategy)
        }
    };

    // Subscribe to market data via the connector.
    let ws_rx = connector.subscribe(std::slice::from_ref(&symbol)).await?;

    let mut engine = MarketMakerEngine::new(
        symbol,
        config,
        product,
        strategy,
        connector,
        Some(dashboard_state),
        Some(alert_manager),
    );
    engine.run(ws_rx, shutdown_rx).await
}

/// Create the exchange connector based on config.
fn create_connector(config: &AppConfig) -> Result<Arc<dyn ExchangeConnector>> {
    let api_key = config.exchange.api_key.clone().unwrap_or_default();
    let api_secret = config.exchange.api_secret.clone().unwrap_or_default();

    match config.exchange.exchange_type {
        ExchangeType::Custom => {
            info!(
                rest_url = %config.exchange.rest_url,
                ws_url = %config.exchange.ws_url,
                "connecting to custom exchange"
            );
            Ok(Arc::new(mm_exchange_client::CustomConnector::new(
                &config.exchange.rest_url,
                &config.exchange.ws_url,
            )))
        }
        ExchangeType::Binance => {
            info!("connecting to Binance");
            Ok(Arc::new(mm_exchange_binance::BinanceConnector::new(
                "https://api.binance.com",
                "wss://stream.binance.com:9443/ws",
                &api_key,
                &api_secret,
            )))
        }
        ExchangeType::BinanceTestnet => {
            info!("connecting to Binance Testnet");
            Ok(Arc::new(mm_exchange_binance::BinanceConnector::testnet(
                &api_key,
                &api_secret,
            )))
        }
        ExchangeType::Bybit => {
            info!("connecting to Bybit");
            Ok(Arc::new(mm_exchange_bybit::BybitConnector::new(
                &api_key,
                &api_secret,
            )))
        }
        ExchangeType::BybitTestnet => {
            info!("connecting to Bybit Testnet");
            Ok(Arc::new(mm_exchange_bybit::BybitConnector::testnet(
                &api_key,
                &api_secret,
            )))
        }
    }
}

fn product_for_symbol(symbol: &str) -> ProductSpec {
    match symbol {
        "BTCUSDT" => ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
        },
        "ETHUSDT" => ProductSpec {
            symbol: "ETHUSDT".into(),
            base_asset: "ETH".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
        },
        "SOLUSDT" => ProductSpec {
            symbol: "SOLUSDT".into(),
            base_asset: "SOL".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.001),
            lot_size: dec!(0.01),
            min_notional: dec!(5),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
        },
        _ => panic!("unknown symbol: {symbol}"),
    }
}

fn init_logging(config: &AppConfig) {
    use tracing_subscriber::prelude::*;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,mm_engine=debug,mm_strategy=debug".into());

    if config.log_file.is_empty() {
        // Stdout only.
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        // Stdout + file with rotation.
        let log_dir = std::path::Path::new(&config.log_file)
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let log_name = std::path::Path::new(&config.log_file)
            .file_name()
            .unwrap_or(std::ffi::OsStr::new("mm.log"));

        let file_appender = tracing_appender::rolling::daily(log_dir, log_name);
        let (file_writer, _guard) = tracing_appender::non_blocking(file_appender);
        // Leak the guard so it lives for the program duration.
        std::mem::forget(_guard);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(file_writer),
            )
            .init();

        info!(path = %config.log_file, "file logging enabled");
    }
}
