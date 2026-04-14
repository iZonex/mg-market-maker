//! Minimal FIX 4.4 message codec — encode, decode, checksum validation.
//!
//! Scope: wire-level messages only. Session state (logon sequence, heartbeat
//! watchdog, resend requests, sequence-number persistence) lives one layer up
//! and is not in this crate. A venue-specific connector (e.g. Deribit, OKX,
//! Coinbase Prime) will own the session engine and use this crate to marshal
//! individual messages.
//!
//! Provided message constructors: Logon (A), Heartbeat (0), TestRequest (1),
//! NewOrderSingle (D), OrderCancelRequest (F). Arbitrary tags can be set
//! directly via `Message::set`.
//!
//! Encoding is deterministic — the caller supplies `sending_time` as a
//! pre-formatted string so tests can pin the exact output bytes without
//! depending on a clock.

pub mod message;
pub mod session;
pub mod tags;

pub use message::{Message, OrdType, Side, TimeInForce, FIX_4_4, SOH};
pub use session::{
    FixSession, InMemorySeqStore, SeqNumStore, SessionAction, SessionConfig, SessionState,
};
