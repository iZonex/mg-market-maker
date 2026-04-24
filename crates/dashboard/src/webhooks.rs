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
    /// I3 (2026-04-21) — every fill, not only above the
    /// "large" threshold. Fires from the controller's
    /// periodic webhook-fanout loop once per new fill that
    /// landed in the tenant's per-client metrics snapshot.
    Fill {
        symbol: String,
        side: String,
        price: Decimal,
        qty: Decimal,
        timestamp: String,
        is_maker: bool,
        fee: Decimal,
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
    /// Scheduled report generated (Epic 5 item 5.3).
    ReportReady {
        report_type: String,
        date: String,
        download_url: String,
    },
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
    /// Wave D3 — ring buffer of the last `DELIVERY_LOG_CAP`
    /// dispatches. Operators read via
    /// `/api/admin/clients/{id}/webhooks/deliveries` to verify
    /// their endpoint is receiving + acknowledging the payload.
    /// Newest last; drained FIFO once the cap is reached.
    deliveries: std::collections::VecDeque<DeliveryRecord>,
}

const DELIVERY_LOG_CAP: usize = 50;

/// One delivery attempt — which URL, status, error (if any),
/// plus enough payload context to correlate on the receiver
/// side. Kept small on purpose: event body lives in audit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeliveryRecord {
    pub timestamp: String,
    pub url: String,
    pub event_type: String,
    pub ok: bool,
    pub http_status: Option<u16>,
    pub error: Option<String>,
    /// Round-trip latency in milliseconds, from the moment we
    /// invoked `.send()` to the moment we got the response (or
    /// the error). `None` for fire-and-forget paths.
    pub latency_ms: Option<u64>,
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
                deliveries: std::collections::VecDeque::with_capacity(DELIVERY_LOG_CAP),
            })),
        }
    }

    /// Wave D3 — read recent delivery records (newest first).
    pub fn recent_deliveries(&self) -> Vec<DeliveryRecord> {
        let g = self.inner.lock().unwrap();
        g.deliveries.iter().rev().cloned().collect()
    }

    /// Wave D3 — synchronous, blocking test dispatch with a
    /// minimal synthetic payload. Used by the admin "Test
    /// webhook" button so operators get an immediate yes/no
    /// on whether their URL is reachable + returning 2xx.
    /// Delivery records are appended just like the async
    /// `dispatch` path so the result shows up in the log too.
    pub async fn test_dispatch(&self) -> Vec<DeliveryRecord> {
        let urls = {
            let inner = self.inner.lock().unwrap();
            inner.urls.clone()
        };
        let payload = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event": { "kind": "test", "source": "mm-controller" },
        });
        let body = payload.to_string();
        let mut out = Vec::new();
        for url in &urls {
            let client = reqwest::Client::new();
            let started = std::time::Instant::now();
            let (ok, http_status, error) = match client
                .post(url)
                .header("Content-Type", "application/json")
                .body(body.clone())
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
            {
                Ok(resp) => {
                    let s = resp.status();
                    (s.is_success(), Some(s.as_u16()), None)
                }
                Err(e) => (false, None, Some(e.to_string())),
            };
            let rec = DeliveryRecord {
                timestamp: chrono::Utc::now().to_rfc3339(),
                url: url.clone(),
                event_type: "test".into(),
                ok,
                http_status,
                error,
                latency_ms: Some(started.elapsed().as_millis() as u64),
            };
            if let Ok(mut inner) = self.inner.lock() {
                if ok {
                    inner.events_sent += 1;
                } else {
                    inner.events_failed += 1;
                }
                if inner.deliveries.len() >= DELIVERY_LOG_CAP {
                    inner.deliveries.pop_front();
                }
                inner.deliveries.push_back(rec.clone());
            }
            out.push(rec);
        }
        out
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

    /// Snapshot of the configured URLs. Read-only view for
    /// surface readback (self-service panel, admin UI) that
    /// doesn't need to mutate state. Cloned under the lock so
    /// the caller's iteration cannot race with adds/removes.
    pub fn list_urls(&self) -> Vec<String> {
        self.inner.lock().unwrap().urls.clone()
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
        // Capture event kind (tag name) for the delivery log so
        // operators can spot patterns ("all price-update posts
        // are failing but fill events work") without replaying
        // the full audit stream.
        let event_type = serde_json::from_str::<serde_json::Value>(&json)
            .ok()
            .and_then(|v| {
                v.get("event").and_then(|e| {
                    e.get("kind")
                        .and_then(|k| k.as_str())
                        .map(String::from)
                        .or_else(|| {
                            // Unwrap the `#[serde(tag = "kind")]`
                            // variant where serde flattens the tag
                            // into the enclosing object.
                            e.as_object().and_then(|o| o.keys().next().cloned())
                        })
                })
            })
            .unwrap_or_else(|| "unknown".into());
        tokio::spawn(async move {
            for url in &urls {
                let client = reqwest::Client::new();
                let started = std::time::Instant::now();
                let (ok, http_status, error) = match client
                    .post(url)
                    .header("Content-Type", "application/json")
                    .body(json.clone())
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await
                {
                    Ok(resp) => {
                        let s = resp.status();
                        let ok = s.is_success();
                        if ok {
                            debug!(url, "webhook delivered");
                        } else {
                            warn!(url, status = %s, "webhook delivery failed");
                        }
                        (ok, Some(s.as_u16()), None)
                    }
                    Err(e) => {
                        warn!(url, error = %e, "webhook delivery error");
                        (false, None, Some(e.to_string()))
                    }
                };
                let latency_ms = Some(started.elapsed().as_millis() as u64);
                if let Ok(mut i) = inner.lock() {
                    if ok {
                        i.events_sent += 1;
                    } else {
                        i.events_failed += 1;
                    }
                    if i.deliveries.len() >= DELIVERY_LOG_CAP {
                        i.deliveries.pop_front();
                    }
                    i.deliveries.push_back(DeliveryRecord {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        url: url.clone(),
                        event_type: event_type.clone(),
                        ok,
                        http_status,
                        error,
                        latency_ms,
                    });
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
    fn list_urls_returns_snapshot() {
        let d = WebhookDispatcher::new();
        assert!(d.list_urls().is_empty());
        d.add_url("https://a.example/hook".into());
        d.add_url("https://b.example/hook".into());
        let urls = d.list_urls();
        assert_eq!(urls.len(), 2);
        assert!(urls.contains(&"https://a.example/hook".to_string()));
        assert!(urls.contains(&"https://b.example/hook".to_string()));
        d.remove_url("https://a.example/hook");
        let urls = d.list_urls();
        assert_eq!(urls, vec!["https://b.example/hook".to_string()]);
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
