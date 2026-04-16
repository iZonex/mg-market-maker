//! Two-way Telegram control: receive commands from the configured
//! chat and forward them to the engine over an mpsc channel.
//!
//! Complements `alerts::telegram_sender` which is one-way (outbound).
//! The engine side plugs a receiver into its main event loop and
//! reacts to commands the same way it would react to a dashboard
//! HTTP call.
//!
//! ## Security
//!
//! Only messages from the `chat_id` configured in `TelegramConfig`
//! are honoured. Everything else is silently dropped — no echo, no
//! "unauthorised" response, so attackers that stumble on the bot
//! token cannot confirm the bot is active via a crafted command.
//!
//! ## Supported commands
//!
//! - `/status` — engine should send back a status alert
//! - `/stop` — trigger kill switch level 5 (disconnect)
//! - `/pause SYMBOL` — stop emitting new orders for a symbol
//! - `/resume SYMBOL` — resume quoting a paused symbol
//! - `/force_exit SYMBOL` — emergency flatten of a symbol's inventory
//!
//! ## Long polling
//!
//! We use Telegram's `getUpdates` long-polling API with a 30-second
//! timeout, keeping `offset` state in memory so updates are never
//! consumed twice. Start-up skips any updates that arrived before
//! the bot started — control commands should reflect current intent,
//! not a backlog.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::alerts::TelegramConfig;

/// Commands the engine can receive from Telegram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TelegramCommand {
    Status,
    Stop,
    Pause { symbol: String },
    Resume { symbol: String },
    ForceExit { symbol: String },
    /// Query current positions — responds with per-symbol
    /// inventory, PnL, and spread. No side effects.
    Positions,
    /// Show available commands.
    Help,
}

/// Handle returned by `spawn` — the engine pulls commands off its
/// receiver half.
pub struct TelegramControl {
    rx: mpsc::UnboundedReceiver<TelegramCommand>,
}

impl TelegramControl {
    /// Spawn the long-polling task. Returns immediately; commands
    /// flow through the internal channel as they arrive.
    ///
    /// If `config` is `None`, returns a handle whose receiver will
    /// never yield a command — a no-op mode for installations that
    /// don't have Telegram configured.
    pub fn spawn(config: Option<TelegramConfig>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(cfg) = config {
            tokio::spawn(poll_loop(cfg, tx));
        }
        Self { rx }
    }

    /// Await the next command from Telegram.
    pub async fn next_command(&mut self) -> Option<TelegramCommand> {
        self.rx.recv().await
    }

    /// Non-blocking poll — returns `None` if the channel is empty
    /// and yields none immediately.
    pub fn try_next(&mut self) -> Option<TelegramCommand> {
        self.rx.try_recv().ok()
    }
}

async fn poll_loop(config: TelegramConfig, tx: mpsc::UnboundedSender<TelegramCommand>) {
    let client = reqwest::Client::new();
    let url = format!(
        "https://api.telegram.org/bot{}/getUpdates",
        config.bot_token
    );
    let mut offset: i64 = 0;

    // Telegram returns stale updates after a cold start — prime the
    // offset by dropping the first batch.
    if let Ok(Some(max_id)) = initial_offset(&client, &url).await {
        offset = max_id + 1;
        debug!(offset, "Telegram control primed initial offset");
    }

    info!(chat_id = %config.chat_id, "Telegram control loop started");

    loop {
        let body = serde_json::json!({
            "offset": offset,
            "timeout": 30,
            "allowed_updates": ["message"],
        });

        let resp = match client.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Telegram getUpdates network error");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let v: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "Telegram getUpdates decode error");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let Some(updates) = v.get("result").and_then(|r| r.as_array()) else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };

        for update in updates {
            let update_id = update
                .get("update_id")
                .and_then(|i| i.as_i64())
                .unwrap_or(0);
            if update_id >= offset {
                offset = update_id + 1;
            }
            let Some(cmd) = extract_command_for_chat(update, &config.chat_id) else {
                continue;
            };
            debug!(?cmd, "Telegram control received command");
            if tx.send(cmd).is_err() {
                // Receiver dropped — engine is gone, no point polling.
                return;
            }
        }
    }
}

/// Prime the offset on startup by fetching whatever's queued and
/// returning the highest update_id. Next real poll uses
/// `max_id + 1` so we skip the stale backlog.
async fn initial_offset(client: &reqwest::Client, url: &str) -> anyhow::Result<Option<i64>> {
    let body = serde_json::json!({"offset": -1, "timeout": 0});
    let v: serde_json::Value = client.post(url).json(&body).send().await?.json().await?;
    let updates = v
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(updates
        .iter()
        .filter_map(|u| u.get("update_id").and_then(|i| i.as_i64()))
        .max())
}

/// Extract a command from a Telegram update, gated on the chat id.
/// Pulled out for unit testing — no network.
pub(crate) fn extract_command_for_chat(
    update: &serde_json::Value,
    expected_chat_id: &str,
) -> Option<TelegramCommand> {
    let message = update.get("message")?;
    let chat_id = message.get("chat").and_then(|c| c.get("id"));
    let chat_id_str = chat_id
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
        .or_else(|| chat_id.and_then(|v| v.as_str()).map(|s| s.to_string()))?;
    if chat_id_str != expected_chat_id {
        return None;
    }
    let text = message.get("text").and_then(|t| t.as_str())?;
    parse_command(text)
}

/// Parse a plain-text message into a `TelegramCommand`. Public for
/// easy unit testing and reuse in custom transports (SMS, Slack, etc.).
pub fn parse_command(text: &str) -> Option<TelegramCommand> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    // Normalise `/stop@MyBot` → `/stop` for bots in group chats.
    let head = parts.next()?.split('@').next()?;
    let rest = parts.next().map(|s| s.trim());

    match head {
        "/status" => Some(TelegramCommand::Status),
        "/stop" => Some(TelegramCommand::Stop),
        "/positions" | "/pos" => Some(TelegramCommand::Positions),
        "/help" => Some(TelegramCommand::Help),
        "/pause" => rest
            .filter(|s| !s.is_empty())
            .map(|s| TelegramCommand::Pause {
                symbol: s.to_string(),
            }),
        "/resume" => rest
            .filter(|s| !s.is_empty())
            .map(|s| TelegramCommand::Resume {
                symbol: s.to_string(),
            }),
        "/force_exit" => rest
            .filter(|s| !s.is_empty())
            .map(|s| TelegramCommand::ForceExit {
                symbol: s.to_string(),
            }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_status_without_args() {
        assert_eq!(parse_command("/status"), Some(TelegramCommand::Status));
        assert_eq!(parse_command("  /status  "), Some(TelegramCommand::Status));
    }

    #[test]
    fn parses_stop() {
        assert_eq!(parse_command("/stop"), Some(TelegramCommand::Stop));
    }

    #[test]
    fn parses_pause_with_symbol() {
        assert_eq!(
            parse_command("/pause BTCUSDT"),
            Some(TelegramCommand::Pause {
                symbol: "BTCUSDT".into()
            })
        );
    }

    #[test]
    fn parses_resume_with_symbol() {
        assert_eq!(
            parse_command("/resume BTCUSDT"),
            Some(TelegramCommand::Resume {
                symbol: "BTCUSDT".into()
            })
        );
    }

    #[test]
    fn parses_force_exit_with_symbol() {
        assert_eq!(
            parse_command("/force_exit ETHUSDT"),
            Some(TelegramCommand::ForceExit {
                symbol: "ETHUSDT".into()
            })
        );
    }

    #[test]
    fn pause_without_symbol_is_rejected() {
        assert_eq!(parse_command("/pause"), None);
        assert_eq!(parse_command("/pause   "), None);
    }

    #[test]
    fn plain_text_is_ignored() {
        assert_eq!(parse_command("hello bot"), None);
        assert_eq!(parse_command("status"), None);
    }

    #[test]
    fn bot_username_suffix_is_normalised() {
        assert_eq!(
            parse_command("/status@MyMarketMakerBot"),
            Some(TelegramCommand::Status)
        );
        assert_eq!(
            parse_command("/pause@MyMarketMakerBot BTCUSDT"),
            Some(TelegramCommand::Pause {
                symbol: "BTCUSDT".into()
            })
        );
    }

    #[test]
    fn chat_id_filter_rejects_other_chats() {
        let update = json!({
            "update_id": 42,
            "message": {
                "chat": {"id": 99999},
                "text": "/stop"
            }
        });
        assert_eq!(extract_command_for_chat(&update, "12345"), None);
    }

    #[test]
    fn chat_id_filter_accepts_configured_chat() {
        let update = json!({
            "update_id": 42,
            "message": {
                "chat": {"id": 12345},
                "text": "/stop"
            }
        });
        assert_eq!(
            extract_command_for_chat(&update, "12345"),
            Some(TelegramCommand::Stop)
        );
    }

    #[test]
    fn chat_id_supports_string_id() {
        let update = json!({
            "update_id": 42,
            "message": {
                "chat": {"id": "-100123456"},
                "text": "/status"
            }
        });
        assert_eq!(
            extract_command_for_chat(&update, "-100123456"),
            Some(TelegramCommand::Status)
        );
    }

    #[test]
    fn update_without_message_is_ignored() {
        let update = json!({
            "update_id": 42,
            "edited_message": {"chat": {"id": 12345}, "text": "/stop"}
        });
        assert_eq!(extract_command_for_chat(&update, "12345"), None);
    }
}
