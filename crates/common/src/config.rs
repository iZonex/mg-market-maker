use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub exchange: ExchangeConfig,
    pub market_maker: MarketMakerConfig,
    pub risk: RiskConfig,
    pub kill_switch: KillSwitchCfg,
    pub sla: SlaObligationConfig,
    pub toxicity: ToxicityConfig,
    pub symbols: Vec<String>,

    /// Dashboard HTTP port (0 = disabled).
    #[serde(default = "default_dashboard_port")]
    pub dashboard_port: u16,

    /// Path for checkpoint file.
    #[serde(default = "default_checkpoint_path")]
    pub checkpoint_path: String,

    /// Log file path (empty = stdout only).
    #[serde(default)]
    pub log_file: String,

    /// Trading mode: "live" or "paper".
    #[serde(default = "default_mode")]
    pub mode: String,

    /// Pre-configured API users.
    #[serde(default)]
    pub users: Vec<UserConfig>,

    /// Telegram alert config (populated from env vars).
    #[serde(default)]
    pub telegram: TelegramAlertConfig,

    /// Per-symbol loan configuration (keyed by symbol, e.g., "BTCUSDT").
    #[serde(default)]
    pub loans: std::collections::HashMap<String, LoanConfig>,

    /// Optional hedge connector for cross-product strategies.
    ///
    /// When set, the engine builds a `ConnectorBundle` with both
    /// primary and hedge connectors and maintains a second
    /// `BookKeeper` for the hedge leg. Cross-product strategies
    /// (`BasisStrategy`, `FundingArbExecutor`) consume the hedge
    /// book via `StrategyContext.ref_price`.
    #[serde(default)]
    pub hedge: Option<HedgeConfig>,

    /// Funding-arb driver config. Required when
    /// `StrategyType::FundingArb` is selected; ignored otherwise.
    #[serde(default)]
    pub funding_arb: Option<FundingArbCfg>,
}

/// Serializable shape of `FundingArbDriverConfig` — same fields,
/// plus flat TOML-friendly representations of the nested
/// `FundingArbConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingArbCfg {
    /// Driver tick interval in seconds. Default 60 s.
    #[serde(default = "default_funding_tick_secs")]
    pub tick_interval_secs: u64,
    /// Minimum annualised funding rate (%) required to open a
    /// position. Default 10.
    #[serde(default = "default_min_rate_apr")]
    pub min_rate_annual_pct: Decimal,
    /// Maximum position size in base asset per pair. Default 0.1.
    #[serde(default = "default_max_position")]
    pub max_position: Decimal,
    /// Maximum basis deviation (bps) before forcing exit.
    #[serde(default = "default_max_basis")]
    pub max_basis_bps: Decimal,
    /// Enable the driver. Safety default is `false` so a
    /// stray config section does not unexpectedly start
    /// trading cross-product.
    #[serde(default)]
    pub enabled: bool,
}

fn default_funding_tick_secs() -> u64 {
    60
}
fn default_min_rate_apr() -> Decimal {
    dec!(10)
}
fn default_max_position() -> Decimal {
    dec!(0.1)
}
fn default_max_basis() -> Decimal {
    dec!(50)
}

/// Hedge-leg exchange + instrument pair config.
///
/// The hedge exchange config mirrors `ExchangeConfig`; the
/// `pair` describes how the primary symbol relates to the hedge
/// symbol. Strategies that do not need a hedge leg leave this
/// unset and the engine runs in single-connector mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HedgeConfig {
    pub exchange: ExchangeConfig,
    pub pair: HedgePairConfig,
}

/// Serializable flavour of `InstrumentPair` for TOML config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HedgePairConfig {
    pub primary_symbol: String,
    pub hedge_symbol: String,
    #[serde(default = "default_multiplier")]
    pub multiplier: Decimal,
    #[serde(default)]
    pub funding_interval_secs: Option<u64>,
    #[serde(default = "default_basis_threshold_bps")]
    pub basis_threshold_bps: Decimal,
}

fn default_multiplier() -> Decimal {
    dec!(1)
}
fn default_basis_threshold_bps() -> Decimal {
    dec!(20)
}

/// Pre-configured user for dashboard access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    pub id: String,
    pub name: String,
    /// "admin", "operator", or "viewer".
    pub role: String,
    pub api_key: String,
    /// Optional: restrict to specific symbols.
    #[serde(default)]
    pub allowed_symbols: Vec<String>,
}

fn default_dashboard_port() -> u16 {
    9090
}
fn default_checkpoint_path() -> String {
    "data/checkpoint.json".into()
}
fn default_mode() -> String {
    "live".into()
}

/// Strategy selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyType {
    AvellanedaStoikov,
    Glft,
    Grid,
    /// Basis-aware quoting for cross-product MM. Requires
    /// `AppConfig.hedge` to be set so the engine can build a
    /// `ConnectorBundle` with the hedge-leg price reference.
    /// See `BasisStrategy` and `docs/research/spot-mm-specifics.md` §10.
    Basis,
    /// Funding-rate arbitrage driver. Runs a `BasisStrategy`
    /// for the primary quoting leg AND spins up a
    /// `FundingArbDriver` that periodically samples the hedge
    /// venue's funding rate + both mids and dispatches
    /// atomic-pair entries/exits via `FundingArbExecutor`.
    /// Requires `AppConfig.hedge` and `funding_arb` sections.
    FundingArb,
}

/// Which exchange to connect to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeType {
    #[default]
    Custom,
    Binance,
    BinanceTestnet,
    Bybit,
    BybitTestnet,
    HyperLiquid,
    HyperLiquidTestnet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    /// Exchange type: custom, binance, binance_testnet, bybit, bybit_testnet.
    #[serde(default)]
    pub exchange_type: ExchangeType,
    pub rest_url: String,
    pub ws_url: String,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketMakerConfig {
    /// Risk aversion parameter (γ) for Avellaneda-Stoikov.
    /// Higher = tighter spread, less inventory risk.
    pub gamma: Decimal,

    /// Order arrival intensity (κ).
    /// Higher = more aggressive quoting (tighter spread).
    pub kappa: Decimal,

    /// Volatility estimate (σ) — annualized, overridden by live calc if available.
    pub sigma: Decimal,

    /// Time horizon in seconds for the strategy cycle.
    pub time_horizon_secs: u64,

    /// Number of quote levels on each side.
    pub num_levels: usize,

    /// Base order size in base asset.
    pub order_size: Decimal,

    /// How often to refresh quotes (milliseconds).
    pub refresh_interval_ms: u64,

    /// Minimum spread in bps — never quote tighter than this.
    pub min_spread_bps: Decimal,

    /// Maximum distance from mid in bps for outermost level.
    pub max_distance_bps: Decimal,

    /// Strategy type.
    #[serde(default = "default_strategy")]
    pub strategy: StrategyType,

    /// Enable momentum (alpha) signals to shift reservation price.
    #[serde(default = "default_true")]
    pub momentum_enabled: bool,

    /// Momentum signal window size (number of recent trades).
    #[serde(default = "default_momentum_window")]
    pub momentum_window: usize,

    /// `BasisStrategy` reservation-price shift toward the hedge
    /// mid, in `[0, 1]`. Default 0.5. Ignored when strategy is
    /// not `Basis`.
    #[serde(default = "default_basis_shift")]
    pub basis_shift: Decimal,
}

fn default_basis_shift() -> Decimal {
    dec!(0.5)
}

fn default_strategy() -> StrategyType {
    StrategyType::AvellanedaStoikov
}
fn default_true() -> bool {
    true
}
fn default_momentum_window() -> usize {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Maximum absolute inventory in base asset.
    pub max_inventory: Decimal,

    /// Maximum position value in quote asset.
    pub max_exposure_quote: Decimal,

    /// Maximum drawdown in quote asset before circuit breaker trips.
    pub max_drawdown_quote: Decimal,

    /// Inventory skew factor — how aggressively to skew quotes.
    /// 0 = no skew, 1.0 = full Avellaneda skew.
    pub inventory_skew_factor: Decimal,

    /// If spread exceeds this (bps), pause quoting (likely manipulation).
    pub max_spread_bps: Decimal,

    /// Soft spread gate. When `Some(bps)` and the current book
    /// spread exceeds it, `refresh_quotes` skips quoting for
    /// this tick **without** tripping the circuit breaker. Use
    /// for transient wide-spread events (book resync, thin-book
    /// volatility blip) where a full cancel-all is overkill —
    /// next tick resumes quoting when the spread narrows. Set
    /// to `None` to disable. Typical value: tighter than
    /// `max_spread_bps` so the soft gate catches degradation
    /// before the hard circuit breaker trips.
    #[serde(default)]
    pub max_spread_to_quote_bps: Option<Decimal>,

    /// Seconds without a book update before we cancel all orders.
    pub stale_book_timeout_secs: u64,

    /// Maximum single order size in base asset (0 = unlimited).
    #[serde(default)]
    pub max_order_size: Decimal,

    /// Maximum daily trade volume in quote asset (0 = unlimited).
    #[serde(default)]
    pub max_daily_volume_quote: Decimal,

    /// Maximum hourly trade volume in quote asset (0 = unlimited).
    #[serde(default)]
    pub max_hourly_volume_quote: Decimal,
}

/// Kill switch configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillSwitchCfg {
    /// Daily PnL loss limit (quote) → Cancel All.
    pub daily_loss_limit: Decimal,
    /// Warning threshold → Widen Spreads.
    pub daily_loss_warning: Decimal,
    /// Max position value (quote) → Stop New Orders.
    pub max_position_value: Decimal,
    /// Max messages/sec → runaway algo detection.
    pub max_message_rate: u32,
    /// Max consecutive API errors → Stop New Orders.
    pub max_consecutive_errors: u32,
}

impl Default for KillSwitchCfg {
    fn default() -> Self {
        Self {
            daily_loss_limit: "1000".parse().unwrap(),
            daily_loss_warning: "500".parse().unwrap(),
            max_position_value: "50000".parse().unwrap(),
            max_message_rate: 100,
            max_consecutive_errors: 10,
        }
    }
}

/// SLA obligations — what the exchange requires from the MM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaObligationConfig {
    /// Maximum spread in bps to count as "quoting".
    pub max_spread_bps: Decimal,
    /// Minimum depth per side in quote asset (e.g., 2000 USDT).
    pub min_depth_quote: Decimal,
    /// Required uptime percentage.
    pub min_uptime_pct: Decimal,
    /// Must maintain both bid and ask.
    pub two_sided_required: bool,
    /// Max seconds to requote after a fill.
    pub max_requote_secs: u64,
    /// Min seconds an order must rest on book to count.
    pub min_order_rest_secs: u64,
}

impl Default for SlaObligationConfig {
    fn default() -> Self {
        Self {
            max_spread_bps: "100".parse().unwrap(),
            min_depth_quote: "2000".parse().unwrap(),
            min_uptime_pct: "95".parse().unwrap(),
            two_sided_required: true,
            max_requote_secs: 5,
            min_order_rest_secs: 3,
        }
    }
}

/// Toxicity detection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToxicityConfig {
    /// VPIN bucket size in quote asset.
    pub vpin_bucket_size: Decimal,
    /// Number of VPIN buckets.
    pub vpin_num_buckets: usize,
    /// VPIN threshold to trigger spread widening.
    pub vpin_threshold: Decimal,
    /// Kyle's Lambda window size (number of bars).
    pub kyle_window: usize,
    /// Adverse selection lookback (ms after fill).
    pub adverse_selection_lookback_ms: i64,
    /// Enable auto-tuning of parameters based on regime + toxicity.
    pub autotune_enabled: bool,
}

impl Default for ToxicityConfig {
    fn default() -> Self {
        Self {
            vpin_bucket_size: "50000".parse().unwrap(),
            vpin_num_buckets: 50,
            vpin_threshold: "0.7".parse().unwrap(),
            kyle_window: 100,
            adverse_selection_lookback_ms: 3000,
            autotune_enabled: true,
        }
    }
}

/// Telegram alert configuration (loaded from env vars, never config files).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TelegramAlertConfig {
    /// Bot token (from @BotFather).
    #[serde(default)]
    pub bot_token: String,
    /// Chat ID to send alerts to.
    #[serde(default)]
    pub chat_id: String,
}

impl TelegramAlertConfig {
    pub fn is_configured(&self) -> bool {
        !self.bot_token.is_empty() && !self.chat_id.is_empty()
    }
}

/// Loan configuration for token loan tracking (optional).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoanConfig {
    /// Original loan amount in base asset.
    pub loan_amount: Decimal,
    /// Call option strike price (if applicable).
    pub option_strike: Option<Decimal>,
    /// Option expiry date (ISO 8601 string, e.g., "2026-12-31").
    pub option_expiry: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            exchange: ExchangeConfig {
                exchange_type: ExchangeType::Custom,
                rest_url: "http://localhost:8080".into(),
                ws_url: "ws://localhost:8080/ws/v1".into(),
                api_key: None,
                api_secret: None,
            },
            market_maker: MarketMakerConfig {
                gamma: "0.1".parse().unwrap(),
                kappa: "1.5".parse().unwrap(),
                sigma: "0.02".parse().unwrap(),
                time_horizon_secs: 300,
                num_levels: 3,
                order_size: "0.001".parse().unwrap(),
                refresh_interval_ms: 500,
                min_spread_bps: "5".parse().unwrap(),
                max_distance_bps: "100".parse().unwrap(),
                strategy: StrategyType::AvellanedaStoikov,
                momentum_enabled: true,
                momentum_window: 200,
                basis_shift: dec!(0.5),
            },
            kill_switch: KillSwitchCfg::default(),
            risk: RiskConfig {
                max_inventory: "0.1".parse().unwrap(),
                max_exposure_quote: "10000".parse().unwrap(),
                max_drawdown_quote: "500".parse().unwrap(),
                inventory_skew_factor: "1.0".parse().unwrap(),
                max_spread_bps: "500".parse().unwrap(),
                max_spread_to_quote_bps: None,
                stale_book_timeout_secs: 10,
                max_order_size: dec!(0),
                max_daily_volume_quote: dec!(0),
                max_hourly_volume_quote: dec!(0),
            },
            sla: SlaObligationConfig::default(),
            toxicity: ToxicityConfig::default(),
            symbols: vec!["BTCUSDT".into()],
            dashboard_port: 9090,
            checkpoint_path: "data/checkpoint.json".into(),
            log_file: String::new(),
            mode: "live".into(),
            users: vec![],
            telegram: TelegramAlertConfig::default(),
            loans: std::collections::HashMap::new(),
            hedge: None,
            funding_arb: None,
        }
    }
}

impl From<HedgePairConfig> for crate::types::InstrumentPair {
    fn from(c: HedgePairConfig) -> Self {
        Self {
            primary_symbol: c.primary_symbol,
            hedge_symbol: c.hedge_symbol,
            multiplier: c.multiplier,
            funding_interval_secs: c.funding_interval_secs,
            basis_threshold_bps: c.basis_threshold_bps,
        }
    }
}
