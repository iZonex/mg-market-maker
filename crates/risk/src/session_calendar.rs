//! Epic R Week 5b — venue session calendar.
//!
//! Crypto venues are nominally 24/7 but every perp exchange has
//! funding windows (00:00 / 08:00 / 16:00 UTC for most) and every
//! settled product has a daily settlement window. These are the
//! moments where manipulation (marking-the-close, marking-the-open,
//! squeeze-on-settlement) pays off — so the `MarkingCloseDetector`
//! needs to know "are we within N seconds of a session boundary?"
//!
//! Shape kept dumb on purpose: one table of UTC minutes-of-day
//! marking session boundaries, a `seconds_to_next_boundary(now)`
//! query. `Session.TimeToClose` source node reads from here and
//! surfaces the countdown to graphs.

use chrono::{DateTime, Timelike, Utc};

/// Session boundary in UTC minutes-of-day (0..=1440). 1440 is
/// treated as 0 of the next day.
pub type MinuteOfDayUtc = u32;

#[derive(Debug, Clone, Default)]
pub struct SessionCalendar {
    /// Sorted list of minute-of-day marks. Default is the funding
    /// cadence every major perp venue uses (00:00, 08:00, 16:00
    /// UTC). Operators can override when pointing at a venue with
    /// a different schedule.
    pub boundaries_utc_minutes: Vec<MinuteOfDayUtc>,
}

impl SessionCalendar {
    /// Default funding-window calendar — the one operators usually
    /// want on crypto.
    pub fn funding_8h() -> Self {
        Self {
            boundaries_utc_minutes: vec![0, 480, 960], // 00:00, 08:00, 16:00
        }
    }

    /// Operator override — provide your own UTC-minute marks.
    pub fn with_boundaries(mut marks: Vec<MinuteOfDayUtc>) -> Self {
        marks.sort();
        marks.dedup();
        Self {
            boundaries_utc_minutes: marks,
        }
    }

    /// Seconds from `now` to the next boundary. Returns `None`
    /// when the calendar is empty (a truly 24/7 venue with no
    /// session boundaries — `MarkingCloseDetector` treats that
    /// as "always score 0").
    pub fn seconds_to_next(&self, now: DateTime<Utc>) -> Option<i64> {
        if self.boundaries_utc_minutes.is_empty() {
            return None;
        }
        let now_minutes = (now.hour() * 60 + now.minute()) as i64;
        let now_seconds_in_minute = now.second() as i64;
        // Distance in seconds to each boundary. A boundary earlier
        // today is expressed as "tomorrow's version" by adding a
        // full day (1440 minutes).
        let mut best: i64 = i64::MAX;
        for &m in &self.boundaries_utc_minutes {
            let m = m as i64;
            // Offset from current minute to boundary minute. If the
            // boundary is this very minute and we're past second 0,
            // we still count that as "next boundary tomorrow" —
            // otherwise the detector's "within N seconds of a
            // boundary" guard fires permanently for that minute.
            // A boundary that's strictly in the future today.
            // Boundaries at or before the current minute roll to
            // tomorrow — including `m == now_minutes` at second 0,
            // because we're AT the boundary, not waiting for it.
            let minute_delta = if m > now_minutes {
                m - now_minutes
            } else {
                m + 1440 - now_minutes
            };
            let seconds_delta = minute_delta * 60 - now_seconds_in_minute;
            if seconds_delta >= 0 && seconds_delta < best {
                best = seconds_delta;
            }
        }
        if best == i64::MAX {
            None
        } else {
            Some(best)
        }
    }

    /// Seconds *since* the most-recent boundary. Mirror of
    /// `seconds_to_next` for "marking-the-open" detection.
    pub fn seconds_since_last(&self, now: DateTime<Utc>) -> Option<i64> {
        if self.boundaries_utc_minutes.is_empty() {
            return None;
        }
        let now_minutes = (now.hour() * 60 + now.minute()) as i64;
        let now_seconds_in_minute = now.second() as i64;
        let mut best: i64 = i64::MAX;
        for &m in &self.boundaries_utc_minutes {
            let m = m as i64;
            let minute_delta = if m <= now_minutes {
                now_minutes - m
            } else {
                now_minutes + 1440 - m
            };
            let seconds_delta = minute_delta * 60 + now_seconds_in_minute;
            if seconds_delta >= 0 && seconds_delta < best {
                best = seconds_delta;
            }
        }
        if best == i64::MAX {
            None
        } else {
            Some(best)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn funding_8h_schedule() {
        let cal = SessionCalendar::funding_8h();
        // 07:00 UTC → 1h to next (08:00).
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 7, 0, 0).unwrap();
        assert_eq!(cal.seconds_to_next(now), Some(3600));
        // 07:59:30 → 30 s to next.
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 7, 59, 30).unwrap();
        assert_eq!(cal.seconds_to_next(now), Some(30));
        // 16:00:00 exactly → next is 00:00 tomorrow = 8h.
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 16, 0, 0).unwrap();
        assert_eq!(cal.seconds_to_next(now), Some(8 * 3600));
    }

    #[test]
    fn seconds_since_last_matches_schedule() {
        let cal = SessionCalendar::funding_8h();
        // 08:30 → 30 min since 08:00.
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 8, 30, 0).unwrap();
        assert_eq!(cal.seconds_since_last(now), Some(30 * 60));
    }

    #[test]
    fn empty_calendar_returns_none() {
        let cal = SessionCalendar::default();
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        assert_eq!(cal.seconds_to_next(now), None);
    }
}
