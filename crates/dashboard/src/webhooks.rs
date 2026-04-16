//! Webhook notification framework for client event delivery.
//!
//! Operators configure one or more webhook URLs; the system
//! POSTs JSON payloads on key events (SLA breach, kill switch
//! escalation, large fill, daily report). Clients and exchanges
//! consume these to trigger their own monitoring/alerting.

use rust_decimal::Decimal;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};

/// Event types that trigger webhook notifications.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event_type")]
pub enum WebhookEvent {
    /// SLA compliance dropped below threshold.
    SlaBreached {
        symbol: String,
        current_pct: Decimal,
        threshold_pct: Decimal,
    },
    /// Kill switch escalated to a higher level.
    KillSwitchEscalated {
        symbol: String,
        level: u8,
        reason: String,
    },
    /// Large fill executed (above configurable threshold).
    LargeFill {
        symbol: String,
        side: String,
        price: Decimal,
        qty: Decimal,
        value_quote: Decimal,
    },
    /// Daily report generated.
    DailyReportReady {
        date: String,
        total_pnl: Decimal,
        total_volume: Decimal,
    },
    /// Engine started.
    EngineStarted { symbol: String },
    /// Engine shutdown.
    EngineStopped { symbol: String },
}

/// Webhook payload sent to configured URLs.
#[derive(Debug, Clone, Serialize)]
struct WebhookPayload {
    timestamp: String,
    #[serde(flatten)]
    event: WebhookEvent,
}

/// Webhook dispatcher. Manages URLs and sends notifications
/// asynchronously via `reqwest` (or falls back to logging
/// when no HTTP client is available).
#[derive(Debug, Clone)]
pub struct WebhookDispatcher {
    inner: Arc<Mutex<WebhookInner>>,
}

#[derive(Debug)]
struct WebhookInner {
    urls: Vec<String>,
    events_sent: u64,
    events_failed: u64,
}

impl WebhookDispatcher {
    /// Create a dispatcher with no URLs configured. Add URLs
    /// via `add_url()`.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(WebhookInner {
                urls: Vec::new(),
                events_sent: 0,
                events_failed: 0,
            })),
        }
    }

    /// Register a webhook URL. Duplicates are silently ignored.
    pub fn add_url(&self, url: String) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.urls.contains(&url) {
            inner.urls.push(url);
        }
    }

    /// Remove a webhook URL.
    pub fn remove_url(&self, url: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.urls.retain(|u| u != url);
    }

    /// Number of configured webhook URLs.
    pub fn url_count(&self) -> usize {
        self.inner.lock().unwrap().urls.len()
    }

    /// Total events successfully sent.
    pub fn events_sent(&self) -> u64 {
        self.inner.lock().unwrap().events_sent
    }

    /// Total events that failed to deliver.
    pub fn events_failed(&self) -> u64 {
        self.inner.lock().unwrap().events_failed
    }

    /// Dispatch an event to all configured URLs. Non-blocking —
    /// fires HTTP POSTs in the background via `tokio::spawn`.
    pub fn dispatch(&self, event: WebhookEvent) {
        let urls = {
            let inner = self.inner.lock().unwrap();
            if inner.urls.is_empty() {
                return;
            }
            inner.urls.clone()
        };

        let payload = WebhookPayload {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event,
        };
        let json = match serde_json::to_string(&payload) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "failed to serialize webhook payload");
                return;
            }
        };

        let inner = self.inner.clone();
        tokio::spawn(async move {
            for url in &urls {
                let client = reqwest::Client::new();
                match client
                    .post(url)
                    .header("Content-Type", "application/json")
                    .body(json.clone())
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        debug!(url, "webhook delivered");
                        if let Ok(mut i) = inner.lock() {
                            i.events_sent += 1;
                        }
                    }
                    Ok(resp) => {
                        warn!(url, status = %resp.status(), "webhook delivery failed");
                        if let Ok(mut i) = inner.lock() {
                            i.events_failed += 1;
                        }
                    }
                    Err(e) => {
                        warn!(url, error = %e, "webhook delivery error");
                        if let Ok(mut i) = inner.lock() {
                            i.events_failed += 1;
                        }
                    }
                }
            }
        });
    }
}

impl Default for WebhookDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn empty_dispatcher_does_nothing() {
        let d = WebhookDispatcher::new();
        d.dispatch(WebhookEvent::EngineStarted {
            symbol: "BTCUSDT".into(),
        });
        assert_eq!(d.events_sent(), 0);
        assert_eq!(d.url_count(), 0);
    }

    #[test]
    fn add_and_remove_url() {
        let d = WebhookDispatcher::new();
        d.add_url("https://example.com/hook".into());
        assert_eq!(d.url_count(), 1);
        d.add_url("https://example.com/hook".into()); // duplicate
        assert_eq!(d.url_count(), 1);
        d.remove_url("https://example.com/hook");
        assert_eq!(d.url_count(), 0);
    }

    #[test]
    fn sla_breached_serializes_correctly() {
        let event = WebhookEvent::SlaBreached {
            symbol: "BTCUSDT".into(),
            current_pct: dec!(93.5),
            threshold_pct: dec!(95),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("SlaBreached"));
        assert!(json.contains("BTCUSDT"));
    }
}
