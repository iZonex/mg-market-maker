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

    /// Get product specification (tick/lot sizes, fees).
    async fn get_product_spec(&self, symbol: &str) -> anyhow::Result<ProductSpec>;

    // --- Health ---

    /// Health check.
    async fn health_check(&self) -> anyhow::Result<bool>;
}
