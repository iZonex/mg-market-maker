//! Block C — S3 compliance archive.
//!
//! Three layers:
//!
//! 1. [`client`] — thin wrapper over `aws-sdk-s3` with endpoint
//!    override + SSE-S3 / SSE-KMS selection driven by
//!    [`mm_common::config::ArchiveConfig`]. Built once at
//!    server start; shared by the shipper and the bundle
//!    endpoint via `Arc<ArchiveClient>`.
//!
//! 2. [`shipper`] — background task that tails local artefacts
//!    (audit JSONL, fills JSONL, daily report snapshots) and
//!    uploads deltas on a timer. Byte-offset checkpoints live
//!    in `data/archive_offsets.json` so restarts never
//!    re-upload the same chunk.
//!
//! 3. [`bundle`] — synchronous assembler that zips a period's
//!    worth of summary + fills + audit + HMAC-signed manifest
//!    into a single stream. Served by `/api/v1/export/bundle`
//!    and optionally pushed to S3 for long-lived handover
//!    links.
//!
//! Credentials never touch `ArchiveConfig` — the SDK resolves
//! them from the usual AWS provider chain (env / IAM role /
//! profile / SSO). An operator who doesn't set `[archive]` in
//! their config gets no S3 behaviour at all, zero boot-time
//! dependencies on AWS.

pub mod bundle;
pub mod client;
pub mod shipper;

pub use client::ArchiveClient;
