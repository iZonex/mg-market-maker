//! Last-known-good cache — the "survive controller outage" schema.
//!
//! Every time the agent applies a command that changes durable
//! state (strategy set, fail-ladder, credential envelope version)
//! it writes a new LKG snapshot to disk. On startup — after a
//! crash or a cold boot in a partitioned network — the agent
//! loads the most recent LKG and resumes under those values until
//! either (a) it re-establishes a session with controller and receives
//! a fresh snapshot, or (b) the snapshot's TTL expires, at which
//! point the agent trips its fail-ladder.
//!
//! This is the Consul-Vault-Agent pattern: local authority,
//! upstream reconcile, no blank-slate reboot into a dangerous
//! state.
//!
//! PR-1 keeps the format + typed accessors; actual file IO (atomic
//! write, fsync, schema-version recovery) lands with PR-2 where it
//! has real payloads to persist.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::messages::DesiredStrategy;
use crate::seq::Seq;

/// Top-level LKG file. One file per agent — small (~tens of KB) —
/// rewritten atomically on every meaningful state change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LkgCache {
    /// Agent identifier the cache belongs to. Sanity check on
    /// load — if the file's agent_id doesn't match the binary's
    /// configured id we refuse to use it rather than risk quoting
    /// under a peer's desired state.
    pub agent_id: String,
    /// Bumped on every schema change so migrations can gate off it.
    /// Today it is always 1; a future value 2 triggers an explicit
    /// rewrite before the agent accepts the cache as authoritative.
    pub schema_version: u16,
    /// When the agent last wrote this cache. An agent that finds
    /// an LKG older than `ttl` at startup refuses to trade on it
    /// and waits for a fresh controller handshake.
    pub written_at: DateTime<Utc>,
    /// The last command-seq the agent had applied when this cache
    /// was written. On reconnect the agent advertises this to the
    /// controller so replay resumes cleanly.
    pub last_applied_seq: Seq,
    /// Snapshot of every desired strategy as of `written_at`.
    /// Empty vec is a legitimate state — "no strategies assigned".
    pub strategies: Vec<DesiredStrategy>,
}

impl LkgCache {
    pub fn fresh(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            schema_version: 1,
            written_at: Utc::now(),
            last_applied_seq: Seq::ZERO,
            strategies: Vec::new(),
        }
    }

    /// True iff the cache is younger than `ttl`. Older caches MUST
    /// NOT be used to authorise live trading — treat as absent.
    pub fn is_fresh_within(&self, ttl: chrono::Duration, now: DateTime<Utc>) -> bool {
        now - self.written_at < ttl
    }
}

/// Convenience accessor for a single strategy entry inside the
/// cache — borrowed alias so consumers don't have to scan the vec
/// themselves.
pub type LkgEntry<'a> = &'a DesiredStrategy;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn fresh_cache_starts_empty() {
        let c = LkgCache::fresh("agent-1");
        assert_eq!(c.strategies.len(), 0);
        assert_eq!(c.last_applied_seq, Seq::ZERO);
        assert_eq!(c.schema_version, 1);
    }

    #[test]
    fn freshness_window() {
        let mut c = LkgCache::fresh("a");
        c.written_at = Utc::now() - Duration::seconds(10);
        assert!(c.is_fresh_within(Duration::seconds(30), Utc::now()));
        assert!(!c.is_fresh_within(Duration::seconds(5), Utc::now()));
    }

    #[test]
    fn serde_roundtrip_preserves_fields() {
        let mut c = LkgCache::fresh("agent-1");
        c.last_applied_seq = Seq(42);
        c.strategies.push(DesiredStrategy {
            deployment_id: "dep-a".into(),
            template: "avellaneda-via-graph".into(),
            symbol: "BTCUSDT".into(),
            ..Default::default()
        });
        let json = serde_json::to_string(&c).unwrap();
        let back: LkgCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}
