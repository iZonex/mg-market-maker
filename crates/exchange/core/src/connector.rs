use async_trait::async_trait;
use mm_common::types::{
    Balance, LiveOrder, OrderId, OrderType, Price, PriceLevel, ProductSpec, Qty, Side, TimeInForce,
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
        };
        self.place_order(&new).await?;
        Ok(())
    }

    /// Get all open orders for a symbol (for reconciliation).
    async fn get_open_orders(&self, symbol: &str) -> anyhow::Result<Vec<LiveOrder>>;

    // --- Account ---

    /// Get balances.
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>>;

    /// Withdraw `qty` of `asset` to an external `address` on the
    /// given `network` (e.g. "ETH", "TRX", "SOL"). Returns the
    /// venue's withdrawal ID for tracking.
    ///
    /// Default returns `Err` — venue connectors that support
    /// programmatic withdrawals override this.
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
