//! Sliding-window mention + sentiment aggregator.
//!
//! For each asset the counter keeps a deque of
//! `(ts, score)` events within a 1-hour retention window.
//! On [`snapshot_for`] it derives the `SentimentTick` the
//! risk engine consumes: level counts, rate, acceleration,
//! sentiment EWMA, delta.
//!
//! Acceleration is computed as
//! `mentions_rate_now - mentions_rate_one_minute_ago`. The
//! "one minute ago" series lives in a second deque of
//! `(ts, rate)` pairs populated each time the caller emits a
//! tick. This is an O(1) per-tick data structure bounded by
//! the retention window — no re-scanning of raw events on
//! every read.

use crate::types::SentimentTick;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const RETENTION_MINUTES: i64 = 60;
const RATE_HISTORY_MINUTES: i64 = 5;

#[derive(Debug, Clone)]
struct AssetState {
    /// Raw observations (ts, sentiment_score).
    events: VecDeque<(DateTime<Utc>, Decimal)>,
    /// Recent `(ts, mentions_rate)` samples so we can look
    /// back 1 minute to compute acceleration.
    rate_history: VecDeque<(DateTime<Utc>, Decimal)>,
    /// Last-emitted sentiment EWMA (5-min) so the next
    /// emit can compute `sentiment_delta`.
    last_sentiment_5min: Decimal,
}

impl Default for AssetState {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            rate_history: VecDeque::new(),
            last_sentiment_5min: dec!(0),
        }
    }
}

/// One counter per process, shared across analyzers. Internally
/// partitioned by normalised asset ticker.
#[derive(Debug, Default)]
pub struct MentionCounter {
    assets: HashMap<String, AssetState>,
}

impl MentionCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a scored article against every asset it touches.
    /// `ts` is usually the article's `analyzed_at` or
    /// `published_at` — analyzer-time is closer to the
    /// signal we care about (when *we* saw it) than when
    /// the publisher stamped it.
    pub fn record(&mut self, ts: DateTime<Utc>, asset: &str, score: Decimal) {
        let state = self.assets.entry(asset.to_string()).or_default();
        state.events.push_back((ts, score));
        Self::prune_events(state, ts);
    }

    /// Compute the next `SentimentTick` for `asset`. Returns
    /// `None` when no events have ever been recorded —
    /// caller's choice whether to skip that asset this
    /// cycle or emit a zero tick.
    pub fn snapshot_for(&mut self, asset: &str, now: DateTime<Utc>) -> Option<SentimentTick> {
        let state = self.assets.get_mut(asset)?;
        Self::prune_events(state, now);
        if state.events.is_empty() {
            return None;
        }
        let five_min_ago = now - Duration::minutes(5);
        let mentions_5min: u64 = state
            .events
            .iter()
            .filter(|(ts, _)| *ts >= five_min_ago)
            .count() as u64;
        let mentions_1h: u64 = state.events.len() as u64;

        // Rate normalised so "12" (i.e. 5-min count × 12 =
        // hourly-equivalent) becomes 1.0 at steady state.
        // `mentions_1h / 12` is the expected 5-min count if
        // the rate were uniform over the hour.
        let expected_5min = Decimal::from(mentions_1h) / dec!(12);
        let mentions_rate = if expected_5min > dec!(0) {
            Decimal::from(mentions_5min) / expected_5min
        } else {
            dec!(0)
        };

        // Acceleration: compare current rate to rate one
        // minute ago (the oldest entry still inside a 1-min
        // look-back window).
        let one_min_ago = now - Duration::minutes(1);
        let prev_rate = state
            .rate_history
            .iter()
            .rev()
            .find(|(ts, _)| *ts <= one_min_ago)
            .map(|(_, r)| *r)
            .unwrap_or(dec!(0));
        let mentions_acceleration = mentions_rate - prev_rate;

        // Push current rate into history + prune to retention.
        state.rate_history.push_back((now, mentions_rate));
        let history_cutoff = now - Duration::minutes(RATE_HISTORY_MINUTES);
        while let Some((ts, _)) = state.rate_history.front() {
            if *ts < history_cutoff {
                state.rate_history.pop_front();
            } else {
                break;
            }
        }

        // Sentiment EWMA over the 5-minute window (equal-
        // weight mean is fine at low event counts; EWMA
        // becomes interesting at higher volumes but the
        // simple mean is robust and traceable for audit).
        let (count_5min, sum_5min) =
            state
                .events
                .iter()
                .filter(|(ts, _)| *ts >= five_min_ago)
                .fold((0u64, dec!(0)), |(c, s), (_, v)| (c + 1, s + v));
        let sentiment_5min = if count_5min > 0 {
            sum_5min / Decimal::from(count_5min)
        } else {
            dec!(0)
        };
        let sentiment_prev = state.last_sentiment_5min;
        let sentiment_delta = sentiment_5min - sentiment_prev;
        state.last_sentiment_5min = sentiment_5min;

        Some(SentimentTick {
            asset: asset.to_string(),
            ts: now,
            mentions_5min,
            mentions_1h,
            mentions_rate,
            mentions_acceleration,
            sentiment_score_5min: sentiment_5min,
            sentiment_score_prev: sentiment_prev,
            sentiment_delta,
        })
    }

    /// Canonical asset list this counter tracks. Useful for
    /// the background task that emits ticks each cycle.
    pub fn assets(&self) -> Vec<String> {
        self.assets.keys().cloned().collect()
    }

    fn prune_events(state: &mut AssetState, now: DateTime<Utc>) {
        let cutoff = now - Duration::minutes(RETENTION_MINUTES);
        while let Some((ts, _)) = state.events.front() {
            if *ts < cutoff {
                state.events.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-04-18T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn empty_counter_yields_none() {
        let mut c = MentionCounter::new();
        assert!(c.snapshot_for("BTC", t0()).is_none());
    }

    #[test]
    fn flat_rate_scores_around_one() {
        let mut c = MentionCounter::new();
        // 60 events, one per minute starting 60 min ago. The
        // last 5 minutes (60-i in {1,2,3,4,5}) sit inside
        // the five-minute window, giving `mentions_5min = 5`
        // and a flat rate of 1.0 (5 vs expected 60/12 = 5).
        for i in 0..60 {
            let ts = t0() - Duration::minutes(60 - i);
            c.record(ts, "BTC", dec!(0));
        }
        let tick = c.snapshot_for("BTC", t0()).unwrap();
        assert_eq!(tick.mentions_1h, 60);
        assert_eq!(tick.mentions_5min, 5);
        assert_eq!(tick.mentions_rate, dec!(1));
    }

    #[test]
    fn spike_in_last_five_minutes_raises_rate() {
        let mut c = MentionCounter::new();
        // 12 old events (1/5min uniform) plus 20 in the last 5 min.
        for i in 0..12 {
            let ts = t0() - Duration::minutes(59 - (i * 5));
            c.record(ts, "BTC", dec!(0));
        }
        for _ in 0..20 {
            c.record(t0() - Duration::seconds(30), "BTC", dec!(0));
        }
        let tick = c.snapshot_for("BTC", t0()).unwrap();
        // Rate = 20 / ((32) / 12) = 20 / 2.67 ≈ 7.5.
        assert!(tick.mentions_rate > dec!(5));
    }

    #[test]
    fn sentiment_delta_captures_shift() {
        let mut c = MentionCounter::new();
        // First pass: bearish events → sentiment_5min < 0.
        for i in 0..5 {
            c.record(t0() - Duration::seconds(30 * i), "BTC", dec!(-0.8));
        }
        let first = c.snapshot_for("BTC", t0()).unwrap();
        assert!(first.sentiment_score_5min < dec!(-0.5));
        assert_eq!(first.sentiment_score_prev, dec!(0));
        assert!(first.sentiment_delta < dec!(0));

        // Second pass: bullish events arrive → next tick
        // sees a positive delta vs the previous EWMA.
        for _ in 0..10 {
            c.record(t0() + Duration::minutes(1), "BTC", dec!(0.9));
        }
        let second = c.snapshot_for("BTC", t0() + Duration::minutes(1)).unwrap();
        assert!(second.sentiment_score_5min > dec!(0));
        assert!(second.sentiment_delta > dec!(0));
    }

    #[test]
    fn old_events_pruned_out_of_window() {
        let mut c = MentionCounter::new();
        // 5 events from 2 hours ago — should be pruned.
        for i in 0..5 {
            c.record(t0() - Duration::hours(2) - Duration::seconds(i), "BTC", dec!(0));
        }
        // One fresh event keeps the asset alive.
        c.record(t0(), "BTC", dec!(0));
        let tick = c.snapshot_for("BTC", t0()).unwrap();
        assert_eq!(tick.mentions_1h, 1);
    }
}
