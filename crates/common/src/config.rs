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

    /// Restore engine state from checkpoint on startup (Epic 7).
    /// When `true`, `InventoryManager` is initialized from the
    /// last saved checkpoint. When `false` (default), engines
    /// start fresh and rely on reconciliation to detect drift.
    #[serde(default)]
    pub checkpoint_restore: bool,

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

    /// Stat-arb driver config (22A-1). Required when
    /// `StrategyType::StatArb` is selected; ignored otherwise.
    /// The driver trades a cointegrated two-leg pair using
    /// Engle-Granger + Kalman hedge ratio + z-score signal.
    #[serde(default)]
    pub stat_arb: Option<StatArbCfg>,

    /// Protections stack (22W-1). Freqtrade-inspired rate-limit
    /// guards that pause pairs without tripping the full kill
    /// switch — N-stops-in-window StoplossGuard, CooldownPeriod,
    /// MaxDrawdownPause (per-pair equity drawdown),
    /// LowProfitPairs (rolling PnL demotion). Each sub-guard is
    /// independently optional. When `protections = None` no
    /// guards are built.
    #[serde(default)]
    pub protections: Option<ProtectionsCfg>,

    /// Portfolio-wide VaR guard (22W-2). Complements the
    /// per-strategy `var_guard_*` with a book-wide PnL-delta
    /// rolling window and parametric Gaussian VaR at 95 %+99 %.
    /// Breach of `var_limit_95`/`99` broadcasts a size multiplier
    /// (0.5/0.0) to every engine via
    /// `ConfigOverride::PortfolioVarMult`. `None` disables the
    /// portfolio-level guard; per-strategy var_guard still runs.
    #[serde(default)]
    pub portfolio_var: Option<PortfolioVarCfg>,

    /// Cross-exchange executor config (22W-5). Opt-in upgrade
    /// for `StrategyType::CrossExchange`: when enabled, every
    /// primary-venue maker fill routes through `XemmExecutor`
    /// which re-checks the hedge book's top-of-book, rejects
    /// the hedge if adverse slippage exceeds `max_slippage_bps`,
    /// and flags unfavourable crosses below `min_edge_bps` for
    /// operator audit. `None` falls through to the legacy
    /// profit-floor-only pattern.
    #[serde(default)]
    pub xemm: Option<XemmCfg>,

    /// Execution algorithm config for the kill-switch L4
    /// flatten path (22A-3). Exposes the `TwapExecutor` knobs
    /// that were previously hardcoded (60 s / 10 slices / 5
    /// bps). The `algo` discriminator currently accepts only
    /// `"twap"` — the other ExecAlgorithm variants (`vwap`,
    /// `pov`, `iceberg`) are library-complete in
    /// `mm_strategy::exec_algo` but require an ExecAlgorithm→
    /// TwapExecutor adapter before the engine's L4 dispatch
    /// loop can consume them. Validate refuses non-twap with a
    /// clear pointer to that follow-up work.
    #[serde(default)]
    pub execution: Option<ExecutionCfg>,

    /// Record live market data to JSONL for offline backtesting.
    /// When `true`, each engine writes BookSnapshot + Trade events
    /// to `data/recorded/{symbol}.jsonl`. Data accumulates across
    /// restarts (append mode).
    #[serde(default)]
    pub record_market_data: bool,

    /// Paper mode fill simulation configuration (Epic 8).
    /// When set and `mode == "paper"`, fills are simulated
    /// using the `ProbabilisticFiller` with these parameters.
    #[serde(default)]
    pub paper_fill: Option<PaperFillCfg>,

    /// A/B split test configuration. Runs two parameter variants
    /// side-by-side; the engine folds per-variant multipliers into
    /// `gamma`, `spread`, and `order_size` every tick and records
    /// PnL deltas per variant. `None` disables the split (legacy
    /// single-variant behaviour).
    #[serde(default)]
    pub ab_split: Option<AbSplitCfg>,

    /// Cross-venue rebalancer configuration (Epic 4).
    #[serde(default)]
    pub rebalancer: Option<RebalancerCfg>,

    /// R3.7 — on-chain surveillance configuration. `None`
    /// disables the whole feature (no poller task, no graph
    /// sources populated). See [`OnchainCfg`].
    #[serde(default)]
    pub onchain: Option<OnchainCfg>,

    /// Portfolio-level risk configuration (Epic 3).
    /// When `Some`, the server spawns a background task that
    /// evaluates portfolio risk on a 30-second interval and
    /// broadcasts spread multipliers or halt signals.
    #[serde(default)]
    pub portfolio_risk: Option<PortfolioRiskCfg>,

    /// Per-client configuration (Epic 1: Multi-Client Isolation).
    /// When non-empty, each client owns a disjoint set of symbols
    /// with separate SLA, webhooks, and API auth scoping. When
    /// empty, the system runs in legacy single-client mode — a
    /// synthetic `"default"` client is created owning all
    /// `config.symbols`.
    #[serde(default)]
    pub clients: Vec<ClientConfig>,

    /// Margin guard plus per-symbol mode configuration (Epic
    /// 40.4 and 40.7). Required when `exchange.product` has
    /// funding (linear/inverse perp); ignored for spot. Engine
    /// startup rejects a perp config with `margin = None` so
    /// the guard is never silently disabled on a live perp
    /// account.
    #[serde(default)]
    pub margin: Option<MarginConfig>,

    /// Epic A stage-2 #3 — additional venue connectors the
    /// SOR routes across on top of `exchange` + optional
    /// `hedge`. Each entry produces one `ExchangeConnector`
    /// at startup via the same `create_connector` path, gets
    /// appended to the bundle's `extra` list, and gets
    /// registered on the engine's `VenueStateAggregator` so
    /// the greedy router can pick it.
    ///
    /// Leave empty to run single-venue (or single + hedge)
    /// — the default. Typical multi-venue config lists 2–4
    /// entries, all on the same base asset (BTCUSDT on
    /// Binance spot + Bybit spot + HyperLiquid perp is a
    /// canonical setup).
    #[serde(default)]
    pub sor_extra_venues: Vec<SorVenueConfig>,

    /// Epic F stage-3 — listing sniper real-entry policy.
    /// Opt-in: when `entry.enter_on_discovery = true` the
    /// sniper places a single IOC BUY once a newly-detected
    /// symbol clears the quarantine window. Default off so
    /// upgrading the binary does NOT silently start
    /// trading. Safety envelope (quarantine, max notional,
    /// max concurrent entries, trading-status gate) lives
    /// on the entry config.
    #[serde(default)]
    pub listing_sniper_entry: Option<ListingSniperEntryConfig>,

    /// Epic B stage-2 — background pair-screener config.
    /// When `enabled`, the server spawns a task that polls
    /// mid prices for every symbol in `pairs` and runs
    /// `PairScreener::screen_all` every
    /// `scan_interval_secs`. Results are audited +
    /// surfaced on the dashboard so operators can pick
    /// candidate pairs for a `stat_arb_driver` without
    /// manually running cointegration tests.
    #[serde(default)]
    pub pair_screener: Option<PairScreenerConfig>,

    /// Block C — S3 archive pipeline. When set, the server
    /// ships the hash-chained audit log, fill log, and daily
    /// report snapshots to the configured bucket on a
    /// background timer. Client / regulator handover pulls from
    /// S3 instead of the local filesystem. Credentials come
    /// from the usual AWS chain (env / IAM role / profile) —
    /// NEVER from this config. Leave unset for single-host
    /// deployments that do their own backup.
    #[serde(default)]
    pub archive: Option<ArchiveConfig>,

    /// Block B — scheduled compliance report generator. When
    /// any of `daily_enabled` / `weekly_enabled` /
    /// `monthly_enabled` is `true`, the server spawns a
    /// `ReportScheduler` at boot and fires a
    /// `BuiltinReportJob` on the configured cron cadence.
    /// Reports land under `data/reports/{cadence}/` and (when
    /// `archive` is configured) get shipped to S3 on the next
    /// shipper tick. Catch-up window keeps missed runs from
    /// operator-side downtime intact on restart.
    #[serde(default)]
    pub schedule: Option<ScheduleRef>,

    /// Epic F #1 — defensive lead-lag guard. When `Some` AND
    /// a hedge connector is configured, every hedge-side mid
    /// update flows into an EWMA z-score tracker; outsized
    /// leader moves feed a 1..N multiplier into the
    /// autotuner so the follower-side engine widens quotes
    /// before the cross-venue arb hits. Leave unset to keep
    /// the multiplier pinned at 1.0.
    #[serde(default)]
    pub lead_lag: Option<LeadLagCfg>,

    /// Epic F #2 — headline-driven retreat state machine.
    /// When set, each engine wires a
    /// `NewsRetreatStateMachine` with the configured regex
    /// tables + cooldowns + multipliers. Headlines arrive via
    /// `POST /api/admin/config` (broadcast) with
    /// `field = "News"`, text in `value`. Critical-class
    /// transitions escalate the kill switch to L2.
    #[serde(default)]
    pub news_retreat: Option<NewsRetreatCfg>,

    /// Epic G — native sentiment/social-risk pipeline. When
    /// set, the server spawns the `mm-sentiment`
    /// orchestrator (collectors → Ollama analyzer →
    /// mention counter → periodic `SentimentTick` broadcast)
    /// and every engine wires a `SocialRiskEngine`. Leave
    /// unset to skip both sides cleanly.
    #[serde(default)]
    pub sentiment: Option<SentimentCfg>,
}

/// Epic G — sentiment pipeline configuration. All the knobs
/// operators need to run the native social-risk loop end-to-
/// end in-process, no FastAPI / sidecar required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentimentCfg {
    /// Poll cadence for collectors + ticks. Default 60 s.
    #[serde(default = "default_sentiment_interval")]
    pub poll_interval_secs: u64,
    /// Canonical tickers the orchestrator emits ticks for
    /// (e.g. `["BTC", "ETH"]`). An engine's symbol is matched
    /// by calling the ticker normaliser on the symbol's base
    /// asset; a tick with a mismatched asset lands in the
    /// counter but doesn't broadcast.
    #[serde(default)]
    pub monitored_assets: Vec<String>,

    /// Ollama endpoint + model. Defaults target `gemma3:4b`
    /// on `localhost:11434` — fast multimodal model that
    /// handles Twitter screenshots well in JSON mode.
    #[serde(default)]
    pub ollama: OllamaCfg,

    /// Source collectors. Empty sections disable that source.
    #[serde(default)]
    pub rss: RssCfg,
    #[serde(default)]
    pub cryptopanic: CryptoPanicCfg,
    #[serde(default)]
    pub twitter: TwitterCfg,

    /// Risk engine knobs. Mirrors
    /// `mm_risk::social_risk::SocialRiskConfig`; same default
    /// values so leaving `[sentiment.risk] = {}` in TOML
    /// produces the tested baseline.
    #[serde(default)]
    pub risk: SocialRiskCfg,

    /// Optional JSONL path — when set, each analysed article
    /// lands as one line. Archive shipper uploads the file on
    /// the same cadence as `audit.jsonl` when both are
    /// configured. Default: `data/sentiment/articles.jsonl`.
    #[serde(default = "default_sentiment_persist_path")]
    pub persist_path: String,
    #[serde(default = "default_sentiment_persist")]
    pub persist_articles: bool,
}
fn default_sentiment_persist_path() -> String {
    "data/sentiment/articles.jsonl".into()
}
fn default_sentiment_persist() -> bool {
    true
}

fn default_sentiment_interval() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaCfg {
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
    #[serde(default = "default_ollama_model")]
    pub model: String,
    #[serde(default = "default_ollama_timeout")]
    pub timeout_secs: u64,
}
impl Default for OllamaCfg {
    fn default() -> Self {
        Self {
            base_url: default_ollama_url(),
            model: default_ollama_model(),
            timeout_secs: default_ollama_timeout(),
        }
    }
}
fn default_ollama_url() -> String {
    "http://localhost:11434".into()
}
fn default_ollama_model() -> String {
    "gemma3:4b".into()
}
fn default_ollama_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RssCfg {
    #[serde(default)]
    pub feeds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CryptoPanicCfg {
    /// Full JSON URL including auth + query params. Leave
    /// empty to disable.
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TwitterCfg {
    /// Env var name holding the bearer token
    /// (`TWITTER_BEARER` by default). The server looks it up
    /// at boot so the secret never touches TOML.
    #[serde(default = "default_twitter_bearer_env")]
    pub bearer_env: String,
    /// Search queries (see X API v2 recent-search syntax).
    /// Empty list = Twitter disabled.
    #[serde(default)]
    pub queries: Vec<String>,
}
fn default_twitter_bearer_env() -> String {
    "TWITTER_BEARER".into()
}

/// Mirror of `mm_risk::social_risk::SocialRiskConfig` using
/// strings where the risk-side uses `Decimal`. Exact same
/// defaults as the struct's `Default` impl.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialRiskCfg {
    #[serde(default = "default_social_rate_warn")]
    pub rate_warn: String,
    #[serde(default = "default_social_rate_alarm")]
    pub rate_alarm: String,
    #[serde(default = "default_social_max_vol_mult")]
    pub max_vol_multiplier: String,
    #[serde(default = "default_social_min_size_mult")]
    pub min_size_multiplier: String,
    #[serde(default = "default_social_kill_rate")]
    pub kill_mentions_rate: String,
    #[serde(default = "default_social_kill_vol")]
    pub kill_vol_threshold: String,
    #[serde(default = "default_social_skew_threshold")]
    pub skew_threshold: String,
    #[serde(default = "default_social_max_skew_bps")]
    pub max_skew_bps: String,
    #[serde(default = "default_social_ofi_confirm")]
    pub ofi_confirm_z: String,
    #[serde(default = "default_social_staleness_mins")]
    pub staleness_mins: i64,
}

impl Default for SocialRiskCfg {
    fn default() -> Self {
        Self {
            rate_warn: default_social_rate_warn(),
            rate_alarm: default_social_rate_alarm(),
            max_vol_multiplier: default_social_max_vol_mult(),
            min_size_multiplier: default_social_min_size_mult(),
            kill_mentions_rate: default_social_kill_rate(),
            kill_vol_threshold: default_social_kill_vol(),
            skew_threshold: default_social_skew_threshold(),
            max_skew_bps: default_social_max_skew_bps(),
            ofi_confirm_z: default_social_ofi_confirm(),
            staleness_mins: default_social_staleness_mins(),
        }
    }
}

fn default_social_rate_warn() -> String {
    "2".into()
}
fn default_social_rate_alarm() -> String {
    "5".into()
}
fn default_social_max_vol_mult() -> String {
    "3".into()
}
fn default_social_min_size_mult() -> String {
    "0.5".into()
}
fn default_social_kill_rate() -> String {
    "10".into()
}
fn default_social_kill_vol() -> String {
    "0.8".into()
}
fn default_social_skew_threshold() -> String {
    "0.3".into()
}
fn default_social_max_skew_bps() -> String {
    "15".into()
}
fn default_social_ofi_confirm() -> String {
    "1.5".into()
}
fn default_social_staleness_mins() -> i64 {
    10
}

/// Serialisable mirror of
/// `mm_risk::news_retreat::NewsRetreatConfig`. Same field set,
/// Decimal replaced with string for readable TOML.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewsRetreatCfg {
    #[serde(default)]
    pub critical_keywords: Vec<String>,
    #[serde(default)]
    pub high_keywords: Vec<String>,
    #[serde(default)]
    pub low_keywords: Vec<String>,
    #[serde(default = "default_news_critical_cooldown_ms")]
    pub critical_cooldown_ms: i64,
    #[serde(default = "default_news_high_cooldown_ms")]
    pub high_cooldown_ms: i64,
    #[serde(default)]
    pub low_cooldown_ms: i64,
    #[serde(default = "default_news_high_mult")]
    pub high_multiplier: String,
    #[serde(default = "default_news_critical_mult")]
    pub critical_multiplier: String,
}

fn default_news_critical_cooldown_ms() -> i64 {
    30 * 60_000
}
fn default_news_high_cooldown_ms() -> i64 {
    5 * 60_000
}
fn default_news_high_mult() -> String {
    "2".into()
}
fn default_news_critical_mult() -> String {
    "3".into()
}

/// Serialisable mirror of
/// `mm_risk::lead_lag_guard::LeadLagGuardConfig`. Same field
/// set, `Decimal` replaced by `String` so TOML stays readable.
/// The server parses the strings via `Decimal::from_str` before
/// handing them to the guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadLagCfg {
    /// EWMA half-life in observation count. 20 events ≈ 5 s
    /// at a 250 ms hedge-side tick.
    #[serde(default = "default_lead_lag_half_life")]
    pub half_life_events: usize,
    /// Lower ramp edge. `|z| < z_min` keeps multiplier at
    /// 1.0. Default `"2"`.
    #[serde(default = "default_lead_lag_z_min")]
    pub z_min: String,
    /// Upper ramp edge. `|z| > z_max` saturates at
    /// `max_mult`. Default `"4"`.
    #[serde(default = "default_lead_lag_z_max")]
    pub z_max: String,
    /// Saturation multiplier. Default `"3"`.
    #[serde(default = "default_lead_lag_max_mult")]
    pub max_mult: String,
}

impl Default for LeadLagCfg {
    fn default() -> Self {
        Self {
            half_life_events: 20,
            z_min: "2".into(),
            z_max: "4".into(),
            max_mult: "3".into(),
        }
    }
}

fn default_lead_lag_half_life() -> usize {
    20
}
fn default_lead_lag_z_min() -> String {
    "2".into()
}
fn default_lead_lag_z_max() -> String {
    "4".into()
}
fn default_lead_lag_max_mult() -> String {
    "3".into()
}

/// Serialisable shape mirroring
/// `mm_dashboard::report_scheduler::ScheduleConfig`. Duplicated
/// here because the `common` crate must stay free of the
/// dashboard dep to avoid circular imports. Fields stay 1-for-1
/// so the server can trivially convert between them.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScheduleRef {
    #[serde(default)]
    pub daily_enabled: bool,
    #[serde(default)]
    pub weekly_enabled: bool,
    #[serde(default)]
    pub monthly_enabled: bool,
    #[serde(default = "default_catchup_hours")]
    pub catchup_hours: u32,
    #[serde(default = "default_last_run_path")]
    pub last_run_path: String,
}

fn default_catchup_hours() -> u32 {
    6
}
fn default_last_run_path() -> String {
    "data/report_last_run.jsonl".into()
}

/// Block C — compliance archive target.
///
/// Endpoint URL is optional so the same code works against AWS
/// S3 (leave `s3_endpoint_url = None`), MinIO, Cloudflare R2,
/// Backblaze B2, etc. The only requirements on the backend:
///   - PUT Object with x-amz-server-side-encryption headers
///   - GET Object (bundle download fallback path)
///
/// Retention defaults to 2555 days (7 years) to clear the
/// longest mainstream requirement (MiFID II). MiCA Article 17
/// currently asks for 5 years; sizing the default at the
/// stricter bar means operators don't have to touch this for
/// most regulators. Actual retention enforcement lives on the
/// bucket (Object Lock + lifecycle policy) — the shipper just
/// uploads; the bucket is the source of truth for deletes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveConfig {
    /// S3 bucket name. Required.
    pub s3_bucket: String,

    /// AWS region (or compatible region, e.g. `"auto"` for R2).
    #[serde(default = "default_s3_region")]
    pub s3_region: String,

    /// Optional endpoint override for S3-compatible backends
    /// (MinIO `http://minio:9000`, R2
    /// `https://<account>.r2.cloudflarestorage.com`, etc.).
    /// Leave unset for AWS S3 proper.
    #[serde(default)]
    pub s3_endpoint_url: Option<String>,

    /// Key prefix prepended to every uploaded object. Lets one
    /// bucket host multiple deployments / tenants without
    /// cross-contamination. Example: `"prod/venue-maker-a"`.
    #[serde(default)]
    pub s3_prefix: String,

    /// Retention target, in days. Informational at the shipper
    /// level — enforcement lives on the bucket (Object Lock +
    /// lifecycle). Recorded in the manifest so auditors can
    /// verify the policy the operator claimed to run.
    #[serde(default = "default_archive_retention_days")]
    pub retention_days: u32,

    /// When `Some`, uploads go up with
    /// `x-amz-server-side-encryption = aws:kms` and
    /// `x-amz-server-side-encryption-aws-kms-key-id` set to
    /// this ID. When `None`, SSE-S3 (`AES256`) is used — still
    /// encrypted at rest, just managed by S3 instead of KMS.
    #[serde(default)]
    pub encrypt_kms_key: Option<String>,

    /// Ship the hash-chained audit log on the shipper timer.
    #[serde(default = "default_true")]
    pub ship_audit_log: bool,

    /// Ship the persistent fill log on the shipper timer.
    #[serde(default = "default_true")]
    pub ship_fills: bool,

    /// Ship daily report snapshots (one JSON per day).
    #[serde(default = "default_true")]
    pub ship_daily_reports: bool,

    /// Shipper tick interval, seconds. Default 3600 (1 h).
    /// Lower values increase S3 PUT counts + cost; higher
    /// values widen the RPO window.
    #[serde(default = "default_shipper_interval_secs")]
    pub shipper_interval_secs: u64,
}

fn default_s3_region() -> String {
    "us-east-1".into()
}
fn default_archive_retention_days() -> u32 {
    2555
}
fn default_shipper_interval_secs() -> u64 {
    3600
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

/// Stat-arb driver TOML config — mirrors `mm_strategy::stat_arb::StatArbDriverConfig`
/// one-to-one so operators can tune the driver without touching
/// Rust. Field shapes match the non-config struct so the
/// translation in `main.rs` is a pure rename.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatArbCfg {
    /// Driver tick interval in seconds. Default 60 s —
    /// matches `StatArbDriverConfig::default`.
    #[serde(default = "default_stat_arb_tick_secs")]
    pub tick_interval_secs: u64,
    /// Symbol of the dependent leg (the regression predicts its
    /// mid). Required.
    pub y_symbol: String,
    /// Symbol of the independent leg (hedge-ratio β applies to
    /// its mid). Required — usually different from the primary
    /// engine symbol, routed to the hedge connector.
    pub x_symbol: String,
    /// Rolling z-score window size. Default 120 ticks (matches
    /// `ZScoreConfig::default`).
    #[serde(default = "default_stat_arb_window")]
    pub zscore_window: usize,
    /// z entry threshold. Default 2.0 — open when |z| > 2.
    #[serde(default = "default_stat_arb_entry")]
    pub zscore_entry: Decimal,
    /// z exit threshold. Must be strictly less than
    /// `zscore_entry`. Default 0.5.
    #[serde(default = "default_stat_arb_exit")]
    pub zscore_exit: Decimal,
    /// Kalman transition variance Q. Default 1e-6.
    #[serde(default = "default_stat_arb_q")]
    pub kalman_transition_var: Decimal,
    /// Kalman observation variance R. Default 1e-3.
    #[serde(default = "default_stat_arb_r")]
    pub kalman_observation_var: Decimal,
    /// Notional USD to commit to the Y leg at entry. X leg is
    /// sized as `β · y_qty` for book-neutral exposure.
    /// Default 1000 USDT.
    #[serde(default = "default_stat_arb_notional")]
    pub leg_notional_usd: Decimal,
    /// Enable the driver. Safety default is `false` so a stray
    /// `[stat_arb]` block does not unexpectedly start trading.
    #[serde(default)]
    pub enabled: bool,
}

fn default_stat_arb_tick_secs() -> u64 {
    60
}
fn default_stat_arb_window() -> usize {
    120
}
fn default_stat_arb_entry() -> Decimal {
    dec!(2)
}
fn default_stat_arb_exit() -> Decimal {
    dec!(0.5)
}
fn default_stat_arb_q() -> Decimal {
    dec!(0.000001)
}
fn default_stat_arb_r() -> Decimal {
    dec!(0.001)
}
fn default_stat_arb_notional() -> Decimal {
    dec!(1000)
}

/// TOML-facing protections stack config (22W-1). Mirrors
/// `mm_risk::protections::ProtectionsConfig` but with
/// `Duration` replaced by `*_secs: u64` so operators can write
/// `[protections.stoploss_guard]` blocks in TOML. Every
/// sub-guard is independently optional — a missing block
/// disables that guard without disabling the whole stack.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProtectionsCfg {
    #[serde(default)]
    pub stoploss_guard: Option<StoplossGuardCfg>,
    #[serde(default)]
    pub cooldown: Option<CooldownCfg>,
    #[serde(default)]
    pub max_drawdown: Option<MaxDrawdownPauseCfg>,
    #[serde(default)]
    pub low_profit_pairs: Option<LowProfitPairsCfg>,
}

/// Halt a pair after `max_stops` stop events within `window_secs`.
/// Lock lifts after `lockout_secs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoplossGuardCfg {
    pub window_secs: u64,
    pub max_stops: usize,
    pub lockout_secs: u64,
}

/// Mandatory pause of `duration_secs` after any stop event
/// before the pair can re-quote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CooldownCfg {
    pub duration_secs: u64,
}

/// Equity-peak-to-trough pause in quote currency. Per-pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaxDrawdownPauseCfg {
    pub max_drawdown_quote: Decimal,
    pub lockout_secs: u64,
    /// Fraction of peak that equity must recover to before the
    /// lockout clears early. `1.0` requires full recovery.
    /// `0.95` lets the pair re-quote after a 5% bounce.
    pub recovery_fraction: Decimal,
}

/// Rolling-window PnL demotion. Below `min_pnl_quote` over
/// `window_secs` (need ≥ `min_trades`), pair is paused for
/// `lockout_secs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LowProfitPairsCfg {
    pub window_secs: u64,
    pub min_pnl_quote: Decimal,
    pub lockout_secs: u64,
    pub min_trades: usize,
}

/// Portfolio-wide VaR guard config (22W-2). Mirrors
/// `mm_risk::portfolio_var::PortfolioVarConfig` 1:1; server
/// converts directly. Sampling cadence is fixed at 30 s in
/// `main.rs` — same as `portfolio_risk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioVarCfg {
    #[serde(default)]
    pub var_limit_95: Option<Decimal>,
    #[serde(default)]
    pub var_limit_99: Option<Decimal>,
    #[serde(default = "default_portfolio_var_max_samples")]
    pub max_samples: usize,
    #[serde(default = "default_portfolio_var_min_samples")]
    pub min_samples: usize,
}

fn default_portfolio_var_max_samples() -> usize {
    1440
}
fn default_portfolio_var_min_samples() -> usize {
    30
}

impl Default for PortfolioVarCfg {
    fn default() -> Self {
        Self {
            var_limit_95: None,
            var_limit_99: None,
            max_samples: default_portfolio_var_max_samples(),
            min_samples: default_portfolio_var_min_samples(),
        }
    }
}

/// XEMM executor config (22W-5). Mirrors
/// `mm_strategy::xemm::XemmConfig` with an `enabled` flag and
/// sensible defaults. Only takes effect under
/// `StrategyType::CrossExchange` with a configured hedge
/// connector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XemmCfg {
    /// Master switch. `false` → executor never built, engine
    /// stays on the legacy CrossExchangeStrategy profit-floor
    /// path.
    #[serde(default)]
    pub enabled: bool,
    /// Max adverse slippage on the hedge leg, in bps of the
    /// maker fill price. Over this, the hedge is rejected.
    /// Default 20 bps matches the Hummingbot V2 reference.
    #[serde(default = "default_xemm_max_slippage_bps")]
    pub max_slippage_bps: Decimal,
    /// Minimum expected edge on the cross. Below this, the
    /// executor still hedges but flags the round-trip for
    /// audit. Default 0 — no flag.
    #[serde(default = "default_xemm_min_edge_bps")]
    pub min_edge_bps: Decimal,
}

fn default_xemm_max_slippage_bps() -> Decimal {
    dec!(20)
}
fn default_xemm_min_edge_bps() -> Decimal {
    dec!(0)
}

impl Default for XemmCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            max_slippage_bps: default_xemm_max_slippage_bps(),
            min_edge_bps: default_xemm_min_edge_bps(),
        }
    }
}

/// Execution algorithm config for the kill-switch L4 flatten
/// path. Operator-tunable replacement for the three hardcoded
/// constants that previously lived at
/// `market_maker.rs:6762-6764`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionCfg {
    /// Algorithm discriminator. Currently accepts `"twap"` only
    /// — validate refuses other values with a pointer to the
    /// mm-strategy::exec_algo follow-up work.
    #[serde(default = "default_exec_algo")]
    pub algo: String,
    /// Total flatten duration in seconds. Hardcoded 60 before
    /// this knob; operators with larger inventory or thinner
    /// venues typically want 300–1800.
    #[serde(default = "default_exec_duration_secs")]
    pub duration_secs: u64,
    /// Number of slices over `duration_secs`. Hardcoded 10
    /// before this knob.
    #[serde(default = "default_exec_num_slices")]
    pub num_slices: u32,
    /// Aggressiveness in bps from mid. 0 = at mid (aggressive,
    /// often a taker), 10+ = passive limit (maker, slower
    /// fill). Hardcoded 5 before this knob.
    #[serde(default = "default_exec_aggressiveness_bps")]
    pub aggressiveness_bps: Decimal,
}

fn default_exec_algo() -> String {
    "twap".to_string()
}
fn default_exec_duration_secs() -> u64 {
    60
}
fn default_exec_num_slices() -> u32 {
    10
}
fn default_exec_aggressiveness_bps() -> Decimal {
    dec!(5)
}

impl Default for ExecutionCfg {
    fn default() -> Self {
        Self {
            algo: default_exec_algo(),
            duration_secs: default_exec_duration_secs(),
            num_slices: default_exec_num_slices(),
            aggressiveness_bps: default_exec_aggressiveness_bps(),
        }
    }
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
    /// Cross-Exchange Market Making. Make on venue A (primary)
    /// with prices that guarantee profit vs venue B (hedge) after
    /// fees. Unlike `Basis` which *prices* against the hedge mid,
    /// this strategy only quotes when a fill-then-hedge round
    /// trip nets ≥ `min_profit_bps` after both legs' fees and the
    /// `hedge_taker_fee`. Requires `AppConfig.hedge` to be set.
    ///
    /// Different from `CrossVenueBasis` in that we *guarantee*
    /// profit on each round trip instead of riding a mean-reverting
    /// basis spread. Use when the two venues have persistent price
    /// dislocations and our hedge-side taker costs are the only
    /// drag on the opportunity (e.g. Binance spot ↔ Bybit linear
    /// perp when the funding path is exhausted).
    CrossExchange,
    /// Statistical arbitrage driver (22A-1). Runs a
    /// `StatArbDriver` that tracks a cointegrated two-leg pair
    /// via Engle-Granger + Kalman + z-score and dispatches
    /// market-neutral round trips when the spread deviates.
    /// Requires `AppConfig.hedge` (X-leg connector) and a
    /// `[stat_arb]` section.
    StatArb,
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

/// Product type on an exchange. Epic 40.1 foundation — before this,
/// product was implicit in the `ExchangeType` (Binance → spot,
/// Bybit → linear perp, HL → perp). Explicit typing lets the
/// connector factory pick the right constructor (`BybitConnector::
/// spot()` vs `::linear()` vs `::inverse()`; `BinanceConnector` vs
/// `BinanceFuturesConnector`), propagates product-specific fee
/// tables + margin semantics to strategy + risk layers, and drives
/// the per-product toxicity widen multiplier (Epic 40.8).
///
/// Serialised as snake_case: `spot`, `linear_perp`, `inverse_perp`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductType {
    #[default]
    Spot,
    /// USDⓈ-M / USDT-M / USDC-M linear perpetual (Binance USDM,
    /// Bybit linear, HL perp, OKX USDT-swap).
    LinearPerp,
    /// Coin-margined inverse perpetual (Bybit inverse, Deribit,
    /// legacy BitMEX). Funding in base asset, not quote.
    InversePerp,
}

impl ProductType {
    /// Short label for dashboards / audit. Stable — do not change.
    pub fn label(self) -> &'static str {
        match self {
            ProductType::Spot => "spot",
            ProductType::LinearPerp => "linear_perp",
            ProductType::InversePerp => "inverse_perp",
        }
    }

    /// True when the product accrues funding (all perps).
    pub fn has_funding(self) -> bool {
        matches!(self, ProductType::LinearPerp | ProductType::InversePerp)
    }

    /// True when the product uses leverage / margin (all perps).
    pub fn has_margin(self) -> bool {
        matches!(self, ProductType::LinearPerp | ProductType::InversePerp)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    /// Exchange type: custom, binance, binance_testnet, bybit, bybit_testnet.
    #[serde(default)]
    pub exchange_type: ExchangeType,
    /// Product type on this exchange (Epic 40.1). Default `spot`
    /// preserves legacy config compatibility — existing
    /// `binance-paper.toml` stays on spot. Set to `linear_perp` to
    /// route Binance/Bybit to futures/linear constructors; set to
    /// `inverse_perp` for coin-margined. Ignored for HyperLiquid
    /// since that connector has distinct `new()` (perp) and
    /// `spot()` constructors already.
    #[serde(default)]
    pub product: ProductType,
    /// REST base URL. Empty → the connector uses its built-in
    /// default for the `(exchange_type, product)` pair
    /// (`https://api.binance.com` for binance spot,
    /// `https://fapi.binance.com` for binance linear_perp,
    /// Bybit/HL hardcode their own REST base). Only set
    /// explicitly for `exchange_type = "custom"` or testnet
    /// overrides.
    #[serde(default)]
    pub rest_url: String,
    /// WebSocket base URL. Same defaulting rule as `rest_url`.
    #[serde(default)]
    pub ws_url: String,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
    /// Optional read-only key (market data, balance polling, fee
    /// tier lookup). When set, connectors use this key for
    /// non-mutating requests so a compromised *trading* key cannot
    /// also read historical orders/fills. Typical setup:
    /// generate two keys on the venue, flag the MM_READ_KEY as
    /// read-only, keep MM_API_KEY restricted to spot-trading with
    /// an IP whitelist, and disable withdrawals on both. When
    /// unset the trading key is used for both paths (legacy
    /// single-key mode).
    #[serde(default)]
    pub read_key: Option<String>,
    #[serde(default)]
    pub read_secret: Option<String>,
    /// Fail-closed withdraw address whitelist (Epic 8). Guards
    /// every `connector.withdraw(...)` call:
    /// - `None`: legacy behaviour — the venue's own whitelist
    ///   is the sole line of defence. Operators who have locked
    ///   their API key to withdrawal-disabled can leave this
    ///   unset; operators who keep withdraw permission enabled
    ///   **must** populate it.
    /// - `Some([])`: block every withdraw attempt. Used as a
    ///   paranoid default during incident response.
    /// - `Some(addrs)`: only the listed addresses are accepted.
    ///   Entries are compared case-sensitively to the `address`
    ///   passed to `withdraw` — enforce whatever normalisation
    ///   the venue uses (lowercase hex for EVM, base58 for SOL,
    ///   etc.) when you populate the list.
    ///
    /// Config this at the network boundary so a compromised
    /// trading key cannot drain the account to an attacker-
    /// controlled address even if venue-side withdraw scopes
    /// are accidentally left enabled.
    #[serde(default)]
    pub withdraw_whitelist: Option<Vec<String>>,
}

impl std::fmt::Debug for ExchangeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual Debug to keep secrets out of accidental log lines.
        f.debug_struct("ExchangeConfig")
            .field("exchange_type", &self.exchange_type)
            .field("rest_url", &self.rest_url)
            .field("ws_url", &self.ws_url)
            .field("api_key", &redact_secret(&self.api_key))
            .field("api_secret", &redact_secret(&self.api_secret))
            .field("read_key", &redact_secret(&self.read_key))
            .field("read_secret", &redact_secret(&self.read_secret))
            .finish()
    }
}

fn redact_secret(s: &Option<String>) -> &'static str {
    match s {
        Some(v) if !v.is_empty() => "<set:redacted>",
        _ => "<unset>",
    }
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

    /// Epic 30 — enable the online `AdaptiveTuner`. When `true`,
    /// the engine feeds fills / inventory / adverse-selection
    /// readings into the tuner and multiplies γ by its output in
    /// every `refresh_quotes` cycle. Off by default — existing
    /// deployments see byte-identical behaviour unless they flip
    /// this on.
    #[serde(default = "default_false")]
    pub adaptive_enabled: bool,

    /// Epic 31 — auto-apply the matching pair-class template at
    /// engine startup. Merges `config/pair-classes/<class>.toml`
    /// into the running config between `AppConfig` deserialise
    /// and engine construction. User-set fields still win because
    /// they were already loaded; the template only fills in
    /// class-appropriate defaults for fields the user did not
    /// specify. Off by default for backwards compatibility.
    #[serde(default = "default_false")]
    pub apply_pair_class_template: bool,

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

    /// Epic D stage-2 — enable the **online streaming fit** on
    /// top of the loaded learned-microprice model. When `true`
    /// (and a model was successfully loaded from
    /// `momentum_learned_microprice_path` or its per-pair
    /// override), the engine attaches the model via
    /// `MomentumSignals::with_learned_microprice_online` so
    /// every L1 snapshot feeds the model's online ring and
    /// the g-matrix rebuilds on the cadence set by the
    /// model's own `refit_every` config. Intraday regime
    /// drift adapts without the offline-CLI round-trip.
    #[serde(default)]
    pub momentum_learned_microprice_online: bool,

    /// Epic D stage-2 — forward-mid horizon the online fit
    /// pairs `(imbalance_{t-k}, spread_{t-k}, mid_t − mid_{t-k})`
    /// against. MUST match the horizon the offline fit was
    /// trained with (default of the CLI binary is 10 L1
    /// ticks) — otherwise the online path biases the
    /// g-matrix against a different lookahead than it was
    /// trained on. Ignored when
    /// `momentum_learned_microprice_online = false`.
    #[serde(default = "default_learned_microprice_horizon")]
    pub momentum_learned_microprice_horizon: usize,

    /// Enable SOR inline dispatch (Epic A stage-2 #1). When
    /// `true`, the engine fires `dispatch_route()`
    /// automatically every [`Self::sor_dispatch_interval_secs`]
    /// if the qty-source
    /// [`Self::sor_target_qty_source`] produces a non-zero
    /// target. When `false` (default), SOR stays
    /// advisory-only via `recommend_route()`.
    #[serde(default)]
    pub sor_inline_enabled: bool,

    /// How often the inline SOR dispatch tick fires. Ignored
    /// when `sor_inline_enabled = false`. Default 5 s — cheap
    /// enough to keep routing decisions fresh, loose enough
    /// that transient route churn doesn't batter the venue
    /// rate limits.
    #[serde(default = "default_sor_dispatch_interval_secs")]
    pub sor_dispatch_interval_secs: u64,

    /// Urgency parameter forwarded to `dispatch_route`. `≥ 0.5`
    /// → taker-leg (IOC), `< 0.5` → maker-leg (PostOnly). The
    /// cost model's fee vs queue-wait tradeoff picks per-leg;
    /// this global knob tilts the bias. Default 0.4 — leans
    /// maker, preferring queue patience over immediate
    /// execution. Operators running a more aggressive hedge
    /// lift it toward 0.7.
    #[serde(default = "default_sor_urgency")]
    pub sor_urgency: Decimal,

    /// Where the auto-dispatch tick sources its target qty
    /// from. See [`SorTargetSource`] variants. Default
    /// `InventoryExcess` — the safest: only dispatches when
    /// the engine is holding more than
    /// [`Self::sor_inventory_threshold`] of net inventory.
    #[serde(default)]
    pub sor_target_qty_source: SorTargetSource,

    /// Absolute inventory (base asset) threshold for the
    /// `InventoryExcess` qty source. Dispatch ticks fire only
    /// when `|inventory| > threshold`; the target qty is
    /// `|inventory| − threshold` so the engine unloads the
    /// excess in a single routing pass. Ignored for other
    /// qty sources.
    #[serde(default)]
    pub sor_inventory_threshold: Decimal,

    /// Epic A stage-2 #2 — rolling-window length (secs) for
    /// the per-venue trade-rate estimator. The engine feeds
    /// every `MarketEvent::Trade` into the matching venue's
    /// ring, divides the windowed `qty` by the window length,
    /// and publishes the derived `queue_wait_secs` into the
    /// SOR aggregator. Longer window = smoother but slower
    /// to react. Default 60 s.
    #[serde(default = "default_sor_trade_rate_window_secs")]
    pub sor_trade_rate_window_secs: u64,

    /// Epic A stage-2 #2 — how often the engine refreshes
    /// `queue_wait_secs` on the SOR aggregator from the
    /// estimator. Default 2 s — fast enough that routing
    /// reacts within a few quote refreshes of a regime
    /// change, slow enough that we don't re-walk the deque
    /// on every tick.
    #[serde(default = "default_sor_queue_refresh_secs")]
    pub sor_queue_refresh_secs: u64,

    /// UX-VENUE-1 gap close — how often the engine polls each
    /// `sor_extra_venues` connector for its L1 top-of-book and
    /// republishes it onto `DataBus::books_l1` so the
    /// Overview's per-venue market strip renders extras
    /// alongside primary + hedge. Default 5 s: fast enough for
    /// the 2 s UI poll to get a fresh sample every other tick,
    /// slow enough that a REST `get_orderbook(depth=1)` per
    /// extra venue doesn't chew rate-limit budget. Set to 0 to
    /// disable the poll entirely.
    #[serde(default = "default_sor_extra_l1_poll_secs")]
    pub sor_extra_l1_poll_secs: u64,

    /// UX-VENUE-2 — how often the engine samples its own
    /// `(venue, symbol, product)` L1 streams off the data bus,
    /// feeds returns into a per-venue `RegimeDetector`, and
    /// republishes the label so the Overview per-venue strip
    /// can render a regime chip next to each row. Default 2 s
    /// — matches the strip's UI poll cadence so a new regime
    /// shows up within one render tick. Set to 0 to disable
    /// the per-venue classifier entirely (the autotuner's
    /// primary regime detector is unaffected).
    #[serde(default = "default_venue_regime_classify_secs")]
    pub venue_regime_classify_secs: u64,

    /// S1.2 — per-strategy capital budget cap in quote-asset
    /// units. Keys are `Strategy::name()` tags (e.g.
    /// `"avellaneda"`, `"funding_arb"`, `"basis"`, `"grid"`).
    /// When a strategy's cumulative live notional
    /// (|inventory| × mid + open-order notional) exceeds the
    /// entry's budget, new quotes that would grow the
    /// strategy's exposure are zeroed out until a fill or
    /// cancel frees capital. Missing keys fall through to the
    /// unbounded legacy behaviour — set the key explicitly to
    /// opt into the gate. Prevents one runaway strategy
    /// (e.g. grid hitting a cliff) from starving the rest of
    /// the engine.
    #[serde(default)]
    pub strategy_capital_budget: std::collections::HashMap<String, Decimal>,

    /// R2.12 — operator-supplied circulating supply per symbol
    /// used by `MarketCapProxyGuard` to compute a `mcap_proxy
    /// = supply × mid` ratio against recent traded notional.
    /// Missing entries disable the guard for that symbol
    /// (signal stays at zero — neutral, not suspicious).
    /// Supply values are in base-asset units (e.g. `1_000_000_000`
    /// for a token with 1B circulating).
    #[serde(default)]
    pub symbol_circulating_supply: std::collections::HashMap<String, Decimal>,

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

    /// PERP-1 — fraction of absolute inventory to unload per
    /// margin-guard tick when `Reduce` fires. Default 0.1 =
    /// 10% per tick (~every 5–10 s at typical poll cadence).
    /// Clamped to [0.01, 1.0] at use-site.
    #[serde(default = "default_margin_reduce_slice_pct")]
    pub margin_reduce_slice_pct: Decimal,

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

    /// EWMA decay factor λ ∈ (0, 1) for the VaR guard's
    /// variance estimator. RiskMetrics default is 0.94 for
    /// daily data. `None` disables the EWMA path — the guard
    /// uses only the equally-weighted sample variance.
    #[serde(default)]
    pub var_guard_ewma_lambda: Option<Decimal>,

    /// 95 %-CVaR (Expected Shortfall) floor. CVaR is the
    /// average loss *conditional on* being in the 5 % worst
    /// tail — strictly more negative than VaR_95 by
    /// construction. Breach throttles size to 0.5 via the
    /// same ladder as `var_guard_limit_95`. `None` skips the
    /// tier. Epic C stage-2 — implemented in
    /// `mm-risk::var_guard` with 4 unit tests; exposed here
    /// so operators can configure tail-severity gating.
    #[serde(default)]
    pub var_guard_cvar_limit_95: Option<Decimal>,

    /// 99 %-CVaR floor. Hard-halt tier; breach drops size to
    /// 0.0. Typically more negative than `var_guard_limit_99`.
    /// `None` skips the tier.
    #[serde(default)]
    pub var_guard_cvar_limit_99: Option<Decimal>,

    /// Maximum acceptable hedge-book staleness in milliseconds
    /// for the cross-venue basis strategy (P1.4 stage-1).
    /// Default `1500` — typical cross-venue WS feeds jitter
    /// 200-800 ms in steady state, so 1.5 s catches a stalled
    /// feed without false positives. The engine threads this
    /// into `BasisStrategy::cross_venue` whenever
    /// `StrategyType::CrossVenueBasis` is selected.
    #[serde(default = "default_cross_venue_basis_max_staleness_ms")]
    pub cross_venue_basis_max_staleness_ms: i64,

    /// Minimum round-trip profit (bps) required before the
    /// `CrossExchange` strategy is willing to quote. Applied on
    /// top of the expected taker fee on the hedge venue + our
    /// maker rebate on the primary. Default `5` bps — tight
    /// enough to catch most dislocations on BTC/ETH, wide enough
    /// that stale-book wiggle does not fake a profitable quote.
    /// Ignored when strategy is not `CrossExchange`.
    #[serde(default = "default_cross_exchange_min_profit_bps")]
    pub cross_exchange_min_profit_bps: Decimal,

    /// Maximum acceptable divergence between the primary book's
    /// mid and the hedge book's mid, expressed as a fraction of
    /// the primary mid. When the two venues disagree by more than
    /// this percentage the engine skips the refresh tick instead
    /// of feeding a stale / bogus ref_price into a cross-product
    /// strategy. Typical value `0.005` (50 bps): BTC/ETH cross-
    /// venue steady state is under 10 bps, so 50 bps catches a
    /// halted feed or a bad venue before it drives a bad quote,
    /// without false-flagging normal wiggle. `None` disables the
    /// guard — legacy behaviour. Ignored when no hedge book is
    /// configured.
    #[serde(default)]
    pub max_cross_venue_divergence_pct: Option<Decimal>,
}

fn default_cross_exchange_min_profit_bps() -> Decimal {
    dec!(5)
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

fn default_learned_microprice_horizon() -> usize {
    10
}

fn default_sor_dispatch_interval_secs() -> u64 {
    5
}

fn default_sor_urgency() -> Decimal {
    dec!(0.4)
}

fn default_sor_trade_rate_window_secs() -> u64 {
    60
}

fn default_sor_queue_refresh_secs() -> u64 {
    2
}

fn default_sor_extra_l1_poll_secs() -> u64 {
    5
}

fn default_venue_regime_classify_secs() -> u64 {
    2
}

/// Qty-source policy for the auto-dispatch SOR tick (Epic A
/// stage-2 #1). Picks which engine-level signal drives the
/// target qty + side of each dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SorTargetSource {
    /// Dispatch only when `|inventory| > sor_inventory_threshold`,
    /// targeting `|inventory| − threshold` of reduction. The
    /// safest default — SOR only fires when the engine is
    /// over-exposed.
    #[default]
    InventoryExcess,
    /// Dispatch every tick with `|last_hedge_basket.entries|`
    /// worth of the first leg matching this symbol. Used by
    /// operators running a live hedge optimizer advisory.
    /// Skips when the optimizer's basket is empty.
    HedgeBudget,
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

/// Epic F stage-3 — listing sniper real-entry policy with
/// a layered safety envelope. Every check below is a hard
/// prerequisite — failure on any one skips the entry and
/// logs a reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingSniperEntryConfig {
    /// Master switch. When `false` the sniper stays in
    /// observer-only mode: audit + alert on discovery but
    /// do not place orders. Default `false` so opting in
    /// is explicit. Operators who want automated sniping
    /// flip this alongside a tight `max_notional_usd`.
    #[serde(default)]
    pub enter_on_discovery: bool,
    /// Entry notional cap in quote asset. Each sniper entry
    /// buys `max_notional_usd / ask` base-asset units (lot-
    /// rounded). Keep modest — new listings on thin books
    /// slip hard. Default 50 USDT.
    #[serde(default = "default_sniper_notional_usd")]
    pub max_notional_usd: Decimal,
    /// Quarantine window in seconds. A newly-discovered
    /// symbol must be observed for at least this long
    /// before any entry fires. Defends against venue data
    /// glitches and "fake listing" wire bursts.
    #[serde(default = "default_sniper_quarantine_secs")]
    pub quarantine_secs: u64,
    /// Max simultaneously-open sniper entries across every
    /// venue. Once reached, new discoveries accumulate in
    /// the pending queue but no orders are placed until
    /// active count drops (via cancel / fill / time-out).
    /// Default 3 so one bad wire snapshot cannot blow up
    /// the account.
    #[serde(default = "default_sniper_max_active")]
    pub max_active_entries: u32,
    /// Only snipe symbols whose `trading_status == Trading`.
    /// Skips `PreTrading`, `Halted`, `Break`, `Delisted`
    /// states. Default `true`. Operators wanting to snipe
    /// pre-open (where some venues expose the symbol before
    /// it trades) can flip this.
    #[serde(default = "default_true")]
    pub require_trading_status: bool,
}

fn default_sniper_notional_usd() -> Decimal {
    dec!(50)
}
fn default_sniper_quarantine_secs() -> u64 {
    30
}
fn default_sniper_max_active() -> u32 {
    3
}

impl Default for ListingSniperEntryConfig {
    fn default() -> Self {
        Self {
            enter_on_discovery: false,
            max_notional_usd: default_sniper_notional_usd(),
            quarantine_secs: default_sniper_quarantine_secs(),
            max_active_entries: default_sniper_max_active(),
            require_trading_status: true,
        }
    }
}

/// Epic B stage-2 — background cointegration pair screener.
/// Scans a fixed list of candidate pairs on a periodic
/// schedule and writes results into the audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairScreenerConfig {
    /// Candidate pairs as `(y_symbol, x_symbol)` tuples. The
    /// Engle-Granger test treats `y = β·x + ε` so ordering
    /// matters for the reported β — operators pick `y` as
    /// the more-liquid asset by convention.
    pub pairs: Vec<(String, String)>,
    /// How often the task polls one fresh mid per configured
    /// symbol from the primary connector's
    /// `get_orderbook(symbol, 1)`. Shorter cadence = the
    /// sample window covers a shorter wall-clock span for
    /// the same number of samples. Default 10 s — the
    /// cointegration regression does not need sub-second
    /// resolution, and slower sampling conserves rate-limit
    /// budget. Must be ≥ 1.
    #[serde(default = "default_pair_screener_sample_secs")]
    pub sample_interval_secs: u64,
    /// How often the task runs `PairScreener::screen_all()`
    /// and emits audit events for the cointegration result.
    /// Default 300 s — screening is a diagnostic tool,
    /// minute-by-minute output would be noise. Must be ≥
    /// `sample_interval_secs`.
    #[serde(default = "default_pair_screener_scan_secs")]
    pub scan_interval_secs: u64,
}

fn default_pair_screener_sample_secs() -> u64 {
    10
}
fn default_pair_screener_scan_secs() -> u64 {
    300
}

/// Epic A stage-2 #3 — one SOR-only venue that the greedy
/// router considers alongside the primary and hedge venues.
/// The connector is built with the same `create_connector`
/// path as `config.exchange`, so every field (auth,
/// withdraw whitelist, etc.) is supported uniformly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SorVenueConfig {
    /// Full exchange connector config for this venue.
    pub exchange: ExchangeConfig,
    /// Symbol to quote on this venue. Usually the same base
    /// asset as the primary engine (e.g. `"BTCUSDT"`) but
    /// can differ on venues that use distinct ticker
    /// conventions (`"BTC-USDT"`, `"BTCUSD_PERP"`).
    pub symbol: String,
    /// Max base-asset qty the router may push through this
    /// venue. Maps 1:1 to `VenueSeed.available_qty`.
    pub max_inventory: Decimal,
}

/// Margin mode keyword for perp accounts (Epic 40.7). Mirrors
/// the `exchange::core::MarginMode` enum but lives in `common`
/// so `AppConfig` can deserialize it without a circular dep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MarginModeCfg {
    #[default]
    Isolated,
    Cross,
}

/// Per-symbol margin overrides (Epic 40.7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerSymbolMargin {
    #[serde(default)]
    pub mode: MarginModeCfg,
    #[serde(default = "default_leverage")]
    pub leverage: u32,
}

fn default_leverage() -> u32 {
    5
}

fn default_margin_refresh_secs() -> u64 {
    5
}

fn default_margin_reduce_slice_pct() -> Decimal {
    rust_decimal_macros::dec!(0.1)
}

fn default_widen_ratio() -> Decimal {
    dec!(0.50)
}

fn default_stop_ratio() -> Decimal {
    dec!(0.80)
}

fn default_cancel_ratio() -> Decimal {
    dec!(0.90)
}

fn default_max_stale_secs() -> u64 {
    30
}

fn default_mmr() -> Decimal {
    dec!(0.005)
}

/// Margin guard configuration (Epic 40.4). Thresholds express
/// `margin_ratio = totalMaintMargin / totalMarginBalance`; when
/// the observed or projected ratio crosses a threshold the kill
/// switch is escalated monotonically. Values must satisfy
/// `widen < stop < cancel` — `validate.rs` rejects anything
/// else. Same `KillLevel` cascade applies whether the trip is
/// observed (poll loop) or projected (pre-order hook).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginConfig {
    /// How often the engine polls `account_margin_info()` from
    /// the venue. Venues publish every 1–2 s (Binance push) or
    /// allow a 5 s poll under weight budget; too-frequent polls
    /// burn rate-limit tokens without changing guard decisions.
    #[serde(default = "default_margin_refresh_secs")]
    pub refresh_interval_secs: u64,
    /// Ratio threshold for `KillLevel::WidenSpreads`.
    #[serde(default = "default_widen_ratio")]
    pub widen_ratio: Decimal,
    /// PERP-1 — ratio threshold at which the engine starts
    /// shipping proactive reduce-only IoC slices to lower
    /// position *before* `stop_ratio`. `None` falls back to
    /// the midpoint of `(widen, stop)`.
    #[serde(default)]
    pub reduce_ratio: Option<Decimal>,
    /// Ratio threshold for `KillLevel::StopNewOrders`.
    #[serde(default = "default_stop_ratio")]
    pub stop_ratio: Decimal,
    /// Ratio threshold for `KillLevel::CancelAll`.
    #[serde(default = "default_cancel_ratio")]
    pub cancel_ratio: Decimal,
    /// Maximum acceptable age of the last-received margin
    /// snapshot before the guard escalates to `WidenSpreads`
    /// on stale data. Defends against a silent venue data
    /// outage after a successful handshake.
    #[serde(default = "default_max_stale_secs")]
    pub max_stale_secs: u64,
    /// Default mode applied to every perp symbol that has no
    /// explicit entry in `per_symbol`. Startup hard-fails if
    /// `set_margin_mode` returns anything other than `Ok` or
    /// `NotSupported`.
    #[serde(default)]
    pub default_mode: MarginModeCfg,
    /// Default leverage applied where `per_symbol` is silent.
    #[serde(default = "default_leverage")]
    pub default_leverage: u32,
    /// PERP-2 — fallback maintenance-margin rate (MM as a
    /// fraction of notional) used for projected-ratio
    /// calculations when the venue snapshot has no open
    /// position to infer the effective rate from. Most
    /// venues publish MMRs in `[0.004, 0.01]` for majors; we
    /// default to `0.005` (0.5%) as a conservative middle
    /// value. When positions are open the guard prefers the
    /// inferred MMR over this constant.
    #[serde(default = "default_mmr")]
    pub default_maintenance_margin_rate: Decimal,
    /// Symbol-scoped overrides. Missing symbols fall back to
    /// `default_mode` + `default_leverage`.
    #[serde(default)]
    pub per_symbol: std::collections::HashMap<String, PerSymbolMargin>,
}

impl Default for MarginConfig {
    fn default() -> Self {
        Self {
            refresh_interval_secs: default_margin_refresh_secs(),
            widen_ratio: default_widen_ratio(),
            reduce_ratio: None,
            stop_ratio: default_stop_ratio(),
            cancel_ratio: default_cancel_ratio(),
            max_stale_secs: default_max_stale_secs(),
            default_mode: MarginModeCfg::Isolated,
            default_leverage: default_leverage(),
            default_maintenance_margin_rate: default_mmr(),
            per_symbol: Default::default(),
        }
    }
}

impl MarginConfig {
    /// Resolve effective (`mode`, `leverage`) for a symbol,
    /// falling back to the defaults.
    pub fn for_symbol(&self, symbol: &str) -> (MarginModeCfg, u32) {
        match self.per_symbol.get(symbol) {
            Some(ps) => (ps.mode, ps.leverage),
            None => (self.default_mode, self.default_leverage),
        }
    }

    /// `widen < stop < cancel`, all ∈ (0, 1]. Returns a
    /// descriptive error on violation so `validate.rs` can
    /// surface the bad field without hand-rolled checks.
    pub fn validate_thresholds(&self) -> Result<(), String> {
        for (name, v) in [
            ("widen_ratio", self.widen_ratio),
            ("stop_ratio", self.stop_ratio),
            ("cancel_ratio", self.cancel_ratio),
        ] {
            if v <= Decimal::ZERO || v > Decimal::ONE {
                return Err(format!("margin.{name} must be in (0, 1], got {v}"));
            }
        }
        if self.widen_ratio >= self.stop_ratio {
            return Err(format!(
                "margin.widen_ratio ({}) must be < stop_ratio ({})",
                self.widen_ratio, self.stop_ratio
            ));
        }
        if self.stop_ratio >= self.cancel_ratio {
            return Err(format!(
                "margin.stop_ratio ({}) must be < cancel_ratio ({})",
                self.stop_ratio, self.cancel_ratio
            ));
        }
        Ok(())
    }

    /// Returns `true` if any configured entry (default or per-
    /// symbol) selects cross margin. Startup uses this to
    /// enforce "cross only safe when hedged" — a cross entry
    /// without a live `hedge_optimizer` is a boot-time refusal.
    pub fn uses_cross_margin(&self) -> bool {
        self.default_mode == MarginModeCfg::Cross
            || self
                .per_symbol
                .values()
                .any(|ps| ps.mode == MarginModeCfg::Cross)
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
    /// Epic D-stage-2 — route trade volume through the
    /// Easley-López de Prado-O'Hara Bulk Volume Classification
    /// instead of the per-trade tick-rule on `Trade::taker_side`.
    /// BVC reads a bar's total volume + price change and splits
    /// into buy / sell via the Student-t CDF. Cleaner signal on
    /// fast tapes where `taker_side` is noisy. When `false`
    /// (default) the engine keeps the legacy `on_trade` path
    /// byte-identical to pre-stage-2.
    #[serde(default)]
    pub bvc_enabled: bool,
    /// Student-t degrees of freedom ν for the BVC CDF. Easley
    /// et al. 2012 used `0.25` on S&P E-minis; crypto's heavier
    /// tails may warrant tuning per venue.
    #[serde(default = "default_bvc_nu")]
    pub bvc_nu: Decimal,
    /// Rolling-window size for the bar-Δ mean / std that
    /// standardise the Student-t input.
    #[serde(default = "default_bvc_window")]
    pub bvc_window: usize,
    /// Bar length in seconds — duration we aggregate trade
    /// volume + Δprice over before sending one `(dp, vol)`
    /// observation to the classifier. Shorter = fresher
    /// signal, noisier; 1 s is the Easley-Prado default for
    /// HFT.
    #[serde(default = "default_bvc_bar_secs")]
    pub bvc_bar_secs: u64,
}

fn default_bvc_nu() -> Decimal {
    dec!(0.25)
}
fn default_bvc_window() -> usize {
    50
}
fn default_bvc_bar_secs() -> u64 {
    1
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
            bvc_enabled: false,
            bvc_nu: default_bvc_nu(),
            bvc_window: default_bvc_window(),
            bvc_bar_secs: default_bvc_bar_secs(),
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

/// Paper mode fill simulation configuration (Epic 8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperFillCfg {
    /// Probability of fill when price touches order level.
    #[serde(default = "default_paper_fill_prob")]
    pub prob_fill_on_touch: Decimal,
    /// Probability of adverse slippage on fill.
    #[serde(default = "default_paper_slip_prob")]
    pub prob_slippage: Decimal,
    /// Slippage magnitude in bps.
    #[serde(default = "default_paper_slip_bps")]
    pub slippage_bps: Decimal,
    /// Simulated round-trip latency in ms.
    #[serde(default = "default_paper_latency")]
    pub latency_ms: u64,
}

fn default_paper_fill_prob() -> Decimal {
    dec!(0.6)
}
fn default_paper_slip_prob() -> Decimal {
    dec!(0.05)
}
fn default_paper_slip_bps() -> Decimal {
    dec!(1)
}
fn default_paper_latency() -> u64 {
    5
}

/// A/B split test configuration (Epic 6 item 6.2).
///
/// Declares two parameter variants and how the engine alternates
/// between them. The engine applies `variant.gamma_mult`,
/// `spread_mult`, and `size_mult` to the tuned strategy output
/// every tick and records per-variant PnL so operators can pick
/// the winner after a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbSplitCfg {
    pub variant_a: AbVariantCfg,
    pub variant_b: AbVariantCfg,
    /// "time_based" → alternates every `period_ticks`; "symbol_based"
    /// → partitions by symbol hash (stable per symbol).
    #[serde(default = "default_ab_split_mode")]
    pub mode: String,
    /// Ticks per variant in time-based mode. Ignored in symbol-based
    /// mode.
    #[serde(default = "default_ab_period_ticks")]
    pub period_ticks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbVariantCfg {
    pub name: String,
    #[serde(default = "one")]
    pub gamma_mult: Decimal,
    #[serde(default = "one")]
    pub spread_mult: Decimal,
    #[serde(default = "one")]
    pub size_mult: Decimal,
}

fn default_ab_split_mode() -> String {
    "time_based".into()
}
fn default_ab_period_ticks() -> u64 {
    60
}
fn one() -> Decimal {
    dec!(1)
}

/// Cross-venue rebalancer configuration (Epic 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebalancerCfg {
    /// Auto-execute transfers when thresholds are breached.
    #[serde(default)]
    pub auto_execute: bool,
    /// Check interval in seconds. Default 300.
    #[serde(default = "default_rebalancer_interval")]
    pub check_interval_secs: u64,
    /// Minimum cooldown between transfers per asset. Default 600.
    #[serde(default = "default_rebalancer_cooldown")]
    pub cooldown_secs: u64,
    /// Minimum balance per venue per asset. Default 0.
    #[serde(default)]
    pub min_balance_per_venue: Decimal,
    /// Target balance per venue per asset. Default 0.
    #[serde(default)]
    pub target_balance_per_venue: Decimal,
    /// Max transfer amount per cycle. Default 0 (unlimited).
    #[serde(default)]
    pub max_transfer_per_cycle: Decimal,
}

fn default_rebalancer_interval() -> u64 {
    300
}
fn default_rebalancer_cooldown() -> u64 {
    600
}

/// R3.7 — on-chain surveillance config. Operators wire this
/// when they want the engine to consult an on-chain API for
/// holder concentration and suspect-wallet CEX deposit flow.
/// Provider choice is a string so adding a fifth provider is
/// a one-line workspace addition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnchainCfg {
    /// Primary provider: `"goldrush"`, `"etherscan"`,
    /// `"moralis"`, or `"alchemy"`. API key is supplied via
    /// the matching `MM_{PROVIDER}_KEY` env var — never in
    /// config.
    pub provider: String,
    /// Fallback provider used when the primary returns
    /// `UnsupportedChain` or repeatedly rate-limits. Same
    /// enum of names; `None` disables fallback.
    #[serde(default)]
    pub fallback: Option<String>,
    /// Per-symbol chain + token + suspect wallet list. Key
    /// is symbol (matches `config.symbols`), value is the
    /// on-chain context for that symbol's base token.
    #[serde(default)]
    pub symbols: std::collections::HashMap<String, OnchainSymbolCfg>,
    /// Known CEX deposit address allowlist for the tracker
    /// (lowercase hex). Transfers from a suspect wallet to
    /// an address in this set count as CEX inflow events.
    #[serde(default)]
    pub cex_deposit_addresses: Vec<String>,
    /// Holder concentration refresh interval (seconds).
    /// Default 3600 — 1 hour; distribution moves slowly for
    /// most symbols.
    #[serde(default = "default_onchain_holder_refresh")]
    pub holder_refresh_secs: u64,
    /// Suspect wallet inflow poll interval (seconds).
    /// Default 300 — 5 min; tight enough to catch pre-dump
    /// loading, loose enough to stay in the free tier.
    #[serde(default = "default_onchain_inflow_poll")]
    pub inflow_poll_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnchainSymbolCfg {
    /// Chain slug (`"eth-mainnet"`, `"bsc-mainnet"`, …).
    pub chain: String,
    /// Token contract address on that chain.
    pub token: String,
    /// Per-symbol suspect wallet list. Team + known whales.
    #[serde(default)]
    pub suspect_wallets: Vec<String>,
}

fn default_onchain_holder_refresh() -> u64 {
    3600
}
fn default_onchain_inflow_poll() -> u64 {
    300
}

/// Per-client report branding (Epic 5 item 5.6).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReportBranding {
    #[serde(default)]
    pub company_name: String,
    #[serde(default)]
    pub logo_url: Option<String>,
    #[serde(default)]
    pub footer_text: Option<String>,
    #[serde(default)]
    pub contact_email: Option<String>,
}

/// Portfolio-level risk configuration (Epic 3). Serializable
/// TOML-friendly mirror of `mm_risk::portfolio_risk::PortfolioRiskConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioRiskCfg {
    /// Maximum total absolute delta across all factors in USD.
    #[serde(default = "default_portfolio_max_delta")]
    pub max_total_delta_usd: Decimal,
    /// Per-factor limits.
    #[serde(default)]
    pub factor_limits: Vec<PortfolioFactorLimitCfg>,
}

/// Per-factor limit entry for portfolio risk config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioFactorLimitCfg {
    pub factor: String,
    pub max_net_delta: Decimal,
    #[serde(default = "default_portfolio_widen_mult")]
    pub widen_mult: Decimal,
    #[serde(default = "default_portfolio_warn_pct")]
    pub warn_pct: Decimal,
}

fn default_portfolio_max_delta() -> Decimal {
    dec!(100_000)
}
fn default_portfolio_widen_mult() -> Decimal {
    dec!(2)
}
fn default_portfolio_warn_pct() -> Decimal {
    dec!(0.8)
}

/// Per-client configuration for multi-client isolation (Epic 1).
///
/// Each client owns a set of symbols and has its own SLA targets,
/// webhook URLs, and API keys. The engine spawns per-symbol tasks
/// tagged with the owning `client_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Unique client identifier (e.g., "acme-capital").
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Symbols this client owns. Each symbol must appear in
    /// exactly one client; validated at startup.
    pub symbols: Vec<String>,
    /// Per-client SLA targets. Falls back to the global
    /// `AppConfig.sla` when `None`.
    #[serde(default)]
    pub sla: Option<SlaObligationConfig>,
    /// Per-client webhook URLs for event delivery.
    #[serde(default)]
    pub webhook_urls: Vec<String>,
    /// API keys scoped to this client. Users authenticating
    /// with one of these keys see only this client's symbols.
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// Per-client report branding (Epic 5 item 5.6).
    #[serde(default)]
    pub report_branding: Option<ReportBranding>,
    /// Per-client daily loss circuit breaker (Epic 6). When set
    /// and the client's aggregate daily PnL across all owned
    /// symbols drops below `-daily_loss_limit_usd`, every one of
    /// this client's symbols is halted (kill switch L3 / cancel
    /// all) and new orders are refused until manual reset. `None`
    /// inherits the global `KillSwitchCfg.daily_loss_limit` from
    /// the server — per-client isolation means one client blowing
    /// up their budget does NOT stop the other clients from
    /// trading.
    #[serde(default)]
    pub daily_loss_limit_usd: Option<Decimal>,
    /// Client jurisdiction (Epic 40.10) — ISO 3166-1 alpha-2
    /// country code or `"global"`. Drives product gating:
    /// `"US"` blocks perp products entirely (Binance/Bybit/OKX/
    /// HyperLiquid perp access is KYC-gated for US persons; serving
    /// them through the MM would put the operator in breach).
    /// Default `"global"` = no restriction. Enforced at
    /// `POST /api/admin/clients` ingress and at engine startup —
    /// hard-fails boot if a US-tagged client owns a symbol whose
    /// config product is not spot. Intentionally strict: the cost
    /// of an accidental perp order for a US client is regulatory,
    /// not operational, so we fail closed.
    #[serde(default = "default_jurisdiction")]
    pub jurisdiction: String,
}

fn default_jurisdiction() -> String {
    "global".to_string()
}

impl ClientConfig {
    /// Whether this client is permitted to trade the given
    /// product type. Returns `false` only for explicitly
    /// gated combinations (currently `US × perp`). Extend the
    /// match arm as new jurisdictions are onboarded.
    pub fn allows_product(&self, product: ProductType) -> bool {
        let j = self.jurisdiction.to_ascii_uppercase();
        !matches!(
            (j.as_str(), product),
            ("US", ProductType::LinearPerp) | ("US", ProductType::InversePerp)
        )
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
                product: ProductType::Spot,
                rest_url: "http://localhost:8080".into(),
                ws_url: "ws://localhost:8080/ws/v1".into(),
                api_key: None,
                api_secret: None,
                read_key: None,
                read_secret: None,
                withdraw_whitelist: None,
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
                adaptive_enabled: false,
                apply_pair_class_template: false,
                hma_window: 9,
                momentum_ofi_enabled: false,
                momentum_learned_microprice_path: None,
                momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
                momentum_learned_microprice_online: false,
                momentum_learned_microprice_horizon: default_learned_microprice_horizon(),
                user_stream_enabled: true,
                inventory_drift_tolerance: dec!(0.0001),
                inventory_drift_auto_correct: false,
                amend_enabled: true,
                amend_max_ticks: 2,
                margin_reduce_slice_pct: "0.1".parse().unwrap(),
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
                var_guard_ewma_lambda: None,
                var_guard_cvar_limit_95: None,
                var_guard_cvar_limit_99: None,
                cross_venue_basis_max_staleness_ms: 1500,
                sor_inline_enabled: false,
                sor_dispatch_interval_secs: default_sor_dispatch_interval_secs(),
                sor_urgency: default_sor_urgency(),
                sor_target_qty_source: SorTargetSource::default(),
                sor_inventory_threshold: dec!(0),
                sor_trade_rate_window_secs: default_sor_trade_rate_window_secs(),
                sor_queue_refresh_secs: default_sor_queue_refresh_secs(),
                sor_extra_l1_poll_secs: default_sor_extra_l1_poll_secs(),
                venue_regime_classify_secs: default_venue_regime_classify_secs(),
                strategy_capital_budget: std::collections::HashMap::new(),
                symbol_circulating_supply: std::collections::HashMap::new(),
                cross_exchange_min_profit_bps: dec!(5),
                max_cross_venue_divergence_pct: None,
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
            checkpoint_restore: false,
            log_file: String::new(),
            mode: "live".into(),
            users: vec![],
            telegram: TelegramAlertConfig::default(),
            listing_sniper: ListingSniperConfig::default(),
            loans: std::collections::HashMap::new(),
            hedge: None,
            funding_arb: None,
            stat_arb: None,
            protections: None,
            portfolio_var: None,
            xemm: None,
            execution: None,
            record_market_data: false,
            paper_fill: None,
            ab_split: None,
            rebalancer: None,
            onchain: None,
            portfolio_risk: None,
            clients: Vec::new(),
            margin: None,
            sor_extra_venues: Vec::new(),
            pair_screener: None,
            archive: None,
            schedule: None,
            lead_lag: None,
            news_retreat: None,
            sentiment: None,
            listing_sniper_entry: None,
        }
    }
}

impl AppConfig {
    /// Resolve the effective client list. When `clients` is
    /// non-empty, returns it as-is. When empty (legacy mode),
    /// synthesises a single `"default"` client owning all
    /// `self.symbols`.
    pub fn effective_clients(&self) -> Vec<ClientConfig> {
        if !self.clients.is_empty() {
            return self.clients.clone();
        }
        vec![ClientConfig {
            id: "default".to_string(),
            name: "Default".to_string(),
            symbols: self.symbols.clone(),
            sla: None,
            webhook_urls: Vec::new(),
            api_keys: Vec::new(),
            report_branding: None,
            daily_loss_limit_usd: None,
            jurisdiction: default_jurisdiction(),
        }]
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
