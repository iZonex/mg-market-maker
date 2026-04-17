use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use mm_common::{Fill, PriceLevel, Side, Trade};
use mm_exchange_core::metrics::WS_RECONNECTS_TOTAL;
use rust_decimal::Decimal;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tracing::{debug, error, info, warn};

use crate::error::ExchangeError;
use crate::types::{WsMessage, WsRequest};

/// Initial reconnect delay after a WS failure. Exponential backoff
/// doubles this each consecutive failure, with jitter, up to
/// [`WS_RECONNECT_MAX_SECS`]. Reset to the initial delay as soon as
/// a connection survives long enough to complete the subscribe
/// handshake — sustained outages stay at the cap instead of
/// hammering the endpoint at 0.5 req/sec (which used to trip venue
/// rate-limits and keep the engine disconnected even after the
/// server recovered).
const WS_RECONNECT_INITIAL_SECS: u64 = 1;
const WS_RECONNECT_MAX_SECS: u64 = 30;

/// Cheap deterministic-enough jitter without pulling in the `rand`
/// crate. Mixes nanos from the system clock and the previous delay
/// so two clients that failed at the same moment do not sync up on
/// retries.
fn jittered_backoff(base_secs: u64) -> tokio::time::Duration {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    // ±25% jitter around the base delay.
    let jitter_pct = (nanos.wrapping_mul(2654435761) % 51) as i64 - 25; // [-25, +25]
    let base_ms = base_secs as i64 * 1000;
    let delta_ms = base_ms * jitter_pct / 100;
    let total_ms = (base_ms + delta_ms).max(100) as u64;
    tokio::time::Duration::from_millis(total_ms)
}

/// Events received from the exchange WebSocket.
#[derive(Debug, Clone)]
pub enum WsEvent {
    /// Full orderbook snapshot.
    BookSnapshot {
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        sequence: u64,
    },
    /// Incremental orderbook update.
    BookDelta {
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        sequence: u64,
    },
    /// Public trade.
    Trade(Trade),
    /// Private: our order was updated.
    OrderUpdate { data: Value },
    /// Private: we got a fill.
    FillUpdate(Fill),
    /// Connection established.
    Connected,
    /// Connection lost.
    Disconnected,
}

/// WebSocket client for real-time market data and private events.
pub struct ExchangeWsClient {
    ws_url: String,
}

impl ExchangeWsClient {
    pub fn new(ws_url: &str) -> Self {
        Self {
            ws_url: ws_url.to_string(),
        }
    }

    /// Connect and start receiving events. Returns a receiver channel.
    /// Spawns a background task that handles reconnection.
    pub async fn connect(
        &self,
        subscriptions: Vec<String>,
    ) -> Result<mpsc::UnboundedReceiver<WsEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let ws_url = self.ws_url.clone();

        tokio::spawn(async move {
            let mut delay_secs = WS_RECONNECT_INITIAL_SECS;
            loop {
                let started = std::time::Instant::now();
                match Self::run_connection(&ws_url, &subscriptions, &tx).await {
                    Ok(()) => {
                        info!("WebSocket closed cleanly");
                        break;
                    }
                    Err(e) => {
                        error!(error = %e, "WebSocket error");
                        let _ = tx.send(WsEvent::Disconnected);
                        // A connection that lasted long enough to
                        // reach steady state resets the backoff —
                        // this was a one-off, not a sustained
                        // outage. The 60 s threshold is well past
                        // the subscribe handshake and the first
                        // ping interval.
                        if started.elapsed() > tokio::time::Duration::from_secs(60) {
                            delay_secs = WS_RECONNECT_INITIAL_SECS;
                        }
                        let outcome = if delay_secs >= WS_RECONNECT_MAX_SECS {
                            "backoff_cap"
                        } else {
                            "retry"
                        };
                        WS_RECONNECTS_TOTAL
                            .with_label_values(&["custom", "market_data", outcome])
                            .inc();
                        let sleep = jittered_backoff(delay_secs);
                        warn!(
                            delay_ms = sleep.as_millis() as u64,
                            outcome, "reconnecting WebSocket after backoff"
                        );
                        tokio::time::sleep(sleep).await;
                        delay_secs = (delay_secs.saturating_mul(2)).min(WS_RECONNECT_MAX_SECS);
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn run_connection(
        ws_url: &str,
        subscriptions: &[String],
        tx: &mpsc::UnboundedSender<WsEvent>,
    ) -> Result<()> {
        let (ws_stream, _) = connect_async(ws_url)
            .await
            .map_err(|e| ExchangeError::WebSocket(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();
        info!("WebSocket connected");
        let _ = tx.send(WsEvent::Connected);

        // Subscribe to topics.
        let sub_msg = WsRequest {
            op: "subscribe".into(),
            args: Some(subscriptions.to_vec()),
        };
        let msg_text = serde_json::to_string(&sub_msg)?;
        write
            .send(tokio_tungstenite::tungstenite::Message::Text(msg_text))
            .await
            .map_err(|e| ExchangeError::WebSocket(e.to_string()))?;
        debug!(?subscriptions, "subscribed");

        // Ping interval.
        let mut ping_interval = tokio::time::interval(tokio::time::Duration::from_secs(15));

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            if let Err(e) = Self::handle_message(&text, tx) {
                                warn!("Failed to parse WS message: {e}");
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                            info!("WebSocket close frame received");
                            return Ok(());
                        }
                        Some(Err(e)) => {
                            return Err(ExchangeError::WebSocket(e.to_string()).into());
                        }
                        None => {
                            return Err(ExchangeError::Disconnected.into());
                        }
                        _ => {} // Binary, Ping, Pong — ignore
                    }
                }
                _ = ping_interval.tick() => {
                    let ping = WsRequest { op: "ping".into(), args: None };
                    let msg_text = serde_json::to_string(&ping)?;
                    write
                        .send(tokio_tungstenite::tungstenite::Message::Text(msg_text))
                        .await
                        .map_err(|e| ExchangeError::WebSocket(e.to_string()))?;
                }
            }
        }
    }

    fn handle_message(text: &str, tx: &mpsc::UnboundedSender<WsEvent>) -> Result<()> {
        let msg: WsMessage = serde_json::from_str(text)?;

        // Skip ack/pong messages.
        if let Some(op) = &msg.op {
            match op.as_str() {
                "pong" | "subscribe" | "auth" => return Ok(()),
                _ => {}
            }
        }

        let Some(topic) = &msg.topic else {
            return Ok(());
        };
        let Some(data) = &msg.data else {
            return Ok(());
        };

        if topic.starts_with("orderbook.") {
            let symbol = topic.strip_prefix("orderbook.").unwrap_or("").to_string();
            let bids = parse_book_side(data.get("bids"))?;
            let asks = parse_book_side(data.get("asks"))?;
            let sequence = data.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
            let is_snapshot = data
                .get("type")
                .and_then(|v| v.as_str())
                .map(|t| t == "snapshot")
                .unwrap_or(true);

            let event = if is_snapshot {
                WsEvent::BookSnapshot {
                    symbol,
                    bids,
                    asks,
                    sequence,
                }
            } else {
                WsEvent::BookDelta {
                    symbol,
                    bids,
                    asks,
                    sequence,
                }
            };
            let _ = tx.send(event);
        } else if topic.starts_with("trade.") {
            let symbol = topic.strip_prefix("trade.").unwrap_or("").to_string();
            let price: Decimal = data
                .get("price")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let qty: Decimal = data
                .get("qty")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let taker_side = match data.get("taker_side").and_then(|v| v.as_str()) {
                Some("buy") => Side::Buy,
                _ => Side::Sell,
            };
            let trade = Trade {
                trade_id: data.get("trade_id").and_then(|v| v.as_u64()).unwrap_or(0),
                symbol,
                price,
                qty,
                taker_side,
                timestamp: chrono::Utc::now(),
            };
            let _ = tx.send(WsEvent::Trade(trade));
        } else if topic == "orders" {
            let _ = tx.send(WsEvent::OrderUpdate { data: data.clone() });
        } else if topic == "fills" {
            if let Ok(fill) = serde_json::from_value::<Fill>(data.clone()) {
                let _ = tx.send(WsEvent::FillUpdate(fill));
            }
        }

        Ok(())
    }
}

fn parse_book_side(value: Option<&Value>) -> Result<Vec<PriceLevel>> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Ok(vec![]);
    };
    arr.iter()
        .map(|entry| {
            let arr = entry
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("expected array for price level"))?;
            let price: Decimal = arr
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .parse()?;
            let qty: Decimal = arr.get(1).and_then(|v| v.as_str()).unwrap_or("0").parse()?;
            Ok(PriceLevel { price, qty })
        })
        .collect()
}
