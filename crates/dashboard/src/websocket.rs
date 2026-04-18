use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::auth::{AuthState, TokenClaims};
use crate::state::DashboardState;

/// WebSocket broadcast channel — engine pushes updates, all WS clients receive.
#[derive(Clone, Debug)]
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

#[derive(Deserialize)]
pub struct WsAuthQuery {
    #[serde(default)]
    token: Option<String>,
}

/// WebSocket upgrade handler. Browsers cannot set request headers
/// on the WS upgrade, so we accept the session token as a `?token=`
/// query parameter and verify it here. Unauthenticated upgrades
/// return 401 before any socket state is allocated.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsAuthQuery>,
    State((dashboard, broadcast, auth)): State<(DashboardState, Arc<WsBroadcast>, AuthState)>,
) -> Response {
    let Some(token) = q.token.as_deref() else {
        warn!("WS upgrade without token");
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(claims) = auth.verify_token(token) else {
        warn!("WS upgrade with invalid token");
        return StatusCode::UNAUTHORIZED.into_response();
    };
    ws.on_upgrade(move |socket| handle_socket(socket, dashboard, broadcast, claims))
}

async fn handle_socket(
    socket: WebSocket,
    dashboard: DashboardState,
    broadcast: Arc<WsBroadcast>,
    claims: TokenClaims,
) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = broadcast.subscribe();

    info!(user_id = %claims.user_id, role = ?claims.role, "WebSocket client connected");

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

    info!(user_id = %claims.user_id, "WebSocket client disconnected");
}
