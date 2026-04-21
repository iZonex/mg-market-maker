//! Transport abstraction — the contract controller + agent compile
//! against independent of whether the wire is WS-RPC over TLS
//! (production colo) or an in-memory tokio channel (local / dev /
//! integration tests).
//!
//! The trait is intentionally narrow:
//!
//! - [`Transport::send`] — deliver one [`SignedEnvelope`] to the
//!   peer. Implementations handle their own batching / framing.
//! - [`Transport::recv`] — await the next envelope from the peer,
//!   returning `None` when the peer has cleanly hung up. Drops
//!   the caller out of its loop so it can decide whether to
//!   reconnect or shut down.
//!
//! Reconnect, retry, and sequence-number gap detection live one
//! layer above this trait — inside the controller / agent loops — so
//! the two transport impls stay small and replaceable.

use async_trait::async_trait;

use crate::envelope::SignedEnvelope;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport closed by peer")]
    Closed,
    #[error("transport I/O error: {0}")]
    Io(String),
    #[error("serialization error: {0}")]
    Encode(String),
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// Push one envelope to the peer. Returns when the transport
    /// has accepted the message — this does not prove the peer
    /// has received it. End-to-end delivery is confirmed via the
    /// application-level ACK sequence, not this trait.
    async fn send(&self, envelope: SignedEnvelope) -> Result<(), TransportError>;

    /// Wait for the next envelope from the peer. `Ok(None)` means
    /// the peer closed cleanly; callers should exit their loop.
    /// `Err(_)` means the transport itself failed — reconnect
    /// logic applies.
    async fn recv(&mut self) -> Result<Option<SignedEnvelope>, TransportError>;
}
