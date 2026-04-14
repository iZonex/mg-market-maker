use std::time::Duration;

use thiserror::Error;

/// Errors surfaced by `WsRpcClient::send_request`.
///
/// Callers use the variant to decide fallback behaviour: `Disconnected` and
/// `Timeout` are transient and may be retried (or the caller falls back to
/// REST); `Server` is a legitimate business response; `Fatal` means the
/// connection is dead for good and no amount of retrying will help.
#[derive(Debug, Error)]
pub enum WsRpcError {
    #[error("request timed out after {0:?}")]
    Timeout(Duration),

    #[error("connection closed before response arrived")]
    Disconnected,

    #[error("client shutdown requested")]
    Shutdown,

    #[error("server returned error: {0}")]
    Server(serde_json::Value),

    #[error("wire format error: {0}")]
    Wire(String),

    #[error("fatal: {0}")]
    Fatal(String),
}
