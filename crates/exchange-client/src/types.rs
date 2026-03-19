use mm_common::{OrderId, OrderType, Price, Qty, Side, TimeInForce};
use serde::{Deserialize, Serialize};

/// Request to place an order on the exchange.
#[derive(Debug, Serialize)]
pub struct PlaceOrderRequest {
    pub symbol: String,
    pub side: Side,
    pub order_type: OrderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    pub qty: Qty,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<TimeInForce>,
}

/// Response from placing an order.
#[derive(Debug, Deserialize)]
pub struct PlaceOrderResponse {
    pub order_id: OrderId,
    pub status: String,
    #[serde(default)]
    pub fills: Vec<FillResponse>,
}

#[derive(Debug, Deserialize)]
pub struct FillResponse {
    pub trade_id: u64,
    pub price: Price,
    pub qty: Qty,
    pub role: String,
}

/// Response from cancel order.
#[derive(Debug, Deserialize)]
pub struct CancelOrderResponse {
    pub order_id: OrderId,
    pub status: String,
}

/// Orderbook snapshot from REST.
#[derive(Debug, Deserialize)]
pub struct OrderbookResponse {
    pub symbol: String,
    pub bids: Vec<[String; 2]>, // [price, qty]
    pub asks: Vec<[String; 2]>,
    pub sequence: u64,
}

/// Balance entry from REST.
#[derive(Debug, Deserialize)]
pub struct BalanceResponse {
    pub asset: String,
    pub total: String,
    pub locked: String,
    pub available: String,
}

// --- WebSocket messages ---

#[derive(Debug, Serialize)]
pub struct WsRequest {
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct WsMessage {
    pub op: Option<String>,
    pub topic: Option<String>,
    pub data: Option<serde_json::Value>,
    pub success: Option<bool>,
}
