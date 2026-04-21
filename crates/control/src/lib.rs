//! Controller ↔ agent control-plane protocol.
//!
//! This crate carries every shape that flows between the central
//! `mm-server` controller surface and its colo-resident
//! `mm-agent` peers. It
//! deliberately contains types and traits only — no IO, no network
//! stack — so both sides compile against one canonical envelope
//! definition and cannot drift.
//!
//! # Architecture at a glance
//!
//! - **Controller** holds the desired state of the fleet: which strategy
//!   graphs should run on which agents, what fail-ladder applies,
//!   what credential envelope belongs where. It signs every command
//!   and tracks the sequence number each agent has applied.
//! - **Agent** holds the authoritative runtime: live orders, audit
//!   log, positions, local kill switch. It reconciles its state
//!   against the controller's desired state and survives controller outages
//!   using the last-known-good (LKG) config cache on disk.
//! - **Transport** is pluggable. Colo deployments ride on the
//!   WS-RPC channel from `mm-protocols-ws-rpc`; the local / dev /
//!   CI binary uses an in-memory channel that implements the same
//!   [`Transport`] trait so integration tests don't spin up a
//!   network.
//!
//! # Invariants
//!
//! - Edge-authoritative audit: the agent's JSONL audit log is the
//!   MiCA ground truth. The controller is a downstream consumer only.
//! - Three-layer dead-man's switch: leader lease (authority),
//!   watchdog grace (liveness), systemd `WatchdogSec` (wedge). All
//!   three fail independently.
//! - Fail-ladder per strategy class: makers widen → stop → flatten;
//!   drivers halt-dispatch → cancel-legs → flatten. See
//!   [`fail_ladder`].
//! - Monotonic sequence numbers both directions; agent persists the
//!   last-applied command seq to disk so reconnect replays exactly
//!   the right tail.

pub mod cursor_store;
pub mod envelope;
pub mod fail_ladder;
pub mod identity;
pub mod in_memory;
pub mod lease;
pub mod lkg;
pub mod messages;
pub mod seq;
pub mod tls;
pub mod transport;
pub mod ws_transport;

pub use envelope::{Envelope, EnvelopeKind, SignedEnvelope};
pub use identity::{IdentityError, IdentityKey, PublicKey};
pub use in_memory::in_memory_pair;
pub use cursor_store::{CursorStoreError, FileCursorStore};
pub use tls::{build_acceptor, TlsError};
pub use ws_transport::{WsListener, WsTransport};
pub use fail_ladder::{FailLadder, FailRung, StrategyClass};
pub use lease::{LeaderLease, LeaseState};
pub use lkg::{LkgCache, LkgEntry};
pub use messages::{
    AgentId, CommandPayload, DeploymentStateRow, DesiredStrategy, PushedCredential,
    TelemetryPayload,
};
pub use seq::{Cursor, Seq};
pub use transport::{Transport, TransportError};

/// Protocol version. Bumped when any wire-format shape changes in a
/// non-additive way. Controller and agent refuse to interop across
/// mismatching major versions.
pub const PROTOCOL_VERSION: u16 = 1;
