use mm_common::config::AppConfig;
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
    if config.exchange.rest_url.is_empty() {
        errors.push("exchange.rest_url is required".to_string());
    }
    if config.exchange.ws_url.is_empty() {
        errors.push("exchange.ws_url is required".to_string());
    }

    // --- Symbols ---
    if config.symbols.is_empty() {
        errors.push("at least one symbol is required".to_string());
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
