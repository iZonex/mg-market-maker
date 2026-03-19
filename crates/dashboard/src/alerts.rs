use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::VecDeque;
use tracing::{error, info, warn};

/// Alert severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum AlertSeverity {
    /// Dashboard only — informational.
    Info,
    /// Slack/Telegram notification.
    Warning,
    /// Page on-call immediately.
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
    /// Has this been acknowledged?
    pub acknowledged: bool,
}

/// Alert manager — deduplicates, routes, and tracks alerts.
pub struct AlertManager {
    /// Recent alerts (for dedup and dashboard).
    recent: VecDeque<Alert>,
    max_recent: usize,
    /// Telegram config.
    telegram: Option<TelegramConfig>,
    /// HTTP client for sending webhooks.
    http_client: reqwest::Client,
    /// Dedup: don't re-alert same title within this window (seconds).
    dedup_window_secs: i64,
}

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
}

impl AlertManager {
    pub fn new(telegram: Option<TelegramConfig>) -> Self {
        Self {
            recent: VecDeque::with_capacity(1000),
            max_recent: 1000,
            telegram,
            http_client: reqwest::Client::new(),
            dedup_window_secs: 300, // 5 min dedup window.
        }
    }

    /// Fire an alert.
    pub async fn alert(
        &mut self,
        severity: AlertSeverity,
        title: &str,
        message: &str,
        symbol: Option<&str>,
    ) {
        // Dedup check.
        let now = Utc::now();
        let is_dup = self.recent.iter().any(|a| {
            a.title == title && (now - a.timestamp).num_seconds() < self.dedup_window_secs
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

        // Send to Telegram for Warning and Critical.
        if severity >= AlertSeverity::Warning {
            self.send_telegram(&alert).await;
        }

        self.recent.push_back(alert);
        if self.recent.len() > self.max_recent {
            self.recent.pop_front();
        }
    }

    /// Send alert to Telegram.
    async fn send_telegram(&self, alert: &Alert) {
        let Some(tg) = &self.telegram else { return };

        let emoji = match alert.severity {
            AlertSeverity::Info => "ℹ️",
            AlertSeverity::Warning => "⚠️",
            AlertSeverity::Critical => "🚨",
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

        let url = format!("https://api.telegram.org/bot{}/sendMessage", tg.bot_token);
        let body = serde_json::json!({
            "chat_id": tg.chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });

        match self.http_client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                warn!(status = %resp.status(), "Telegram send failed");
            }
            Err(e) => {
                warn!(error = %e, "Telegram send error");
            }
        }
    }

    /// Get recent alerts (for dashboard API).
    pub fn recent_alerts(&self) -> Vec<&Alert> {
        self.recent.iter().collect()
    }

    /// Acknowledge an alert by index.
    pub fn acknowledge(&mut self, index: usize) {
        if let Some(alert) = self.recent.get_mut(index) {
            alert.acknowledged = true;
        }
    }
}
