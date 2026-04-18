use std::sync::Arc;

use anyhow::Result;
use mm_common::config::{AppConfig, ExchangeType, ProductType, StrategyType};
use mm_common::types::ProductSpec;
use mm_dashboard::alerts::{AlertManager, TelegramConfig};
use mm_dashboard::auth::{ApiUser, AuthState, Role};
use mm_dashboard::state::DashboardState;
use mm_dashboard::websocket::WsBroadcast;
use mm_engine::MarketMakerEngine;
use mm_exchange_core::connector::ExchangeConnector;
use mm_exchange_core::events::MarketEvent;
use mm_persistence::checkpoint::CheckpointManager;
use mm_strategy::{
    AvellanedaStoikov, BasisStrategy, CrossExchangeStrategy, GlftStrategy, GridStrategy, Strategy,
};
use rust_decimal_macros::dec;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

mod config;
mod hyperopt_worker;
mod otel;
mod pair_template;
mod preflight;
mod smoke_test;
mod validate;

#[tokio::main]
async fn main() -> Result<()> {
    // Load config first (needed for log_file).
    let config = config::load_config()?;

    // Initialize logging + Sentry. The guard MUST live for the
    // program's duration so in-flight Sentry events flush on
    // exit; we hold it in `main` and drop it on return.
    // `_otel_guard` flushes OTLP spans on drop when built with
    // `--features otel`; in the default build it is a zero-sized
    // marker.
    let (_sentry_guard, _otel_guard) = init_logging(&config);

    // Startup banner — one glance tells the operator what they
    // are about to run against. Real keys + live mode show up in
    // the same log line as the venue, so a paste in a bug report
    // answers "what deployment hit this" immediately.
    let exchange = format!("{:?}", config.exchange.exchange_type);
    let api_key_hint = config
        .exchange
        .api_key
        .as_deref()
        .unwrap_or("")
        .chars()
        .take(6)
        .collect::<String>();
    let mode_label = match config.mode.as_str() {
        "paper" => "PAPER (simulated order egress)",
        "live" => "LIVE (REAL ORDERS)",
        "smoke" => "SMOKE (single place/cancel test)",
        other => other,
    };
    let key_env_name = match config.exchange.exchange_type {
        ExchangeType::Binance | ExchangeType::BinanceTestnet => "MM_BINANCE_API_KEY",
        ExchangeType::Bybit | ExchangeType::BybitTestnet => "MM_BYBIT_API_KEY",
        ExchangeType::HyperLiquid | ExchangeType::HyperLiquidTestnet => "MM_HL_API_SECRET",
        ExchangeType::Custom => "(custom)",
    };
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    info!(version = env!("CARGO_PKG_VERSION"), "Market Maker");
    info!("  mode     : {mode_label}");
    info!("  exchange : {exchange}");
    info!("  symbols  : {:?}", config.symbols);
    info!("  strategy : {:?}", config.market_maker.strategy);
    info!(
        "  api_key  : {}",
        if api_key_hint.is_empty() {
            "(unset — public endpoints only)".into()
        } else {
            format!("{api_key_hint}… (from {key_env_name})")
        }
    );
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

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
        info!(
            "PAPER MODE active — OrderManager and BalanceCache are built with \
             paper gates. No place / cancel / amend / withdraw call ever reaches \
             the venue connector. Paper fills are synthesised from public trades."
        );
    } else if config.mode == "live" {
        warn!(
            "LIVE MODE — real orders will be submitted. Make sure the config \
             has calibrated strategy params and realistic risk / kill-switch \
             limits. `MM_MODE=paper` gives a safe dry-run on the same feed."
        );
    }

    // Build exchange connector.
    let connector: Arc<dyn ExchangeConnector> = create_connector(&config)?;
    info!(
        exchange = ?config.exchange.exchange_type,
        "exchange connector created"
    );

    // Smoke test mode — validate connector, place/cancel test
    // order, fetch balances, print report, exit.
    if config.mode == "smoke" {
        info!("SMOKE TEST MODE — validating connector stack");
        let smoke_symbol = config.symbols.first().cloned().unwrap_or("BTCUSDT".into());
        smoke_test::run_smoke_test(&connector, &smoke_symbol).await?;
        return Ok(());
    }

    // Epic 40.10 — jurisdiction gate at startup. If ANY client is
    // tagged US and the engine's product is a perp, fail closed
    // before preflight. Operator must split the config (US clients
    // on a spot engine, non-US clients on a perp engine) rather
    // than relying on runtime checks. Cheaper to catch here than
    // to discover at first order.
    {
        let perp = config.exchange.product.has_funding();
        if perp {
            for c in config.effective_clients() {
                if !c.allows_product(config.exchange.product) {
                    anyhow::bail!(
                        "client `{}` jurisdiction `{}` is not permitted to trade \
                         product `{}`. Move this client to a spot engine or drop \
                         their jurisdiction tag (Epic 40.10).",
                        c.id,
                        c.jurisdiction,
                        config.exchange.product.label()
                    );
                }
            }
        }
    }

    // Pre-flight checks — validate venue, symbols, balances,
    // config sanity before starting engines.
    let effective_symbols: Vec<String> = config
        .effective_clients()
        .iter()
        .flat_map(|c| c.symbols.clone())
        .collect();
    let preflight_symbols = if effective_symbols.is_empty() {
        config.symbols.clone()
    } else {
        effective_symbols
    };
    match preflight::run_preflight(&config, &connector, &preflight_symbols).await {
        Ok(_results) => info!("preflight checks passed"),
        Err(e) => {
            if config.mode == "live" {
                error!(error = %e, "preflight failed — aborting");
                anyhow::bail!("preflight failed: {e}");
            } else {
                warn!(error = %e, "preflight failed (paper mode, continuing)");
            }
        }
    }

    // Epic 40.7 — set per-symbol margin mode + leverage on the
    // venue before the engine places its first order. A live
    // account on the wrong mode is an unacceptable state: cross
    // without a hedge is a liquidation-cascade risk, and
    // mis-leveraged positions drift the IM/MM arithmetic the
    // margin guard relies on. Hard-fail on anything other than
    // `Ok(())` or `NotSupported`; paper mode downgrades the
    // hard-fail to a warn so operators can still dry-run
    // against the real wire without the account being
    // pre-configured.
    if let Some(margin_cfg) = &config.margin {
        if config.exchange.product.has_funding() {
            use mm_exchange_core::connector::{MarginError, MarginMode};
            for sym in &preflight_symbols {
                let (mode_cfg, leverage) = margin_cfg.for_symbol(sym);
                let mode = match mode_cfg {
                    mm_common::config::MarginModeCfg::Isolated => MarginMode::Isolated,
                    mm_common::config::MarginModeCfg::Cross => MarginMode::Cross,
                };
                match connector.set_margin_mode(sym, mode).await {
                    Ok(()) | Err(MarginError::NotSupported) => {}
                    Err(e) => {
                        let msg = format!(
                            "set_margin_mode({sym}, {mode:?}) failed: {e}"
                        );
                        if config.mode == "live" {
                            error!(error = %msg, "margin mode setup failed — aborting");
                            anyhow::bail!(msg);
                        }
                        warn!(error = %msg, "margin mode setup failed (paper mode, continuing)");
                    }
                }
                match connector.set_leverage(sym, leverage).await {
                    Ok(()) | Err(MarginError::NotSupported) => {}
                    Err(e) => {
                        let msg = format!(
                            "set_leverage({sym}, {leverage}) failed: {e}"
                        );
                        if config.mode == "live" {
                            error!(error = %msg, "leverage setup failed — aborting");
                            anyhow::bail!(msg);
                        }
                        warn!(error = %msg, "leverage setup failed (paper mode, continuing)");
                    }
                }
                info!(
                    symbol = %sym,
                    mode = %mode.as_str(),
                    leverage,
                    "margin mode + leverage configured"
                );
            }
        }
    }

    // Auth: create state and load users from config.
    let auth_secret =
        std::env::var("MM_AUTH_SECRET").unwrap_or_else(|_| "change-me-in-production".to_string());
    // Shared audit sink — attached to AuthState so login /
    // logout events land in the MiCA trail next to order + risk
    // rows (Epic 38).
    let shared_audit = Arc::new(
        mm_risk::audit::AuditLog::new(std::path::Path::new("data/audit.jsonl"))
            .expect("audit log for auth"),
    );
    let auth_state = AuthState::new(&auth_secret).with_audit(shared_audit.clone());

    // Load pre-configured users.
    for u in &config.users {
        let role = match u.role.as_str() {
            "admin" => Role::Admin,
            "operator" => Role::Operator,
            _ => Role::Viewer,
        };
        // Resolve client_id from ClientConfig.api_keys.
        let client_id = config
            .clients
            .iter()
            .find(|c| c.api_keys.contains(&u.api_key))
            .map(|c| c.id.clone());
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
            client_id,
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
            client_id: None,
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
    // Epic 40.10 — seed the engine product so admin_clients POST
    // can gate US-jurisdiction clients at ingress time.
    dashboard_state.set_engine_product(config.exchange.product);
    dashboard_state.set_loans(config.loans.clone());
    // UX-5 — publish the effective AppConfig so operators can
    // inspect which features are configured vs on defaults from
    // the dashboard. Secrets live in env, not in `AppConfig`.
    dashboard_state.set_app_config(std::sync::Arc::new(config.clone()));
    // A1 — expose the hash-chained audit log path to the
    // monthly-report aggregator. Keep in sync with the
    // `AuditLog::new` path used when the engine initialises
    // its risk subsystem.
    dashboard_state.set_audit_log_path(std::path::PathBuf::from("data/audit.jsonl"));
    // Epic H Phase 3 — shared AuditLog instance for the dashboard's
    // deploy / rollback / reject rows. Reuses the same Arc auth uses
    // so writers stay on one hash-chained timeline.
    dashboard_state.set_audit_log(shared_audit.clone());
    // A1 — HMAC secret for report manifests. Sourced from
    // `MM_REPORT_SECRET` env; falls back to a process-scoped
    // default (marked unsigned) when unset.
    if let Ok(sec) = std::env::var("MM_REPORT_SECRET") {
        if !sec.is_empty() {
            dashboard_state.set_report_secret(sec.into_bytes());
        }
    }

    // Epic H — strategy graph store. Always initialised so the
    // admin deploy endpoint works even on fresh deployments; the
    // directory is created on the first save.
    match mm_strategy_graph::GraphStore::new("data/strategy_graphs") {
        Ok(store) => {
            dashboard_state.set_strategy_graph_store(std::sync::Arc::new(store));
            info!("strategy graph store ready");
        }
        Err(e) => {
            tracing::warn!(error = %e, "strategy graph store init failed — graphs disabled");
        }
    }

    // Block B — spawn the compliance report scheduler when
    // at least one cadence is enabled. The concrete
    // `BuiltinReportJob` writes daily / weekly / monthly
    // bundles under `data/reports/<cadence>/` which the
    // archive shipper picks up on its next tick when
    // `ship_daily_reports = true`.
    if let Some(sched_cfg) = config.schedule.clone() {
        if sched_cfg.daily_enabled || sched_cfg.weekly_enabled || sched_cfg.monthly_enabled {
            use mm_dashboard::builtin_report_job::BuiltinReportJob;
            use mm_dashboard::report_scheduler::{ReportScheduler, ScheduleConfig};
            let job = std::sync::Arc::new(BuiltinReportJob::new(
                dashboard_state.clone(),
                std::path::PathBuf::from("data/reports"),
            ));
            let internal_cfg = ScheduleConfig {
                daily_enabled: sched_cfg.daily_enabled,
                weekly_enabled: sched_cfg.weekly_enabled,
                monthly_enabled: sched_cfg.monthly_enabled,
                catchup_hours: sched_cfg.catchup_hours,
                last_run_path: sched_cfg.last_run_path,
            };
            let mut scheduler = ReportScheduler::new(internal_cfg, job);
            if let Err(e) = scheduler.start().await {
                tracing::error!(error = %e, "report scheduler start failed");
            } else {
                // Leak the handle — the scheduler lives for the
                // rest of the process. `Drop` on shutdown logs
                // any in-flight job truncation.
                std::mem::forget(scheduler);
                tracing::info!("compliance report scheduler spawned");
            }
        }
    }

    // Epic G — spawn the sentiment orchestrator when
    // configured. On every poll cycle it fans out collectors
    // → Ollama analyzer → mention counter → one
    // `ConfigOverride::SentimentTick` broadcast per monitored
    // asset. Every engine that `with_social_risk` attached
    // consumes the tick in its existing config-override path.
    if let Some(sent_cfg) = config.sentiment.clone() {
        use mm_sentiment::collector::{
            Collector, cryptopanic::CryptoPanicCollector, rss::RssCollector,
            twitter::TwitterCollector,
        };
        let mut collectors: Vec<Box<dyn Collector>> = Vec::new();
        if !sent_cfg.rss.feeds.is_empty() {
            match RssCollector::new(sent_cfg.rss.feeds.clone()) {
                Ok(c) => collectors.push(Box::new(c)),
                Err(e) => tracing::warn!(error = %e, "rss collector init failed"),
            }
        }
        if !sent_cfg.cryptopanic.url.is_empty() {
            match CryptoPanicCollector::new(sent_cfg.cryptopanic.url.clone()) {
                Ok(c) => collectors.push(Box::new(c)),
                Err(e) => tracing::warn!(error = %e, "cryptopanic collector init failed"),
            }
        }
        if !sent_cfg.twitter.queries.is_empty() {
            match std::env::var(&sent_cfg.twitter.bearer_env) {
                Ok(bearer) if !bearer.is_empty() => {
                    match TwitterCollector::new(sent_cfg.twitter.queries.clone(), bearer) {
                        Ok(c) => collectors.push(Box::new(c)),
                        Err(e) => {
                            tracing::warn!(error = %e, "twitter collector init failed")
                        }
                    }
                }
                _ => tracing::warn!(
                    env = %sent_cfg.twitter.bearer_env,
                    "twitter queries configured but bearer env var missing"
                ),
            }
        }

        let ollama_client = mm_sentiment::OllamaClient::new(mm_sentiment::OllamaConfig {
            base_url: sent_cfg.ollama.base_url.clone(),
            model: sent_cfg.ollama.model.clone(),
            timeout: std::time::Duration::from_secs(sent_cfg.ollama.timeout_secs),
            temperature: 0.1,
        })
        .ok();

        let broadcast_state = dashboard_state.clone();
        let sink: mm_sentiment::orchestrator::TickSink =
            std::sync::Arc::new(move |tick: mm_sentiment::SentimentTick| {
                use rust_decimal::prelude::ToPrimitive;
                // Prometheus — one counter + two gauges per
                // asset so dashboards can chart the feed
                // going hot/cold in real time.
                mm_dashboard::metrics::SENTIMENT_TICKS_TOTAL
                    .with_label_values(&[&tick.asset])
                    .inc();
                mm_dashboard::metrics::SENTIMENT_MENTIONS_RATE
                    .with_label_values(&[&tick.asset])
                    .set(tick.mentions_rate.to_f64().unwrap_or(0.0));
                mm_dashboard::metrics::SENTIMENT_SCORE_5MIN
                    .with_label_values(&[&tick.asset])
                    .set(tick.sentiment_score_5min.to_f64().unwrap_or(0.0));
                broadcast_state.push_sentiment_tick(tick.clone());
                broadcast_state.broadcast_config_override(
                    mm_dashboard::state::ConfigOverride::SentimentTick(tick),
                );
            });
        let analyze_hook: mm_sentiment::orchestrator::AnalyzeHook =
            std::sync::Arc::new(|scorer: &str| {
                mm_dashboard::metrics::SENTIMENT_ARTICLES_TOTAL
                    .with_label_values(&[scorer])
                    .inc();
            });

        let mut orch = mm_sentiment::orchestrator::Orchestrator::new(
            collectors,
            ollama_client,
            sent_cfg.monitored_assets.clone(),
            sink,
        )
        .with_analyze_hook(analyze_hook);
        if sent_cfg.persist_articles {
            match mm_sentiment::persistence::ArticleWriter::new(
                std::path::PathBuf::from(&sent_cfg.persist_path),
            ) {
                Ok(w) => {
                    orch = orch.with_article_writer(std::sync::Arc::new(w));
                    info!(
                        path = %sent_cfg.persist_path,
                        "sentiment article persistence enabled"
                    );
                }
                Err(e) => tracing::warn!(error = %e, "article writer init failed"),
            }
        }
        let poll_interval =
            std::time::Duration::from_secs(sent_cfg.poll_interval_secs.max(10));
        // Self-contained shutdown channel. The orchestrator
        // runs as a daemon and winds down when the process
        // exits — no coordination with the per-symbol
        // shutdown signal is required, because it holds no
        // per-symbol state that needs a graceful flush.
        let (orch_shutdown_tx, orch_shutdown_rx) =
            tokio::sync::watch::channel(false);
        std::mem::forget(orch_shutdown_tx);
        tokio::spawn(orch.run(poll_interval, orch_shutdown_rx));
        info!(
            interval_secs = sent_cfg.poll_interval_secs,
            monitored = sent_cfg.monitored_assets.len(),
            "sentiment orchestrator spawned"
        );
    }

    // Block C — spawn S3 archive shipper when configured.
    // Credentials come from the AWS default chain. `archive = None`
    // (the default) keeps the server entirely aws-free at runtime.
    if let Some(arch_cfg) = config.archive.clone() {
        match mm_dashboard::archive::ArchiveClient::from_config(arch_cfg.clone()).await {
            Ok(client) => {
                // Block D — share the client with the HTTP layer so
                // the `/api/v1/archive/health` probe uses the same
                // config + creds the shipper will.
                dashboard_state.set_archive_client(client.clone());
                let shipper_cfg = mm_dashboard::archive::shipper::ShipperConfig {
                    interval: std::time::Duration::from_secs(arch_cfg.shipper_interval_secs),
                    audit_log: arch_cfg
                        .ship_audit_log
                        .then(|| std::path::PathBuf::from("data/audit.jsonl")),
                    fill_log: arch_cfg
                        .ship_fills
                        .then(|| std::path::PathBuf::from("data/fills.jsonl")),
                    daily_reports_dir: arch_cfg
                        .ship_daily_reports
                        .then(|| std::path::PathBuf::from("data/reports/daily")),
                    // Epic G — ship the sentiment article
                    // JSONL when both pipelines are on.
                    sentiment_log: config.sentiment.as_ref().and_then(|s| {
                        s.persist_articles.then(|| std::path::PathBuf::from(&s.persist_path))
                    }),
                    offset_file: std::path::PathBuf::from("data/archive_offsets.json"),
                };
                let _handle = mm_dashboard::archive::shipper::spawn(client, shipper_cfg);
                tracing::info!(
                    bucket = %arch_cfg.s3_bucket,
                    region = %arch_cfg.s3_region,
                    endpoint = ?arch_cfg.s3_endpoint_url,
                    "archive shipper spawned"
                );
            }
            Err(e) => {
                tracing::error!(error = %e, "archive shipper init failed — continuing without S3 archive");
            }
        }
    }
    // Webhook dispatcher — shared across all engines.
    let webhook_dispatcher = mm_dashboard::webhooks::WebhookDispatcher::new();
    dashboard_state.set_webhook_dispatcher(webhook_dispatcher.clone());

    // Enable persistent fill logging for client API fill history.
    let fills_path = std::path::Path::new("data/fills.jsonl");
    dashboard_state.load_fill_history(fills_path);
    dashboard_state.enable_fill_log(fills_path);
    let ws_broadcast = Arc::new(WsBroadcast::new(1024));
    // Let the dashboard state push typed updates (venue balance
    // snapshots, etc.) through the same channel WS clients
    // subscribe to. Without this the panel falls back to HTTP
    // polling only.
    dashboard_state.enable_ws_broadcast(ws_broadcast.clone());

    // Epic 33 — hyperopt admin worker. Dashboard admin endpoint
    // posts `HyperoptTrigger` through this channel; the worker
    // runs the optimiser and stages the best trial as a
    // `PendingCalibration` for operator review. Not guarded by
    // a feature flag — if nobody posts to `/api/admin/optimize/trigger`,
    // the worker idles.
    let (hopt_tx, hopt_rx) = tokio::sync::mpsc::unbounded_channel();
    dashboard_state.register_hyperopt_trigger_channel(hopt_tx);
    let _hopt_handle = hyperopt_worker::spawn_worker(
        hopt_rx,
        dashboard_state.clone(),
        config.clone(),
    );
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

    // Shared factor covariance estimator (Epic 3). All engines
    // push returns; portfolio risk task reads correlation matrix.
    let shared_factor_cov = Arc::new(std::sync::Mutex::new(
        mm_risk::hedge_optimizer::FactorCovarianceEstimator::new(vec![], 1440),
    ));

    // Shared per-client daily-loss circuit (Epic 6). Registered
    // with every configured client's limit at startup; each
    // engine holds an Arc clone and reports/checks on every
    // summary + refresh tick. The synthetic "default" client
    // (legacy single-client mode) always registers with `None`
    // so existing deployments see the aggregate on the
    // dashboard without enforcement changing behaviour.
    let per_client_circuit = Arc::new(mm_risk::PerClientLossCircuit::new());
    for client in config.effective_clients() {
        per_client_circuit.register(&client.id, client.daily_loss_limit_usd);
        info!(
            client_id = %client.id,
            limit = ?client.daily_loss_limit_usd,
            "per-client loss circuit registered"
        );
    }
    // Share the circuit with the dashboard state so the
    // `/api/v1/clients/loss-state` endpoint can snapshot it.
    dashboard_state.set_per_client_circuit(per_client_circuit.clone());

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
        let wh = webhook_dispatcher.clone();
        let shared_cov = shared_factor_cov.clone();
        let circuit = per_client_circuit.clone();

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
                wh,
                shared_cov,
                circuit,
            )
            .await
            {
                error!(symbol = %symbol, error = %e, "market maker crashed");
            }
        });
        handles.push(handle);
    }

    // Listing sniper background task (Epic F stage-2 + 3).
    // Wired below, after the bundle has been built, so we
    // can hand it every connector the operator configured
    // (primary + hedge + sor_extra_venues).

    // Portfolio risk background task (Epic 3). Evaluates factor
    // deltas every 30s and broadcasts spread multipliers.
    if let Some(ref pr_cfg) = config.portfolio_risk {
        let pr_portfolio = portfolio.clone();
        let pr_dashboard = dashboard_state.clone();
        let pr_factor_cov = shared_factor_cov.clone();
        let mut pr_shutdown = shutdown_tx.subscribe();
        let pr_config = mm_risk::portfolio_risk::PortfolioRiskConfig {
            max_total_delta_usd: pr_cfg.max_total_delta_usd,
            factor_limits: pr_cfg
                .factor_limits
                .iter()
                .map(|f| mm_risk::portfolio_risk::FactorLimitConfig {
                    factor: f.factor.clone(),
                    max_net_delta: f.max_net_delta,
                    widen_mult: f.widen_mult,
                    warn_pct: f.warn_pct,
                })
                .collect(),
        };
        let pr_manager = mm_risk::portfolio_risk::PortfolioRiskManager::new(pr_config);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Ok(port) = pr_portfolio.lock() {
                            let snap = port.snapshot();
                            let factor_map: std::collections::HashMap<String, rust_decimal::Decimal> =
                                snap.per_factor.into_iter().collect();
                            let summary = pr_manager.evaluate(&factor_map);
                            // Broadcast spread multiplier if needed.
                            for action in &summary.actions {
                                match action {
                                    mm_risk::portfolio_risk::PortfolioRiskAction::WidenAll { mult, .. } => {
                                        pr_dashboard.broadcast_config_override(
                                            mm_dashboard::state::ConfigOverride::PortfolioRiskMult(*mult),
                                        );
                                    }
                                    mm_risk::portfolio_risk::PortfolioRiskAction::HaltFactor { .. } => {
                                        pr_dashboard.broadcast_config_override(
                                            mm_dashboard::state::ConfigOverride::PauseQuoting,
                                        );
                                    }
                                    _ => {}
                                }
                            }
                            pr_dashboard.set_portfolio_risk_summary(summary);
                            // Push correlation matrix from shared estimator.
                            if let Ok(cov) = pr_factor_cov.lock() {
                                pr_dashboard.set_correlation_matrix(cov.correlation_matrix());
                            }
                        }
                    }
                    _ = pr_shutdown.changed() => break,
                }
            }
        });
        info!("portfolio risk monitor started (30s interval)");
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
    mut config: AppConfig,
    connector: Arc<dyn ExchangeConnector>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    _checkpoint: Arc<std::sync::Mutex<CheckpointManager>>,
    dashboard_state: DashboardState,
    alert_manager: AlertManager,
    portfolio: Arc<std::sync::Mutex<mm_portfolio::Portfolio>>,
    asset_class_switch: Option<Arc<std::sync::Mutex<mm_risk::KillSwitch>>>,
    webhook_dispatcher: mm_dashboard::webhooks::WebhookDispatcher,
    shared_factor_cov: Arc<std::sync::Mutex<mm_risk::hedge_optimizer::FactorCovarianceEstimator>>,
    per_client_circuit: Arc<mm_risk::PerClientLossCircuit>,
) -> Result<()> {
    let product = product_for_symbol(&symbol, &connector).await;

    // Epic 31 — classify the symbol once at startup so the pair
    // class appears in the dashboard and so the per-class template
    // (E31.3) can merge in before engine construction.
    let pair_class = classify_and_maybe_apply_template(
        &mut config,
        &symbol,
        &product,
        &connector,
    )
    .await;

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
        StrategyType::CrossExchange => {
            let min_profit_bps = config.market_maker.cross_exchange_min_profit_bps;
            // Seed the strategy's fee expectations from the
            // primary product so a fresh connector does not quote
            // through its own rebate/taker assumptions. A hedge
            // connector is required — the factory falls through to
            // an Err at subscribe time if `config.hedge` is unset
            // (validate.rs enforces the check at startup).
            let mut s = CrossExchangeStrategy::new(min_profit_bps);
            // Primary venue: we're the maker, fee is typically a
            // rebate. Use the configured maker fee from product
            // spec resolved above.
            s.set_fees(product.maker_fee, product.taker_fee);
            info!(
                symbol = %symbol,
                %min_profit_bps,
                maker_fee = %product.maker_fee,
                hedge_taker_fee = %product.taker_fee,
                "using CrossExchange strategy — make on primary, hedge on hedge venue"
            );
            Box::new(s)
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

    // Epic B stage-2 — background pair-screener. Spawns a
    // long-running task that polls a mid per configured
    // symbol on a fast cadence, runs the Engle-Granger
    // cointegration test on every configured pair on a
    // slower cadence, and audits every result so operators
    // can pick stat-arb candidates without re-running the
    // test manually.
    if let Some(scr_cfg) = config.pair_screener.clone() {
        let primary_for_screener = bundle.primary.clone();
        let screener_shutdown = shutdown_rx.clone();
        let screener_audit = Arc::new(
            mm_risk::audit::AuditLog::new(std::path::Path::new("data/audit.jsonl"))
                .expect("audit log for pair screener"),
        );
        tokio::spawn(async move {
            run_pair_screener_task(
                primary_for_screener,
                scr_cfg,
                screener_audit,
                screener_shutdown,
            )
            .await;
        });
        info!("pair screener spawned");
    }

    // Epic A stage-2 #3 — build and attach SOR-only extra
    // venues. Each `SorVenueConfig` entry produces one
    // connector via the same `create_hedge_connector` path
    // (which takes an `ExchangeConfig`); the resulting list
    // is appended to the bundle so the dispatcher can look
    // them up by venue id. Per-venue `VenueSeed`s are
    // prepared up-front and registered on the engine below.
    let sor_extras = build_sor_extras(&config)?;
    let sor_extra_seeds: Vec<(
        mm_exchange_core::connector::VenueId,
        mm_engine::sor::venue_state::VenueSeed,
    )> = config
        .sor_extra_venues
        .iter()
        .map(|v| {
            let seed = mm_engine::sor::venue_state::VenueSeed::new(
                &v.symbol,
                product.clone(),
                v.max_inventory,
            );
            (venue_id_from_exchange_type(v.exchange.exchange_type), seed)
        })
        .collect();
    let mut bundle = bundle;
    if !sor_extras.is_empty() {
        info!(
            count = sor_extras.len(),
            "attaching SOR extra venues to bundle"
        );
        bundle = bundle.with_extra(sor_extras.into_iter().map(|(_, c)| c).collect());
    }

    // If the operator selected FundingArb, build the driver
    // from the `funding_arb` config section and inject it into
    // the engine. The engine's run loop picks up the periodic
    // tick + event routing.
    // Listing sniper background task (Epic F stage-2 + 3).
    // Scans the primary connector plus any hedge and every
    // `sor_extra_venues` entry so multi-venue deployments
    // catch new listings across the whole surface, not just
    // the primary. The sniper tolerates per-venue
    // `list_symbols` errors (unsupported, timeout) and keeps
    // scanning the rest — no fatal failure mode.
    if config.listing_sniper.enabled {
        let mut sniper_connectors: Vec<Arc<dyn ExchangeConnector>> = Vec::new();
        sniper_connectors.push(bundle.primary.clone());
        if let Some(h) = bundle.hedge.as_ref() {
            sniper_connectors.push(h.clone());
        }
        for extra in &bundle.extra {
            sniper_connectors.push(extra.clone());
        }
        let sniper_count = sniper_connectors.len();
        let sniper_shutdown = shutdown_rx.clone();
        let sniper_audit = Arc::new(
            mm_risk::audit::AuditLog::new(std::path::Path::new("data/audit.jsonl"))
                .expect("audit log for listing sniper"),
        );
        let sniper_alerts = Some(alert_manager.clone());
        let scan_secs = config.listing_sniper.scan_interval_secs;
        let alert_on_disc = config.listing_sniper.alert_on_discovery;
        let entry_cfg = config.listing_sniper_entry.clone();
        tokio::spawn(async move {
            let mut runner = mm_engine::listing_sniper::ListingSniperRunner::new(
                sniper_connectors,
                sniper_audit,
                sniper_alerts,
                scan_secs,
                alert_on_disc,
            );
            // Epic F stage-3 — attach the real-entry policy
            // only when the operator explicitly configured
            // it. Observer-only runs stay byte-identical
            // without `listing_sniper_entry`.
            if let Some(cfg) = entry_cfg {
                runner = runner.with_entry_policy(cfg);
            }
            runner.run(sniper_shutdown).await;
        });
        info!(
            scan_interval_secs = config.listing_sniper.scan_interval_secs,
            venues = sniper_count,
            "listing sniper started"
        );
    }

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
                        ..Default::default()
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

    // Hot config override channel — admin endpoints send
    // overrides through the dashboard state; the engine polls
    // the receiver in its select loop.
    let (config_tx, config_rx) = tokio::sync::mpsc::unbounded_channel();
    dashboard_state.register_config_channel(&symbol, config_tx);

    let record_path = if config.record_market_data {
        Some(format!("data/recorded/{}.jsonl", symbol.to_lowercase()))
    } else {
        None
    };

    // Block B — snapshot fields we need after the `config` /
    // `symbol` values are moved into `MarketMakerEngine::new`.
    let checkpoint_restore_enabled = config.checkpoint_restore;
    let checkpoint_path = config.checkpoint_path.clone();
    let symbol_for_restore = symbol.clone();
    // Epic F #1 — capture lead-lag inputs before `config` is
    // moved. The guard only makes sense with a hedge, so the
    // presence flag is captured alongside the config.
    let lead_lag_cfg = config.lead_lag.clone();
    let hedge_configured = config.hedge.is_some();
    // Epic F #2 — snapshot the news-retreat config. Headlines
    // are pushed post-boot via `/api/admin/config` broadcast;
    // here we just build the state machine.
    let news_retreat_cfg = config.news_retreat.clone();
    // Epic G — capture the social-risk sub-config so we can
    // build a `SocialRiskEngine` per symbol after `config`
    // is moved into `MarketMakerEngine::new`.
    let social_risk_cfg = config.sentiment.as_ref().map(|s| s.risk.clone());

    // Block D — resolve the owning client id via the dashboard
    // state's reverse index. `None` in single-client mode; the
    // engine then tags fills + audit events with the owning
    // client so Epic 1 per-client audit trails are accurate.
    let client_id_for_engine = dashboard_state.get_client_for_symbol(&symbol_for_restore);

    let mut engine_builder = MarketMakerEngine::new(
        symbol,
        config,
        product,
        strategy,
        bundle,
        Some(dashboard_state),
        Some(alert_manager),
    )
    .with_portfolio(portfolio)
    .with_config_overrides(config_rx)
    .with_webhooks(webhook_dispatcher)
    .with_shared_factor_covariance(shared_factor_cov)
    .with_per_client_circuit(per_client_circuit)
    .with_pair_class(pair_class);
    if let Some(cid) = client_id_for_engine {
        engine_builder = engine_builder.with_client_id(cid);
    }

    // Epic F #1 — construct the lead-lag guard when the
    // operator opted in AND a hedge connector is present (the
    // guard needs a leader venue; without a hedge there's no
    // leader mid to read). The engine's hedge-book event
    // handler auto-feeds the mid into
    // `update_lead_lag_from_mid`, no extra wiring required.
    if let Some(ll_cfg) = lead_lag_cfg.as_ref() {
        if hedge_configured {
            use std::str::FromStr;
            let parsed = mm_risk::lead_lag_guard::LeadLagGuardConfig {
                half_life_events: ll_cfg.half_life_events,
                z_min: rust_decimal::Decimal::from_str(&ll_cfg.z_min)
                    .unwrap_or(rust_decimal::Decimal::new(2, 0)),
                z_max: rust_decimal::Decimal::from_str(&ll_cfg.z_max)
                    .unwrap_or(rust_decimal::Decimal::new(4, 0)),
                max_mult: rust_decimal::Decimal::from_str(&ll_cfg.max_mult)
                    .unwrap_or(rust_decimal::Decimal::new(3, 0)),
            };
            let guard = mm_risk::lead_lag_guard::LeadLagGuard::new(parsed);
            engine_builder = engine_builder.with_lead_lag_guard(guard);
            info!(symbol = %symbol_for_restore, "lead-lag guard attached");
        } else {
            warn!(
                symbol = %symbol_for_restore,
                "lead_lag config set but no hedge connector — guard skipped"
            );
        }
    }

    // Epic G — wire `SocialRiskEngine` when sentiment is
    // configured. The orchestrator spawns separately at
    // `main()` level and broadcasts ticks through
    // `ConfigOverride::SentimentTick`; this is the per-engine
    // receiver half.
    if let Some(sr_cfg) = social_risk_cfg.as_ref() {
        use std::str::FromStr;
        let parsed = mm_risk::social_risk::SocialRiskConfig {
            rate_warn: rust_decimal::Decimal::from_str(&sr_cfg.rate_warn)
                .unwrap_or(rust_decimal::Decimal::new(2, 0)),
            rate_alarm: rust_decimal::Decimal::from_str(&sr_cfg.rate_alarm)
                .unwrap_or(rust_decimal::Decimal::new(5, 0)),
            max_vol_multiplier: rust_decimal::Decimal::from_str(&sr_cfg.max_vol_multiplier)
                .unwrap_or(rust_decimal::Decimal::new(3, 0)),
            min_size_multiplier: rust_decimal::Decimal::from_str(&sr_cfg.min_size_multiplier)
                .unwrap_or(rust_decimal::Decimal::new(5, 1)),
            kill_mentions_rate: rust_decimal::Decimal::from_str(&sr_cfg.kill_mentions_rate)
                .unwrap_or(rust_decimal::Decimal::new(10, 0)),
            kill_vol_threshold: rust_decimal::Decimal::from_str(&sr_cfg.kill_vol_threshold)
                .unwrap_or(rust_decimal::Decimal::new(8, 1)),
            skew_threshold: rust_decimal::Decimal::from_str(&sr_cfg.skew_threshold)
                .unwrap_or(rust_decimal::Decimal::new(3, 1)),
            max_skew_bps: rust_decimal::Decimal::from_str(&sr_cfg.max_skew_bps)
                .unwrap_or(rust_decimal::Decimal::new(15, 0)),
            ofi_confirm_z: rust_decimal::Decimal::from_str(&sr_cfg.ofi_confirm_z)
                .unwrap_or(rust_decimal::Decimal::new(15, 1)),
            staleness: chrono::Duration::minutes(sr_cfg.staleness_mins),
        };
        engine_builder = engine_builder
            .with_social_risk(mm_risk::social_risk::SocialRiskEngine::new(parsed));
        info!(symbol = %symbol_for_restore, "social risk engine attached");
    }

    // Epic F #2 — news-retreat state machine. Input source:
    // `POST /api/admin/config` broadcast with `field = "News"`
    // which the dashboard routes through the existing
    // per-symbol ConfigOverride channel.
    if let Some(nr_cfg) = news_retreat_cfg.as_ref() {
        use std::str::FromStr;
        let parsed = mm_risk::news_retreat::NewsRetreatConfig {
            critical_keywords: nr_cfg.critical_keywords.clone(),
            high_keywords: nr_cfg.high_keywords.clone(),
            low_keywords: nr_cfg.low_keywords.clone(),
            critical_cooldown_ms: nr_cfg.critical_cooldown_ms,
            high_cooldown_ms: nr_cfg.high_cooldown_ms,
            low_cooldown_ms: nr_cfg.low_cooldown_ms,
            high_multiplier: rust_decimal::Decimal::from_str(&nr_cfg.high_multiplier)
                .unwrap_or(rust_decimal::Decimal::new(2, 0)),
            critical_multiplier: rust_decimal::Decimal::from_str(&nr_cfg.critical_multiplier)
                .unwrap_or(rust_decimal::Decimal::new(3, 0)),
        };
        match mm_risk::news_retreat::NewsRetreatStateMachine::new(parsed) {
            Ok(sm) => {
                engine_builder = engine_builder.with_news_retreat(sm);
                info!(symbol = %symbol_for_restore, "news-retreat state machine attached");
            }
            Err(e) => {
                warn!(
                    symbol = %symbol_for_restore,
                    error = %e,
                    "news_retreat config invalid — state machine skipped"
                );
            }
        }
    }
    if let Some(arc) = asset_class_switch {
        engine_builder = engine_builder.with_asset_class_switch(arc);
    }
    if let Some(ref path) = record_path {
        engine_builder = engine_builder.with_event_recorder(std::path::Path::new(path));
    }

    let mut engine = match funding_arb_wiring {
        Some((driver, tick)) => engine_builder.with_funding_arb_driver(driver, tick),
        None => engine_builder,
    };

    // Block B — checkpoint restore + fill replay validation.
    // Opt-in via `checkpoint_restore = true` in AppConfig. The
    // engine rehydrates inventory + realised-PnL baseline from
    // the last saved checkpoint, and when an audit log is
    // present we replay OrderFilled events to cross-check the
    // checkpoint wasn't stale or truncated. Discrepancy logs
    // at WARN so operators can investigate without the engine
    // refusing to start (a good checkpoint is better than no
    // checkpoint; an inconsistent one is flagged + served).
    if checkpoint_restore_enabled {
        let cp_path = std::path::Path::new(&checkpoint_path);
        let cp_mgr =
            mm_persistence::checkpoint::CheckpointManager::new(cp_path, u64::MAX);
        if let Some(symcp) = cp_mgr.get_symbol(&symbol_for_restore) {
            let audit_path = std::path::Path::new("data/audit.jsonl");
            if let Some(replay) =
                mm_persistence::fill_replay::replay_fills_from_audit(audit_path)
            {
                let issues =
                    mm_persistence::fill_replay::validate_checkpoint_against_replay(
                        symcp,
                        &replay,
                        rust_decimal::Decimal::new(1, 6),
                    );
                if issues.is_empty() {
                    info!(
                        symbol = %symbol_for_restore,
                        fills = replay.fill_count,
                        "fill replay confirms checkpoint"
                    );
                } else {
                    warn!(
                        symbol = %symbol_for_restore,
                        issues = ?issues,
                        "fill replay found checkpoint discrepancies — restoring anyway"
                    );
                }
            }
            engine = engine.with_checkpoint_restore(symcp);
        } else {
            info!(symbol = %symbol_for_restore, "no checkpoint available for symbol");
        }
    }

    // Register pre-built SOR seeds on the engine so the
    // greedy router can see the extra venues. Per-venue
    // best-bid/ask will be populated by the aggregator's
    // live feed once the first L1 snapshot lands.
    for (vid, seed) in sor_extra_seeds {
        info!(
            venue = ?vid,
            symbol = %seed.symbol,
            max_inv = %seed.available_qty,
            "registered extra SOR venue"
        );
        engine = engine.with_sor_venue(vid, seed);
    }

    engine.run_with_hedge(ws_rx, hedge_rx, shutdown_rx).await
}

/// Map an `ExchangeType` to the corresponding runtime
/// `VenueId` tag. Used by Epic A stage-2 #3 to bridge the
/// `common` config layer (which knows ExchangeType) and the
/// `exchange-core` runtime (which keys by VenueId).
/// Epic B stage-2 — background cointegration pair-screener
/// task. Polls one mid per configured symbol on the sample
/// cadence, feeds the rolling buffer in
/// `PairScreener::push_price`, and runs `screen_all()` on
/// the scan cadence, writing one audit event per result.
///
/// Error policy: a per-symbol `get_orderbook` failure logs
/// and moves on — the rolling buffer simply misses one
/// sample. An unrecoverable (venue permanently down) failure
/// just means the screener stays at warm-up forever, which
/// is visible via the lack of audit rows.
async fn run_pair_screener_task(
    connector: Arc<dyn ExchangeConnector>,
    cfg: mm_common::config::PairScreenerConfig,
    audit: Arc<mm_risk::audit::AuditLog>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    use mm_risk::audit::AuditEventType;
    use mm_strategy::stat_arb::screener::PairScreener;

    let sample_secs = cfg.sample_interval_secs.max(1);
    let scan_secs = cfg.scan_interval_secs.max(sample_secs);
    let mut screener = PairScreener::new(cfg.pairs.clone());

    // Union of every symbol referenced by any pair. Ordered
    // + deduped so the poll sequence is deterministic across
    // runs — eases post-mortem.
    let mut symbols: Vec<String> = cfg
        .pairs
        .iter()
        .flat_map(|(y, x)| [y.clone(), x.clone()])
        .collect();
    symbols.sort();
    symbols.dedup();

    let mut sample_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(sample_secs));
    let mut scan_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(scan_secs));
    // Skip the first immediate ticks on both clocks so we do
    // not scan on a cold-start empty buffer.
    sample_interval.tick().await;
    scan_interval.tick().await;

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("pair screener shutting down");
                    return;
                }
            }
            _ = sample_interval.tick() => {
                for sym in &symbols {
                    match connector.get_orderbook(sym, 1).await {
                        Ok((bids, asks, _seq)) => {
                            let bid = bids.first().map(|l| l.price);
                            let ask = asks.first().map(|l| l.price);
                            if let (Some(b), Some(a)) = (bid, ask) {
                                if b > rust_decimal::Decimal::ZERO
                                    && a > rust_decimal::Decimal::ZERO
                                {
                                    let mid = (b + a) / rust_decimal::Decimal::from(2u32);
                                    screener.push_price(sym, mid);
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                symbol = %sym,
                                error = %e,
                                "pair screener sample fetch failed"
                            );
                        }
                    }
                }
            }
            _ = scan_interval.tick() => {
                let results = screener.screen_all();
                for r in results {
                    let detail = match &r.cointegration {
                        Some(c) => format!(
                            "y={}, x={}, coint={}, adf={}, crit={}, beta={}, n={}",
                            r.y_symbol,
                            r.x_symbol,
                            c.is_cointegrated,
                            c.adf_statistic,
                            c.critical_value_5pct,
                            c.beta,
                            r.sample_size,
                        ),
                        None => format!(
                            "y={}, x={}, coint=insufficient_samples, n={}",
                            r.y_symbol, r.x_symbol, r.sample_size,
                        ),
                    };
                    info!(%detail, "pair screener scan");
                    let audit_symbol = format!("{}_{}", r.y_symbol, r.x_symbol);
                    audit.risk_event(
                        &audit_symbol,
                        AuditEventType::CointegrationScreened,
                        &detail,
                    );
                }
            }
        }
    }
}

fn venue_id_from_exchange_type(
    ex: ExchangeType,
) -> mm_exchange_core::connector::VenueId {
    use mm_exchange_core::connector::VenueId;
    match ex {
        ExchangeType::Custom => VenueId::Custom,
        ExchangeType::Binance | ExchangeType::BinanceTestnet => VenueId::Binance,
        ExchangeType::Bybit | ExchangeType::BybitTestnet => VenueId::Bybit,
        ExchangeType::HyperLiquid | ExchangeType::HyperLiquidTestnet => VenueId::HyperLiquid,
    }
}

/// Epic A stage-2 #3 — build the list of `(venue, connector)`
/// pairs for every `SorVenueConfig` entry. Rejects duplicates
/// of the primary or hedge venue because the dispatcher's
/// single-connector-per-venue lookup can't route two
/// connectors to the same venue id.
fn build_sor_extras(
    config: &AppConfig,
) -> Result<Vec<(mm_exchange_core::connector::VenueId, Arc<dyn ExchangeConnector>)>> {
    use std::collections::HashSet;
    let mut seen: HashSet<mm_exchange_core::connector::VenueId> = HashSet::new();
    seen.insert(venue_id_from_exchange_type(config.exchange.exchange_type));
    if let Some(h) = &config.hedge {
        seen.insert(venue_id_from_exchange_type(h.exchange.exchange_type));
    }

    let mut out = Vec::with_capacity(config.sor_extra_venues.len());
    for v in &config.sor_extra_venues {
        let vid = venue_id_from_exchange_type(v.exchange.exchange_type);
        if !seen.insert(vid) {
            anyhow::bail!(
                "sor_extra_venues references {vid:?} which is already \
                 the primary or hedge venue — the dispatcher's \
                 single-connector-per-venue lookup cannot route two \
                 connectors to the same venue id"
            );
        }
        // Reuse `create_hedge_connector` — same signature
        // (`&ExchangeConfig`), same per-venue branching, no
        // reason to duplicate.
        let conn = create_hedge_connector(&v.exchange)?;
        out.push((vid, conn));
    }
    Ok(out)
}

/// Build a hedge-leg connector from its `ExchangeConfig`.
///
/// Epic 40.6 — hedge direction defaults to **perp-short** when the
/// user sets `[hedge.exchange] product = "spot"` *implicitly*
/// (i.e. leaves it at the default). Research
/// (`docs/research/spot-vs-perp-mm-apr17.md`, section 3 "Risk →
/// Delta-hedging asymmetry") shows long-spot + short-perp is
/// operationally clean, while short-spot + long-perp requires a
/// borrow channel (5–20% APR, recall risk). Making perp the
/// hedge default aligns the engine with the institutional
/// XEMM / basis convention. Users who genuinely want spot-on-spot
/// cross-exchange set `product = "spot"` explicitly — we warn
/// once at startup so the choice is visible in the log.
///
/// Kept separate from `create_connector` so the primary and hedge
/// paths can evolve independently — cross-venue basis trades
/// (Binance spot vs HyperLiquid perps) live here.
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
        ExchangeType::Binance | ExchangeType::BinanceTestnet => match cfg.product {
            ProductType::Spot => {
                info!(
                    "hedge leg on Binance spot — short-spot hedging requires a borrow \
                     channel (borrow APR applies). Prefer `product = \"linear_perp\"` \
                     for XEMM / basis (Epic 40.6)."
                );
                let testnet = matches!(cfg.exchange_type, ExchangeType::BinanceTestnet);
                let connector = if testnet {
                    mm_exchange_binance::BinanceConnector::testnet(&api_key, &api_secret)
                } else {
                    mm_exchange_binance::BinanceConnector::new(
                        "https://api.binance.com",
                        "wss://stream.binance.com:9443",
                        &api_key,
                        &api_secret,
                    )
                };
                Ok(Arc::new(connector))
            }
            ProductType::LinearPerp => {
                info!("hedge leg on Binance USDⓈ-M linear futures");
                Ok(Arc::new(mm_exchange_binance::BinanceFuturesConnector::new(
                    &api_key, &api_secret,
                )))
            }
            ProductType::InversePerp => anyhow::bail!(
                "Binance inverse (COIN-M) hedge leg not supported — use `linear_perp`"
            ),
        },
        ExchangeType::Bybit | ExchangeType::BybitTestnet => {
            let testnet = matches!(cfg.exchange_type, ExchangeType::BybitTestnet);
            let connector = match (cfg.product, testnet) {
                (ProductType::Spot, false) => {
                    info!(
                        "hedge leg on Bybit spot — short-spot hedging requires borrow. \
                         Prefer `product = \"linear_perp\"` for XEMM (Epic 40.6)."
                    );
                    mm_exchange_bybit::BybitConnector::spot(&api_key, &api_secret)
                }
                (ProductType::Spot, true) => {
                    mm_exchange_bybit::BybitConnector::testnet_spot(&api_key, &api_secret)
                }
                (ProductType::LinearPerp, false) => {
                    info!("hedge leg on Bybit V5 linear perp");
                    mm_exchange_bybit::BybitConnector::linear(&api_key, &api_secret)
                }
                (ProductType::LinearPerp, true) => {
                    mm_exchange_bybit::BybitConnector::testnet(&api_key, &api_secret)
                }
                (ProductType::InversePerp, false) => {
                    info!("hedge leg on Bybit V5 inverse perp");
                    mm_exchange_bybit::BybitConnector::inverse(&api_key, &api_secret)
                }
                (ProductType::InversePerp, true) => {
                    mm_exchange_bybit::BybitConnector::testnet_inverse(&api_key, &api_secret)
                }
            };
            Ok(Arc::new(connector))
        }
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
    let whitelist = config.exchange.withdraw_whitelist.clone();
    match &whitelist {
        Some(list) if list.is_empty() => {
            info!("withdraw_whitelist configured as empty — ALL withdraws will be blocked");
        }
        Some(list) => {
            info!(addresses = list.len(), "withdraw_whitelist configured");
        }
        None => {
            info!("withdraw_whitelist not set — venue-side controls are the only guard");
        }
    }

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
            // Epic 40.1 — product-aware routing. Default (spot)
            // preserves legacy configs. Linear/inverse perp route
            // to the USDⓈ-M futures connector (inverse is a
            // Binance-side product type but our futures crate
            // targets USDⓈ-M only; flag it as unsupported until
            // the inverse-futures crate lands).
            match config.exchange.product {
                ProductType::Spot => {
                    info!("connecting to Binance spot");
                    Ok(Arc::new(
                        mm_exchange_binance::BinanceConnector::new(
                            "https://api.binance.com",
                            "wss://stream.binance.com:9443",
                            &api_key,
                            &api_secret,
                        )
                        .with_withdraw_whitelist(whitelist),
                    ))
                }
                ProductType::LinearPerp => {
                    info!("connecting to Binance USDⓈ-M linear futures");
                    Ok(Arc::new(mm_exchange_binance::BinanceFuturesConnector::new(
                        &api_key, &api_secret,
                    )))
                }
                ProductType::InversePerp => {
                    anyhow::bail!(
                        "Binance inverse (COIN-M) futures is not supported by the current \
                         connector crate. Use `product = \"linear_perp\"` for USDⓈ-M or \
                         run the symbol on Bybit inverse instead."
                    )
                }
            }
        }
        ExchangeType::BinanceTestnet => {
            info!("connecting to Binance Testnet (spot)");
            Ok(Arc::new(
                mm_exchange_binance::BinanceConnector::testnet(&api_key, &api_secret)
                    .with_withdraw_whitelist(whitelist),
            ))
        }
        ExchangeType::Bybit => {
            // Epic 40.1 — Bybit category routes per-product:
            // spot → `BybitCategory::Spot`, linear_perp →
            // `::Linear` (USDT-margined), inverse_perp →
            // `::Inverse` (coin-margined).
            match config.exchange.product {
                ProductType::Spot => {
                    info!("connecting to Bybit V5 spot");
                    Ok(Arc::new(
                        mm_exchange_bybit::BybitConnector::spot(&api_key, &api_secret)
                            .with_withdraw_whitelist(whitelist),
                    ))
                }
                ProductType::LinearPerp => {
                    info!("connecting to Bybit V5 linear perp");
                    Ok(Arc::new(
                        mm_exchange_bybit::BybitConnector::linear(&api_key, &api_secret)
                            .with_withdraw_whitelist(whitelist),
                    ))
                }
                ProductType::InversePerp => {
                    info!("connecting to Bybit V5 inverse perp");
                    Ok(Arc::new(
                        mm_exchange_bybit::BybitConnector::inverse(&api_key, &api_secret)
                            .with_withdraw_whitelist(whitelist),
                    ))
                }
            }
        }
        ExchangeType::BybitTestnet => {
            info!("connecting to Bybit Testnet ({:?})", config.exchange.product);
            let ctor = match config.exchange.product {
                ProductType::Spot => mm_exchange_bybit::BybitConnector::testnet_spot,
                ProductType::LinearPerp => mm_exchange_bybit::BybitConnector::testnet,
                ProductType::InversePerp => mm_exchange_bybit::BybitConnector::testnet_inverse,
            };
            Ok(Arc::new(ctor(&api_key, &api_secret).with_withdraw_whitelist(whitelist)))
        }
        ExchangeType::HyperLiquid => {
            if api_secret.is_empty() {
                anyhow::bail!(
                    "HyperLiquid connector requires MM_API_SECRET to hold the \
                     hex-encoded wallet private key (32 bytes, 0x prefix \
                     optional). Public read-only endpoints work without a key \
                     but MM trading does not — set the env var or switch to \
                     `exchange_type = \"custom\"` for offline testing."
                );
            }
            info!("connecting to HyperLiquid");
            // For HL: api_secret holds the hex-encoded wallet private key.
            // api_key is unused — the address is derived from the private key.
            Ok(Arc::new(
                mm_exchange_hyperliquid::HyperLiquidConnector::new(&api_secret).map_err(|e| {
                    anyhow::anyhow!(
                        "HyperLiquid init failed: {e}. Check MM_API_SECRET is a \
                         valid 32-byte hex private key."
                    )
                })?,
            ))
        }
        ExchangeType::HyperLiquidTestnet => {
            if api_secret.is_empty() {
                anyhow::bail!(
                    "HyperLiquid Testnet requires MM_API_SECRET — see mainnet \
                     error message for details."
                );
            }
            info!("connecting to HyperLiquid Testnet");
            Ok(Arc::new(
                mm_exchange_hyperliquid::HyperLiquidConnector::testnet(&api_secret).map_err(
                    |e| {
                        anyhow::anyhow!(
                            "HyperLiquid Testnet init failed: {e}. Check \
                             MM_API_SECRET is a valid 32-byte hex private key."
                        )
                    },
                )?,
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

/// Classify the symbol's pair class from the connector and
/// (when `market_maker.apply_pair_class_template = true`) merge
/// the matching template into the running config before the
/// engine is built. Returns the detected `PairClass`.
///
/// Volume fetch + classifier are best-effort: venue doesn't
/// support `get_24h_volume_usd` or the call fails → classifier
/// treats the symbol as unknown-liquidity and falls back to its
/// conservative `MemeSpot` / `AltPerp` default.
async fn classify_and_maybe_apply_template(
    cfg: &mut AppConfig,
    symbol: &str,
    product: &ProductSpec,
    connector: &Arc<dyn ExchangeConnector>,
) -> mm_common::PairClass {
    use mm_common::pair_class::classify_symbol;
    use mm_exchange_core::connector::VenueProduct;

    let daily_vol = match connector.get_24h_volume_usd(symbol).await {
        Ok(Some(v)) => Some(v),
        Ok(None) => {
            info!(symbol, "venue returned no 24h volume — classifier falls back to conservative defaults");
            None
        }
        Err(e) => {
            warn!(symbol, error = %e, "get_24h_volume_usd failed — classifier falls back");
            None
        }
    };
    let is_perp = matches!(
        connector.product(),
        VenueProduct::LinearPerp | VenueProduct::InversePerp
    );
    let class = classify_symbol(product, daily_vol, is_perp);
    info!(symbol, class = %class, vol = ?daily_vol, "pair-class classified");

    if cfg.market_maker.apply_pair_class_template {
        let dir = std::path::Path::new("config/pair-classes");
        match crate::pair_template::apply_pair_class_template(cfg, class, dir) {
            Ok(n) => {
                info!(
                    symbol,
                    class = %class,
                    applied = n,
                    "pair-class template merged into config"
                );
            }
            Err(e) => {
                warn!(
                    symbol,
                    class = %class,
                    error = %e,
                    "pair-class template merge failed — continuing with user config only"
                );
            }
        }
    }
    class
}

/// Fetch product spec from the connector. Falls back to a
/// conservative default if the venue doesn't support
/// `get_product_spec` — the fee-tier refresh task will
/// overwrite these on its first tick.
async fn product_for_symbol(symbol: &str, connector: &Arc<dyn ExchangeConnector>) -> ProductSpec {
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

/// Initialize logging + Sentry + optional OpenTelemetry OTLP.
///
/// Returns `(sentry_guard, otel_guard)`. Both must live for the
/// program's lifetime: dropping them flushes in-flight Sentry
/// events and OTLP spans respectively. `main` holds them until
/// return.
///
/// Sentry gate: `MM_SENTRY_DSN` (unset = zero overhead).
///
/// OTLP gate: cargo feature `otel` at build time + env var
/// `OTEL_EXPORTER_OTLP_ENDPOINT` at runtime. Default build omits
/// the OTel deps entirely so the binary footprint stays flat for
/// teams that don't want tracing infra.
fn init_logging(
    config: &AppConfig,
) -> (Option<sentry::ClientInitGuard>, Option<otel::OtelGuard>) {
    use tracing_subscriber::prelude::*;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,mm_engine=debug,mm_strategy=debug".into());

    // Sentry first — so the tracing layer can attach. Gated on
    // the env var so dev / paper builds do not pay network cost
    // or accidentally leak crash data to a shared project.
    let sentry_guard = std::env::var("MM_SENTRY_DSN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|dsn| {
            sentry::init((
                dsn,
                sentry::ClientOptions {
                    release: Some(
                        format!("mm-server@{}", env!("CARGO_PKG_VERSION")).into(),
                    ),
                    environment: Some(config.mode.clone().into()),
                    // Traces sample rate left at 0 (errors only);
                    // operators wanting performance traces can
                    // override via `SENTRY_TRACES_SAMPLE_RATE`
                    // handled by the sentry crate directly.
                    ..Default::default()
                },
            ))
        });
    if sentry_guard.is_some() {
        info!("Sentry enabled (MM_SENTRY_DSN set)");
    }

    // OTel layer — must sit directly on top of Registry because
    // `OpenTelemetryLayer<Registry, Tracer>` only implements
    // `Layer<Registry>` with a concrete tracer. Other layers stack
    // on top of it afterward. On default builds the block compiles
    // to nothing and `otel_guard` is always `None`.
    #[cfg(feature = "otel")]
    let (otel_layer, otel_guard) = match otel::init("mm-server") {
        Some((l, g)) => (Some(l), Some(g)),
        None => (None, None),
    };
    #[cfg(not(feature = "otel"))]
    let otel_guard: Option<otel::OtelGuard> = None;

    // Build the subscriber bottom-up: Registry → OTel (if enabled)
    // → EnvFilter → fmt layers → sentry. Helper macros keep the
    // two-path cfg noise local.
    #[cfg(feature = "otel")]
    macro_rules! with_otel {
        ($base:expr) => {
            $base.with(otel_layer)
        };
    }
    #[cfg(not(feature = "otel"))]
    macro_rules! with_otel {
        ($base:expr) => {
            $base
        };
    }

    if config.log_file.is_empty() {
        with_otel!(tracing_subscriber::registry())
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .with(sentry_tracing::layer())
            .init();
    } else {
        let log_dir = std::path::Path::new(&config.log_file)
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let log_name = std::path::Path::new(&config.log_file)
            .file_name()
            .unwrap_or(std::ffi::OsStr::new("mm.log"));

        let file_appender = tracing_appender::rolling::daily(log_dir, log_name);
        let (file_writer, _guard) = tracing_appender::non_blocking(file_appender);
        std::mem::forget(_guard);

        with_otel!(tracing_subscriber::registry())
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(file_writer),
            )
            .with(sentry_tracing::layer())
            .init();

        info!(path = %config.log_file, "file logging enabled");
    }

    (sentry_guard, otel_guard)
}
