use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info};

use crate::state::DashboardState;

/// WebSocket broadcast channel — engine pushes updates, all WS clients receive.
#[derive(Clone)]
pub struct WsBroadcast {
    tx: broadcast::Sender<String>,
}

impl WsBroadcast {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Send a JSON message to all connected WS clients.
    pub fn send(&self, msg: &str) {
        // Ignore error (no subscribers).
        let _ = self.tx.send(msg.to_string());
    }

    /// Get a receiver for a new WS client.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

/// WebSocket upgrade handler.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<(DashboardState, Arc<WsBroadcast>)>,
) -> impl IntoResponse {
    let (dashboard, broadcast) = state;
    ws.on_upgrade(move |socket| handle_socket(socket, dashboard, broadcast))
}

async fn handle_socket(socket: WebSocket, dashboard: DashboardState, broadcast: Arc<WsBroadcast>) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = broadcast.subscribe();

    info!("WebSocket client connected");

    // Send initial state snapshot.
    if let Ok(json) = serde_json::to_string(&serde_json::json!({
        "type": "snapshot",
        "symbols": dashboard.get_all(),
    })) {
        let _ = sender.send(Message::Text(json.into())).await;
    }

    // Spawn a task to forward broadcast messages to this client.
    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Receive messages from client (for commands like start/stop/configure).
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    debug!(msg = %text, "WS client message");
                    // Handle client commands here in the future.
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Wait for either task to finish.
    tokio::select! {
        _ = &mut send_task => { recv_task.abort(); }
        _ = &mut recv_task => { send_task.abort(); }
    }

    info!("WebSocket client disconnected");
}
