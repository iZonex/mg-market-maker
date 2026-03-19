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
    Gtc,
    Ioc,
    Fok,
    PostOnly,
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

/// Snapshot of balances from the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub asset: String,
    pub total: Decimal,
    pub locked: Decimal,
    pub available: Decimal,
}

/// Product specification from the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductSpec {
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub tick_size: Decimal,
    pub lot_size: Decimal,
    pub min_notional: Decimal,
    pub maker_fee: Decimal,
    pub taker_fee: Decimal,
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
