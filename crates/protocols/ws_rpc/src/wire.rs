use serde_json::Value;

/// A classified inbound frame.
pub enum Frame {
    /// A response to a request we sent, tagged with the request `id` we
    /// originally picked. `result` is `Ok(value)` on success, or
    /// `Err(server_error_json)` on failure — the exact error shape is
    /// venue-defined but lives inside a `serde_json::Value` so the caller
    /// can match on its contents.
    Response {
        id: u64,
        result: Result<Value, Value>,
    },

    /// A server-initiated message that does not correspond to any request
    /// we sent (subscription tick, order update, …). The client routes it
    /// to the push callback configured at connect time.
    Push(Value),

    /// A keepalive frame (ping/pong or similar). The client ignores it for
    /// correlation purposes but returns this variant so implementations
    /// can observe the traffic in tests.
    Keepalive,
}

/// How one venue's wire format differs from another.
///
/// Each method is called on the background task owning the socket, so
/// implementations must be cheap and non-blocking.
pub trait WireFormat: Send + Sync + 'static {
    /// Build the outbound text frame for a new request. `id` is a
    /// monotonic `u64` chosen by the client; venues that expect stringly-
    /// typed ids should format it themselves.
    fn encode_request(&self, id: u64, method: &str, params: Value) -> String;

    /// Classify an inbound text frame.
    ///
    /// Returning `Err` does not tear down the connection — it is logged
    /// and the frame is dropped, on the assumption that the server sent
    /// something we simply do not understand.
    fn decode_frame(&self, frame: &str) -> Result<Frame, String>;

    /// Build an application-level ping payload. Return `None` if the
    /// venue relies on WebSocket protocol pings instead.
    fn encode_ping(&self) -> Option<String> {
        None
    }
}
