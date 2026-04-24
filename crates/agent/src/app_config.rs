//! AppConfig adapter for agent-driven deployments.
//!
//! The `mm-engine::MarketMakerEngine` was designed around
//! [`mm_common::config::AppConfig`] — the monolithic TOML config
//! that single-process `mm-server` reads. In the controller/agent
//! world the operator never authors such a blob; instead they
//! push [`DesiredStrategy`] descriptors from the controller, and the
//! agent owns the venue credentials + safety rails locally via
//! [`SettingsFile`].
//!
//! This module bridges those two worlds. Given a desired strategy
//! + a resolved credential + the agent's local settings, it
//! produces an `AppConfig` ready to hand to
//! `MarketMakerEngine::new`. The produced config is functional —
//! not merely parseable — with sensible defaults derived from
//! the `binance-paper.toml` reference profile.
//!
//! Fields sourced by layer:
//! - **symbol** → `desired.symbol`
//! - **exchange_type / product / api_key / api_secret** →
//!   `ResolvedCredential` (after the settings file resolved the
//!   env-backed secrets)
//! - **mode** → `"paper"` when
//!   `settings.features.paper_fill_simulation`, else `"live"`
//! - **daily_loss_limit / max_drawdown / max_message_rate** →
//!   `settings.rails` when present, kill-switch defaults otherwise
//! - **strategy** → mapped from `desired.template` via
//!   [`template_to_strategy_type`]
//! - **gamma / kappa / sigma / spread_bps / order_size** →
//!   `desired.variables` overrides when present, maker-strategy
//!   defaults otherwise
//!
//! PR-2c-iii-a intentionally stops before wiring the config into
//! `MarketMakerEngine::new()` at runtime — that step is phase-b.
//! This module ships the adapter + tests today so the runtime
//! wire-up in phase-b is a mechanical plug-in.

use anyhow::{Context, Result};
use mm_common::config::{
    AppConfig, ExchangeConfig, HedgeConfig, HedgePairConfig, SorVenueConfig, StrategyType,
};
use mm_common::settings::{ResolvedCredential, SettingsFile};
use mm_control::messages::DesiredStrategy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Baseline TOML — lifted from the proven
/// `config/binance-paper.toml` with the venue bits + credentials
/// stripped. Agent overrides those per-deployment.
const BASELINE_TOML: &str = r#"
symbols = []
mode = "paper"
dashboard_port = 0
checkpoint_path = "data/checkpoint-agent.json"
log_file = ""

[exchange]
exchange_type = "binance"
product = "spot"

[market_maker]
strategy = "avellaneda_stoikov"
gamma = "0.1"
kappa = "42.3124"
sigma = "0.000053"
time_horizon_secs = 300
num_levels = 3
order_size = "0.001"
refresh_interval_ms = 500
min_spread_bps = "3"
max_distance_bps = "80"
momentum_enabled = true
momentum_window = 200
market_resilience_enabled = true
otr_enabled = true
hma_enabled = true
hma_window = 9
user_stream_enabled = false
inventory_drift_tolerance = "0.00001"
inventory_drift_auto_correct = false
adaptive_enabled = true
apply_pair_class_template = true

[risk]
# 10× order_size — lets the strategy post a few levels before
# hitting the INV-1 hard cap. Previous value of "0.001" matched
# `order_size` exactly, which meant every single quote tripped
# the pre-send inventory check (strategy's level-sizing layer
# scales by 1.2× for higher levels, so even level 0 came in
# above the cap). Operators tune this per symbol.
max_inventory = "0.01"
max_exposure_quote = "500"
max_drawdown_quote = "50"
inventory_skew_factor = "1.0"
max_spread_bps = "200"
stale_book_timeout_secs = 10
max_order_size = "0.005"
max_daily_volume_quote = "5000"
max_hourly_volume_quote = "1500"

[kill_switch]
daily_loss_limit = "100"
daily_loss_warning = "50"
max_position_value = "1000"
max_message_rate = 60
max_consecutive_errors = 10

[sla]
max_spread_bps = "100"
min_depth_quote = "2000"
min_uptime_pct = "95"
two_sided_required = true
max_requote_secs = 5
min_order_rest_secs = 3

[toxicity]
vpin_bucket_size = "50000"
vpin_num_buckets = 50
vpin_threshold = "0.7"
kyle_window = 100
adverse_selection_lookback_ms = 3000
autotune_enabled = true

[listing_sniper]
enabled = false
scan_interval_secs = 300
alert_on_discovery = true
"#;

/// Build an AppConfig for a single deployment. Caller-supplied
/// layers override the baseline: credential → exchange + keys,
/// settings → mode + rails, desired → symbol + strategy +
/// variable overrides.
///
/// `hedge_credential` is populated when the deployment binds a
/// hedge leg (`desired.bindings.hedge`). The agent's reconcile
/// loop resolves that id through the catalog before calling
/// this builder; `None` here means "single-venue deployment".
pub fn build_agent_config(
    desired: &DesiredStrategy,
    credential: &ResolvedCredential,
    hedge_credential: Option<&ResolvedCredential>,
    extra_credentials: &[ResolvedCredential],
    settings: &SettingsFile,
) -> Result<AppConfig> {
    let mut cfg: AppConfig = toml::from_str(BASELINE_TOML)
        .context("baseline agent AppConfig TOML failed to parse — code bug")?;

    // Symbol + venue wiring.
    cfg.symbols = vec![desired.symbol.clone()];
    cfg.exchange.exchange_type = credential.exchange;
    cfg.exchange.product = credential.product;
    cfg.exchange.api_key = Some(credential.api_key.clone());
    cfg.exchange.api_secret = Some(credential.api_secret.clone());

    // Hedge leg — populated when the deployment binds a second
    // credential. Primary/hedge share the base asset so both
    // connectors trade the same underlying against (usually) the
    // same quote; the `multiplier` + `basis_threshold_bps`
    // defaults mirror the `cross-exchange-paper.toml` reference.
    if let Some(hedge_cred) = hedge_credential {
        let hedge_symbol = hedge_cred
            .default_symbol
            .clone()
            .unwrap_or_else(|| desired.symbol.clone());
        cfg.hedge = Some(HedgeConfig {
            exchange: ExchangeConfig {
                exchange_type: hedge_cred.exchange,
                product: hedge_cred.product,
                rest_url: String::new(),
                ws_url: String::new(),
                api_key: Some(hedge_cred.api_key.clone()),
                api_secret: Some(hedge_cred.api_secret.clone()),
                read_key: None,
                read_secret: None,
                withdraw_whitelist: None,
            },
            pair: HedgePairConfig {
                primary_symbol: desired.symbol.clone(),
                hedge_symbol,
                multiplier: variable_decimal(desired, "hedge_multiplier").unwrap_or(dec!(1)),
                funding_interval_secs: None,
                basis_threshold_bps: variable_decimal(desired, "basis_threshold_bps")
                    .unwrap_or(dec!(20)),
            },
        });
    }

    // Mode — operator-facing toggle, read per-deployment. The
    // old behaviour hardcoded mode from agent-wide
    // `paper_fill_simulation` flag which defaulted false: every
    // deployment silently went live even when the operator
    // intended paper. Now the deployment's `variables.mode`
    // wins; agent setting is the fallback; if NEITHER is set
    // we default to `paper` because the safer mistake is
    // "operator thought it was live, it wasn't" rather than
    // "operator thought it was paper, it wasn't".
    cfg.mode = desired
        .variables
        .get("mode")
        .and_then(|v| v.as_str())
        .filter(|s| matches!(*s, "paper" | "live" | "smoke"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if settings.features.paper_fill_simulation {
                "paper".into()
            } else {
                // Safer default — was "live", now "paper".
                // Operators explicitly opt into live by either
                // enabling the agent feature flag OR passing
                // `variables.mode = "live"` per deployment.
                "paper".into()
            }
        });

    // SOR extras — credentials that are NOT quoted against but
    // participate in the cross-venue router's pool. One entry
    // per resolved extra credential; the symbol defaults to the
    // deployment's primary symbol unless the credential's
    // `default_symbol` overrides (common when an extra venue
    // uses a different ticker convention for the same asset).
    let default_extra_qty =
        variable_decimal(desired, "sor_extra_max_inventory").unwrap_or(dec!(0.005));
    for extra in extra_credentials {
        let symbol = extra
            .default_symbol
            .clone()
            .unwrap_or_else(|| desired.symbol.clone());
        let max_inventory = extra.max_notional_quote.unwrap_or(default_extra_qty);
        cfg.sor_extra_venues.push(SorVenueConfig {
            exchange: ExchangeConfig {
                exchange_type: extra.exchange,
                product: extra.product,
                rest_url: String::new(),
                ws_url: String::new(),
                api_key: Some(extra.api_key.clone()),
                api_secret: Some(extra.api_secret.clone()),
                read_key: None,
                read_secret: None,
                withdraw_whitelist: None,
            },
            symbol,
            max_inventory,
        });
    }

    // Rails — if the operator set global limits, they win over
    // the baseline.
    if let Some(v) = settings.rails.daily_loss_limit {
        cfg.kill_switch.daily_loss_limit = v;
    }
    if let Some(v) = settings.rails.max_message_rate {
        cfg.kill_switch.max_message_rate = v;
    }
    if let Some(v) = settings.rails.stale_book_timeout_secs {
        cfg.risk.stale_book_timeout_secs = v;
    }

    // Strategy type derived from the template name.
    cfg.market_maker.strategy = template_to_strategy_type(&desired.template);

    // Variable overrides from the DesiredStrategy — the
    // operator-facing knobs that tune a specific deployment on
    // top of the strategy template's defaults.
    apply_variable_overrides(&mut cfg, desired)?;

    Ok(cfg)
}

/// Map a template name to its [`StrategyType`]. Unknown templates
/// fall through to Avellaneda-Stoikov — safer than erroring at
/// config build time because the agent can log the mismatch and
/// still produce a sane config for an operator-level fix.
fn template_to_strategy_type(template: &str) -> StrategyType {
    match template {
        "avellaneda-via-graph" | "avellaneda-stoikov" | "avellaneda" => {
            StrategyType::AvellanedaStoikov
        }
        "glft-via-graph" | "glft" => StrategyType::Glft,
        "grid-via-graph" | "grid" => StrategyType::Grid,
        "cross-exchange-basic" | "cross-exchange" | "xemm-reactive" => StrategyType::CrossExchange,
        "basis-carry-spot-perp" | "basis" => StrategyType::Basis,
        "funding-aware-quoter" | "funding_arb" => StrategyType::FundingArb,
        "stat_arb" => StrategyType::StatArb,
        unknown => {
            tracing::warn!(
                template = %unknown,
                "unknown template — falling back to Avellaneda-Stoikov. \
                 Update template_to_strategy_type when the template is added."
            );
            StrategyType::AvellanedaStoikov
        }
    }
}

/// Apply deployment-time knob overrides from [`DesiredStrategy::variables`].
/// Supported keys (all optional — unknown keys are logged + ignored):
/// - `gamma`, `kappa`, `sigma` (Decimal strings)
/// - `num_levels` (integer)
/// - `order_size` (Decimal string)
/// - `min_spread_bps`, `max_distance_bps` (Decimal strings)
/// - `refresh_interval_ms` (integer)
fn apply_variable_overrides(cfg: &mut AppConfig, desired: &DesiredStrategy) -> Result<()> {
    for (k, v) in &desired.variables {
        match k.as_str() {
            "gamma" => {
                if let Some(d) = parse_decimal(v) {
                    cfg.market_maker.gamma = d;
                }
            }
            "kappa" => {
                if let Some(d) = parse_decimal(v) {
                    cfg.market_maker.kappa = d;
                }
            }
            "sigma" => {
                if let Some(d) = parse_decimal(v) {
                    cfg.market_maker.sigma = d;
                }
            }
            "order_size" => {
                if let Some(d) = parse_decimal(v) {
                    cfg.market_maker.order_size = d;
                }
            }
            "min_spread_bps" => {
                if let Some(d) = parse_decimal(v) {
                    cfg.market_maker.min_spread_bps = d;
                }
            }
            "max_distance_bps" => {
                if let Some(d) = parse_decimal(v) {
                    cfg.market_maker.max_distance_bps = d;
                }
            }
            "num_levels" => {
                if let Some(n) = v.as_u64() {
                    cfg.market_maker.num_levels = n as usize;
                }
            }
            "refresh_interval_ms" => {
                if let Some(n) = v.as_u64() {
                    cfg.market_maker.refresh_interval_ms = n;
                }
            }
            other => {
                tracing::debug!(key = %other, "ignoring unknown deployment variable");
            }
        }
    }
    Ok(())
}

/// Look up a Decimal-valued variable on `desired.variables` and
/// parse it. Returns `None` if the key is absent or the value is
/// not a parseable Decimal — caller substitutes its own default.
fn variable_decimal(desired: &DesiredStrategy, key: &str) -> Option<Decimal> {
    desired.variables.get(key).and_then(parse_decimal)
}

/// Parse a Decimal from a JSON value that could be a string or
/// a numeric literal. Strings are preferred because JSON numbers
/// quietly lose precision for decimals.
fn parse_decimal(v: &serde_json::Value) -> Option<Decimal> {
    match v {
        serde_json::Value::String(s) => s.parse::<Decimal>().ok(),
        serde_json::Value::Number(n) => n.as_f64().and_then(|f| Decimal::try_from(f).ok()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::{ExchangeType, ProductType};

    fn sample_credential() -> ResolvedCredential {
        ResolvedCredential {
            id: "binance_spot_main".into(),
            exchange: ExchangeType::Binance,
            product: ProductType::Spot,
            api_key: "test-key".into(),
            api_secret: "test-secret".into(),
            max_notional_quote: None,
            default_symbol: None,
        }
    }

    fn sample_settings(paper: bool) -> SettingsFile {
        let toml = format!(
            r#"
            [agent]
            id = "test-agent"

            [features]
            paper_fill_simulation = {paper}

            [rails]
            daily_loss_limit = "25"
            max_message_rate = 30
            stale_book_timeout_secs = 15
            "#
        );
        SettingsFile::from_str(&toml).unwrap()
    }

    fn sample_desired() -> DesiredStrategy {
        DesiredStrategy {
            deployment_id: "dep-1".into(),
            template: "avellaneda-via-graph".into(),
            symbol: "BTCUSDT".into(),
            ..Default::default()
        }
    }

    #[test]
    fn baseline_parses_standalone() {
        // Sanity: the inlined baseline must always round-trip —
        // a broken baseline breaks every deployment.
        let _: AppConfig = toml::from_str(BASELINE_TOML).expect("baseline parses");
    }

    #[test]
    fn credential_populates_exchange_and_keys() {
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.exchange.exchange_type, ExchangeType::Binance);
        assert_eq!(cfg.exchange.product, ProductType::Spot);
        assert_eq!(cfg.exchange.api_key.as_deref(), Some("test-key"));
        assert_eq!(cfg.exchange.api_secret.as_deref(), Some("test-secret"));
    }

    #[test]
    fn symbol_list_carries_desired_symbol() {
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.symbols, vec!["BTCUSDT"]);
    }

    #[test]
    fn mode_reads_from_variables_with_paper_fallback() {
        // No `variables.mode` + agent has paper_fill_simulation → paper.
        let paper_ff = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(paper_ff.mode, "paper");
        // No `variables.mode` + agent has NO paper_fill_simulation →
        // NEW SAFER DEFAULT: paper. Previous behaviour silently went
        // live which meant deploying the default TOML on a real agent
        // immediately tried signed endpoints with every credential —
        // operators who intended paper ended up live. Operators opt
        // into live explicitly now via `variables.mode = "live"`.
        let default_no_ff = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &[],
            &sample_settings(false),
        )
        .unwrap();
        assert_eq!(default_no_ff.mode, "paper");
        // Explicit `variables.mode = "live"` overrides.
        let mut live_desc = sample_desired();
        live_desc
            .variables
            .insert("mode".into(), serde_json::Value::String("live".into()));
        let live = build_agent_config(
            &live_desc,
            &sample_credential(),
            None,
            &[],
            &sample_settings(false),
        )
        .unwrap();
        assert_eq!(live.mode, "live");
    }

    #[test]
    fn rails_override_kill_switch_defaults() {
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.kill_switch.daily_loss_limit, "25".parse().unwrap());
        assert_eq!(cfg.kill_switch.max_message_rate, 30);
        assert_eq!(cfg.risk.stale_book_timeout_secs, 15);
    }

    #[test]
    fn variable_override_applies_to_market_maker() {
        let mut desired = sample_desired();
        desired
            .variables
            .insert("gamma".into(), serde_json::json!("0.25"));
        desired
            .variables
            .insert("num_levels".into(), serde_json::json!(5));
        desired
            .variables
            .insert("min_spread_bps".into(), serde_json::json!("7"));
        let cfg = build_agent_config(
            &desired,
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.market_maker.gamma, "0.25".parse().unwrap());
        assert_eq!(cfg.market_maker.num_levels, 5);
        assert_eq!(cfg.market_maker.min_spread_bps, "7".parse().unwrap());
    }

    #[test]
    fn template_name_maps_to_strategy_type() {
        let mut desired = sample_desired();
        desired.template = "glft-via-graph".into();
        let cfg = build_agent_config(
            &desired,
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.market_maker.strategy, StrategyType::Glft);

        desired.template = "cross-exchange-basic".into();
        let cfg = build_agent_config(
            &desired,
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.market_maker.strategy, StrategyType::CrossExchange);
    }

    #[test]
    fn unknown_template_falls_back_to_avellaneda() {
        let mut desired = sample_desired();
        desired.template = "not-a-real-template".into();
        let cfg = build_agent_config(
            &desired,
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.market_maker.strategy, StrategyType::AvellanedaStoikov);
    }

    fn bybit_perp_credential() -> ResolvedCredential {
        ResolvedCredential {
            id: "bybit_perp_hedge".into(),
            exchange: ExchangeType::Bybit,
            product: ProductType::LinearPerp,
            api_key: "hedge-key".into(),
            api_secret: "hedge-secret".into(),
            max_notional_quote: None,
            default_symbol: Some("BTCUSDT".into()),
        }
    }

    #[test]
    fn hedge_credential_populates_hedge_block() {
        let hedge = bybit_perp_credential();
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            Some(&hedge),
            &[],
            &sample_settings(true),
        )
        .unwrap();
        let h = cfg.hedge.expect("hedge populated");
        assert_eq!(h.exchange.exchange_type, ExchangeType::Bybit);
        assert_eq!(h.exchange.product, ProductType::LinearPerp);
        assert_eq!(h.exchange.api_key.as_deref(), Some("hedge-key"));
        assert_eq!(h.pair.primary_symbol, "BTCUSDT");
        assert_eq!(h.pair.hedge_symbol, "BTCUSDT");
        assert_eq!(h.pair.multiplier, "1".parse().unwrap());
        assert_eq!(h.pair.basis_threshold_bps, "20".parse().unwrap());
    }

    #[test]
    fn hedge_uses_credential_default_symbol_when_different() {
        let mut hedge = bybit_perp_credential();
        hedge.default_symbol = Some("BTCPERP".into());
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            Some(&hedge),
            &[],
            &sample_settings(true),
        )
        .unwrap();
        let h = cfg.hedge.unwrap();
        assert_eq!(h.pair.primary_symbol, "BTCUSDT");
        assert_eq!(h.pair.hedge_symbol, "BTCPERP");
    }

    #[test]
    fn hedge_variables_override_multiplier_and_basis() {
        let mut desired = sample_desired();
        desired
            .variables
            .insert("hedge_multiplier".into(), serde_json::json!("2.5"));
        desired
            .variables
            .insert("basis_threshold_bps".into(), serde_json::json!("35"));
        let cfg = build_agent_config(
            &desired,
            &sample_credential(),
            Some(&bybit_perp_credential()),
            &[],
            &sample_settings(true),
        )
        .unwrap();
        let h = cfg.hedge.unwrap();
        assert_eq!(h.pair.multiplier, "2.5".parse().unwrap());
        assert_eq!(h.pair.basis_threshold_bps, "35".parse().unwrap());
    }

    #[test]
    fn no_hedge_credential_leaves_hedge_block_empty() {
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert!(cfg.hedge.is_none());
    }

    fn binance_perp_signal_credential() -> ResolvedCredential {
        ResolvedCredential {
            id: "binance_perp_signal".into(),
            exchange: ExchangeType::Binance,
            product: ProductType::LinearPerp,
            api_key: "extra-key".into(),
            api_secret: "extra-secret".into(),
            max_notional_quote: Some("0.01".parse().unwrap()),
            default_symbol: None,
        }
    }

    #[test]
    fn extra_credentials_populate_sor_extra_venues() {
        let extras = vec![binance_perp_signal_credential()];
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &extras,
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.sor_extra_venues.len(), 1);
        let e = &cfg.sor_extra_venues[0];
        assert_eq!(e.exchange.exchange_type, ExchangeType::Binance);
        assert_eq!(e.exchange.product, ProductType::LinearPerp);
        assert_eq!(e.exchange.api_key.as_deref(), Some("extra-key"));
        // default_symbol is None on this credential → falls back
        // to the deployment's primary symbol.
        assert_eq!(e.symbol, "BTCUSDT");
        // max_notional_quote on the credential wins over the
        // variable default.
        assert_eq!(e.max_inventory, "0.01".parse().unwrap());
    }

    #[test]
    fn extra_uses_variable_default_when_credential_has_no_cap() {
        let mut cred = binance_perp_signal_credential();
        cred.max_notional_quote = None;
        let mut desired = sample_desired();
        desired
            .variables
            .insert("sor_extra_max_inventory".into(), serde_json::json!("0.02"));
        let cfg = build_agent_config(
            &desired,
            &sample_credential(),
            None,
            &[cred],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(
            cfg.sor_extra_venues[0].max_inventory,
            "0.02".parse().unwrap()
        );
    }

    #[test]
    fn extra_credential_default_symbol_overrides_deployment_symbol() {
        let mut cred = binance_perp_signal_credential();
        cred.default_symbol = Some("BTCUSD_PERP".into());
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &[cred],
            &sample_settings(true),
        )
        .unwrap();
        assert_eq!(cfg.sor_extra_venues[0].symbol, "BTCUSD_PERP");
    }

    #[test]
    fn no_extras_leaves_sor_list_empty() {
        let cfg = build_agent_config(
            &sample_desired(),
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        )
        .unwrap();
        assert!(cfg.sor_extra_venues.is_empty());
    }

    #[test]
    fn unknown_variable_is_logged_not_fatal() {
        let mut desired = sample_desired();
        desired
            .variables
            .insert("total_bogus_knob".into(), serde_json::json!("x"));
        let cfg = build_agent_config(
            &desired,
            &sample_credential(),
            None,
            &[],
            &sample_settings(true),
        );
        assert!(cfg.is_ok(), "unknown variables survive without crashing");
    }
}
