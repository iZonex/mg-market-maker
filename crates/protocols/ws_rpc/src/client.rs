use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

use crate::error::WsRpcError;
use crate::wire::{Frame, WireFormat};

/// Client-side configuration.
#[derive(Debug, Clone)]
pub struct WsRpcConfig {
    pub url: String,
    pub request_timeout: Duration,
    pub reconnect_backoff: Duration,
    pub app_ping_interval: Option<Duration>,
}

impl Default for WsRpcConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            request_timeout: Duration::from_secs(10),
            reconnect_backoff: Duration::from_secs(2),
            app_ping_interval: None,
        }
    }
}

enum Command {
    Request {
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value, WsRpcError>>,
    },
    Shutdown,
}

/// Pending-request tracking: the caller's oneshot and when we sent the frame.
struct Pending {
    reply: oneshot::Sender<Result<Value, WsRpcError>>,
    sent_at: Instant,
}

/// A connection-managed WebSocket client doing id-correlated
/// request/response.
///
/// The socket lives in a background task. Callers interact with it through
/// `send_request`, which returns a future that resolves when the matching
/// response arrives, the request times out, or the connection drops.
///
/// **Authentication is the caller's responsibility.** When a venue needs a
/// logon step, the adapter above this client watches `is_connected()` for
/// `true`, fires its auth via `send_request` like any other call, and
/// tracks its own "authenticated" flag. On disconnect the flag is cleared
/// and the adapter re-auths before sending more business requests.
///
/// When the socket drops, all currently-pending requests resolve with
/// `WsRpcError::Disconnected`. The task then reconnects with backoff.
pub struct WsRpcClient {
    cmd_tx: mpsc::UnboundedSender<Command>,
    connected: Arc<AtomicBool>,
}

impl WsRpcClient {
    /// Spawn a new client against a fixed URL. Every reconnect attempt
    /// uses exactly the URL supplied in `config.url`.
    ///
    /// `wire` describes the venue's request/response shape. `push` is
    /// invoked on every server-initiated message (subscription data,
    /// order updates, …) and must be quick and non-blocking.
    pub fn spawn<W, P>(config: WsRpcConfig, wire: W, push: P) -> Self
    where
        W: WireFormat,
        P: Fn(Value) + Send + 'static,
    {
        let url = config.url.clone();
        Self::spawn_with_url_builder(config, wire, push, move || url.clone())
    }

    /// Like `spawn` but re-derives the URL on every (re)connect attempt.
    ///
    /// Required for venues that sign the connection via URL query
    /// parameters (Bybit V5 WS Trade). The builder is invoked on the
    /// background task each time the socket opens, so it must be
    /// cheap and side-effect-free aside from recomputing the signature.
    pub fn spawn_with_url_builder<W, P, U>(
        config: WsRpcConfig,
        wire: W,
        push: P,
        url_builder: U,
    ) -> Self
    where
        W: WireFormat,
        P: Fn(Value) + Send + 'static,
        U: Fn() -> String + Send + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let connected = Arc::new(AtomicBool::new(false));
        let connected_task = connected.clone();

        tokio::spawn(run_task(
            config,
            Box::new(wire),
            Box::new(push),
            Box::new(url_builder),
            cmd_rx,
            connected_task,
        ));

        Self { cmd_tx, connected }
    }

    /// Whether the background task currently holds a live connection.
    /// False during reconnect backoff.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Send a request and await its response.
    pub async fn send_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, WsRpcError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Request {
                method: method.to_string(),
                params,
                reply,
            })
            .map_err(|_| WsRpcError::Shutdown)?;
        rx.await.map_err(|_| WsRpcError::Shutdown)?
    }

    /// Ask the background task to stop. In-flight requests resolve with
    /// `WsRpcError::Shutdown`.
    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
    }
}

async fn run_task(
    config: WsRpcConfig,
    wire: Box<dyn WireFormat>,
    push: Box<dyn Fn(Value) + Send>,
    url_builder: Box<dyn Fn() -> String + Send>,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    connected_flag: Arc<AtomicBool>,
) {
    let next_id = AtomicU64::new(1);
    let pending: Arc<Mutex<HashMap<u64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));

    loop {
        let url = url_builder();
        debug!(%url, "ws_rpc connecting");
        let ws = match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                warn!(error = %e, "ws_rpc connect failed");
                if sleep_or_shutdown(&mut cmd_rx, config.reconnect_backoff).await {
                    fail_pending(&pending, WsRpcError::Shutdown).await;
                    return;
                }
                continue;
            }
        };
        let (mut ws_tx, mut ws_rx) = ws.split();

        connected_flag.store(true, Ordering::Relaxed);
        debug!("ws_rpc connected");

        let mut sweep = tokio::time::interval(Duration::from_millis(250));
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ping_tick = config.app_ping_interval.map(tokio::time::interval);

        let disconnect_reason: Option<WsRpcError> = loop {
            let ping_future = async {
                match ping_tick.as_mut() {
                    Some(tick) => {
                        tick.tick().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                biased;

                // User command: send a request (or stop).
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(Command::Request { method, params, reply }) => {
                            let id = next_id.fetch_add(1, Ordering::Relaxed);
                            let frame = wire.encode_request(id, &method, params);
                            pending.lock().await.insert(
                                id,
                                Pending { reply, sent_at: Instant::now() },
                            );
                            if ws_tx.send(Message::Text(frame)).await.is_err() {
                                break Some(WsRpcError::Disconnected);
                            }
                        }
                        Some(Command::Shutdown) | None => {
                            break Some(WsRpcError::Shutdown);
                        }
                    }
                }

                // Inbound frames from the peer.
                msg = ws_rx.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            warn!(error = %e, "ws_rpc read error");
                            break Some(WsRpcError::Disconnected);
                        }
                        None => break Some(WsRpcError::Disconnected),
                    };
                    match msg {
                        Message::Text(text) => {
                            match wire.decode_frame(&text) {
                                Ok(Frame::Response { id, result }) => {
                                    if let Some(p) = pending.lock().await.remove(&id) {
                                        let _ = p.reply.send(result.map_err(WsRpcError::Server));
                                    }
                                }
                                Ok(Frame::Push(value)) => push(value),
                                Ok(Frame::Keepalive) => {}
                                Err(e) => {
                                    debug!(error = %e, frame = %text, "ws_rpc unparseable frame");
                                }
                            }
                        }
                        Message::Close(_) => {
                            debug!("ws_rpc peer closed");
                            break Some(WsRpcError::Disconnected);
                        }
                        _ => {} // ping / pong / binary ignored
                    }
                }

                // Periodic request-timeout sweep.
                _ = sweep.tick() => {
                    let now = Instant::now();
                    let mut expired: Vec<u64> = Vec::new();
                    {
                        let map = pending.lock().await;
                        for (id, p) in map.iter() {
                            if now.duration_since(p.sent_at) > config.request_timeout {
                                expired.push(*id);
                            }
                        }
                    }
                    if !expired.is_empty() {
                        let mut map = pending.lock().await;
                        for id in expired {
                            if let Some(p) = map.remove(&id) {
                                let _ = p.reply.send(Err(WsRpcError::Timeout(config.request_timeout)));
                            }
                        }
                    }
                }

                // App-level ping, if the wire format wants one.
                _ = ping_future => {
                    if let Some(frame) = wire.encode_ping() {
                        if ws_tx.send(Message::Text(frame)).await.is_err() {
                            break Some(WsRpcError::Disconnected);
                        }
                    }
                }
            }
        };

        connected_flag.store(false, Ordering::Relaxed);

        match disconnect_reason {
            Some(WsRpcError::Shutdown) => {
                fail_pending(&pending, WsRpcError::Shutdown).await;
                return;
            }
            _ => {
                fail_pending(&pending, WsRpcError::Disconnected).await;
                if sleep_or_shutdown(&mut cmd_rx, config.reconnect_backoff).await {
                    return;
                }
            }
        }
    }
}

/// Wait out the reconnect backoff. Returns `true` if a `Shutdown`
/// command arrived (or the channel closed) during the wait and the
/// task should exit.
///
/// Any `Request` commands that arrive during backoff are drained
/// immediately with `WsRpcError::Disconnected` — we can't serve them
/// without a live socket, but we must also not drop their reply
/// oneshots silently (that would surface as a spurious `Shutdown`
/// error at the caller).
async fn sleep_or_shutdown(
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
    backoff: Duration,
) -> bool {
    let sleep = tokio::time::sleep(backoff);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return false,
            cmd = cmd_rx.recv() => match cmd {
                Some(Command::Shutdown) | None => return true,
                Some(Command::Request { reply, .. }) => {
                    let _ = reply.send(Err(WsRpcError::Disconnected));
                    continue;
                }
            }
        }
    }
}

async fn fail_pending(
    pending: &Arc<Mutex<HashMap<u64, Pending>>>,
    err_template: WsRpcError,
) {
    let mut map = pending.lock().await;
    for (_, p) in map.drain() {
        let _ = p.reply.send(Err(clone_err(&err_template)));
    }
}

fn clone_err(err: &WsRpcError) -> WsRpcError {
    match err {
        WsRpcError::Disconnected => WsRpcError::Disconnected,
        WsRpcError::Shutdown => WsRpcError::Shutdown,
        WsRpcError::Timeout(d) => WsRpcError::Timeout(*d),
        WsRpcError::Server(v) => WsRpcError::Server(v.clone()),
        WsRpcError::Wire(s) => WsRpcError::Wire(s.clone()),
        WsRpcError::Fatal(s) => WsRpcError::Fatal(s.clone()),
    }
}
