use mm_common::types::{Fill, OrderId, OrderStatus, PriceLevel, Qty, Trade, WalletType};
use rust_decimal::Decimal;

use crate::connector::VenueId;

/// Normalized market event from any exchange.
///
/// All exchange connectors emit these events regardless of
/// the exchange's native format.
#[derive(Debug, Clone)]
pub enum MarketEvent {
    /// Full orderbook snapshot.
    BookSnapshot {
        venue: VenueId,
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        sequence: u64,
    },

    /// Incremental orderbook delta.
    BookDelta {
        venue: VenueId,
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        sequence: u64,
    },

    /// Public trade.
    Trade { venue: VenueId, trade: Trade },

    /// Our order was filled (private).
    Fill { venue: VenueId, fill: Fill },

    /// Order status changed (private).
    OrderUpdate {
        venue: VenueId,
        order_id: OrderId,
        status: OrderStatus,
        filled_qty: Qty,
    },

    /// Venue pushed a balance snapshot for a specific wallet.
    /// Emitted by listen-key / user-data streams when the exchange
    /// notifies us of a balance change (deposit, withdrawal, fill
    /// settlement). The engine's `BalanceCache` keys on
    /// `(asset, wallet)` so two updates for the same asset across
    /// different wallets don't collide.
    BalanceUpdate {
        venue: VenueId,
        asset: String,
        wallet: WalletType,
        total: Decimal,
        locked: Decimal,
        available: Decimal,
    },

    /// Connection established.
    Connected { venue: VenueId },

    /// Connection lost.
    Disconnected { venue: VenueId },
}

impl MarketEvent {
    pub fn venue(&self) -> VenueId {
        match self {
            MarketEvent::BookSnapshot { venue, .. } => *venue,
            MarketEvent::BookDelta { venue, .. } => *venue,
            MarketEvent::Trade { venue, .. } => *venue,
            MarketEvent::Fill { venue, .. } => *venue,
            MarketEvent::OrderUpdate { venue, .. } => *venue,
            MarketEvent::BalanceUpdate { venue, .. } => *venue,
            MarketEvent::Connected { venue } => *venue,
            MarketEvent::Disconnected { venue } => *venue,
        }
    }
}
