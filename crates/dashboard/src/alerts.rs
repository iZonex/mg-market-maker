use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Alert severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum AlertSeverity {
    /// Dashboard only — informational.
    Info,
    /// Telegram notification.
    Warning,
    /// Critical — immediate Telegram notification.
    Critical,
}

/// An alert event.
#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub timestamp: DateTime<Utc>,
    pub severity: AlertSeverity,
    pub title: String,
    pub message: String,
    pub symbol: Option<String>,
    pub acknowledged: bool,
}

/// Telegram configuration.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
}

/// Shared alert manager — clone-friendly, sends Telegram alerts in background.
#[derive(Clone)]
pub struct AlertManager {
    inner: Arc<Mutex<AlertInner>>,
    tx: mpsc::UnboundedSender<Alert>,
}

struct AlertInner {
    recent: VecDeque<Alert>,
    max_recent: usize,
    dedup_window_secs: i64,
}

impl AlertManager {
    /// Create a new alert manager. Spawns a background task for Telegram delivery.
    pub fn new(telegram: Option<TelegramConfig>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        // Spawn background Telegram sender.
        tokio::spawn(telegram_sender(telegram, rx));

        Self {
            inner: Arc::new(Mutex::new(AlertInner {
                recent: VecDeque::with_capacity(1000),
                max_recent: 1000,
                dedup_window_secs: 300,
            })),
            tx,
        }
    }

    /// Fire an alert (non-async, safe to call from sync code).
    pub fn alert(&self, severity: AlertSeverity, title: &str, message: &str, symbol: Option<&str>) {
        let now = Utc::now();

        let mut inner = self.inner.lock().unwrap();

        // Dedup check.
        let is_dup = inner.recent.iter().any(|a| {
            a.title == title && (now - a.timestamp).num_seconds() < inner.dedup_window_secs
        });
        if is_dup {
            return;
        }

        let alert = Alert {
            timestamp: now,
            severity,
            title: title.to_string(),
            message: message.to_string(),
            symbol: symbol.map(|s| s.to_string()),
            acknowledged: false,
        };

        // Log.
        match severity {
            AlertSeverity::Info => info!(title = title, "ALERT [INFO]: {message}"),
            AlertSeverity::Warning => warn!(title = title, "ALERT [WARNING]: {message}"),
            AlertSeverity::Critical => error!(title = title, "ALERT [CRITICAL]: {message}"),
        }

        // Send to background Telegram task for Warning+Critical.
        if severity >= AlertSeverity::Warning {
            let _ = self.tx.send(alert.clone());
        }

        inner.recent.push_back(alert);
        if inner.recent.len() > inner.max_recent {
            inner.recent.pop_front();
        }
    }

    /// Get recent alerts (for dashboard API).
    pub fn recent_alerts(&self) -> Vec<Alert> {
        let inner = self.inner.lock().unwrap();
        inner.recent.iter().cloned().collect()
    }

    /// Acknowledge an alert by index.
    pub fn acknowledge(&self, index: usize) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(alert) = inner.recent.get_mut(index) {
            alert.acknowledged = true;
        }
    }
}

/// Background task that delivers alerts to Telegram.
async fn telegram_sender(config: Option<TelegramConfig>, mut rx: mpsc::UnboundedReceiver<Alert>) {
    let Some(tg) = config else {
        // No Telegram configured — drain channel silently.
        while rx.recv().await.is_some() {}
        return;
    };

    let client = reqwest::Client::new();
    let url = format!("https://api.telegram.org/bot{}/sendMessage", tg.bot_token);

    while let Some(alert) = rx.recv().await {
        let emoji = match alert.severity {
            AlertSeverity::Info => "\u{2139}\u{fe0f}",
            AlertSeverity::Warning => "\u{26a0}\u{fe0f}",
            AlertSeverity::Critical => "\u{1f6a8}",
        };

        let symbol_tag = alert
            .symbol
            .as_deref()
            .map(|s| format!(" [{s}]"))
            .unwrap_or_default();

        let text = format!(
            "{emoji} *{title}*{symbol_tag}\n{message}",
            title = alert.title,
            message = alert.message,
        );

        let body = serde_json::json!({
            "chat_id": tg.chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });

        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                warn!(status = %resp.status(), "Telegram send failed");
            }
            Err(e) => {
                warn!(error = %e, "Telegram send error");
            }
        }
    }
}
