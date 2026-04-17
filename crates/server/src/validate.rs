use mm_common::config::{AppConfig, ExchangeType, StrategyType};
use rust_decimal_macros::dec;
use tracing::warn;

/// Validate configuration and return a list of warnings/errors.
/// Returns Err if config is fatally invalid.
pub fn validate_config(config: &AppConfig) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let mm = &config.market_maker;
    let risk = &config.risk;

    // --- Market Maker params ---
    if mm.gamma <= dec!(0) {
        errors.push("gamma must be > 0".to_string());
    }
    if mm.gamma > dec!(10) {
        warnings.push(format!(
            "gamma={} is very high, spread will be extremely wide",
            mm.gamma
        ));
    }
    if mm.kappa <= dec!(0) {
        errors.push("kappa must be > 0".to_string());
    }
    if mm.sigma <= dec!(0) || mm.sigma > dec!(1) {
        warnings.push(format!(
            "sigma={} looks unusual (expected 0.001-0.5)",
            mm.sigma
        ));
    }
    if mm.order_size <= dec!(0) {
        errors.push("order_size must be > 0".to_string());
    }
    if mm.num_levels == 0 {
        errors.push("num_levels must be >= 1".to_string());
    }
    if mm.num_levels > 20 {
        warnings.push(format!(
            "num_levels={} is very high, consider reducing",
            mm.num_levels
        ));
    }
    if mm.refresh_interval_ms < 50 {
        warnings.push("refresh_interval_ms < 50 may cause excessive API calls".to_string());
    }
    if mm.min_spread_bps <= dec!(0) {
        errors.push("min_spread_bps must be > 0".to_string());
    }
    if mm.time_horizon_secs == 0 {
        errors.push("time_horizon_secs must be > 0".to_string());
    }

    // --- Risk params ---
    if risk.max_inventory <= dec!(0) {
        errors.push("max_inventory must be > 0".to_string());
    }
    if risk.max_exposure_quote <= dec!(0) {
        errors.push("max_exposure_quote must be > 0".to_string());
    }
    if risk.max_drawdown_quote <= dec!(0) {
        errors.push("max_drawdown_quote must be > 0".to_string());
    }
    if risk.max_drawdown_quote >= risk.max_exposure_quote {
        warnings.push("max_drawdown_quote >= max_exposure_quote, circuit breaker may trigger before exposure limit".to_string());
    }

    // --- Kill switch ---
    let ks = &config.kill_switch;
    if ks.daily_loss_warning >= ks.daily_loss_limit {
        warnings.push(
            "daily_loss_warning >= daily_loss_limit, warning level will be skipped".to_string(),
        );
    }

    // --- SLA ---
    let sla = &config.sla;
    if sla.min_uptime_pct > dec!(100) || sla.min_uptime_pct < dec!(0) {
        errors.push("min_uptime_pct must be in [0, 100]".to_string());
    }

    // --- Exchange ---
    // HyperLiquid hardcodes endpoints inside the connector, so rest_url/ws_url
    // in the config are ignored there. Other venues still require them.
    let hl = matches!(
        config.exchange.exchange_type,
        ExchangeType::HyperLiquid | ExchangeType::HyperLiquidTestnet
    );
    if !hl {
        if config.exchange.rest_url.is_empty() {
            errors.push("exchange.rest_url is required".to_string());
        }
        if config.exchange.ws_url.is_empty() {
            errors.push("exchange.ws_url is required".to_string());
        }
    } else if config
        .exchange
        .api_secret
        .as_deref()
        .unwrap_or("")
        .is_empty()
    {
        errors.push(
            "HyperLiquid requires MM_API_SECRET to be set to a hex-encoded private key".to_string(),
        );
    }

    // --- Symbols ---
    if config.symbols.is_empty() && config.clients.is_empty() {
        errors.push("at least one symbol is required (via symbols or clients)".to_string());
    }

    // --- Cross-product strategies ---
    // Both `Basis` and `FundingArb` need a hedge connector.
    // `FundingArb` also needs a `[funding_arb]` section with
    // `enabled = true` for the driver to do anything.
    // `basis_shift` is a [0, 1] fraction and makes no sense
    // outside that range.
    if !(dec!(0)..=dec!(1)).contains(&mm.basis_shift) {
        errors.push(format!(
            "market_maker.basis_shift={} must be in [0, 1]",
            mm.basis_shift
        ));
    }
    match mm.strategy {
        StrategyType::Basis => {
            if config.hedge.is_none() {
                errors.push(
                    "strategy=basis requires a [hedge] section with primary_symbol, \
                     hedge_symbol, and exchange config for the hedge leg"
                        .to_string(),
                );
            }
        }
        StrategyType::CrossVenueBasis => {
            if config.hedge.is_none() {
                errors.push(
                    "strategy=cross_venue_basis requires a [hedge] section with \
                     primary_symbol, hedge_symbol, and exchange config on a \
                     different venue from [exchange]"
                        .to_string(),
                );
            } else if let Some(hedge) = &config.hedge {
                if hedge.exchange.exchange_type == config.exchange.exchange_type {
                    warnings.push(
                        "strategy=cross_venue_basis with hedge.exchange == exchange — \
                         configure the hedge on a different venue, otherwise pick \
                         strategy=basis (same-venue) instead"
                            .to_string(),
                    );
                }
            }
            if mm.cross_venue_basis_max_staleness_ms <= 0 {
                errors.push(
                    "market_maker.cross_venue_basis_max_staleness_ms must be > 0 \
                     for strategy=cross_venue_basis (default 1500)"
                        .to_string(),
                );
            }
        }
        StrategyType::FundingArb => {
            if config.hedge.is_none() {
                errors.push(
                    "strategy=funding_arb requires a [hedge] section (spot ↔ perp pair)"
                        .to_string(),
                );
            }
            match &config.funding_arb {
                None => errors.push(
                    "strategy=funding_arb requires a [funding_arb] section with \
                     tick_interval_secs, min_rate_annual_pct, max_position, \
                     max_basis_bps, and enabled"
                        .to_string(),
                ),
                Some(fa) => {
                    if !fa.enabled {
                        warnings.push(
                            "funding_arb.enabled=false — driver will tick but never \
                             dispatch, effectively disabling cross-product trades"
                                .to_string(),
                        );
                    }
                    if fa.min_rate_annual_pct <= dec!(0) {
                        errors.push("funding_arb.min_rate_annual_pct must be > 0".to_string());
                    }
                    if fa.max_position <= dec!(0) {
                        errors.push("funding_arb.max_position must be > 0".to_string());
                    }
                    if fa.max_basis_bps <= dec!(0) {
                        errors.push("funding_arb.max_basis_bps must be > 0".to_string());
                    }
                    if fa.tick_interval_secs == 0 {
                        errors.push("funding_arb.tick_interval_secs must be > 0".to_string());
                    }
                }
            }
        }
        StrategyType::AvellanedaStoikov | StrategyType::Glft | StrategyType::Grid => {
            if config.hedge.is_some() {
                warnings.push(format!(
                    "[hedge] section is set but strategy={:?} is single-venue — \
                     the hedge connector will be built and market-data subscribed \
                     but never used for quoting",
                    mm.strategy
                ));
            }
        }
        StrategyType::CrossExchange => {
            if config.hedge.is_none() {
                errors.push(
                    "strategy=cross_exchange requires a [hedge] section — the \
                     strategy quotes on the primary venue only when a round-trip \
                     hedge on the hedge venue nets ≥ cross_exchange_min_profit_bps \
                     after fees"
                        .to_string(),
                );
            }
            if mm.cross_exchange_min_profit_bps <= dec!(0) {
                errors.push(
                    "market_maker.cross_exchange_min_profit_bps must be > 0 — a \
                     non-positive profit floor would quote unconditionally and \
                     burn fees on every round trip"
                        .to_string(),
                );
            }
        }
    }

    // --- VaR guard ---
    if mm.var_guard_enabled {
        if mm.var_guard_limit_95.is_none() && mm.var_guard_limit_99.is_none() {
            warnings.push(
                "var_guard_enabled=true but both limit_95 and limit_99 are None — \
                 VaR guard will never throttle"
                    .to_string(),
            );
        }
        if let Some(lambda) = mm.var_guard_ewma_lambda {
            if lambda <= dec!(0) || lambda >= dec!(1) {
                errors.push(format!(
                    "var_guard_ewma_lambda={} must be in (0, 1)",
                    lambda
                ));
            }
        }
    }

    // --- Multi-client isolation (Epic 1) ---
    if !config.clients.is_empty() {
        let mut client_ids = std::collections::HashSet::new();
        let mut symbol_owner: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for client in &config.clients {
            if client.id.trim().is_empty() {
                errors.push("clients[*].id must be non-empty".to_string());
            }
            if !client_ids.insert(client.id.clone()) {
                errors.push(format!("duplicate client id: {}", client.id));
            }
            if client.symbols.is_empty() {
                warnings.push(format!("client {} has no symbols assigned", client.id));
            }
            for sym in &client.symbols {
                if let Some(prev) = symbol_owner.get(sym) {
                    errors.push(format!(
                        "symbol {sym} assigned to both client {prev} and {} — each symbol must belong to exactly one client",
                        client.id
                    ));
                } else {
                    symbol_owner.insert(sym.clone(), client.id.clone());
                }
            }
        }
        // When clients are configured, warn if top-level symbols is also set.
        if !config.symbols.is_empty() {
            warnings.push(
                "both [clients] and top-level symbols are set — top-level symbols \
                 are ignored when clients are configured"
                    .to_string(),
            );
        }
    }

    // --- Listing sniper ---
    if config.listing_sniper.enabled && config.listing_sniper.scan_interval_secs < 10 {
        warnings.push(format!(
            "listing_sniper.scan_interval_secs={} is very aggressive, may hit rate limits",
            config.listing_sniper.scan_interval_secs
        ));
    }

    // --- Toxicity ---
    let tox = &config.toxicity;
    if tox.vpin_bucket_size <= dec!(0) {
        errors.push("toxicity.vpin_bucket_size must be > 0".to_string());
    }
    if tox.vpin_num_buckets == 0 {
        errors.push("toxicity.vpin_num_buckets must be > 0".to_string());
    }

    // P2.1 — per-asset-class kill switch sanity. Each symbol
    // listed in `kill_switch.asset_classes[*].symbols` must
    // exist in the top-level `symbols` array, and no symbol
    // may belong to more than one class. Either mistake means
    // an asset-wide escalation either misses an engine or
    // double-fires across classes.
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for cls in &config.kill_switch.asset_classes {
        if cls.name.trim().is_empty() {
            errors.push("kill_switch.asset_classes[*].name must be non-empty".to_string());
        }
        if cls.symbols.is_empty() {
            warnings.push(format!(
                "kill_switch.asset_classes.{} has no symbols — the class will never escalate",
                cls.name
            ));
        }
        for sym in &cls.symbols {
            if !config.symbols.contains(sym) {
                errors.push(format!(
                    "kill_switch.asset_classes.{}: symbol {} is not in the top-level [[symbols]] array",
                    cls.name, sym
                ));
            }
            if let Some(prev) = seen.get(sym) {
                errors.push(format!(
                    "symbol {sym} appears in two asset classes ({prev} and {}) — pick one",
                    cls.name
                ));
            } else {
                seen.insert(sym.clone(), cls.name.clone());
            }
        }
    }

    // Report.
    for w in &warnings {
        warn!("config warning: {w}");
    }

    if !errors.is_empty() {
        let msg = errors.join("; ");
        anyhow::bail!("config validation failed: {msg}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::{AppConfig, FundingArbCfg, HedgeConfig, HedgePairConfig};

    fn base_config() -> AppConfig {
        AppConfig::default()
    }

    fn valid_hedge() -> HedgeConfig {
        HedgeConfig {
            exchange: mm_common::config::ExchangeConfig {
                exchange_type: ExchangeType::Custom,
                rest_url: "http://localhost:8080".to_string(),
                ws_url: "ws://localhost:8080/ws".to_string(),
                api_key: Some("k".to_string()),
                api_secret: Some("s".to_string()),
                read_key: None,
                read_secret: None,
            },
            pair: HedgePairConfig {
                primary_symbol: "BTCUSDT".to_string(),
                hedge_symbol: "BTCUSDT-PERP".to_string(),
                multiplier: dec!(1),
                funding_interval_secs: Some(28_800),
                basis_threshold_bps: dec!(20),
            },
        }
    }

    fn valid_funding_arb() -> FundingArbCfg {
        FundingArbCfg {
            tick_interval_secs: 60,
            min_rate_annual_pct: dec!(10),
            max_position: dec!(0.1),
            max_basis_bps: dec!(50),
            enabled: true,
        }
    }

    #[test]
    fn default_config_is_valid() {
        assert!(validate_config(&base_config()).is_ok());
    }

    #[test]
    fn basis_strategy_without_hedge_is_rejected() {
        let mut cfg = base_config();
        cfg.market_maker.strategy = StrategyType::Basis;
        cfg.hedge = None;
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("strategy=basis requires a [hedge] section"),
            "{err}"
        );
    }

    #[test]
    fn basis_strategy_with_hedge_is_accepted() {
        let mut cfg = base_config();
        cfg.market_maker.strategy = StrategyType::Basis;
        cfg.hedge = Some(valid_hedge());
        validate_config(&cfg).unwrap();
    }

    #[test]
    fn funding_arb_without_hedge_is_rejected() {
        let mut cfg = base_config();
        cfg.market_maker.strategy = StrategyType::FundingArb;
        cfg.hedge = None;
        cfg.funding_arb = Some(valid_funding_arb());
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("requires a [hedge] section"), "{err}");
    }

    #[test]
    fn funding_arb_without_funding_arb_section_is_rejected() {
        let mut cfg = base_config();
        cfg.market_maker.strategy = StrategyType::FundingArb;
        cfg.hedge = Some(valid_hedge());
        cfg.funding_arb = None;
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("requires a [funding_arb] section"), "{err}");
    }

    #[test]
    fn funding_arb_with_bad_values_is_rejected() {
        let mut cfg = base_config();
        cfg.market_maker.strategy = StrategyType::FundingArb;
        cfg.hedge = Some(valid_hedge());
        let mut fa = valid_funding_arb();
        fa.min_rate_annual_pct = dec!(0);
        fa.tick_interval_secs = 0;
        cfg.funding_arb = Some(fa);
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("min_rate_annual_pct"), "{err}");
        assert!(err.contains("tick_interval_secs"), "{err}");
    }

    #[test]
    fn basis_shift_out_of_range_is_rejected() {
        let mut cfg = base_config();
        cfg.market_maker.basis_shift = dec!(1.5);
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("basis_shift"), "{err}");
    }

    #[test]
    fn basis_shift_zero_and_one_are_valid() {
        let mut cfg = base_config();
        cfg.market_maker.basis_shift = dec!(0);
        validate_config(&cfg).unwrap();
        cfg.market_maker.basis_shift = dec!(1);
        validate_config(&cfg).unwrap();
    }

    #[test]
    fn clients_with_duplicate_symbol_is_rejected() {
        let mut cfg = base_config();
        cfg.clients = vec![
            mm_common::config::ClientConfig {
                id: "alice".into(),
                name: "Alice".into(),
                symbols: vec!["BTCUSDT".into()],
                sla: None,
                webhook_urls: vec![],
                api_keys: vec![],
                report_branding: None,
                daily_loss_limit_usd: None,
            },
            mm_common::config::ClientConfig {
                id: "bob".into(),
                name: "Bob".into(),
                symbols: vec!["BTCUSDT".into(), "ETHUSDT".into()],
                sla: None,
                webhook_urls: vec![],
                api_keys: vec![],
                report_branding: None,
                daily_loss_limit_usd: None,
            },
        ];
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("BTCUSDT"), "{err}");
        assert!(err.contains("assigned to both"), "{err}");
    }

    #[test]
    fn clients_with_duplicate_id_is_rejected() {
        let mut cfg = base_config();
        cfg.clients = vec![
            mm_common::config::ClientConfig {
                id: "alice".into(),
                name: "Alice".into(),
                symbols: vec!["BTCUSDT".into()],
                sla: None,
                webhook_urls: vec![],
                api_keys: vec![],
                report_branding: None,
                daily_loss_limit_usd: None,
            },
            mm_common::config::ClientConfig {
                id: "alice".into(),
                name: "Alice 2".into(),
                symbols: vec!["ETHUSDT".into()],
                sla: None,
                webhook_urls: vec![],
                api_keys: vec![],
                report_branding: None,
                daily_loss_limit_usd: None,
            },
        ];
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("duplicate client id"), "{err}");
    }

    #[test]
    fn clients_disjoint_symbols_accepted() {
        let mut cfg = base_config();
        cfg.clients = vec![
            mm_common::config::ClientConfig {
                id: "alice".into(),
                name: "Alice".into(),
                symbols: vec!["BTCUSDT".into()],
                sla: None,
                webhook_urls: vec![],
                api_keys: vec![],
                report_branding: None,
                daily_loss_limit_usd: None,
            },
            mm_common::config::ClientConfig {
                id: "bob".into(),
                name: "Bob".into(),
                symbols: vec!["ETHUSDT".into()],
                sla: None,
                webhook_urls: vec![],
                api_keys: vec![],
                report_branding: None,
                daily_loss_limit_usd: None,
            },
        ];
        // clients mode — top-level symbols is ignored, but warns
        validate_config(&cfg).unwrap();
    }

    #[test]
    fn empty_clients_uses_top_level_symbols() {
        let cfg = base_config();
        assert!(cfg.clients.is_empty());
        let eff = cfg.effective_clients();
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].id, "default");
        assert_eq!(eff[0].symbols, cfg.symbols);
    }

    #[test]
    fn effective_clients_returns_configured_when_present() {
        let mut cfg = base_config();
        cfg.clients = vec![mm_common::config::ClientConfig {
            id: "acme".into(),
            name: "Acme".into(),
            symbols: vec!["SOLUSDT".into()],
            sla: None,
            webhook_urls: vec![],
            api_keys: vec![],
            report_branding: None,
            daily_loss_limit_usd: None,
        }];
        let eff = cfg.effective_clients();
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].id, "acme");
    }

    #[test]
    fn funding_arb_disabled_emits_warning_not_error() {
        let mut cfg = base_config();
        cfg.market_maker.strategy = StrategyType::FundingArb;
        cfg.hedge = Some(valid_hedge());
        let mut fa = valid_funding_arb();
        fa.enabled = false;
        cfg.funding_arb = Some(fa);
        // Warning, not error — accepted.
        validate_config(&cfg).unwrap();
    }
}
