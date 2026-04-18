//! G1 — source collectors.
//!
//! Every collector implements [`Collector`] and returns a
//! `Vec<Article>` on each poll. The background orchestrator
//! (lives in `server::main`, not here) fans out across all
//! configured sources, dedups by URL, feeds the keep-set into
//! the sentiment analyzer, then into the mention counter.
//!
//! Keeping the trait small makes it trivial to bolt on new
//! sources without touching the core crate: Reddit JSON,
//! Mastodon ActivityPub, a custom on-prem Kafka topic — the
//! public surface is one async function.

use crate::types::Article;
use async_trait::async_trait;

pub mod rss;
pub mod cryptopanic;
pub mod twitter;

/// Trait implemented by every news/social collector.
///
/// Implementations MUST:
/// - Be side-effect-free beyond the network fetch they make.
/// - Populate `Article::url` unique per logical item (the
///   dedup layer keys on url).
/// - Fail open: return an empty `Vec` on network errors so
///   one flaky source does not stall the whole poll cycle.
#[async_trait]
pub trait Collector: Send + Sync {
    /// Stable identifier used for metrics + audit tags
    /// (`rss`, `cryptopanic`, `twitter`). Operators see this
    /// in logs, so keep it short.
    fn name(&self) -> &'static str;

    async fn collect(&self) -> Vec<Article>;
}
