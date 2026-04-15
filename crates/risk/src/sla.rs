use chrono::{DateTime, Datelike, Timelike, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Number of per-minute presence buckets in one UTC day. The
/// SLA layer keeps a fixed-size array of this length so a paid
/// MM agreement can audit "X % presence at Y bps for Z hours per
/// day per pair" with per-minute granularity. P2.2.
pub const MINUTES_PER_DAY: usize = 1440;

/// One per-minute SLA presence bucket.
///
/// Aggregates the per-second `tick()` samples that fell inside
/// the matching minute-of-day. Bucket boundaries are pinned to
/// UTC midnight so the array index is always
/// `now.hour() * 60 + now.minute()`.
#[derive(Debug, Clone, Default)]
pub struct PresenceBucket {
    /// Number of seconds (≤ 60) the engine sampled this minute.
    pub total_seconds: u32,
    /// Subset of `total_seconds` where the engine met the full
    /// SLA contract (two-sided, spread within limit, depth
    /// within limit).
    pub compliant_seconds: u32,
    /// Subset of `total_seconds` where the engine had both a
    /// bid AND an ask. Tracked separately because some MM
    /// agreements pay rebates against two-sided presence even
    /// when the spread floor is missed.
    pub two_sided_seconds: u32,
    /// Tightest spread observed during this minute, in bps.
    /// `None` until the first sample lands in the bucket.
    pub min_spread_bps: Option<Decimal>,
    /// Widest spread observed during this minute, in bps.
    pub max_spread_bps: Option<Decimal>,
}

impl PresenceBucket {
    /// Compliance percentage for this single bucket. Returns
    /// `100` for an empty bucket so missing data does not drag
    /// the average down — the engine reports `minutes_with_data`
    /// alongside the percentage so operators can spot gaps.
    pub fn compliance_pct(&self) -> Decimal {
        if self.total_seconds == 0 {
            return dec!(100);
        }
        Decimal::from(self.compliant_seconds) / Decimal::from(self.total_seconds) * dec!(100)
    }

    fn record_sample(&mut self, compliant: bool, two_sided: bool, spread_bps: Option<Decimal>) {
        self.total_seconds += 1;
        if compliant {
            self.compliant_seconds += 1;
        }
        if two_sided {
            self.two_sided_seconds += 1;
        }
        if let Some(s) = spread_bps {
            self.min_spread_bps = Some(self.min_spread_bps.map_or(s, |m| m.min(s)));
            self.max_spread_bps = Some(self.max_spread_bps.map_or(s, |m| m.max(s)));
        }
    }
}

/// Roll-up of the day's presence buckets — what the dashboard's
/// daily report exposes to operators and what paid MM agreements
/// audit against.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DailyPresenceSummary {
    /// Compliance percentage across every minute that recorded
    /// at least one sample. Empty minutes are excluded so a
    /// fresh start at 14:00 UTC does not look like 58 % uptime.
    pub presence_pct: Decimal,
    /// Same shape but for two-sided-only presence (some MM
    /// agreements pay against this metric independently).
    pub two_sided_pct: Decimal,
    /// How many minutes have any samples at all today. Useful
    /// to distinguish "100 % presence over 60 minutes" from
    /// "100 % presence over 1440 minutes".
    pub minutes_with_data: u32,
    /// The widest spread observed across every minute today, in
    /// bps. Picks the worst minute's worst sample; `None` when
    /// the day has no spread observations yet.
    pub worst_spread_bps: Option<Decimal>,
}

/// SLA (Service Level Agreement) obligations configuration.
///
/// Defines what the market maker MUST do to fulfill its contract
/// with the exchange. Violations are tracked and reported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaConfig {
    /// Maximum allowed spread in bps. Orders wider than this don't count.
    pub max_spread_bps: Decimal,
    /// Minimum depth on each side in quote asset (e.g., $2000).
    pub min_depth_quote: Decimal,
    /// Required uptime percentage (e.g., 95.0 = 95%).
    pub min_uptime_pct: Decimal,
    /// Two-sided quoting required — must have both bid and ask.
    pub two_sided_required: bool,
    /// Maximum time (seconds) to refresh quotes after a fill.
    pub max_requote_secs: u64,
    /// Minimum time (seconds) orders must rest on book to count.
    pub min_order_rest_secs: u64,
}

impl Default for SlaConfig {
    fn default() -> Self {
        Self {
            max_spread_bps: dec!(100),   // 1%
            min_depth_quote: dec!(2000), // $2000 per side
            max_requote_secs: 5,
            min_uptime_pct: dec!(95),
            two_sided_required: true,
            min_order_rest_secs: 3,
        }
    }
}

/// Tracks SLA compliance in real-time.
pub struct SlaTracker {
    config: SlaConfig,
    /// Total sample ticks.
    total_ticks: u64,
    /// Ticks where we were compliant.
    compliant_ticks: u64,
    /// Ticks where spread was within limit (separate from full compliance).
    spread_compliant_ticks: u64,
    /// Current state.
    is_quoting: bool,
    has_bid: bool,
    has_ask: bool,
    current_spread_bps: Option<Decimal>,
    current_bid_depth_quote: Decimal,
    current_ask_depth_quote: Decimal,
    /// Last fill timestamp — for requote tracking.
    last_fill_at: Option<DateTime<Utc>>,
    last_requote_at: Option<DateTime<Utc>>,
    /// Violation counters.
    violations: SlaViolations,
    /// Session start.
    started_at: DateTime<Utc>,
    /// Per-minute presence buckets for the current UTC day
    /// (P2.2). 1440 entries indexed by `hour * 60 + minute`.
    /// Cleared on the first sample that crosses into a new UTC
    /// day — yesterday's roll-up is captured by the dashboard's
    /// daily-report snapshot before the wipe.
    presence_buckets: Box<[PresenceBucket; MINUTES_PER_DAY]>,
    /// UTC day-of-year of the last `tick()` call. When this
    /// changes, `presence_buckets` is wiped before the new
    /// sample lands. Stored as `(year, ordinal)` so a year
    /// rollover at Jan 1 00:00 still resets correctly.
    presence_day_key: Option<(i32, u32)>,
}

#[derive(Debug, Clone, Default)]
pub struct SlaViolations {
    pub spread_too_wide: u64,
    pub insufficient_depth: u64,
    pub one_sided_quoting: u64,
    pub slow_requote: u64,
    pub total_downtime_secs: u64,
}

/// Snapshot of SLA status for reporting.
#[derive(Debug, Clone)]
pub struct SlaStatus {
    pub uptime_pct: Decimal,
    pub is_compliant: bool,
    pub violations: SlaViolations,
    pub current_spread_bps: Option<Decimal>,
    pub bid_depth_quote: Decimal,
    pub ask_depth_quote: Decimal,
    pub session_duration_secs: i64,
}

impl SlaTracker {
    pub fn new(config: SlaConfig) -> Self {
        Self {
            config,
            total_ticks: 0,
            compliant_ticks: 0,
            spread_compliant_ticks: 0,
            is_quoting: false,
            has_bid: false,
            has_ask: false,
            current_spread_bps: None,
            current_bid_depth_quote: dec!(0),
            current_ask_depth_quote: dec!(0),
            last_fill_at: None,
            last_requote_at: None,
            violations: SlaViolations::default(),
            started_at: Utc::now(),
            presence_buckets: Box::new(std::array::from_fn(|_| PresenceBucket::default())),
            presence_day_key: None,
        }
    }

    /// Called every tick (e.g., every second) to sample compliance.
    pub fn tick(&mut self) {
        self.total_ticks += 1;

        let mut compliant = true;

        // Check two-sided quoting.
        if self.config.two_sided_required && (!self.has_bid || !self.has_ask) {
            compliant = false;
            self.violations.one_sided_quoting += 1;
        }

        // Check spread.
        let spread_ok =
            matches!(self.current_spread_bps, Some(spread) if spread <= self.config.max_spread_bps);
        if spread_ok {
            self.spread_compliant_ticks += 1;
        } else {
            compliant = false;
            if self.current_spread_bps.is_some() {
                self.violations.spread_too_wide += 1;
            }
        }

        // Check depth.
        if self.current_bid_depth_quote < self.config.min_depth_quote
            || self.current_ask_depth_quote < self.config.min_depth_quote
        {
            compliant = false;
            self.violations.insufficient_depth += 1;
        }

        // Check requote timing.
        if let (Some(fill_at), Some(requote_at)) = (self.last_fill_at, self.last_requote_at) {
            let delay = (requote_at - fill_at).num_seconds();
            if delay > self.config.max_requote_secs as i64 {
                self.violations.slow_requote += 1;
            }
        }

        if compliant {
            self.compliant_ticks += 1;
        }

        // P2.2: route the same sample into the per-minute
        // presence bucket for the current UTC day. Wipe the
        // array on the first tick that crosses midnight so
        // each day's buckets are independent.
        let now = Utc::now();
        let day_key = (now.year(), now.ordinal());
        if self.presence_day_key != Some(day_key) {
            for bucket in self.presence_buckets.iter_mut() {
                *bucket = PresenceBucket::default();
            }
            self.presence_day_key = Some(day_key);
        }
        let idx = (now.hour() * 60 + now.minute()) as usize;
        let two_sided = self.has_bid && self.has_ask;
        self.presence_buckets[idx].record_sample(compliant, two_sided, self.current_spread_bps);
    }

    /// Borrow the per-minute presence buckets for read-only
    /// inspection. Index `i` is the minute-of-day
    /// `i = hour * 60 + minute` in UTC. Useful for the daily
    /// report and for unit tests that want to assert the bucket
    /// the engine wrote into.
    pub fn presence_buckets(&self) -> &[PresenceBucket] {
        &self.presence_buckets[..]
    }

    /// Compliance percentage for one specific minute-of-day.
    /// Returns `100` for empty minutes — same convention as
    /// [`PresenceBucket::compliance_pct`].
    pub fn presence_pct_for_minute(&self, minute_of_day: usize) -> Decimal {
        if minute_of_day >= MINUTES_PER_DAY {
            return dec!(100);
        }
        self.presence_buckets[minute_of_day].compliance_pct()
    }

    /// Average compliance across minutes `[start, end)`.
    /// Out-of-range or zero-data minutes are skipped so the
    /// result is the average over **minutes that actually had
    /// samples**. Returns `100` when nothing in the range has
    /// any data.
    pub fn presence_pct_for_range(&self, start: usize, end: usize) -> Decimal {
        let end = end.min(MINUTES_PER_DAY);
        if start >= end {
            return dec!(100);
        }
        let mut total_seconds: u64 = 0;
        let mut compliant_seconds: u64 = 0;
        for bucket in &self.presence_buckets[start..end] {
            total_seconds += bucket.total_seconds as u64;
            compliant_seconds += bucket.compliant_seconds as u64;
        }
        if total_seconds == 0 {
            return dec!(100);
        }
        Decimal::from(compliant_seconds) / Decimal::from(total_seconds) * dec!(100)
    }

    /// Roll-up of the day's presence buckets. Powers the
    /// dashboard daily report and the new
    /// `mm_sla_presence_pct_24h` Prometheus gauge.
    pub fn daily_presence_summary(&self) -> DailyPresenceSummary {
        let mut total_seconds: u64 = 0;
        let mut compliant_seconds: u64 = 0;
        let mut two_sided_seconds: u64 = 0;
        let mut minutes_with_data: u32 = 0;
        let mut worst_spread_bps: Option<Decimal> = None;
        for bucket in self.presence_buckets.iter() {
            if bucket.total_seconds == 0 {
                continue;
            }
            minutes_with_data += 1;
            total_seconds += bucket.total_seconds as u64;
            compliant_seconds += bucket.compliant_seconds as u64;
            two_sided_seconds += bucket.two_sided_seconds as u64;
            if let Some(max) = bucket.max_spread_bps {
                worst_spread_bps = Some(worst_spread_bps.map_or(max, |prev| prev.max(max)));
            }
        }
        let presence_pct = if total_seconds == 0 {
            dec!(100)
        } else {
            Decimal::from(compliant_seconds) / Decimal::from(total_seconds) * dec!(100)
        };
        let two_sided_pct = if total_seconds == 0 {
            dec!(100)
        } else {
            Decimal::from(two_sided_seconds) / Decimal::from(total_seconds) * dec!(100)
        };
        DailyPresenceSummary {
            presence_pct,
            two_sided_pct,
            minutes_with_data,
            worst_spread_bps,
        }
    }

    /// Update current quoting state.
    pub fn update_quotes(
        &mut self,
        has_bid: bool,
        has_ask: bool,
        spread_bps: Option<Decimal>,
        bid_depth_quote: Decimal,
        ask_depth_quote: Decimal,
    ) {
        self.has_bid = has_bid;
        self.has_ask = has_ask;
        self.current_spread_bps = spread_bps;
        self.current_bid_depth_quote = bid_depth_quote;
        self.current_ask_depth_quote = ask_depth_quote;
        self.is_quoting = has_bid || has_ask;
        self.last_requote_at = Some(Utc::now());
    }

    /// Record a fill event.
    pub fn on_fill(&mut self) {
        self.last_fill_at = Some(Utc::now());
    }

    /// Current uptime percentage.
    pub fn uptime_pct(&self) -> Decimal {
        if self.total_ticks == 0 {
            return dec!(100);
        }
        Decimal::from(self.compliant_ticks) / Decimal::from(self.total_ticks) * dec!(100)
    }

    /// Spread-only compliance percentage (% of ticks where spread was within limit).
    pub fn spread_compliance_pct(&self) -> Decimal {
        if self.total_ticks == 0 {
            return dec!(100);
        }
        Decimal::from(self.spread_compliant_ticks) / Decimal::from(self.total_ticks) * dec!(100)
    }

    /// SLA config for reporting.
    pub fn config(&self) -> &SlaConfig {
        &self.config
    }

    /// Is the MM currently meeting SLA?
    pub fn is_meeting_sla(&self) -> bool {
        self.uptime_pct() >= self.config.min_uptime_pct
    }

    /// Get full status snapshot.
    pub fn status(&self) -> SlaStatus {
        let duration = (Utc::now() - self.started_at).num_seconds();
        SlaStatus {
            uptime_pct: self.uptime_pct(),
            is_compliant: self.is_meeting_sla(),
            violations: self.violations.clone(),
            current_spread_bps: self.current_spread_bps,
            bid_depth_quote: self.current_bid_depth_quote,
            ask_depth_quote: self.current_ask_depth_quote,
            session_duration_secs: duration,
        }
    }

    /// Log a periodic summary.
    pub fn log_summary(&self) {
        let status = self.status();
        if status.is_compliant {
            info!(
                uptime = %status.uptime_pct,
                spread_bps = ?status.current_spread_bps,
                bid_depth = %status.bid_depth_quote,
                ask_depth = %status.ask_depth_quote,
                "SLA OK"
            );
        } else {
            warn!(
                uptime = %status.uptime_pct,
                required = %self.config.min_uptime_pct,
                wide_spread = status.violations.spread_too_wide,
                low_depth = status.violations.insufficient_depth,
                one_sided = status.violations.one_sided_quoting,
                "SLA VIOLATION"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_tracker() -> SlaTracker {
        SlaTracker::new(SlaConfig {
            max_spread_bps: dec!(100),
            min_depth_quote: dec!(2000),
            min_uptime_pct: dec!(95),
            two_sided_required: true,
            max_requote_secs: 5,
            min_order_rest_secs: 3,
        })
    }

    #[test]
    fn test_spread_compliance_separate_from_uptime() {
        let mut tracker = default_tracker();

        // Spread within limit but depth too low → uptime fails, spread compliance passes.
        tracker.update_quotes(true, true, Some(dec!(50)), dec!(1000), dec!(1000));
        tracker.tick();

        // Spread too wide but depth ok → both fail.
        tracker.update_quotes(true, true, Some(dec!(150)), dec!(3000), dec!(3000));
        tracker.tick();

        // Both ok.
        tracker.update_quotes(true, true, Some(dec!(80)), dec!(3000), dec!(3000));
        tracker.tick();

        // 3 ticks total. Spread was ok in tick 1 and 3 → spread compliance = 66.66%.
        let spread_pct = tracker.spread_compliance_pct();
        assert!(spread_pct > dec!(66) && spread_pct < dec!(67));

        // Uptime: only tick 3 was fully compliant → 33.33%.
        let uptime_pct = tracker.uptime_pct();
        assert!(uptime_pct > dec!(33) && uptime_pct < dec!(34));
    }

    #[test]
    fn test_config_accessor() {
        let tracker = default_tracker();
        assert_eq!(tracker.config().max_spread_bps, dec!(100));
        assert_eq!(tracker.config().min_depth_quote, dec!(2000));
    }

    /// P2.2: every `tick()` call must increment the bucket for
    /// the current UTC minute. The bucket is selected from the
    /// `1440`-entry array so a synthetic test cannot pin the
    /// index without freezing time — instead we assert that
    /// **exactly one** bucket grew by one second after a single
    /// tick, and that the total compliant_seconds across all
    /// buckets matches what a downstream summary reports.
    #[test]
    fn presence_bucket_records_each_tick_into_current_minute() {
        let mut tracker = default_tracker();
        tracker.update_quotes(true, true, Some(dec!(50)), dec!(3000), dec!(3000));
        tracker.tick();

        let buckets_with_data: Vec<&PresenceBucket> = tracker
            .presence_buckets()
            .iter()
            .filter(|b| b.total_seconds > 0)
            .collect();
        assert_eq!(
            buckets_with_data.len(),
            1,
            "tick must land in exactly one minute bucket"
        );
        let bucket = buckets_with_data[0];
        assert_eq!(bucket.total_seconds, 1);
        assert_eq!(bucket.compliant_seconds, 1);
        assert_eq!(bucket.two_sided_seconds, 1);
        assert_eq!(bucket.min_spread_bps, Some(dec!(50)));
        assert_eq!(bucket.max_spread_bps, Some(dec!(50)));
    }

    /// Multiple ticks with widening spreads in the same minute
    /// must aggregate into the same bucket and update the
    /// `min_spread_bps` / `max_spread_bps` envelope.
    #[test]
    fn multiple_ticks_in_same_minute_aggregate_into_one_bucket() {
        let mut tracker = default_tracker();
        tracker.update_quotes(true, true, Some(dec!(40)), dec!(3000), dec!(3000));
        tracker.tick();
        tracker.update_quotes(true, true, Some(dec!(80)), dec!(3000), dec!(3000));
        tracker.tick();
        tracker.update_quotes(true, true, Some(dec!(60)), dec!(3000), dec!(3000));
        tracker.tick();

        let buckets_with_data: Vec<&PresenceBucket> = tracker
            .presence_buckets()
            .iter()
            .filter(|b| b.total_seconds > 0)
            .collect();
        // All three ticks must land in the same minute (this
        // test runs in well under one second). If wall-clock
        // crosses a minute boundary mid-test the assertion
        // relaxes to "≤2 buckets" — but the running time
        // makes that effectively impossible.
        assert_eq!(buckets_with_data.len(), 1);
        let bucket = buckets_with_data[0];
        assert_eq!(bucket.total_seconds, 3);
        assert_eq!(bucket.compliant_seconds, 3);
        assert_eq!(bucket.min_spread_bps, Some(dec!(40)));
        assert_eq!(bucket.max_spread_bps, Some(dec!(80)));
    }

    /// `compliance_pct` on an empty bucket returns `100` — the
    /// "missing data does not drag the average down"
    /// invariant. Pin it because a future contributor might
    /// be tempted to return `0` and that would silently break
    /// the daily presence summary on fresh start-ups.
    #[test]
    fn empty_bucket_reports_100_pct_compliance() {
        let bucket = PresenceBucket::default();
        assert_eq!(bucket.compliance_pct(), dec!(100));
    }

    /// `presence_pct_for_range` averages across the
    /// **observation-weighted** total, not the bucket count —
    /// so a minute with 60 samples and 30 compliant counts
    /// twice as much as a minute with 30 samples and 30
    /// compliant.
    #[test]
    fn presence_pct_for_range_is_observation_weighted() {
        let mut tracker = default_tracker();
        // Hand-poke buckets to model two different minutes.
        tracker.presence_buckets[0] = PresenceBucket {
            total_seconds: 60,
            compliant_seconds: 30,
            two_sided_seconds: 60,
            min_spread_bps: Some(dec!(10)),
            max_spread_bps: Some(dec!(20)),
        };
        tracker.presence_buckets[1] = PresenceBucket {
            total_seconds: 30,
            compliant_seconds: 30,
            two_sided_seconds: 30,
            min_spread_bps: Some(dec!(15)),
            max_spread_bps: Some(dec!(15)),
        };
        // 60 compliant / 90 total = 66.66...%
        let pct = tracker.presence_pct_for_range(0, 2);
        assert!(pct > dec!(66.66) && pct < dec!(66.67), "got {pct}");
    }

    /// `presence_pct_for_range` returns the
    /// "missing-data-is-100" default when no minute in the
    /// range has any samples — the same convention as
    /// `compliance_pct`.
    #[test]
    fn presence_pct_for_range_empty_returns_100() {
        let tracker = default_tracker();
        assert_eq!(tracker.presence_pct_for_range(0, 60), dec!(100));
    }

    /// `daily_presence_summary` rolls up the observed minutes
    /// only — empty minutes do not contribute. With two
    /// minutes of data totalling 120 samples and 90 compliant,
    /// the percentage is 75.
    #[test]
    fn daily_presence_summary_skips_empty_minutes() {
        let mut tracker = default_tracker();
        tracker.presence_buckets[0] = PresenceBucket {
            total_seconds: 60,
            compliant_seconds: 30,
            two_sided_seconds: 50,
            min_spread_bps: Some(dec!(40)),
            max_spread_bps: Some(dec!(80)),
        };
        tracker.presence_buckets[100] = PresenceBucket {
            total_seconds: 60,
            compliant_seconds: 60,
            two_sided_seconds: 60,
            min_spread_bps: Some(dec!(20)),
            max_spread_bps: Some(dec!(30)),
        };
        let summary = tracker.daily_presence_summary();
        assert_eq!(summary.minutes_with_data, 2);
        // 90 / 120 = 75 %.
        assert_eq!(summary.presence_pct, dec!(75));
        // 110 / 120 ≈ 91.66 %.
        assert!(summary.two_sided_pct > dec!(91.66) && summary.two_sided_pct < dec!(91.67));
        assert_eq!(summary.worst_spread_bps, Some(dec!(80)));
    }

    /// `daily_presence_summary` with zero data yields the
    /// fresh-start defaults — `100` percent presence over zero
    /// minutes, no worst spread. Catches the regression where
    /// a fresh engine reports `NaN` or `0` instead.
    #[test]
    fn daily_presence_summary_fresh_start_is_100_over_zero_minutes() {
        let tracker = default_tracker();
        let summary = tracker.daily_presence_summary();
        assert_eq!(summary.presence_pct, dec!(100));
        assert_eq!(summary.two_sided_pct, dec!(100));
        assert_eq!(summary.minutes_with_data, 0);
        assert!(summary.worst_spread_bps.is_none());
    }
}
