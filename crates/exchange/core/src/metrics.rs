//! Connector-level metrics.
//!
//! These are defined in `mm-exchange-core` (rather than `mm-dashboard`)
//! so the venue connector crates can observe them without depending on
//! the dashboard crate — `mm-dashboard` re-exports them for the HTTP
//! `/metrics` endpoint.
//!
//! The histogram is `prometheus::register_histogram_vec!`-backed, so
//! all observers — regardless of which crate they live in — share the
//! same process-global timeseries.

use once_cell::sync::Lazy;
use prometheus::{register_histogram_vec, register_int_counter_vec, HistogramVec, IntCounterVec};

/// Round-trip latency of `place_order` calls, labelled by venue and
/// transport path. Observers must call:
///
/// ```ignore
/// let t0 = std::time::Instant::now();
/// // ... REST or WS call ...
/// ORDER_ENTRY_LATENCY
///     .with_label_values(&[venue, path, method])
///     .observe(t0.elapsed().as_secs_f64());
/// ```
///
/// Label conventions:
///
/// - `venue`: "binance" | "bybit" | "hyperliquid" | ...
/// - `path`: "rest" | "ws" | "fix"
/// - `method`: "place_order" | "cancel_order" | ...
pub static ORDER_ENTRY_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "mm_order_entry_duration_seconds",
        "Round-trip latency of place_order calls, by venue and transport path",
        &["venue", "path", "method"],
        vec![0.0005, 0.001, 0.002, 0.005, 0.010, 0.020, 0.050, 0.100, 0.200, 0.500, 1.0, 2.0, 5.0]
    )
    .unwrap()
});

/// Count of WebSocket reconnect attempts per venue and stream kind.
/// Incremented each time a stream transitions disconnected→reconnecting.
/// A steady increase indicates flaky connectivity, venue maintenance,
/// or an account-level ban — dashboards should alert on `rate(5m) > 0.5`.
///
/// Label conventions:
///
/// - `venue`: "binance" | "bybit" | "hyperliquid" | "custom" | ...
/// - `stream`: "market_data" | "user_data" | "order_entry"
/// - `outcome`: "retry" (about to sleep+reconnect) | "backoff_cap"
///   (capped at max delay, likely sustained outage)
pub static WS_RECONNECTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_ws_reconnects_total",
        "WebSocket reconnect attempts by venue / stream / outcome",
        &["venue", "stream", "outcome"]
    )
    .unwrap()
});

/// Count of silently-dropped exchange events (malformed payloads,
/// parse errors in user-data streams). A non-zero rate means the
/// connector is quietly discarding data — positions and balances
/// drift until the next full reconciliation.
pub static WS_PARSE_ERRORS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "mm_ws_parse_errors_total",
        "Malformed / unparseable WebSocket payloads by venue and stream",
        &["venue", "stream", "kind"]
    )
    .unwrap()
});
