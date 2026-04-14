//! Generic WebSocket request/response client with `id` correlation.
//!
//! Several venue APIs look almost identical at the transport level:
//!
//! - A persistent WebSocket connection authenticated once or per-request.
//! - JSON frames tagged with a client-chosen `id`.
//! - Responses carry the same `id` so the caller can pair them with the
//!   original request.
//! - Server-initiated "push" messages (subscription data, order-update
//!   notifications) share the same frame stream.
//! - A keepalive frame every N seconds or the connection dies.
//!
//! This crate captures that pattern once. Each venue provides a tiny
//! [`WireFormat`] impl describing its request envelope, response
//! classification rules, and ping/pong expectations. The [`WsRpcClient`]
//! owns the socket, correlates requests, enforces request timeouts,
//! reconnects with backoff, and re-runs an optional auth hook after each
//! reconnect.
//!
//! See `docs/protocols/binance-ws-api.md`, `docs/protocols/bybit-ws-trade.md`,
//! and `docs/protocols/hyperliquid-ws-post.md` for the three first
//! consumers.

mod client;
mod error;
mod wire;

pub use client::{WsRpcClient, WsRpcConfig};
pub use error::WsRpcError;
pub use wire::{Frame, WireFormat};
