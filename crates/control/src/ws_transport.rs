//! [`Transport`] implemented over a raw WebSocket connection.
//!
//! PR-2e brings the first on-wire implementation of the control
//! channel. JSON-encoded envelopes ride inside WebSocket text
//! frames; close frames propagate as `Ok(None)` on the peer's
//! receive side so the rest of the stack reacts identically to
//! how it reacts to [`InMemoryEndpoint`] close.
//!
//! Structure mirrors [`crate::in_memory::InMemoryEndpoint`]:
//! two background tasks own the `SplitSink` / `SplitStream` of
//! the websocket, forwarding envelopes to and from a pair of
//! mpsc channels that the `WsTransport` owns publicly. That gives
//! us:
//!
//! - `send(&self)` via the outbound mpsc sender — no lock
//!   contention with `recv(&mut self)`.
//! - Symmetric close semantics on either direction dropping.
//! - Trivial integration tests: spawn two WsTransports over a
//!   local TCP listener, push envelopes, assert they arrive.
//!
//! Intentionally out of scope for PR-2e:
//! - TLS (plain `ws://` only). TLS termination typically handled
//!   by a load balancer or reverse proxy in production; native
//!   TLS lands in PR-2f alongside signed envelopes.
//! - Reconnect / resume semantics. If the connection drops, the
//!   transport surfaces `Ok(None)` on recv; the agent binary
//!   exits and its supervisor restarts it. Sticky-cursor resume
//!   arrives with PR-2f.
//! - Framing backpressure — we rely on mpsc channel buffering to
//!   absorb bursts.

use std::net::SocketAddr;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream, WebSocketStream};

use crate::envelope::SignedEnvelope;
use crate::transport::{Transport, TransportError};

/// Single side of a WS-backed control channel.
pub struct WsTransport {
    tx: mpsc::UnboundedSender<SignedEnvelope>,
    rx: Mutex<mpsc::UnboundedReceiver<SignedEnvelope>>,
}

#[async_trait]
impl Transport for WsTransport {
    async fn send(&self, envelope: SignedEnvelope) -> Result<(), TransportError> {
        self.tx.send(envelope).map_err(|_| TransportError::Closed)
    }

    async fn recv(&mut self) -> Result<Option<SignedEnvelope>, TransportError> {
        let mut rx = self.rx.lock().await;
        Ok(rx.recv().await)
    }
}

impl WsTransport {
    /// Dial `addr` as `ws://addr/` and return a ready transport.
    /// Connection setup failures (TCP or WS handshake) surface as
    /// `TransportError::Io`. Future `TransportError::TlsFailure`
    /// etc. can be added without breaking the return type.
    pub async fn connect(addr: &str) -> Result<Self, TransportError> {
        let url = if addr.starts_with("ws://") || addr.starts_with("wss://") {
            addr.to_string()
        } else {
            format!("ws://{}/", addr)
        };
        let (ws_stream, _response) = connect_async(&url)
            .await
            .map_err(|e| TransportError::Io(format!("ws connect {url}: {e}")))?;
        Ok(Self::from_client_stream(ws_stream))
    }

    /// Accept helper — takes an already-handshaken WebSocket
    /// produced by `accept_async` on a raw TCP stream. The
    /// [`WsListener`] below drives this on the controller side.
    pub fn from_server_stream(stream: WebSocketStream<TcpStream>) -> Self {
        let (sink, source) = stream.split();
        spawn_pumps::<_, _>(sink, source)
    }

    pub fn from_client_stream(stream: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
        let (sink, source) = stream.split();
        spawn_pumps::<_, _>(sink, source)
    }

    /// Accept helper for a TLS-wrapped server stream. Called by
    /// [`WsListener::accept`] when the listener has a
    /// `TlsAcceptor` attached. The `MaybeTlsStream` alias from
    /// `tokio-tungstenite` works for both plain + client-TLS;
    /// on the server-accept side we have a concrete
    /// `tokio_rustls::server::TlsStream<TcpStream>` so the type
    /// is explicit here.
    pub fn from_server_tls_stream(
        stream: WebSocketStream<tokio_rustls::server::TlsStream<TcpStream>>,
    ) -> Self {
        let (sink, source) = stream.split();
        spawn_pumps::<_, _>(sink, source)
    }
}

/// Spawn the two forwarding tasks and return a [`WsTransport`]
/// whose mpsc endpoints front the channels. Generic over the
/// concrete sink / source types so both client-side
/// `MaybeTlsStream` and server-side `TcpStream` flavours reuse
/// the same plumbing.
fn spawn_pumps<S, R>(mut sink: S, mut source: R) -> WsTransport
where
    S: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin + Send + 'static,
    R: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin
        + Send
        + 'static,
{
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<SignedEnvelope>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<SignedEnvelope>();

    // Outbound pump — serialize each envelope to JSON + text frame.
    tokio::spawn(async move {
        while let Some(env) = out_rx.recv().await {
            let json = match serde_json::to_string(&env) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "ws outbound: envelope encode failed — dropping");
                    continue;
                }
            };
            if let Err(e) = sink.send(Message::Text(json)).await {
                tracing::info!(error = %e, "ws outbound: send failed — closing transport");
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Inbound pump — decode each text frame into a SignedEnvelope.
    tokio::spawn(async move {
        while let Some(msg) = source.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    tracing::info!(error = %e, "ws inbound: recv failed — closing transport");
                    break;
                }
            };
            match msg {
                Message::Text(txt) => {
                    match serde_json::from_str::<SignedEnvelope>(&txt) {
                        Ok(env) => {
                            if in_tx.send(env).is_err() {
                                // receiver dropped — nothing to do
                                return;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "ws inbound: malformed envelope JSON");
                        }
                    }
                }
                Message::Binary(_) => {
                    tracing::warn!("ws inbound: unexpected binary frame — ignoring");
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => {
                    tracing::debug!("ws inbound: peer sent close frame");
                    break;
                }
                Message::Frame(_) => {}
            }
        }
        // When we fall out of the loop the `in_tx` drops, which
        // makes the transport's `recv()` return `Ok(None)`.
    });

    WsTransport {
        tx: out_tx,
        rx: Mutex::new(in_rx),
    }
}

/// TCP listener + WS upgrade driver. Yields a ready `WsTransport`
/// for each successfully-handshaken client. Callers spawn a task
/// per accepted transport (see controller main + tests).
///
/// When `tls` is attached, [`WsListener::accept`] wraps each
/// incoming TCP stream in a TLS acceptor before the WebSocket
/// upgrade. Callers opt in via [`WsListener::with_tls`] — plain
/// ws:// deployments pay nothing for the TLS code path.
pub struct WsListener {
    tcp: TcpListener,
    tls: Option<TlsAcceptor>,
}

impl WsListener {
    pub async fn bind(addr: SocketAddr) -> Result<Self, TransportError> {
        let tcp = TcpListener::bind(addr)
            .await
            .map_err(|e| TransportError::Io(format!("ws bind {addr}: {e}")))?;
        Ok(Self { tcp, tls: None })
    }

    /// Attach a rustls `TlsAcceptor`. Every subsequent
    /// [`WsListener::accept`] call terminates TLS against this
    /// acceptor before the WebSocket handshake. Operators build
    /// the acceptor from their PEM material via
    /// [`crate::tls::build_acceptor`].
    pub fn with_tls(mut self, tls: TlsAcceptor) -> Self {
        self.tls = Some(tls);
        self
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.tcp
            .local_addr()
            .map_err(|e| TransportError::Io(format!("ws local_addr: {e}")))
    }

    /// Accept one connection + upgrade to WebSocket. Returns the
    /// `WsTransport` wrapping the handshaken stream plus the
    /// peer's socket address for logging.
    pub async fn accept(&self) -> Result<(WsTransport, SocketAddr), TransportError> {
        let (stream, peer) = self
            .tcp
            .accept()
            .await
            .map_err(|e| TransportError::Io(format!("ws accept: {e}")))?;
        match &self.tls {
            None => {
                let ws = accept_async(stream)
                    .await
                    .map_err(|e| TransportError::Io(format!("ws handshake from {peer}: {e}")))?;
                Ok((WsTransport::from_server_stream(ws), peer))
            }
            Some(acceptor) => {
                let tls_stream = acceptor
                    .accept(stream)
                    .await
                    .map_err(|e| TransportError::Io(format!("tls handshake from {peer}: {e}")))?;
                let ws = accept_async(tls_stream)
                    .await
                    .map_err(|e| TransportError::Io(format!("wss handshake from {peer}: {e}")))?;
                Ok((WsTransport::from_server_tls_stream(ws), peer))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Envelope;
    use crate::messages::{CommandPayload, TelemetryPayload};
    use crate::seq::Seq;
    use std::time::Duration;

    #[tokio::test]
    async fn envelopes_roundtrip_both_directions_over_ws() {
        let listener = WsListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let (t, _peer) = listener.accept().await.unwrap();
            t
        });

        let client = WsTransport::connect(&addr.to_string()).await.unwrap();
        // `server.send()` takes `&self`, so no `mut` needed on
        // this one. Receive direction uses a fresh pair below.
        let server = accept.await.unwrap();

        // Controller → agent command.
        let cmd = SignedEnvelope::unsigned(Envelope::command(Seq(1), CommandPayload::Heartbeat));
        server.send(cmd).await.unwrap();
        // Give the inbound pump a moment to forward.
        let got = tokio::time::timeout(Duration::from_secs(1), {
            let mut c = client;
            async move { c.recv().await }
        })
        .await
        .unwrap()
        .unwrap()
        .expect("client receives command");
        assert!(got.envelope.command.is_some());

        // We consumed client; redo the other direction with
        // fresh transports.
        let listener = WsListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
        let client = WsTransport::connect(&addr.to_string()).await.unwrap();
        let mut server = accept.await.unwrap();

        let tele = SignedEnvelope::unsigned(Envelope::telemetry(
            Seq(1),
            TelemetryPayload::Heartbeat { agent_clock_ms: 42 },
        ));
        client.send(tele).await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(1), server.recv())
            .await
            .unwrap()
            .unwrap()
            .expect("server receives telemetry");
        assert!(got.envelope.telemetry.is_some());
    }

    #[tokio::test]
    async fn closed_peer_surfaces_as_none_on_recv() {
        let listener = WsListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let (t, _peer) = listener.accept().await.unwrap();
            t
        });
        let client = WsTransport::connect(&addr.to_string()).await.unwrap();
        let mut server = accept.await.unwrap();

        drop(client);
        // The inbound pump on the server sees EOF within a tick.
        let got = tokio::time::timeout(Duration::from_secs(2), server.recv())
            .await
            .expect("recv finishes within timeout");
        assert!(got.unwrap().is_none(), "closed transport yields Ok(None)");
    }
}
