//! Event-sequence deduplicator for replay and WS-reconnect paths.
//!
//! Ported from the atomic-mesh `EventDeduplicator` pattern (see
//! `crates/atomic-node/src/recovery.rs`). Same idea: the dedup
//! works off a monotonic `max_seen` plus a bounded hash set of
//! recent sequence numbers. Events below `max_seen` have to be
//! verified against the hash set; events above `max_seen` bump
//! the watermark and pass through. A periodic prune caps memory.
//!
//! # Why this lives here (not in `mm-common`)
//!
//! The primary consumer today is the backtester's replay path —
//! the JSONL recorder can re-emit the tail of a file after a
//! reconnect, and without dedup the strategy sees duplicate
//! `MarketEvent`s. Live WS reconnect on Binance / Bybit pushes a
//! fresh snapshot followed by already-seen deltas from the queue
//! in flight, which is the same problem and will eventually want
//! this guard. When that second use case lands we can graduate
//! the module to `mm-common`; for now keeping it in backtester
//! avoids an early abstraction.
//!
//! # Semantics
//!
//! `check(seq) -> bool` returns `true` iff the event is fresh
//! (not seen before). Out-of-order events below `max_seen` are
//! accepted on first sight and rejected on repeat — the
//! watermark never goes backwards.

use std::collections::HashSet;

/// Cap on the hash set before a prune kicks in. Picked to match
/// atomic-mesh — 100k entries is ~800 KB at 8 B per u64, fine for
/// any replay or WS backlog we care about.
const PRUNE_THRESHOLD: usize = 100_000;

/// Number of recent entries kept after a prune. Keeps a rolling
/// window of the last 50k sequence numbers so in-flight
/// out-of-order events still get checked correctly.
const PRUNE_KEEP: u64 = 50_000;

/// Event-sequence deduplicator.
#[derive(Debug, Clone, Default)]
pub struct EventDeduplicator {
    seen: HashSet<u64>,
    max_seen: u64,
}

impl EventDeduplicator {
    /// Fresh deduplicator with no seen sequences.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the deduplicator at an existing sequence number —
    /// every `seq ≤ start_seq` that was not explicitly added
    /// will still be considered fresh on first sight. Use this
    /// when resuming from a snapshot that recorded the last
    /// processed sequence.
    pub fn from_seq(start_seq: u64) -> Self {
        Self {
            seen: HashSet::new(),
            max_seen: start_seq,
        }
    }

    /// Test a sequence number and mark it seen. Returns `true`
    /// iff the caller should process the event. Duplicates
    /// return `false`.
    pub fn check(&mut self, seq: u64) -> bool {
        // Fresh seq above the watermark → accept and bump.
        if seq > self.max_seen {
            self.seen.insert(seq);
            self.max_seen = seq;
            self.maybe_prune();
            return true;
        }
        // seq ≤ max_seen — may be a duplicate or a late arrival.
        // `insert` returns `true` on first sight, `false` on
        // duplicate. First sight is a late arrival and we accept
        // it; duplicate is a duplicate and we reject it.
        let fresh = self.seen.insert(seq);
        if fresh {
            self.maybe_prune();
        }
        fresh
    }

    /// Current high-water sequence number.
    pub fn max_seen(&self) -> u64 {
        self.max_seen
    }

    /// Number of sequences currently tracked. Test aid.
    pub fn tracked_len(&self) -> usize {
        self.seen.len()
    }

    fn maybe_prune(&mut self) {
        if self.seen.len() <= PRUNE_THRESHOLD {
            return;
        }
        let cutoff = self.max_seen.saturating_sub(PRUNE_KEEP);
        self.seen.retain(|&s| s > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_sequences_are_accepted() {
        let mut d = EventDeduplicator::new();
        assert!(d.check(1));
        assert!(d.check(2));
        assert!(d.check(3));
        assert_eq!(d.max_seen(), 3);
    }

    #[test]
    fn duplicates_are_rejected() {
        let mut d = EventDeduplicator::new();
        d.check(1);
        d.check(2);
        d.check(3);
        assert!(!d.check(1));
        assert!(!d.check(2));
        assert!(!d.check(3));
    }

    #[test]
    fn out_of_order_below_watermark_is_accepted_once() {
        let mut d = EventDeduplicator::new();
        d.check(10);
        // Late arrival at seq=5 — accepted on first sight
        // (it's fresh, just out of order).
        assert!(d.check(5));
        // But a re-arrival of seq=5 is a duplicate.
        assert!(!d.check(5));
        // Watermark did not go backwards.
        assert_eq!(d.max_seen(), 10);
    }

    #[test]
    fn seed_from_snapshot_seq_rejects_older_duplicates() {
        // Resume the replay at seq=100. Any seq ≤ 100 that
        // arrives and is a real duplicate of something the
        // upstream already processed should be rejected —
        // except that because we seeded with no hash-set state,
        // the first sighting is accepted (fresh). This matches
        // the `atomic-mesh` semantics where dedup runs on the
        // overlap window, not the full history.
        let mut d = EventDeduplicator::from_seq(100);
        assert_eq!(d.max_seen(), 100);
        // First sight of seq=95 (below watermark) is accepted —
        // it's treated as a late arrival.
        assert!(d.check(95));
        // Second sight is a duplicate.
        assert!(!d.check(95));
    }

    #[test]
    fn prune_does_not_drop_recent_entries() {
        let mut d = EventDeduplicator::new();
        // Force past the threshold (+1) so prune runs.
        for i in 1..=(PRUNE_THRESHOLD as u64 + 1) {
            assert!(d.check(i));
        }
        // After prune: keep only seqs > max_seen - PRUNE_KEEP.
        let cutoff = d.max_seen() - PRUNE_KEEP;
        // Recent entries near max_seen must still be detected
        // as duplicates.
        assert!(!d.check(d.max_seen()));
        assert!(!d.check(cutoff + 1));
        // Very old entries below cutoff are no longer tracked
        // and will be accepted as "fresh" on re-presentation.
        // This is an intentional memory tradeoff, matching
        // `atomic-mesh`.
        assert!(d.check(1));
    }

    #[test]
    fn interleaved_fresh_and_duplicate_stream() {
        let mut d = EventDeduplicator::new();
        let script = [
            (1, true),
            (2, true),
            (3, true),
            (2, false), // dup
            (4, true),
            (1, false), // dup
            (5, true),
            (5, false), // dup
        ];
        for (seq, expected) in script {
            assert_eq!(d.check(seq), expected, "seq={seq}");
        }
        assert_eq!(d.max_seen(), 5);
    }

    #[test]
    fn tracked_len_reflects_accepted_events() {
        let mut d = EventDeduplicator::new();
        for i in 1..=10 {
            d.check(i);
        }
        assert_eq!(d.tracked_len(), 10);
        // Duplicates do not grow the set.
        for i in 1..=10 {
            d.check(i);
        }
        assert_eq!(d.tracked_len(), 10);
    }
}
