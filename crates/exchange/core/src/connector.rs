use async_trait::async_trait;
use mm_common::types::{
    Balance, Fill, LiveOrder, OrderId, OrderType, Price, PriceLevel, ProductSpec, Qty, Side,
    TimeInForce,
};
use tokio::sync::mpsc;

use crate::events::MarketEvent;

/// Unique venue identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VenueId {
    /// Our own exchange.
    Custom,
    Binance,
    Bybit,
    Okx,
    Kraken,
    Coinbase,
    HyperLiquid,
}

impl std::fmt::Display for VenueId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VenueId::Custom => write!(f, "custom"),
            VenueId::Binance => write!(f, "binance"),
            VenueId::Bybit => write!(f, "bybit"),
            VenueId::Okx => write!(f, "okx"),
            VenueId::Kraken => write!(f, "kraken"),
            VenueId::Coinbase => write!(f, "coinbase"),
            VenueId::HyperLiquid => write!(f, "hyperliquid"),
        }
    }
}

/// Request to place an order.
#[derive(Debug, Clone)]
pub struct NewOrder {
    pub symbol: String,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub qty: Qty,
    pub time_in_force: Option<TimeInForce>,
    /// Client-generated ID for order correlation.
    pub client_order_id: Option<String>,
    /// Perp-only: when `true` the venue is told to refuse the
    /// order if it would INCREASE the position on the given
    /// symbol — guarantees the fill can only unwind. Spot venues
    /// ignore the flag (no margin concept; no way to "reduce" a
    /// wallet position). Engine sets this on:
    ///   - `MarginGuardDecision::Reduce` proactive slices
    ///   - Kill-switch L4 (Flatten) paired unwind slices
    ///   - Stat-arb / funding-arb explicit close legs
    /// Without it the slice can race an adversarial taker and
    /// flip the position THROUGH zero on a fast mover. Every
    /// major perp venue (Binance USDⓈ-M, Bybit V5 linear,
    /// HyperLiquid) supports the flag natively.
    pub reduce_only: bool,
}

/// Request to amend an existing order (keep queue priority where supported).
#[derive(Debug, Clone)]
pub struct AmendOrder {
    pub order_id: OrderId,
    pub symbol: String,
    pub new_price: Option<Price>,
    pub new_qty: Option<Qty>,
}

/// Which product a connector trades on its venue.
///
/// One `ExchangeConnector` instance handles exactly one product on
/// one venue. A venue that exposes spot **and** futures (Binance,
/// Bybit, HyperLiquid, OKX) has a separate connector struct per
/// product — this keeps rate limiters, wallet types, signing rules,
/// and capability flags cleanly separated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VenueProduct {
    /// Plain spot market.
    Spot,
    /// Linear (USDⓈ-margined) perpetual — Binance USDⓈ-M, Bybit
    /// linear, HyperLiquid perps, OKX linear.
    LinearPerp,
    /// Inverse (coin-margined) perpetual — Bybit inverse, BitMEX.
    InversePerp,
    /// Dated USDⓈ-margined futures with an explicit expiry.
    UsdMarginedFuture,
    /// Dated coin-margined futures.
    CoinMarginedFuture,
    /// Options. Reserved — not used by any current connector.
    Option,
}

impl VenueProduct {
    /// The wallet type that funds orders on this product.
    pub fn default_wallet(self) -> mm_common::types::WalletType {
        use mm_common::types::WalletType;
        match self {
            VenueProduct::Spot => WalletType::Spot,
            VenueProduct::LinearPerp | VenueProduct::UsdMarginedFuture => {
                WalletType::UsdMarginedFutures
            }
            VenueProduct::InversePerp | VenueProduct::CoinMarginedFuture => {
                WalletType::CoinMarginedFutures
            }
            VenueProduct::Option => WalletType::UsdMarginedFutures,
        }
    }

    /// `true` if the product pays / charges funding on a periodic
    /// cadence (perps), `false` for spot and dated futures.
    pub fn has_funding(self) -> bool {
        matches!(self, VenueProduct::LinearPerp | VenueProduct::InversePerp)
    }
}

/// A venue's funding rate at a point in time.
///
/// Returned by `ExchangeConnector::get_funding_rate` for perp
/// products. Spot and dated futures return `Err(FundingRateError::NotSupported)`.
#[derive(Debug, Clone)]
pub struct FundingRate {
    /// Funding rate as a fraction of notional. `0.0001` = 1 bps.
    pub rate: rust_decimal::Decimal,
    /// Timestamp at which the next funding interval settles.
    pub next_funding_time: chrono::DateTime<chrono::Utc>,
    /// Cadence of funding settlement (8h on most venues, 1h on some).
    pub interval: std::time::Duration,
}

/// Error when a connector cannot service a `get_funding_rate` call.
#[derive(Debug, thiserror::Error)]
pub enum FundingRateError {
    #[error("funding rate not supported on this product")]
    NotSupported,
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// A venue's effective fee schedule for one symbol, as the venue
/// reports it for *this* account at the moment of the query.
///
/// Captured by [`ExchangeConnector::fetch_fee_tiers`] so the
/// engine can refresh `PnlTracker` rates and the strategy's
/// spread-floor calculations whenever the operator's VIP tier
/// changes — production prop desks see month-end tier crossings
/// silently shave 1-2 bps off captured edge until the next
/// process restart, which P1.2 closes.
#[derive(Debug, Clone)]
pub struct FeeTierInfo {
    /// Maker fee as a fraction of notional. **Negative values
    /// are rebates** (VIP 9 on Bybit / GTC token tier on
    /// Binance, etc.).
    pub maker_fee: rust_decimal::Decimal,
    /// Taker fee as a fraction of notional. Always non-negative
    /// at every venue this connector targets.
    pub taker_fee: rust_decimal::Decimal,
    /// Optional venue-side label of the current tier
    /// (`"VIP1"`, `"PRO1"`, etc.). Logged for operator
    /// visibility; not load-bearing.
    pub vip_tier: Option<String>,
    /// When the connector observed these rates from the venue.
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

/// Snapshot of a venue's borrow rate for one asset on one
/// account. Returned by [`ExchangeConnector::get_borrow_rate`]
/// so the engine can convert the venue-reported APR into an
/// expected-carry-bps surcharge that strategies bake into the
/// ask reservation. P1.3 stage-1 only exposes the rate; stage-2
/// will wire `borrow_asset` / `repay_asset` for actual loan
/// execution.
#[derive(Debug, Clone)]
pub struct BorrowRateInfo {
    /// The asset being quoted (`"BTC"`, `"ETH"`, …).
    pub asset: String,
    /// Annualised borrow rate as a fraction (`0.05` = 5 % APR).
    /// Always non-negative — venues do not pay you to borrow.
    pub rate_apr: rust_decimal::Decimal,
    /// Per-hour borrow rate in basis points, derived from the
    /// APR. Cached on the struct because most strategies want it
    /// in this unit and the conversion is the same across all
    /// venues.
    pub rate_bps_hourly: rust_decimal::Decimal,
    /// When the connector observed this rate from the venue.
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

impl BorrowRateInfo {
    /// Derive the hourly bps from a fraction APR. `apr × 10_000 / 8_760`
    /// (8 760 hours/year). Pure helper so the conversion stays
    /// consistent across every venue override.
    pub fn from_apr(asset: &str, rate_apr: rust_decimal::Decimal) -> Self {
        use rust_decimal::Decimal;
        let hours_per_year = Decimal::from(8_760u32);
        let rate_bps_hourly = rate_apr * Decimal::from(10_000u32) / hours_per_year;
        Self {
            asset: asset.to_string(),
            rate_apr,
            rate_bps_hourly,
            fetched_at: chrono::Utc::now(),
        }
    }
}

/// Error when a connector cannot service a `get_borrow_rate` /
/// `borrow_asset` / `repay_asset` call.
#[derive(Debug, thiserror::Error)]
pub enum BorrowError {
    /// Venue has no margin / borrow product, or this connector
    /// has not implemented the override yet. The engine treats
    /// `NotSupported` as "skip the borrow refresh" — no carry
    /// surcharge is applied to the reservation price.
    #[error("borrow not supported on this venue")]
    NotSupported,
    /// REST/JSON failure surfaced from the venue.
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Error when a connector cannot service a `fetch_fee_tiers` call.
#[derive(Debug, thiserror::Error)]
pub enum FeeTierError {
    /// Venue has no per-account fee endpoint, or this connector
    /// has not implemented the override yet. The engine treats
    /// `NotSupported` as "keep using the rates from the
    /// `ProductSpec` snapshot at startup" — no refresh happens
    /// and no Prometheus gauge update fires.
    #[error("fee tier query not supported on this venue")]
    NotSupported,
    /// REST/JSON failure surfaced from the venue. Carries the
    /// original error string so the engine's audit log can
    /// distinguish "venue down" from "auth rejected".
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Venue capabilities — what features this exchange supports.
#[derive(Debug, Clone)]
pub struct VenueCapabilities {
    /// Max orders per batch request.
    pub max_batch_size: usize,
    /// Supports amend-in-place (keep queue priority).
    pub supports_amend: bool,
    /// Supports WebSocket order entry (not just REST).
    pub supports_ws_trading: bool,
    /// Supports FIX protocol.
    pub supports_fix: bool,
    /// Max orders per second.
    pub max_order_rate: u32,
    /// `get_funding_rate` returns a real value on this connector.
    /// Spot connectors set this to `false`.
    pub supports_funding_rate: bool,
    /// Epic 40.4 — venue exposes an account margin / position
    /// margin endpoint (`/fapi/v2/account`, `/v5/account/
    /// wallet-balance`, `/info clearinghouseState`). Spot
    /// connectors and the custom test connector set this to
    /// `false`; the `MarginGuard` skips the poll entirely.
    pub supports_margin_info: bool,
    /// Epic 40.7 — venue accepts `set_margin_mode` / `set_leverage`
    /// (Binance `/fapi/v1/marginType`, Bybit
    /// `/v5/account/set-margin-mode`, HL `updateLeverage`). Spot
    /// connectors set this to `false`.
    pub supports_margin_mode: bool,
    /// R5.5 — venue publishes a forced-liquidation WS stream
    /// the connector subscribes to (Binance `!forceOrder@arr`,
    /// Bybit `liquidation`). Engine's `LiquidationHeatmap`
    /// only populates when this is `true` on at least one
    /// connector. Spot venues and test connectors set this to
    /// `false` — surveillance consumers get `Missing` and
    /// fail-open gracefully.
    pub supports_liquidation_feed: bool,
    /// R5.5 — venue accepts `set_leverage(symbol, leverage)`
    /// as a pre-trade account-level mutation. Required for
    /// `Strategy.LeverageBuilder` to actually configure
    /// account leverage; without it the strategy falls back
    /// to whatever leverage the account is already set to and
    /// logs a warning.
    pub supports_set_leverage: bool,
}

/// R7.1 — long/short account ratio on perp markets. Venues
/// expose this to show aggregate positioning of their retail
/// users. Pump-and-dump campaign authors consume it to tell
/// when the crowd is one-sided enough to build a squeeze into.
/// Consumers: the honest MM (widen when positioning is
/// extreme, mean-revert is likely); the pentest operator
/// (target cluster that corresponds to the majority side).
#[derive(Debug, Clone)]
pub struct LongShortRatio {
    pub symbol: String,
    /// Fraction of accounts that are net-long (0..=1).
    pub long_pct: rust_decimal::Decimal,
    /// Fraction of accounts that are net-short (0..=1).
    pub short_pct: rust_decimal::Decimal,
    /// `long_pct / short_pct`. `1.0` = balanced; `> 2.0` =
    /// crowd one-sided long; `< 0.5` = crowd one-sided short.
    pub ratio: rust_decimal::Decimal,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// R6.3 — per-symbol open-interest snapshot. `oi_contracts` is
/// the raw contract / coin count; `oi_usd` is the notional when
/// the venue reports it. One of the two may be `None` —
/// Binance returns both, Bybit returns both, HL derives from
/// `clearinghouseState`. Timestamp is venue-side when provided.
#[derive(Debug, Clone)]
pub struct OpenInterestInfo {
    pub symbol: String,
    pub oi_contracts: Option<rust_decimal::Decimal>,
    pub oi_usd: Option<rust_decimal::Decimal>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Account-level margin snapshot (Epic 40.4). One per venue; the
/// engine polls it every ~5 s and feeds the result through
/// `MarginGuard` into `kill_switch::update_margin_ratio`.
#[derive(Debug, Clone)]
pub struct AccountMarginInfo {
    /// `totalMarginBalance` / `totalEquity` / `accountValue` —
    /// our total notional including unrealised PnL.
    pub total_equity: rust_decimal::Decimal,
    /// `totalInitialMargin` — sum of IM across open positions.
    pub total_initial_margin: rust_decimal::Decimal,
    /// `totalMaintMargin` — sum of MM across open positions. The
    /// guard's primary ratio `MM / equity` drives kill-switch
    /// escalation.
    pub total_maintenance_margin: rust_decimal::Decimal,
    /// Free cash available for new-order IM.
    pub available_balance: rust_decimal::Decimal,
    /// `MM / total_equity` ∈ [0, 1]. Venue publishes this
    /// directly on some APIs (Bybit `accountMMRate`); we compute
    /// it client-side otherwise. Guard reads THIS value, not
    /// the individual fields, for escalation decisions.
    pub margin_ratio: rust_decimal::Decimal,
    /// Per-position detail. Keyed-by-symbol lookup inside the
    /// guard lets us answer "will adding N notional to BTC push
    /// us over the line?".
    pub positions: Vec<PositionMargin>,
    /// Wall-clock timestamp the venue reported this snapshot,
    /// in unix-millis. Used by the guard's staleness check.
    pub reported_at_ms: i64,
}

/// Per-position margin detail from the venue (Epic 40.4).
#[derive(Debug, Clone)]
pub struct PositionMargin {
    pub symbol: String,
    pub side: mm_common::types::Side,
    pub size: rust_decimal::Decimal,
    pub entry_price: rust_decimal::Decimal,
    pub mark_price: rust_decimal::Decimal,
    /// Isolated-margin allocation (if the position is isolated).
    /// `None` for cross-margin positions.
    pub isolated_margin: Option<rust_decimal::Decimal>,
    /// Liquidation price from the venue. Never recomputed locally
    /// — venues publish the canonical value per their tiered MMR
    /// brackets which our engine does not model.
    pub liq_price: Option<rust_decimal::Decimal>,
    /// PERP-4 — auto-deleveraging rank / quantile published by
    /// the venue. `0` = safest (last to be ADL'd); `4` = next
    /// in line when an ADL event fires. Binance publishes
    /// `adlQuantile` on `/fapi/v2/positionRisk`; Bybit V5
    /// reports `adlRankIndicator` on `/v5/position/list`
    /// (same 0–4 scale). `None` on venues that don't expose
    /// it or while waiting for the first snapshot.
    pub adl_quantile: Option<u8>,
}

/// Margin mode selection for perp accounts (Epic 40.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarginMode {
    /// Per-symbol isolated IM. Losses capped to the position's
    /// isolated bucket; other positions unaffected. Default.
    Isolated,
    /// Cross-margin — one shared IM pool across all positions.
    /// More capital-efficient but exposes the engine to
    /// cross-symbol liquidation cascades. Only safe when a live
    /// `hedge_optimizer` is in play (validated at startup).
    Cross,
}

impl MarginMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MarginMode::Isolated => "isolated",
            MarginMode::Cross => "cross",
        }
    }
}

/// Margin-related errors from venue calls (Epic 40.4).
#[derive(Debug, thiserror::Error)]
pub enum MarginError {
    /// The venue has no margin concept (spot connectors,
    /// custom-exchange test). Guard treats this as "skip".
    #[error("venue does not support margin info")]
    NotSupported,
    /// Snapshot is older than the guard's staleness budget.
    /// Guard escalates to WidenSpreads.
    #[error("margin snapshot stale ({0} seconds old)")]
    Stale(i64),
    /// Anything else — network error, auth rejection, unexpected
    /// response shape. Guard treats repeated errors as a kill-
    /// switch escalation cue via existing `on_error` cascade.
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// The core exchange connector trait.
///
/// Every exchange (our custom exchange, Binance, Bybit, OKX, etc.)
/// implements this trait for unified access.
#[async_trait]
pub trait ExchangeConnector: Send + Sync {
    // --- Identity ---

    fn venue_id(&self) -> VenueId;
    fn capabilities(&self) -> &VenueCapabilities;

    /// Lower a venue-specific error string into the shared
    /// [`crate::errors::VenueErrorKind`] taxonomy so the engine
    /// can branch retry policy / metrics / alerts without
    /// substring-matching raw `anyhow::Error` messages.
    ///
    /// Default implementation returns `VenueErrorKind::Other`
    /// wrapping the full message. Venue crates override this to
    /// call their own `classify` module. Engines that do not
    /// care about classification (tests, custom connectors) keep
    /// working verbatim.
    fn classify_error(&self, err: &anyhow::Error) -> crate::errors::VenueError {
        crate::errors::VenueError::other(err.to_string())
    }

    /// Which product this connector trades. One connector instance
    /// = one venue × one product; a venue exposing spot and
    /// futures has two separate connector instances.
    fn product(&self) -> VenueProduct;

    /// Current funding rate for a perp symbol. Spot connectors
    /// return `Err(FundingRateError::NotSupported)` (this is the
    /// default impl).
    async fn get_funding_rate(&self, _symbol: &str) -> Result<FundingRate, FundingRateError> {
        Err(FundingRateError::NotSupported)
    }

    /// Effective fee schedule for `symbol` as the venue reports
    /// it for *this* account right now. Engines call this on a
    /// periodic refresh (default every 10 min) so a month-end
    /// VIP tier crossing tightens captured edge immediately
    /// instead of waiting for a process restart. Connectors
    /// without a per-account fee endpoint return
    /// `Err(FeeTierError::NotSupported)` and the engine keeps
    /// using the rates frozen at startup.
    async fn fetch_fee_tiers(&self, _symbol: &str) -> Result<FeeTierInfo, FeeTierError> {
        Err(FeeTierError::NotSupported)
    }

    /// Current borrow rate for `asset` (P1.3 stage-1). Engines
    /// poll this on a slow cadence to drive the borrow-cost
    /// surcharge that the spot ask side bakes into its
    /// reservation price. Venues without a margin product
    /// return `Err(BorrowError::NotSupported)` and the engine
    /// skips the periodic refresh entirely.
    async fn get_borrow_rate(&self, _asset: &str) -> Result<BorrowRateInfo, BorrowError> {
        Err(BorrowError::NotSupported)
    }

    /// Venue-side server time in milliseconds since Unix epoch.
    /// Consumed by the clock-skew preflight check so a ±500 ms
    /// drift is surfaced before the first signed request — HMAC
    /// auth rejects signatures signed outside the venue's
    /// `recv_window`, which is otherwise a cryptic `-1021` at
    /// trade time.
    ///
    /// Returns `Ok(None)` when the venue has no time endpoint.
    /// Default impl is `Ok(None)` so older connectors stay
    /// silent.
    async fn server_time_ms(&self) -> anyhow::Result<Option<i64>> {
        Ok(None)
    }

    /// Account-level margin snapshot (Epic 40.4). Returns
    /// `MarginError::NotSupported` for spot connectors or the
    /// custom test connector. Perp connectors override to hit
    /// `/fapi/v2/account` (Binance), `/v5/account/wallet-balance
    /// + /v5/position/list` (Bybit), or
    /// `POST /info {type:"clearinghouseState"}` (HyperLiquid).
    ///
    /// The engine's `MarginGuard` polls this every ~5 s and
    /// escalates the kill switch based on `margin_ratio`.
    async fn account_margin_info(&self) -> Result<AccountMarginInfo, MarginError> {
        Err(MarginError::NotSupported)
    }

    /// Set per-symbol margin mode (Epic 40.7) — Isolated or
    /// Cross. Binance uses `POST /fapi/v1/marginType` per-symbol;
    /// Bybit uses `POST /v5/account/set-margin-mode` account-wide
    /// (the connector ignores `symbol` when the venue is Bybit);
    /// HyperLiquid uses `POST /exchange { updateLeverage }` per-
    /// asset with `isCross` flag. Spot connectors return
    /// `NotSupported`.
    ///
    /// Idempotent: a venue returning "already in this mode"
    /// (Binance `-4046`, Bybit `110026`) is treated as `Ok(())`
    /// by the wrapper; only new errors propagate.
    async fn set_margin_mode(&self, _symbol: &str, _mode: MarginMode) -> Result<(), MarginError> {
        Err(MarginError::NotSupported)
    }

    /// Set per-symbol leverage (Epic 40.7). Leverage cap applies
    /// on top of the venue's own bracket limits — venues clamp
    /// to their tier-maximum silently. Spot connectors return
    /// `NotSupported`.
    async fn set_leverage(&self, _symbol: &str, _leverage: u32) -> Result<(), MarginError> {
        Err(MarginError::NotSupported)
    }

    /// 24-hour quote-currency volume (turnover) for `symbol`.
    /// Consumed by the Epic 30 `PairClass` classifier at engine
    /// startup to tag the symbol's liquidity tier.
    ///
    /// Returns `Ok(None)` when the venue exposes a ticker but the
    /// response didn't include a volume field; the classifier
    /// treats `None` as "unknown" and falls back to its
    /// conservative default.
    ///
    /// Default impl returns `Ok(None)` so venues that don't want
    /// to wire this yet stay at the conservative default without
    /// any connector change. Each supported venue overrides with
    /// its native 24h ticker endpoint.
    async fn get_24h_volume_usd(
        &self,
        _symbol: &str,
    ) -> anyhow::Result<Option<rust_decimal::Decimal>> {
        Ok(None)
    }

    /// Open a margin loan for `qty` of `asset` (P1.3 stage-2).
    /// Default `NotSupported` — actual loan execution requires
    /// margin-mode order routing on the venue connector and
    /// will land in the next stage of the borrow rollout.
    async fn borrow_asset(
        &self,
        _asset: &str,
        _qty: rust_decimal::Decimal,
    ) -> Result<(), BorrowError> {
        Err(BorrowError::NotSupported)
    }

    /// Repay a margin loan for `qty` of `asset` (P1.3 stage-2).
    async fn repay_asset(
        &self,
        _asset: &str,
        _qty: rust_decimal::Decimal,
    ) -> Result<(), BorrowError> {
        Err(BorrowError::NotSupported)
    }

    // --- Market Data ---

    /// Connect to market data streams. Returns a channel of normalized events.
    async fn subscribe(
        &self,
        symbols: &[String],
    ) -> anyhow::Result<mpsc::UnboundedReceiver<MarketEvent>>;

    /// Get a one-time L2 orderbook snapshot.
    async fn get_orderbook(
        &self,
        symbol: &str,
        depth: u32,
    ) -> anyhow::Result<(Vec<PriceLevel>, Vec<PriceLevel>, u64)>;

    // --- Order Management ---

    /// Place a single order.
    async fn place_order(&self, order: &NewOrder) -> anyhow::Result<OrderId>;

    /// Place multiple orders in a single batch request.
    async fn place_orders_batch(&self, orders: &[NewOrder]) -> anyhow::Result<Vec<OrderId>>;

    /// Cancel a single order.
    async fn cancel_order(&self, symbol: &str, order_id: OrderId) -> anyhow::Result<()>;

    /// Cancel multiple orders in a batch.
    async fn cancel_orders_batch(&self, symbol: &str, order_ids: &[OrderId]) -> anyhow::Result<()>;

    /// Cancel ALL orders for a symbol.
    async fn cancel_all_orders(&self, symbol: &str) -> anyhow::Result<()>;

    /// Amend an existing order (if supported). Falls back to cancel+new if not.
    async fn amend_order(&self, amend: &AmendOrder) -> anyhow::Result<()> {
        // Default: cancel + re-place. Exchanges that support native amend override this.
        self.cancel_order(&amend.symbol, amend.order_id).await?;
        let new = NewOrder {
            symbol: amend.symbol.clone(),
            side: Side::Buy, // Will be overridden by caller with proper side.
            order_type: OrderType::Limit,
            price: amend.new_price,
            qty: amend.new_qty.unwrap_or_default(),
            time_in_force: Some(TimeInForce::PostOnly),
            client_order_id: None,
            reduce_only: false,
        };
        self.place_order(&new).await?;
        Ok(())
    }

    /// Get all open orders for a symbol (for reconciliation).
    async fn get_open_orders(&self, symbol: &str) -> anyhow::Result<Vec<LiveOrder>>;

    /// Fetch user trade fills that happened on the venue at or
    /// after `since_ms`. Used on engine startup to recover fills
    /// that landed while the agent was disconnected — without
    /// this, reconnect leaves the inventory tracker stale by the
    /// number of fills missed while offline, and the only hint
    /// is the balance-drift detector tripping audit N seconds
    /// later.
    ///
    /// Default returns `Ok(Vec::new())` — venues that don't
    /// implement it leave the gap and the operator relies on
    /// `check_inventory_drift` to notice. Venue adapters should
    /// override against their `get_my_trades` / `query_user_trades`
    /// endpoint, filtering by `symbol` + `since_ms`.
    #[allow(unused_variables)]
    async fn get_my_trades_since(&self, symbol: &str, since_ms: i64) -> anyhow::Result<Vec<Fill>> {
        Ok(Vec::new())
    }

    // --- Account ---

    /// Get balances.
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>>;

    /// Withdraw `qty` of `asset` to an external `address` on the
    /// given `network` (e.g. "ETH", "TRX", "SOL"). Returns the
    /// venue's withdrawal ID for tracking.
    ///
    /// Default returns `Err` — venue connectors that support
    /// programmatic withdrawals override this.
    ///
    /// Implementations MUST call [`validate_withdraw_address`]
    /// with their configured `withdraw_whitelist` BEFORE
    /// hitting the network. A compromised trading key should
    /// not be able to drain the account to an attacker-
    /// controlled address even if venue-side withdraw scopes
    /// are accidentally left enabled.
    async fn withdraw(
        &self,
        _asset: &str,
        _qty: rust_decimal::Decimal,
        _address: &str,
        _network: &str,
    ) -> anyhow::Result<String> {
        Err(anyhow::anyhow!("withdraw not supported on this venue"))
    }

    /// Internal transfer between wallets on the same venue
    /// (e.g. spot → futures, main → trading). Returns the
    /// venue's transfer ID.
    ///
    /// `from_wallet` and `to_wallet` are venue-specific strings
    /// (e.g. "SPOT", "LINEAR", "FUNDING" for Bybit).
    /// R6.3 — open interest for `symbol` on the connector's
    /// product. Returns `Ok(None)` when the venue exposes an
    /// OI endpoint but the response was empty; `Err` when the
    /// call failed. Spot connectors override to return `None`
    /// directly — OI is a perp concept.
    async fn get_open_interest(&self, _symbol: &str) -> anyhow::Result<Option<OpenInterestInfo>> {
        Ok(None)
    }

    /// R7.1 — long/short account ratio for `symbol`. Returns
    /// `Ok(None)` when the venue exposes an endpoint but the
    /// response was empty. Spot / custom override to return
    /// `None` directly — L/S ratio is a perp positioning
    /// concept.
    async fn get_long_short_ratio(&self, _symbol: &str) -> anyhow::Result<Option<LongShortRatio>> {
        Ok(None)
    }

    async fn internal_transfer(
        &self,
        _asset: &str,
        _qty: rust_decimal::Decimal,
        _from_wallet: &str,
        _to_wallet: &str,
    ) -> anyhow::Result<String> {
        Err(anyhow::anyhow!(
            "internal_transfer not supported on this venue"
        ))
    }

    /// Get product specification (tick/lot sizes, fees).
    async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec>;

    /// List **all** symbols currently exposed on the venue's public
    /// exchange-info endpoint, including symbols the engine is not
    /// currently subscribed to. Used by the Epic F listing sniper
    /// (`crates/engine/src/listing_sniper.rs`) to detect new listings
    /// and removed/delisted symbols so operators can spin up a
    /// probation engine instance for the new pair.
    ///
    /// Returned specs carry a `trading_status` populated from the
    /// venue's per-symbol state field where the venue surfaces one
    /// (Binance spot, HyperLiquid perps' `isDelisted` flag, etc.);
    /// consumers that only care about "currently trading" symbols
    /// should filter on `trading_status == TradingStatus::Trading`.
    ///
    /// The default impl returns `Err` so venues that do not expose
    /// a public symbol-list endpoint (the custom client venue today,
    /// for example) surface "unsupported" cleanly and the sniper
    /// skips them on the next scan without polluting its cache.
    async fn list_symbols(&self) -> anyhow::Result<Vec<ProductSpec>> {
        Err(anyhow::anyhow!("list_symbols not supported on this venue"))
    }

    // --- Health ---

    /// Health check.
    async fn health_check(&self) -> anyhow::Result<bool>;

    // --- Routing ---

    /// Remaining rate-limit budget in the venue's native
    /// units (tokens / weights / requests-per-window). The
    /// Smart Order Router (Epic A) queries this during
    /// `VenueStateAggregator::collect` to decide whether a
    /// candidate venue has headroom for another dispatch on
    /// the current tick.
    ///
    /// Default `u32::MAX` means "unlimited" — connectors
    /// that do not maintain a rate limiter inherit the
    /// default and the SOR treats them as infinitely
    /// available. Concrete connectors override with their
    /// `RateLimiter::remaining().await` value.
    ///
    /// Async on purpose — the underlying `RateLimiter`
    /// holds a `tokio::Mutex` so the remaining-token query
    /// has to await the lock.
    async fn rate_limit_remaining(&self) -> u32 {
        u32::MAX
    }
}

/// Fail-closed check against a configured withdraw address
/// whitelist. See [`ExchangeConnector::withdraw`] for the
/// integration contract.
///
/// Semantics (mirrors
/// `mm_common::config::ExchangeConfig::withdraw_whitelist`):
/// - `None`: unchecked — legacy setups that rely on venue-side
///   whitelisting. Returns `Ok(())`. Logs once per call at
///   `warn!` so operators see that the guard is not active.
/// - `Some(&[])`: every address is blocked. Used to freeze
///   outflows during an incident.
/// - `Some(addrs)`: only entries in the slice pass.
///
/// Comparison is case-sensitive — the caller is responsible for
/// normalising the on-wire address (lowercase hex for EVM,
/// base58 for SOL, etc.) to match however operators populated
/// the list. A mismatch returns a redacted error (never echoes
/// the attempted address back) to avoid feeding an attacker's
/// probing with exact-match telemetry.
pub fn validate_withdraw_address(
    whitelist: Option<&[String]>,
    address: &str,
) -> anyhow::Result<()> {
    match whitelist {
        None => {
            tracing::warn!(
                "withdraw_whitelist not configured — venue-side controls are the only guard"
            );
            Ok(())
        }
        Some([]) => {
            tracing::warn!(
                target_len = address.len(),
                "withdraw blocked by empty whitelist (fail-closed)"
            );
            Err(anyhow::anyhow!(
                "withdraw blocked: whitelist is empty — populate \
                 exchange.withdraw_whitelist before attempting withdrawals"
            ))
        }
        Some(addrs) => {
            if addrs.iter().any(|a| a == address) {
                Ok(())
            } else {
                tracing::warn!(
                    allowed = addrs.len(),
                    target_len = address.len(),
                    "withdraw blocked: address not in whitelist"
                );
                Err(anyhow::anyhow!(
                    "withdraw blocked: destination address is not in the configured whitelist"
                ))
            }
        }
    }
}

#[cfg(test)]
mod withdraw_whitelist_tests {
    use super::*;

    #[test]
    fn none_means_unchecked() {
        assert!(validate_withdraw_address(None, "0xabc").is_ok());
    }

    #[test]
    fn empty_blocks_all() {
        let empty: &[String] = &[];
        assert!(validate_withdraw_address(Some(empty), "0xabc").is_err());
    }

    #[test]
    fn exact_match_allowed() {
        let list = vec!["0xaAbBcC".to_string(), "bc1qxyz".to_string()];
        assert!(validate_withdraw_address(Some(&list), "0xaAbBcC").is_ok());
        assert!(validate_withdraw_address(Some(&list), "bc1qxyz").is_ok());
    }

    #[test]
    fn mismatch_rejected_and_redacted() {
        let list = vec!["0xaAbBcC".to_string()];
        let err = validate_withdraw_address(Some(&list), "0xdeadBeef").unwrap_err();
        let msg = err.to_string();
        // Error must NOT echo the attempted address back.
        assert!(!msg.contains("0xdead"), "error leaked target: {msg}");
    }

    #[test]
    fn case_sensitive() {
        let list = vec!["0xAABB".to_string()];
        // Mixed case mismatch — operators should normalise
        // before populating the list.
        assert!(validate_withdraw_address(Some(&list), "0xaabb").is_err());
    }
}
