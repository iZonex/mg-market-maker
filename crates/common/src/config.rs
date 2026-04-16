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

    /// Listing sniper config (Epic F stage-3).
    #[serde(default)]
    pub listing_sniper: ListingSniperConfig,

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
    /// Cross-venue basis (P1.4 stage-1). Same shape as `Basis`
    /// but with an explicit hedge-book staleness gate so the
    /// strategy stands down whenever the cross-venue feed
    /// pauses past `cross_venue_basis_max_staleness_ms`. Use
    /// when the primary and hedge venues are different
    /// exchanges (Binance spot ↔ Bybit perp, Coinbase spot ↔
    /// Binance perp, etc.) — same-venue pairs should still
    /// pick `Basis`.
    CrossVenueBasis,
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

    /// Enable the event-driven Market Resilience detector. When
    /// `false`, the calculator still exists but its score is
    /// never pushed into the autotuner or kill switch — the
    /// book stays at the regime+toxicity spread baseline. Turn
    /// off if MR keeps over-widening in an extremely bursty
    /// venue and you want to fall back to the static heuristic.
    #[serde(default = "default_true")]
    pub market_resilience_enabled: bool,

    /// Enable the Order-to-Trade Ratio compliance counter.
    /// When `false`, OTR snapshots stop flowing into the audit
    /// trail and the Prometheus gauge. **Leave `true` for any
    /// MiCA-regulated venue** — this is a regulatory metric,
    /// not a trading signal.
    #[serde(default = "default_true")]
    pub otr_enabled: bool,

    /// Enable the Hull Moving Average alpha component in
    /// `MomentumSignals`. When `false`, the legacy `0.4 / 0.4 /
    /// 0.2` alpha weight split is used (book imbalance / trade
    /// flow / micro-price only, no HMA slope). Turn off to
    /// A/B-compare alpha quality with and without HMA.
    #[serde(default = "default_true")]
    pub hma_enabled: bool,

    /// Hull Moving Average window for the mid-price feed.
    /// Default 9 — matches the `mm-toolbox` quickstart and
    /// gives a HMA lag of ≈3 samples on typical mid streams.
    #[serde(default = "default_hma_window")]
    pub hma_window: usize,

    /// Epic D stage-3 — enable the Cont-Kukanov-Stoikov
    /// L1 Order Flow Imbalance signal as a fifth alpha
    /// component on the engine's `MomentumSignals`. When
    /// `true`, the engine attaches an `OfiTracker` via
    /// `MomentumSignals::with_ofi()` and feeds top-of-book
    /// snapshots into it on every book event. Default
    /// `false` for backward-compat with operators who tuned
    /// the wave-1 alpha weights.
    #[serde(default)]
    pub momentum_ofi_enabled: bool,

    /// Epic D stage-3 — optional path to a finalized
    /// `LearnedMicroprice` TOML file (produced by the
    /// `mm-learned-microprice-fit` offline CLI binary).
    /// When `Some`, the engine loads the model at startup
    /// and attaches it via
    /// `MomentumSignals::with_learned_microprice(model)`.
    /// On load failure the engine logs a warning and
    /// continues without the learned microprice signal —
    /// it never panics on a missing or malformed file.
    /// Default `None`.
    ///
    /// **Per-pair override:** when the engine's symbol has
    /// an entry in [`Self::momentum_learned_microprice_pair_paths`],
    /// that path takes precedence over this system-wide
    /// fallback. Multi-symbol deployments that want
    /// distinct fitted models per pair use the pair map;
    /// single-symbol or homogeneous deployments use this
    /// system-wide path.
    #[serde(default)]
    pub momentum_learned_microprice_path: Option<String>,

    /// Epic D stage-3 — per-pair learned microprice TOML
    /// paths keyed by symbol. Multi-symbol deployments fit
    /// a separate `LearnedMicroprice` per pair offline and
    /// drop the resulting TOML files into config:
    ///
    /// ```toml
    /// [market_maker.momentum_learned_microprice_pair_paths]
    /// BTCUSDT = "/etc/mm/lmp/btcusdt.toml"
    /// ETHUSDT = "/etc/mm/lmp/ethusdt.toml"
    /// SOLUSDT = "/etc/mm/lmp/solusdt.toml"
    /// ```
    ///
    /// Lookup order at engine construction time:
    /// 1. `momentum_learned_microprice_pair_paths.get(symbol)` —
    ///    per-pair entry takes precedence
    /// 2. `momentum_learned_microprice_path` — system-wide
    ///    fallback
    /// 3. None — no learned MP signal attached
    ///
    /// Same load-failure semantics as the system-wide path:
    /// a malformed or missing file logs a warning and
    /// continues without the signal. Default empty map.
    #[serde(default)]
    pub momentum_learned_microprice_pair_paths: std::collections::HashMap<String, String>,

    /// Enable the Binance listen-key user-data stream. When
    /// `true` (the default), the server spawns a background
    /// task that opens a signed WebSocket against
    /// `wss://stream.binance.com/ws/<listenKey>` and pushes
    /// `MarketEvent::Fill` + `MarketEvent::BalanceUpdate`
    /// into the engine's event loop. Without this, fills
    /// that don't come back through the WS-API response
    /// envelope (REST fallback, partial fills, manual UI
    /// orders, RFQ/OTC) silently drift inventory until the
    /// next reconciliation cycle. **Turn off only for paper
    /// mode or for non-Binance venues.**
    #[serde(default = "default_true")]
    pub user_stream_enabled: bool,

    /// Absolute tolerance (in base asset units) for the
    /// inventory-vs-wallet drift reconciler. Drift under this
    /// threshold is absorbed silently to ignore fee-in-base
    /// rounding noise. Default `0.0001`; operators on a pair
    /// with finer lot sizes should tighten it, and operators
    /// whose venue takes fees in the base asset should widen
    /// it by roughly `2 × maker_fee × max_daily_volume`.
    #[serde(default = "default_drift_tolerance")]
    pub inventory_drift_tolerance: Decimal,

    /// Auto-correct the `InventoryManager` tracker when a
    /// drift is detected. `false` (the default) is **alert
    /// only**: the drift is logged to the audit trail and the
    /// alert manager, and the operator is expected to
    /// investigate manually. `true` force-resets the tracker
    /// to match the wallet delta — use only when the drift
    /// source is known to be a listen-key gap, not a bug in
    /// fill routing.
    #[serde(default = "default_false")]
    pub inventory_drift_auto_correct: bool,

    /// Enable soft cancel-replace (amend) on order diffs.
    /// When `true`, the `OrderManager` pairs cancels and
    /// places on the same side that differ by at most
    /// `amend_max_ticks` ticks with unchanged qty, and issues
    /// `ExchangeConnector::amend_order` instead of a
    /// cancel+place pair — preserving queue priority on
    /// venues that support it. Venues without
    /// `VenueCapabilities::supports_amend` fall back to
    /// cancel+place automatically regardless of this flag.
    #[serde(default = "default_true")]
    pub amend_enabled: bool,

    /// Maximum price tick distance at which an order diff is
    /// eligible for amend instead of cancel+place. `0`
    /// disables amend even when `amend_enabled = true`.
    /// Default `2`: a same-side pair within 2 ticks gets an
    /// amend. Larger budgets risk amend rejections on venues
    /// that only preserve queue priority within a tight
    /// window.
    #[serde(default = "default_amend_max_ticks")]
    pub amend_max_ticks: u32,

    /// Enable the periodic fee-tier refresh task. When `true`,
    /// the engine queries `ExchangeConnector::fetch_fee_tiers`
    /// every `fee_tier_refresh_secs` and hot-swaps the
    /// `PnlTracker` rates plus the `ProductSpec.maker_fee` /
    /// `taker_fee` so a month-end VIP tier crossing affects
    /// captured edge immediately instead of waiting for a
    /// process restart. Connectors without a per-account fee
    /// endpoint return `Err(NotSupported)` — the engine logs the
    /// fallthrough at debug level and keeps the startup
    /// snapshot.
    #[serde(default = "default_true")]
    pub fee_tier_refresh_enabled: bool,

    /// Refresh cadence for the periodic fee-tier task. Default
    /// `600` seconds (10 minutes) — Binance and Bybit only
    /// recalculate VIP tiers on a daily / weekly schedule, so a
    /// faster cadence wastes API budget. Set to `0` to disable
    /// even when `fee_tier_refresh_enabled` is true.
    #[serde(default = "default_fee_tier_refresh_secs")]
    pub fee_tier_refresh_secs: u64,

    /// Enable the P1.3 borrow-cost shim — when `true` the
    /// engine periodically refreshes the venue's borrow rate
    /// for the base asset and threads the resulting
    /// expected-carry bps into the strategy reservation price
    /// so the spot ask side compensates for the loan it would
    /// take to deliver. Stage-1 only widens the reservation;
    /// stage-2 will wire actual loan execution. Defaults to
    /// `false` so existing operators are not silently
    /// re-priced into a wider book.
    #[serde(default = "default_false")]
    pub borrow_enabled: bool,

    /// Refresh cadence for the periodic borrow-rate task.
    /// Default `1800` seconds (30 min). Binance recomputes the
    /// daily margin rate at most once per hour so a faster
    /// cadence wastes API budget. Set to `0` to disable even
    /// when `borrow_enabled` is true.
    #[serde(default = "default_borrow_rate_refresh_secs")]
    pub borrow_rate_refresh_secs: u64,

    /// Average expected holding period for one round-trip of
    /// borrowed inventory in seconds. Used by the
    /// `BorrowManager` to convert APR → expected-carry bps.
    /// Default `3600` (1 hour). Tighter values shrink the
    /// surcharge; longer values inflate it.
    #[serde(default = "default_borrow_holding_secs")]
    pub borrow_holding_secs: u64,

    /// Maximum borrow target in base-asset units. Stage-1 only
    /// uses this for the carry-cost bookkeeping; stage-2 will
    /// enforce it as a hard cap on the actual loan.
    #[serde(default = "default_borrow_max_base")]
    pub borrow_max_base: Decimal,

    /// Pre-borrow buffer in base-asset units the engine wants
    /// available at all times. Stage-1 only persists it.
    #[serde(default = "default_borrow_buffer_base")]
    pub borrow_buffer_base: Decimal,

    /// Enable the P2.3 pair lifecycle automation. When `true`
    /// the engine periodically polls
    /// `connector.get_product_spec(symbol)`, diffs against the
    /// in-memory snapshot via `PairLifecycleManager`, and
    /// routes Halted/Resumed/Delisted/TickLotChanged events
    /// into the audit trail and the engine's paused flag.
    /// Default `true` — operators on venues that do not
    /// surface a status field still get the tick/lot drift
    /// detection for free, with the halt branch as a no-op.
    #[serde(default = "default_true")]
    pub pair_lifecycle_enabled: bool,

    /// Refresh cadence for the pair lifecycle task in seconds.
    /// Default `300` (5 minutes). Faster cadences waste API
    /// budget; slower ones leave a long window where a halt
    /// goes unnoticed.
    #[serde(default = "default_pair_lifecycle_refresh_secs")]
    pub pair_lifecycle_refresh_secs: u64,

    /// Enable the per-strategy VaR guard (Epic C
    /// sub-component #4). When `true` the engine samples PnL
    /// deltas every 60 s, maintains a rolling 24 h window
    /// per strategy class, and throttles quote size via the
    /// same `min()` composition the kill switch / MR / IGP
    /// multipliers already use. Default `false` so existing
    /// deployments do not get a surprise size reduction.
    #[serde(default = "default_false")]
    pub var_guard_enabled: bool,

    /// 95 %-VaR floor in the reporting currency (usually
    /// USDT). The guard drops the strategy's size multiplier
    /// to `0.5` when the computed VaR_95 falls below this
    /// value. Typically a negative number. `None` disables
    /// the 95 % tier even when `var_guard_enabled = true`.
    #[serde(default)]
    pub var_guard_limit_95: Option<Decimal>,

    /// 99 %-VaR floor. On breach the strategy throttles to
    /// `0.0` (hard halt). Typically a more negative number
    /// than `var_guard_limit_95`. `None` disables the 99 %
    /// tier.
    #[serde(default)]
    pub var_guard_limit_99: Option<Decimal>,

    /// Maximum acceptable hedge-book staleness in milliseconds
    /// for the cross-venue basis strategy (P1.4 stage-1).
    /// Default `1500` — typical cross-venue WS feeds jitter
    /// 200-800 ms in steady state, so 1.5 s catches a stalled
    /// feed without false positives. The engine threads this
    /// into `BasisStrategy::cross_venue` whenever
    /// `StrategyType::CrossVenueBasis` is selected.
    #[serde(default = "default_cross_venue_basis_max_staleness_ms")]
    pub cross_venue_basis_max_staleness_ms: i64,
}

fn default_cross_venue_basis_max_staleness_ms() -> i64 {
    1500
}

fn default_pair_lifecycle_refresh_secs() -> u64 {
    300
}

fn default_borrow_rate_refresh_secs() -> u64 {
    1800
}

fn default_borrow_holding_secs() -> u64 {
    3600
}

fn default_borrow_max_base() -> Decimal {
    dec!(0)
}

fn default_borrow_buffer_base() -> Decimal {
    dec!(0)
}

fn default_fee_tier_refresh_secs() -> u64 {
    600
}

fn default_amend_max_ticks() -> u32 {
    2
}

fn default_drift_tolerance() -> Decimal {
    dec!(0.0001)
}

fn default_false() -> bool {
    false
}

fn default_hma_window() -> usize {
    9
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

/// Listing sniper configuration (Epic F stage-3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingSniperConfig {
    /// Enable periodic venue scanning for new/removed symbols.
    #[serde(default = "default_false")]
    pub enabled: bool,
    /// Scan interval in seconds. Default 300 (5 min).
    #[serde(default = "default_listing_sniper_scan_secs")]
    pub scan_interval_secs: u64,
    /// Send Telegram alerts on discovered/removed symbols.
    #[serde(default = "default_true")]
    pub alert_on_discovery: bool,
}

impl Default for ListingSniperConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            scan_interval_secs: default_listing_sniper_scan_secs(),
            alert_on_discovery: true,
        }
    }
}

fn default_listing_sniper_scan_secs() -> u64 {
    300
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
    /// Per-asset-class kill switches (P2.1). Each entry owns its
    /// own `KillSwitchCfg`-like limits and a list of symbols
    /// that share the resulting state. Engines whose symbol
    /// matches an entry receive a shared `Arc<Mutex<KillSwitch>>`
    /// pointer so a coordinated escalation halts every
    /// pair in the class without touching unrelated symbols.
    /// Hard escalation levels (CancelAll / FlattenAll /
    /// Disconnect) still come from the per-engine global state
    /// only — the asset-class layer is intentionally
    /// **soft-only**, so an asset-wide widening cannot
    /// accidentally flatten another pair's inventory.
    #[serde(default)]
    pub asset_classes: Vec<AssetClassKillSwitchCfg>,
}

/// One per-asset-class kill switch entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetClassKillSwitchCfg {
    /// Display name — used in logs, audit, and the dashboard.
    /// Free-form; pick something operators will recognise
    /// ("ETH-family", "stablecoins", "long-tail-alts").
    pub name: String,
    /// Symbols belonging to this class. Each symbol must be
    /// quoted by exactly one engine in the deployment;
    /// `validate.rs` errors if a symbol appears in more than
    /// one class or in a class but not in `[[symbols]]`.
    pub symbols: Vec<String>,
    /// Soft-trigger thresholds for the asset-class layer. The
    /// fields mirror `KillSwitchCfg` because the per-asset-class
    /// `KillSwitch` is the same state machine — just shared
    /// across the symbols listed above. Hard-trigger fields
    /// (`daily_loss_limit`, `max_position_value`) are still
    /// honoured but only escalate up to `StopNewOrders`; the
    /// hard `CancelAll`/`FlattenAll` paths come from the
    /// per-engine global state.
    pub limits: KillSwitchCfg,
}

impl Default for KillSwitchCfg {
    fn default() -> Self {
        Self {
            daily_loss_limit: "1000".parse().unwrap(),
            daily_loss_warning: "500".parse().unwrap(),
            max_position_value: "50000".parse().unwrap(),
            max_message_rate: 100,
            max_consecutive_errors: 10,
            asset_classes: Vec::new(),
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
                market_resilience_enabled: true,
                otr_enabled: true,
                hma_enabled: true,
                hma_window: 9,
                momentum_ofi_enabled: false,
                momentum_learned_microprice_path: None,
                momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
                user_stream_enabled: true,
                inventory_drift_tolerance: dec!(0.0001),
                inventory_drift_auto_correct: false,
                amend_enabled: true,
                amend_max_ticks: 2,
                fee_tier_refresh_enabled: true,
                fee_tier_refresh_secs: 600,
                borrow_enabled: false,
                borrow_rate_refresh_secs: 1800,
                borrow_holding_secs: 3600,
                borrow_max_base: dec!(0),
                borrow_buffer_base: dec!(0),
                pair_lifecycle_enabled: true,
                pair_lifecycle_refresh_secs: 300,
                var_guard_enabled: false,
                var_guard_limit_95: None,
                var_guard_limit_99: None,
                cross_venue_basis_max_staleness_ms: 1500,
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
            listing_sniper: ListingSniperConfig::default(),
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
