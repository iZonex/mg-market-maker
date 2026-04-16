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
use mm_exchange_core::events::MarketEvent;
use mm_persistence::checkpoint::CheckpointManager;
use mm_strategy::{AvellanedaStoikov, BasisStrategy, GlftStrategy, GridStrategy, Strategy};
use rust_decimal_macros::dec;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

mod config;
mod validate;

#[tokio::main]
async fn main() -> Result<()> {
    // Load config first (needed for log_file).
    let config = config::load_config()?;

    // Initialize logging.
    init_logging(&config);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Market Maker starting..."
    );
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

    // Shared multi-currency portfolio across all per-symbol
    // engines. Reporting currency = USDT by default; override
    // per-symbol FX factors inside individual strategies if you
    // quote in a different quote asset.
    let portfolio = Arc::new(std::sync::Mutex::new(mm_portfolio::Portfolio::new("USDT")));

    // P2.1 — build the shared per-asset-class kill switches
    // up-front so every engine that maps to the same class
    // receives the SAME `Arc<Mutex<KillSwitch>>` and a
    // coordinated escalation halts the whole class
    // simultaneously. Engines whose symbol does not appear in
    // any class get `None` and run with the global kill
    // switch only.
    let asset_class_switches: std::collections::HashMap<
        String,
        Arc<std::sync::Mutex<mm_risk::KillSwitch>>,
    > = config
        .kill_switch
        .asset_classes
        .iter()
        .map(|cfg| {
            let ks_cfg = mm_risk::kill_switch::KillSwitchConfig {
                daily_loss_limit: cfg.limits.daily_loss_limit,
                daily_loss_warning: cfg.limits.daily_loss_warning,
                max_position_value: cfg.limits.max_position_value,
                max_message_rate: cfg.limits.max_message_rate,
                max_consecutive_errors: cfg.limits.max_consecutive_errors,
                ..Default::default()
            };
            (
                cfg.name.clone(),
                Arc::new(std::sync::Mutex::new(mm_risk::KillSwitch::new(ks_cfg))),
            )
        })
        .collect();
    let symbol_to_class: std::collections::HashMap<String, String> = config
        .kill_switch
        .asset_classes
        .iter()
        .flat_map(|cfg| {
            let class = cfg.name.clone();
            cfg.symbols.iter().map(move |s| (s.clone(), class.clone()))
        })
        .collect();

    for symbol in &config.symbols {
        let symbol = symbol.clone();
        let config = config.clone();
        let connector = connector.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        let checkpoint = checkpoint.clone();
        let dashboard_state = dashboard_state.clone();
        let alerts = alert_manager.clone();
        let portfolio = portfolio.clone();
        let asset_class_switch = symbol_to_class
            .get(&symbol)
            .and_then(|c| asset_class_switches.get(c).cloned());

        let handle = tokio::spawn(async move {
            if let Err(e) = run_symbol(
                symbol.clone(),
                config,
                connector,
                shutdown_rx,
                checkpoint,
                dashboard_state,
                alerts,
                portfolio,
                asset_class_switch,
            )
            .await
            {
                error!(symbol = %symbol, error = %e, "market maker crashed");
            }
        });
        handles.push(handle);
    }

    // Listing sniper background task (Epic F stage-3).
    if config.listing_sniper.enabled {
        let sniper_connector = connector.clone();
        let sniper_shutdown = shutdown_tx.subscribe();
        let sniper_audit = Arc::new(
            mm_risk::audit::AuditLog::new(std::path::Path::new("data/audit.jsonl"))
                .expect("audit log for listing sniper"),
        );
        let sniper_alerts = Some(alert_manager.clone());
        let scan_secs = config.listing_sniper.scan_interval_secs;
        let alert_on_disc = config.listing_sniper.alert_on_discovery;
        tokio::spawn(async move {
            let runner = mm_engine::listing_sniper::ListingSniperRunner::new(
                vec![sniper_connector],
                sniper_audit,
                sniper_alerts,
                scan_secs,
                alert_on_disc,
            );
            runner.run(sniper_shutdown).await;
        });
        info!(
            scan_interval_secs = config.listing_sniper.scan_interval_secs,
            "listing sniper started"
        );
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

#[allow(clippy::too_many_arguments)]
async fn run_symbol(
    symbol: String,
    config: AppConfig,
    connector: Arc<dyn ExchangeConnector>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    _checkpoint: Arc<std::sync::Mutex<CheckpointManager>>,
    dashboard_state: DashboardState,
    alert_manager: AlertManager,
    portfolio: Arc<std::sync::Mutex<mm_portfolio::Portfolio>>,
    asset_class_switch: Option<Arc<std::sync::Mutex<mm_risk::KillSwitch>>>,
) -> Result<()> {
    let product = product_for_symbol(&symbol, &connector).await;

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
        StrategyType::Basis | StrategyType::FundingArb => {
            let shift = config.market_maker.basis_shift;
            let max_basis_bps = config
                .hedge
                .as_ref()
                .map(|h| h.pair.basis_threshold_bps)
                .unwrap_or(dec!(50));
            info!(
                symbol = %symbol,
                %shift,
                %max_basis_bps,
                kind = ?config.market_maker.strategy,
                "using Basis strategy (quoting leg) — requires hedge connector"
            );
            Box::new(BasisStrategy::new(shift, max_basis_bps))
        }
        StrategyType::CrossVenueBasis => {
            let shift = config.market_maker.basis_shift;
            let max_basis_bps = config
                .hedge
                .as_ref()
                .map(|h| h.pair.basis_threshold_bps)
                .unwrap_or(dec!(50));
            let stale_ms = config.market_maker.cross_venue_basis_max_staleness_ms;
            info!(
                symbol = %symbol,
                %shift,
                %max_basis_bps,
                stale_ms,
                "using CrossVenueBasis strategy — requires hedge connector on a different venue"
            );
            Box::new(BasisStrategy::cross_venue(shift, max_basis_bps, stale_ms))
        }
    };

    // Subscribe to market data via the connector. The public
    // `subscribe` task produces an `UnboundedReceiver` of
    // `MarketEvent`s — order-book snapshots, deltas, and public
    // trades. We merge it with the optional Binance user-data
    // stream (listen-key) so out-of-band fills and balance
    // updates arrive on the same channel the engine consumes
    // in its run loop.
    let public_rx = connector.subscribe(std::slice::from_ref(&symbol)).await?;
    let ws_rx = spawn_event_merger(public_rx, &config, &symbol);

    // Build the connector bundle: single-connector by default,
    // dual when `config.hedge` is set (basis / funding-arb modes).
    let (bundle, hedge_rx) = if let Some(hedge_cfg) = config.hedge.clone() {
        let hedge_conn = create_hedge_connector(&hedge_cfg.exchange)?;
        let hedge_symbol = hedge_cfg.pair.hedge_symbol.clone();
        let hedge_rx = hedge_conn
            .subscribe(std::slice::from_ref(&hedge_symbol))
            .await?;
        let pair = mm_common::types::InstrumentPair::from(hedge_cfg.pair);
        info!(
            primary = %symbol,
            hedge = %pair.hedge_symbol,
            "dual-connector bundle with hedge leg"
        );
        (
            mm_engine::ConnectorBundle::dual(connector, hedge_conn, pair),
            Some(hedge_rx),
        )
    } else {
        (mm_engine::ConnectorBundle::single(connector), None)
    };

    // If the operator selected FundingArb, build the driver
    // from the `funding_arb` config section and inject it into
    // the engine. The engine's run loop picks up the periodic
    // tick + event routing.
    let funding_arb_wiring =
        if matches!(config.market_maker.strategy, StrategyType::FundingArb) {
            let cfg = config.funding_arb.clone().ok_or_else(|| {
                anyhow::anyhow!("strategy=funding_arb requires [funding_arb] section in config")
            })?;
            if !cfg.enabled {
                warn!("funding_arb.enabled=false — driver wired but signals disabled");
            }
            let hedge_conn = bundle.hedge.clone().ok_or_else(|| {
                anyhow::anyhow!("strategy=funding_arb requires a hedge connector")
            })?;
            let pair = bundle.pair.clone().ok_or_else(|| {
                anyhow::anyhow!("strategy=funding_arb requires an instrument pair")
            })?;
            let driver = mm_strategy::FundingArbDriver::new(
                bundle.primary.clone(),
                hedge_conn,
                pair,
                mm_strategy::FundingArbDriverConfig {
                    tick_interval: std::time::Duration::from_secs(cfg.tick_interval_secs),
                    engine: mm_persistence::funding::FundingArbConfig {
                        min_rate_annual_pct: cfg.min_rate_annual_pct,
                        max_position: cfg.max_position,
                        max_basis_bps: cfg.max_basis_bps,
                        enabled: cfg.enabled,
                    },
                },
                Arc::new(mm_strategy::NullSink),
            );
            Some((
                driver,
                std::time::Duration::from_secs(cfg.tick_interval_secs),
            ))
        } else {
            None
        };

    let mut engine_builder = MarketMakerEngine::new(
        symbol,
        config,
        product,
        strategy,
        bundle,
        Some(dashboard_state),
        Some(alert_manager),
    )
    .with_portfolio(portfolio);
    if let Some(arc) = asset_class_switch {
        engine_builder = engine_builder.with_asset_class_switch(arc);
    }

    let mut engine = match funding_arb_wiring {
        Some((driver, tick)) => engine_builder.with_funding_arb_driver(driver, tick),
        None => engine_builder,
    };
    engine.run_with_hedge(ws_rx, hedge_rx, shutdown_rx).await
}

/// Build a hedge-leg connector from its `ExchangeConfig`. Kept
/// separate from `create_connector` so the primary and hedge
/// paths can evolve independently — cross-venue basis trades
/// (Binance spot vs HyperLiquid perps) will land here.
fn create_hedge_connector(
    cfg: &mm_common::config::ExchangeConfig,
) -> Result<Arc<dyn ExchangeConnector>> {
    let api_key = cfg.api_key.clone().unwrap_or_default();
    let api_secret = cfg.api_secret.clone().unwrap_or_default();

    match cfg.exchange_type {
        ExchangeType::Custom => Ok(Arc::new(mm_exchange_client::CustomConnector::new(
            &cfg.rest_url,
            &cfg.ws_url,
        ))),
        ExchangeType::Binance | ExchangeType::BinanceTestnet => {
            // Hedge-leg connector defaults to Binance USDⓈ-M
            // futures — the spot connector is already the
            // typical primary. Sprint I hooks this up via a
            // product-aware selector.
            Ok(Arc::new(mm_exchange_binance::BinanceFuturesConnector::new(
                &api_key,
                &api_secret,
            )))
        }
        ExchangeType::Bybit | ExchangeType::BybitTestnet => Ok(Arc::new(
            mm_exchange_bybit::BybitConnector::linear(&api_key, &api_secret),
        )),
        ExchangeType::HyperLiquid => Ok(Arc::new(
            mm_exchange_hyperliquid::HyperLiquidConnector::new(&api_secret)?,
        )),
        ExchangeType::HyperLiquidTestnet => Ok(Arc::new(
            mm_exchange_hyperliquid::HyperLiquidConnector::testnet(&api_secret)?,
        )),
    }
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
        ExchangeType::HyperLiquid => {
            info!("connecting to HyperLiquid");
            // For HL: api_secret holds the hex-encoded wallet private key.
            // api_key is unused — the address is derived from the private key.
            Ok(Arc::new(
                mm_exchange_hyperliquid::HyperLiquidConnector::new(&api_secret)?,
            ))
        }
        ExchangeType::HyperLiquidTestnet => {
            info!("connecting to HyperLiquid Testnet");
            Ok(Arc::new(
                mm_exchange_hyperliquid::HyperLiquidConnector::testnet(&api_secret)?,
            ))
        }
    }
}

/// Spawn a background task that forwards events from the
/// connector's public feed into a new merged channel. When the
/// configured venue has a private user-data stream and
/// `user_stream_enabled` is on, this also spawns that venue's
/// stream task and points it at the same merged channel — so
/// out-of-band fills and balance updates arrive on the exact
/// path the engine already knows how to consume.
///
/// Currently wired: Binance spot/futures (listen-key), Bybit V5
/// (private WS auth). HyperLiquid `userEvents` is tracked under
/// `ROADMAP.md` P0.1.
fn spawn_event_merger(
    mut public_rx: mpsc::UnboundedReceiver<MarketEvent>,
    config: &AppConfig,
    symbol: &str,
) -> mpsc::UnboundedReceiver<MarketEvent> {
    let (merged_tx, merged_rx) = mpsc::unbounded_channel::<MarketEvent>();
    let forward_tx = merged_tx.clone();
    // Forwarder: public ws_rx → merged channel.
    tokio::spawn(async move {
        while let Some(ev) = public_rx.recv().await {
            if forward_tx.send(ev).is_err() {
                return;
            }
        }
    });

    if !config.market_maker.user_stream_enabled {
        return merged_rx;
    }
    let api_key = config.exchange.api_key.clone().unwrap_or_default();
    let api_secret = config.exchange.api_secret.clone().unwrap_or_default();
    if api_key.is_empty() {
        return merged_rx;
    }

    match config.exchange.exchange_type {
        ExchangeType::Binance => {
            let cfg = mm_exchange_binance::UserStreamConfig::spot(&api_key);
            info!(
                symbol = symbol,
                "starting Binance listen-key user-data stream"
            );
            let _handle = mm_exchange_binance::user_stream::start(cfg, merged_tx);
        }
        ExchangeType::BinanceTestnet => {
            let mut cfg = mm_exchange_binance::UserStreamConfig::spot(&api_key);
            cfg.rest_base = "https://testnet.binance.vision".into();
            cfg.ws_host = "wss://testnet.binance.vision".into();
            info!(
                symbol = symbol,
                "starting Binance testnet listen-key user-data stream"
            );
            let _handle = mm_exchange_binance::user_stream::start(cfg, merged_tx);
        }
        ExchangeType::Bybit if !api_secret.is_empty() => {
            let cfg = mm_exchange_bybit::UserStreamConfig::mainnet(&api_key, &api_secret);
            info!(symbol = symbol, "starting Bybit V5 private WS user stream");
            let _handle = mm_exchange_bybit::user_stream::start(cfg, merged_tx);
        }
        ExchangeType::BybitTestnet if !api_secret.is_empty() => {
            let cfg = mm_exchange_bybit::UserStreamConfig::testnet(&api_key, &api_secret);
            info!(
                symbol = symbol,
                "starting Bybit V5 testnet private WS user stream"
            );
            let _handle = mm_exchange_bybit::user_stream::start(cfg, merged_tx);
        }
        _ => {}
    }
    merged_rx
}

/// Fetch product spec from the connector. Falls back to a
/// conservative default if the venue doesn't support
/// `get_product_spec` — the fee-tier refresh task will
/// overwrite these on its first tick.
async fn product_for_symbol(
    symbol: &str,
    connector: &Arc<dyn ExchangeConnector>,
) -> ProductSpec {
    match connector.get_product_spec(symbol).await {
        Ok(spec) => {
            info!(symbol, tick = %spec.tick_size, lot = %spec.lot_size, "loaded product spec from venue");
            spec
        }
        Err(e) => {
            warn!(symbol, error = %e, "get_product_spec failed — using conservative defaults");
            product_fallback(symbol)
        }
    }
}

/// Conservative fallback specs for common symbols. Used when
/// the venue doesn't support `get_product_spec`. Safe to trade
/// against — tick/lot sizes are the largest (most conservative)
/// values across known venues, and fees are set at retail-tier
/// maximums.
fn product_fallback(symbol: &str) -> ProductSpec {
    // Try to split symbol into base/quote by known suffixes.
    let (base, quote) = split_base_quote(symbol);
    ProductSpec {
        symbol: symbol.to_string(),
        base_asset: base,
        quote_asset: quote,
        tick_size: dec!(0.01),
        lot_size: dec!(0.001),
        min_notional: dec!(10),
        maker_fee: dec!(0.001),
        taker_fee: dec!(0.002),
        trading_status: Default::default(),
    }
}

fn split_base_quote(symbol: &str) -> (String, String) {
    for suffix in ["USDT", "USDC", "BUSD", "FDUSD", "TUSD", "DAI", "BTC", "ETH"] {
        if let Some(base) = symbol.strip_suffix(suffix) {
            return (base.to_string(), suffix.to_string());
        }
    }
    (symbol.to_string(), "USDT".to_string())
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
