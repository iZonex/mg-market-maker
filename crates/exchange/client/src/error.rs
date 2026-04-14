use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExchangeError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("API error: status={status}, message={message}")]
    Api { status: u16, message: String },

    #[error("Connection lost")]
    Disconnected,

    #[error("Auth required but no credentials configured")]
    NoCredentials,
}
