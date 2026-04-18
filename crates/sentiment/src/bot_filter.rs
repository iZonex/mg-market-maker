//! Near-duplicate / spam filter.
//!
//! A real-world feed brings two kinds of repeats that must
//! NOT inflate `mentions_rate`:
//!
//! 1. **Exact URL repost** — already filtered by the
//!    orchestrator's `seen_urls` set.
//! 2. **Text duplicates across distinct URLs** — one tweet
//!    retweeted from 50 accounts; the same headline syndicated
//!    across 5 RSS aggregators. Each has a unique URL but the
//!    same content signal. Counting them as independent
//!    mentions creates a false spike.
//!
//! This module solves (2) via a **normalised text hash**
//! layered on a sliding window. Algorithm:
//!
//!   normalise: lowercase + strip punctuation + collapse
//!              whitespace + drop URLs (they're noise in
//!              retweets: `https://t.co/XXX` changes per
//!              retweet).
//!   hash:      64-bit FNV-1a (fast, good-enough for dedup
//!              at the volumes we see).
//!   window:    N minutes of (hash, first_seen, count). A
//!              hash seen `>= burst_threshold` times within
//!              the window is considered a burst — we admit
//!              at most `burst_cap` mentions from it.
//!
//! Plugged in as a decision gate: `should_count(article) →
//! bool`. The orchestrator calls this before recording the
//! article's asset mentions into the counter.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

/// FNV-1a 64-bit over normalised text.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Strip URLs + punctuation + lowercase + collapse ws.
/// Empty output possible for articles that are pure URLs
/// (some Twitter posts).
pub fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_url = false;
    let mut last_space = true;
    for word in text.split_whitespace() {
        let is_url = word.starts_with("http://")
            || word.starts_with("https://")
            || word.starts_with("t.co/");
        if is_url {
            in_url = true;
            continue;
        }
        in_url = false;
        for ch in word.chars() {
            if ch.is_ascii_alphanumeric() {
                for c in ch.to_lowercase() {
                    out.push(c);
                }
                last_space = false;
            } else if !last_space {
                out.push(' ');
                last_space = true;
            }
        }
        if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    // Tidy trailing space.
    let trimmed = out.trim_end().to_string();
    let _ = in_url;
    trimmed
}

#[derive(Debug, Clone, Copy)]
pub struct BotFilterConfig {
    /// Sliding window retention. Hashes older than this are
    /// evicted. Default 15 minutes.
    pub window: Duration,
    /// Count at which a hash is flagged as a burst.
    /// Default 3 repeats.
    pub burst_threshold: u32,
    /// How many mentions from one burst we'll still admit.
    /// Default 1 — first mention counts, all subsequent
    /// repeats are suppressed.
    pub burst_cap: u32,
}

impl Default for BotFilterConfig {
    fn default() -> Self {
        Self {
            window: Duration::minutes(15),
            burst_threshold: 3,
            burst_cap: 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SeenEntry {
    first_seen: DateTime<Utc>,
    admitted: u32,
    total: u32,
}

#[derive(Debug)]
pub struct BotFilter {
    cfg: BotFilterConfig,
    seen: HashMap<u64, SeenEntry>,
}

impl BotFilter {
    pub fn new(cfg: BotFilterConfig) -> Self {
        Self {
            cfg,
            seen: HashMap::new(),
        }
    }

    /// Returns `true` if the article should count toward the
    /// mention rate, `false` if it's a burst repeat we're
    /// suppressing.
    pub fn should_count(&mut self, text: &str, now: DateTime<Utc>) -> bool {
        self.evict(now);
        let normal = normalise(text);
        if normal.is_empty() {
            // Pure-URL / empty post — admit the URL dedup
            // already filtered; this one is harmless to
            // count.
            return true;
        }
        let h = fnv1a_64(normal.as_bytes());
        let entry = self.seen.entry(h).or_insert(SeenEntry {
            first_seen: now,
            admitted: 0,
            total: 0,
        });
        entry.total += 1;
        // Admit if we're under the burst cap OR we haven't
        // crossed the burst threshold yet.
        if entry.total <= self.cfg.burst_threshold || entry.admitted < self.cfg.burst_cap {
            entry.admitted += 1;
            true
        } else {
            false
        }
    }

    fn evict(&mut self, now: DateTime<Utc>) {
        let cutoff = now - self.cfg.window;
        self.seen.retain(|_, e| e.first_seen >= cutoff);
    }

    /// Diagnostics: number of hashes currently tracked.
    pub fn tracked(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_strips_urls_and_punct() {
        let raw = "BREAKING: Bitcoin hits $100K!! https://t.co/abc https://example.com/x";
        let n = normalise(raw);
        assert_eq!(n, "breaking bitcoin hits 100k");
    }

    #[test]
    fn identical_text_across_distinct_inputs_counted_once() {
        let mut f = BotFilter::new(BotFilterConfig {
            window: Duration::minutes(15),
            burst_threshold: 2,
            burst_cap: 1,
        });
        let now = Utc::now();
        // First three accepted up to burst_threshold=2 AND burst_cap=1.
        // With burst_threshold=2: first 2 pass; 3rd only passes if admitted < cap=1 (no).
        assert!(f.should_count("Bitcoin rally starts now", now));
        assert!(f.should_count("bitcoin rally starts now", now));
        // This is the 3rd occurrence — total=3 > burst_threshold=2 AND
        // admitted=2 ≥ burst_cap=1, so it should be suppressed.
        assert!(!f.should_count("BITCOIN RALLY STARTS NOW!!", now));
    }

    #[test]
    fn burst_cap_one_keeps_first_drops_the_rest() {
        let mut f = BotFilter::new(BotFilterConfig {
            window: Duration::minutes(15),
            burst_threshold: 1,
            burst_cap: 1,
        });
        let now = Utc::now();
        assert!(f.should_count("retweet storm underway", now));
        assert!(!f.should_count("retweet storm underway", now));
        assert!(!f.should_count("retweet storm underway", now));
        assert!(!f.should_count("retweet storm underway", now));
    }

    #[test]
    fn distinct_texts_never_collide() {
        let mut f = BotFilter::new(BotFilterConfig::default());
        let now = Utc::now();
        assert!(f.should_count("a entirely unique article one", now));
        assert!(f.should_count("a entirely unique article two", now));
        assert!(f.should_count("a entirely unique article three", now));
    }

    #[test]
    fn window_expiry_allows_same_text_after_gap() {
        let mut f = BotFilter::new(BotFilterConfig {
            window: Duration::minutes(5),
            burst_threshold: 1,
            burst_cap: 1,
        });
        let t0 = Utc::now();
        assert!(f.should_count("same content", t0));
        assert!(!f.should_count("same content", t0));
        let t1 = t0 + Duration::minutes(10);
        assert!(f.should_count("same content", t1));
    }

    #[test]
    fn empty_normalised_pass_through() {
        let mut f = BotFilter::new(BotFilterConfig::default());
        let now = Utc::now();
        assert!(f.should_count("https://t.co/abc", now));
        assert!(f.should_count("https://t.co/def", now));
    }
}
