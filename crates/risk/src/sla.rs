use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

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
        if let Some(spread) = self.current_spread_bps {
            if spread > self.config.max_spread_bps {
                compliant = false;
                self.violations.spread_too_wide += 1;
            }
        } else {
            compliant = false; // No spread = not quoting.
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
