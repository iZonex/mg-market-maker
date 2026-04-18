//! HyperLiquid perp DEX connector.
//!
//! Authentication is EIP-712 over a secp256k1 wallet private key instead of
//! HMAC. The private key is passed via `MM_API_SECRET` (hex-encoded, 0x prefix
//! optional). The derived Ethereum address is used as the account identity.

pub mod auth;
pub mod classify;
pub mod connector;
pub mod types;
pub mod ws_post;

pub use classify::classify;
pub use connector::{parse_hl_event_for_test, HyperLiquidConnector};
pub use ws_post::{HlPostWire, HlWsTrader};
