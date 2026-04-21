//! In-memory [`Transport`] pair — the dev / CI / local-role
//! implementation that wires controller and agent together in the
//! same process through tokio channels.
//!
//! [`in_memory_pair`] returns two endpoints that talk to each
//! other. Dropping either endpoint closes the channel; the peer's
//! [`Transport::recv`] then returns `Ok(None)` so its loop exits
//! cleanly. That mirrors the TCP / WS-RPC semantics close enough
//! that code targeting the trait does not need to special-case
//! the in-memory case.
//!
//! Use this transport for:
//! - The forthcoming `mm-local` single-process dev binary.
//! - Integration tests that need to verify controller ↔ agent
//!   behaviour end-to-end without spinning up a network.
//! - The first tier of CI, where flaky networks would turn
//!   real-transport tests into heisentests.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use crate::envelope::SignedEnvelope;
use crate::transport::{Transport, TransportError};

/// One endpoint of an in-memory transport pair. Holds a sender
/// to the peer's inbox and a receiver on its own inbox.
pub struct InMemoryEndpoint {
    tx: mpsc::UnboundedSender<SignedEnvelope>,
    rx: Mutex<mpsc::UnboundedReceiver<SignedEnvelope>>,
}

#[async_trait]
impl Transport for InMemoryEndpoint {
    async fn send(&self, envelope: SignedEnvelope) -> Result<(), TransportError> {
        self.tx
            .send(envelope)
            .map_err(|_| TransportError::Closed)
    }

    async fn recv(&mut self) -> Result<Option<SignedEnvelope>, TransportError> {
        // `&mut self` gives exclusive access even though the
        // receiver lives behind a Mutex — we keep the Mutex so
        // the struct stays `Sync` for transport-trait callers
        // that need Arc<dyn Transport>.
        let mut rx = self.rx.lock().await;
        Ok(rx.recv().await)
    }
}

/// Build a back-to-back transport pair. The two endpoints are
/// interchangeable by role — assign one to the controller loop and
/// the other to the agent loop. Every envelope the controller's
/// endpoint sends arrives on the agent's endpoint, and vice
/// versa.
pub fn in_memory_pair() -> (InMemoryEndpoint, InMemoryEndpoint) {
    let (brain_out_tx, agent_in_rx) = mpsc::unbounded_channel();
    let (agent_out_tx, brain_in_rx) = mpsc::unbounded_channel();
    let controller = InMemoryEndpoint {
        tx: brain_out_tx,
        rx: Mutex::new(brain_in_rx),
    };
    let agent = InMemoryEndpoint {
        tx: agent_out_tx,
        rx: Mutex::new(agent_in_rx),
    };
    (controller, agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Envelope;
    use crate::messages::{CommandPayload, TelemetryPayload};
    use crate::seq::Seq;

    #[tokio::test]
    async fn envelopes_roundtrip_both_directions() {
        let (controller, mut agent) = in_memory_pair();

        let cmd = SignedEnvelope::unsigned(Envelope::command(
            Seq(1),
            CommandPayload::Heartbeat,
        ));
        controller.send(cmd.clone()).await.unwrap();

        let got = agent.recv().await.unwrap().expect("agent receives");
        assert!(got.envelope.command.is_some());

        let tele = SignedEnvelope::unsigned(Envelope::telemetry(
            Seq(1),
            TelemetryPayload::Heartbeat { agent_clock_ms: 7 },
        ));
        agent.send(tele).await.unwrap();
        let mut controller = controller;
        let got = controller.recv().await.unwrap().expect("controller receives");
        assert!(got.envelope.telemetry.is_some());
    }

    #[tokio::test]
    async fn closed_peer_surfaces_as_none_on_recv() {
        let (controller, mut agent) = in_memory_pair();
        drop(controller);
        let got = agent.recv().await.unwrap();
        assert!(got.is_none(), "closed channel yields clean None");
    }

    #[tokio::test]
    async fn send_after_peer_drop_returns_closed_error() {
        let (controller, agent) = in_memory_pair();
        drop(agent);
        let cmd = SignedEnvelope::unsigned(Envelope::command(
            Seq(1),
            CommandPayload::Heartbeat,
        ));
        let err = controller.send(cmd).await;
        assert!(matches!(err, Err(TransportError::Closed)));
    }
}
