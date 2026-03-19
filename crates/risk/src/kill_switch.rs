use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{error, info, warn};

/// Multi-level kill switch for emergency risk management.
///
/// Based on FIA best practices and Knight Capital lessons.
/// Kill switch must be AUTOMATED with manual override.
///
/// Levels:
/// 1. Widen spreads — double spread, reduce size 50%
/// 2. Stop new orders — no new orders, let existing expire
/// 3. Cancel all — cancel all open orders on all venues
/// 4. Flatten all — cancel all + aggressively sell/buy to zero inventory
/// 5. Disconnect — sever all exchange connections
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KillLevel {
    Normal = 0,
    WidenSpreads = 1,
    StopNewOrders = 2,
    CancelAll = 3,
    FlattenAll = 4,
    Disconnect = 5,
}

impl std::fmt::Display for KillLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KillLevel::Normal => write!(f, "NORMAL"),
            KillLevel::WidenSpreads => write!(f, "WIDEN_SPREADS"),
            KillLevel::StopNewOrders => write!(f, "STOP_NEW_ORDERS"),
            KillLevel::CancelAll => write!(f, "CANCEL_ALL"),
            KillLevel::FlattenAll => write!(f, "FLATTEN_ALL"),
            KillLevel::Disconnect => write!(f, "DISCONNECT"),
        }
    }
}

/// Kill switch configuration.
#[derive(Debug, Clone)]
pub struct KillSwitchConfig {
    /// Daily PnL loss limit (quote asset) → Level 3 (Cancel All).
    pub daily_loss_limit: Decimal,
    /// Warning threshold for PnL → Level 1 (Widen Spreads).
    pub daily_loss_warning: Decimal,
    /// Maximum position value (quote asset) → Level 2 (Stop New).
    pub max_position_value: Decimal,
    /// Maximum message rate per second → Level 3 (runaway algo detection).
    pub max_message_rate: u32,
    /// Maximum consecutive errors → Level 2.
    pub max_consecutive_errors: u32,
    /// Seconds without any fill before auto-widening → Level 1.
    pub no_fill_timeout_secs: u64,
}

impl Default for KillSwitchConfig {
    fn default() -> Self {
        Self {
            daily_loss_limit: dec!(1000),
            daily_loss_warning: dec!(500),
            max_position_value: dec!(50000),
            max_message_rate: 100,
            max_consecutive_errors: 10,
            no_fill_timeout_secs: 300,
        }
    }
}

/// Multi-level kill switch state machine.
pub struct KillSwitch {
    config: KillSwitchConfig,
    current_level: KillLevel,
    /// Reason for current level.
    reason: String,
    /// When the level was set.
    activated_at: Option<DateTime<Utc>>,

    // Tracking state.
    daily_pnl: Decimal,
    message_count_this_second: u32,
    consecutive_errors: u32,
    last_fill_at: Option<DateTime<Utc>>,
    day_start: DateTime<Utc>,
}

impl KillSwitch {
    pub fn new(config: KillSwitchConfig) -> Self {
        Self {
            config,
            current_level: KillLevel::Normal,
            reason: String::new(),
            activated_at: None,
            daily_pnl: dec!(0),
            message_count_this_second: 0,
            consecutive_errors: 0,
            last_fill_at: None,
            day_start: Utc::now(),
        }
    }

    pub fn level(&self) -> KillLevel {
        self.current_level
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Set level. Can only escalate, not de-escalate (use reset for that).
    fn escalate(&mut self, level: KillLevel, reason: &str) {
        if level > self.current_level {
            error!(
                from = %self.current_level,
                to = %level,
                reason = reason,
                "KILL SWITCH ESCALATED"
            );
            self.current_level = level;
            self.reason = reason.to_string();
            self.activated_at = Some(Utc::now());
        }
    }

    /// Manual trigger — set any level.
    pub fn manual_trigger(&mut self, level: KillLevel, reason: &str) {
        warn!(level = %level, reason = reason, "manual kill switch trigger");
        self.current_level = level;
        self.reason = format!("MANUAL: {reason}");
        self.activated_at = Some(Utc::now());
    }

    /// Reset to normal. Only call after manual review.
    pub fn reset(&mut self) {
        info!(
            from = %self.current_level,
            "kill switch reset to NORMAL"
        );
        self.current_level = KillLevel::Normal;
        self.reason.clear();
        self.activated_at = None;
        self.consecutive_errors = 0;
    }

    /// Update daily PnL.
    pub fn update_pnl(&mut self, pnl: Decimal) {
        // Reset daily tracking at day boundary.
        let now = Utc::now();
        if now.date_naive() != self.day_start.date_naive() {
            self.daily_pnl = dec!(0);
            self.day_start = now;
        }

        self.daily_pnl = pnl;

        if self.daily_pnl < -self.config.daily_loss_limit {
            self.escalate(KillLevel::CancelAll, "daily loss limit breached");
        } else if self.daily_pnl < -self.config.daily_loss_warning {
            self.escalate(KillLevel::WidenSpreads, "daily loss warning threshold");
        }
    }

    /// Update position value.
    pub fn update_position_value(&mut self, value: Decimal) {
        if value > self.config.max_position_value {
            self.escalate(KillLevel::StopNewOrders, "max position value exceeded");
        }
    }

    /// Record a message sent to exchange.
    pub fn on_message_sent(&mut self) {
        self.message_count_this_second += 1;
        if self.message_count_this_second > self.config.max_message_rate {
            self.escalate(
                KillLevel::CancelAll,
                "runaway algorithm detected — max message rate exceeded",
            );
        }
    }

    /// Reset message counter. Call this every second.
    pub fn tick_second(&mut self) {
        self.message_count_this_second = 0;

        // Check no-fill timeout.
        if let Some(last_fill) = self.last_fill_at {
            let elapsed = (Utc::now() - last_fill).num_seconds() as u64;
            if elapsed > self.config.no_fill_timeout_secs {
                self.escalate(KillLevel::WidenSpreads, "no fills for extended period");
            }
        }
    }

    /// Record a fill.
    pub fn on_fill(&mut self) {
        self.last_fill_at = Some(Utc::now());
        self.consecutive_errors = 0; // Fills mean connectivity is OK.
    }

    /// Record an error (API call failure).
    pub fn on_error(&mut self) {
        self.consecutive_errors += 1;
        if self.consecutive_errors >= self.config.max_consecutive_errors {
            self.escalate(KillLevel::StopNewOrders, "too many consecutive API errors");
        }
    }

    /// Should we place new orders?
    pub fn allow_new_orders(&self) -> bool {
        self.current_level < KillLevel::StopNewOrders
    }

    /// Spread multiplier based on kill level.
    pub fn spread_multiplier(&self) -> Decimal {
        match self.current_level {
            KillLevel::Normal => dec!(1),
            KillLevel::WidenSpreads => dec!(2),
            _ => dec!(1), // Higher levels don't quote at all.
        }
    }

    /// Size multiplier based on kill level.
    pub fn size_multiplier(&self) -> Decimal {
        match self.current_level {
            KillLevel::Normal => dec!(1),
            KillLevel::WidenSpreads => dec!(0.5),
            _ => dec!(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escalation() {
        let mut ks = KillSwitch::new(KillSwitchConfig {
            daily_loss_limit: dec!(100),
            daily_loss_warning: dec!(50),
            ..Default::default()
        });

        assert_eq!(ks.level(), KillLevel::Normal);
        assert!(ks.allow_new_orders());

        // Warning threshold.
        ks.update_pnl(dec!(-60));
        assert_eq!(ks.level(), KillLevel::WidenSpreads);
        assert!(ks.allow_new_orders()); // Still can place, but wider.
        assert_eq!(ks.spread_multiplier(), dec!(2));

        // Loss limit.
        ks.update_pnl(dec!(-110));
        assert_eq!(ks.level(), KillLevel::CancelAll);
        assert!(!ks.allow_new_orders());
    }

    #[test]
    fn test_cannot_deescalate() {
        let mut ks = KillSwitch::new(Default::default());
        ks.escalate(KillLevel::CancelAll, "test");
        ks.escalate(KillLevel::WidenSpreads, "test"); // Lower level.
        assert_eq!(ks.level(), KillLevel::CancelAll); // Still at higher.
    }

    #[test]
    fn test_runaway_detection() {
        let mut ks = KillSwitch::new(KillSwitchConfig {
            max_message_rate: 5,
            ..Default::default()
        });

        for _ in 0..6 {
            ks.on_message_sent();
        }
        assert_eq!(ks.level(), KillLevel::CancelAll);
    }

    #[test]
    fn test_reset() {
        let mut ks = KillSwitch::new(Default::default());
        ks.manual_trigger(KillLevel::Disconnect, "emergency");
        assert_eq!(ks.level(), KillLevel::Disconnect);

        ks.reset();
        assert_eq!(ks.level(), KillLevel::Normal);
    }
}
