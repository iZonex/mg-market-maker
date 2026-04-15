use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type Price = Decimal;
pub type Qty = Decimal;
pub type OrderId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    #[serde(rename = "buy")]
    Buy,
    #[serde(rename = "sell")]
    Sell,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    /// Good-Till-Cancelled.
    Gtc,
    /// Immediate-Or-Cancel.
    Ioc,
    /// Fill-Or-Kill.
    Fok,
    /// Post-only — reject if the order would cross the book.
    PostOnly,
    /// Day order — expires at session close.
    Day,
    /// Good-Till-Date. Caller tracks the expiry separately.
    Gtd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// A quote that the strategy wants to place on the book.
#[derive(Debug, Clone)]
pub struct Quote {
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
}

/// Desired two-sided quote from the strategy.
#[derive(Debug, Clone)]
pub struct QuotePair {
    pub bid: Option<Quote>,
    pub ask: Option<Quote>,
}

/// An order we have placed on the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveOrder {
    pub order_id: OrderId,
    pub symbol: String,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub filled_qty: Qty,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
}

/// A fill event from the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub trade_id: u64,
    pub order_id: OrderId,
    pub symbol: String,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub is_maker: bool,
    pub timestamp: DateTime<Utc>,
}

/// Public trade from the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: u64,
    pub symbol: String,
    pub price: Price,
    pub qty: Qty,
    pub taker_side: Side,
    pub timestamp: DateTime<Utc>,
}

/// A single price level in the order book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: Price,
    pub qty: Qty,
}

/// Which sub-account / wallet this balance belongs to.
///
/// On every major venue, `spot`, `margin`, USDⓈ-M futures, COIN-M
/// futures, options, and a dedicated funding wallet are separate
/// sub-accounts with independent balances. The API paths differ
/// (see `docs/research/spot-mm-specifics.md` §5 "Wallet topology").
///
/// Balances from different wallets MUST NOT be conflated by the
/// `BalanceCache`: a spot BTC balance of 1.0 and a futures BTC
/// balance of 0.5 are two different piles of asset, not `max(1.0,
/// 0.5)`. `BalanceCache` is keyed on `(asset, WalletType)` for
/// exactly this reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WalletType {
    /// Plain spot wallet — holds the asset directly.
    Spot,
    /// USDⓈ-margined futures wallet (Binance USDⓈ-M, Bybit linear,
    /// HyperLiquid perps — anything settled in USDT / USDC).
    UsdMarginedFutures,
    /// COIN-margined futures wallet (Binance COIN-M, Bybit inverse).
    CoinMarginedFutures,
    /// Cross/isolated margin spot-with-borrow wallet.
    Margin,
    /// Binance "funding" wallet used for on-chain deposits/withdrawals.
    Funding,
    /// Bybit V5 Unified Trading Account that consolidates spot +
    /// linear + options under one collateral pool.
    Unified,
}

/// Snapshot of balances from the exchange.
///
/// Always carry a `wallet` tag so two connectors reporting the
/// "same" asset from different sub-accounts do not overwrite each
/// other in the `BalanceCache`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub asset: String,
    pub wallet: WalletType,
    pub total: Decimal,
    pub locked: Decimal,
    pub available: Decimal,
}

/// Trading status of a product as the venue currently reports
/// it (P2.3). Polled on a slow cadence by the engine's
/// `PairLifecycleManager` so a halt or delisting event halts
/// quoting before the next refresh tick — venues sometimes send
/// fills *after* a halt, so the engine needs explicit state to
/// reject them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TradingStatus {
    /// Normal trading — the only state where quoting is safe.
    #[default]
    Trading,
    /// Trading is halted (venue circuit breaker, oracle pause,
    /// regulatory hold). Engine cancels all open orders and
    /// stops requesting new ones until the venue flips back.
    Halted,
    /// Symbol exists but trading has not opened yet — common on
    /// new listings during the auction phase.
    PreTrading,
    /// Maintenance break window — same handling as `Halted`
    /// but a soft signal that the venue intends to resume.
    Break,
    /// Symbol has been removed from the venue altogether.
    /// Engine cancels all and refuses to ever requote — only a
    /// process restart can lift this.
    Delisted,
}

/// Product specification from the exchange.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductSpec {
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub tick_size: Decimal,
    pub lot_size: Decimal,
    pub min_notional: Decimal,
    pub maker_fee: Decimal,
    pub taker_fee: Decimal,
    /// Live trading status as the venue reports it (P2.3).
    /// Defaults to `Trading` so existing fixtures and venues
    /// without an explicit status field continue to work
    /// unchanged.
    #[serde(default)]
    pub trading_status: TradingStatus,
}

impl ProductSpec {
    /// Round price down to tick size.
    pub fn round_price(&self, price: Price) -> Price {
        (price / self.tick_size).floor() * self.tick_size
    }

    /// Round quantity down to lot size.
    pub fn round_qty(&self, qty: Qty) -> Qty {
        (qty / self.lot_size).floor() * self.lot_size
    }

    /// Check if an order meets minimum notional.
    pub fn meets_min_notional(&self, price: Price, qty: Qty) -> bool {
        price * qty >= self.min_notional
    }
}

/// A pair of instruments on two venues traded together as one
/// logical position.
///
/// Used by cross-product strategies like basis trade and funding
/// arbitrage: the engine quotes one leg (typically spot) and hedges
/// on the other (typically a perp or futures). The two symbols need
/// not be the same string — Binance spot `BTCUSDT` paired with
/// HyperLiquid perp `BTC` is one `InstrumentPair`.
///
/// `multiplier` is the contract-size multiplier on the futures/perp
/// leg: 1 spot BTC ≈ `multiplier` contracts. For linear perps with
/// 1:1 sizing this is `dec!(1)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentPair {
    /// Symbol on the primary (usually spot) venue.
    pub primary_symbol: String,
    /// Symbol on the hedge (usually perp/futures) venue.
    pub hedge_symbol: String,
    /// Contract multiplier: qty_hedge = qty_primary * multiplier.
    pub multiplier: Decimal,
    /// Funding interval on the hedge leg (None on spot-spot pairs).
    #[serde(default)]
    pub funding_interval_secs: Option<u64>,
    /// Basis threshold in bps — strategies widen quotes / defer
    /// entries when |basis| exceeds this.
    pub basis_threshold_bps: Decimal,
}
