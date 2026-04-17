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

impl KillLevel {
    /// Spread multiplier driven by a kill level — the same shape
    /// the per-engine `KillSwitch::spread_multiplier` uses, but
    /// expressed as a free function so the engine can derive the
    /// multiplier from a *combined* (global ∨ asset-class) level
    /// without holding either kill switch by reference. P2.1.
    pub fn spread_multiplier(self) -> Decimal {
        match self {
            KillLevel::Normal => dec!(1),
            KillLevel::WidenSpreads => dec!(2),
            _ => dec!(1),
        }
    }

    /// Size multiplier driven by a kill level — `0` at and above
    /// `StopNewOrders` so quoting evaporates regardless of which
    /// kill switch tripped the level.
    pub fn size_multiplier(self) -> Decimal {
        match self {
            KillLevel::Normal => dec!(1),
            KillLevel::WidenSpreads => dec!(0.5),
            _ => dec!(0),
        }
    }
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
    /// Timestamp when Market Resilience first dropped below
    /// `mr_low_threshold`. Cleared when the score recovers
    /// above that threshold. When the dip persists longer than
    /// `mr_sustain_secs`, the kill switch escalates to
    /// `WidenSpreads`.
    low_mr_since: Option<DateTime<Utc>>,
}

/// Threshold below which the Market Resilience reading is
/// considered "under shock" for kill-switch escalation
/// purposes.
pub const MR_LOW_THRESHOLD: Decimal = dec!(0.3);
/// Sustain window — MR must stay below [`MR_LOW_THRESHOLD`]
/// for at least this many seconds before the switch widens.
pub const MR_SUSTAIN_SECS: i64 = 3;

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
            low_mr_since: None,
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

    /// Feed the current Market Resilience reading. Escalates
    /// the switch to [`KillLevel::WidenSpreads`] when the score
    /// stays below [`MR_LOW_THRESHOLD`] for at least
    /// [`MR_SUSTAIN_SECS`] seconds. Higher levels
    /// (`StopNewOrders` and above) are not raised from this
    /// signal alone — MR is a soft input, PnL / position value
    /// remain the hard escalation triggers.
    pub fn update_market_resilience(&mut self, score: Decimal, now: DateTime<Utc>) {
        if score < MR_LOW_THRESHOLD {
            let anchor = self.low_mr_since.get_or_insert(now);
            let elapsed = now.signed_duration_since(*anchor).num_seconds();
            if elapsed >= MR_SUSTAIN_SECS && self.current_level == KillLevel::Normal {
                self.escalate(
                    KillLevel::WidenSpreads,
                    "market resilience sustained below threshold",
                );
            }
        } else {
            self.low_mr_since = None;
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
        self.current_level.spread_multiplier()
    }

    /// Size multiplier based on kill level.
    pub fn size_multiplier(&self) -> Decimal {
        self.current_level.size_multiplier()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P2.1 multiplier helpers — pin the per-level constants
    /// so a future contributor cannot silently change them out
    /// from under the asset-class effective-level path.
    #[test]
    fn level_multiplier_helpers_match_per_level_state_machine() {
        assert_eq!(KillLevel::Normal.spread_multiplier(), dec!(1));
        assert_eq!(KillLevel::WidenSpreads.spread_multiplier(), dec!(2));
        assert_eq!(KillLevel::StopNewOrders.spread_multiplier(), dec!(1));
        assert_eq!(KillLevel::CancelAll.spread_multiplier(), dec!(1));

        assert_eq!(KillLevel::Normal.size_multiplier(), dec!(1));
        assert_eq!(KillLevel::WidenSpreads.size_multiplier(), dec!(0.5));
        assert_eq!(KillLevel::StopNewOrders.size_multiplier(), dec!(0));
        assert_eq!(KillLevel::CancelAll.size_multiplier(), dec!(0));

        // The free-fn helpers and the per-instance accessors
        // must agree — otherwise the engine's effective-level
        // path would diverge from the legacy single-instance
        // path. Regression anchor for the P2.1 refactor.
        let mut ks = KillSwitch::new(KillSwitchConfig::default());
        ks.escalate(KillLevel::WidenSpreads, "test");
        assert_eq!(
            ks.spread_multiplier(),
            KillLevel::WidenSpreads.spread_multiplier()
        );
        assert_eq!(
            ks.size_multiplier(),
            KillLevel::WidenSpreads.size_multiplier()
        );
    }

    /// `KillLevel` must satisfy `Ord` so the engine's effective
    /// level can be computed as `global.max(asset_class)`.
    /// Pin the comparison so a future re-ordering of the enum
    /// variants (which would silently break the max) fails the
    /// test loudly.
    #[test]
    fn kill_level_max_picks_higher_severity() {
        assert_eq!(
            KillLevel::Normal.max(KillLevel::WidenSpreads),
            KillLevel::WidenSpreads
        );
        assert_eq!(
            KillLevel::WidenSpreads.max(KillLevel::StopNewOrders),
            KillLevel::StopNewOrders
        );
        assert_eq!(
            KillLevel::StopNewOrders.max(KillLevel::CancelAll),
            KillLevel::CancelAll
        );
        assert_eq!(
            KillLevel::CancelAll.max(KillLevel::FlattenAll),
            KillLevel::FlattenAll
        );
    }

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

    // ----- Market Resilience trigger tests -----

    /// A single low-MR observation must not immediately
    /// escalate — the trigger requires a sustained dip.
    #[test]
    fn mr_dip_below_threshold_does_not_escalate_immediately() {
        let mut ks = KillSwitch::new(Default::default());
        let t0 = Utc::now();
        ks.update_market_resilience(dec!(0.1), t0);
        assert_eq!(ks.level(), KillLevel::Normal);
    }

    /// A sustained dip must escalate to `WidenSpreads` once the
    /// sustain window has elapsed.
    #[test]
    fn sustained_low_mr_escalates_to_widen_spreads() {
        let mut ks = KillSwitch::new(Default::default());
        let t0 = Utc::now();
        ks.update_market_resilience(dec!(0.1), t0);
        let t_after = t0 + chrono::Duration::seconds(MR_SUSTAIN_SECS + 1);
        ks.update_market_resilience(dec!(0.1), t_after);
        assert_eq!(ks.level(), KillLevel::WidenSpreads);
    }

    /// A recovery above the threshold resets the anchor, so a
    /// subsequent dip restarts the sustain window.
    #[test]
    fn mr_recovery_clears_the_anchor() {
        let mut ks = KillSwitch::new(Default::default());
        let t0 = Utc::now();
        ks.update_market_resilience(dec!(0.1), t0);
        // Recover.
        ks.update_market_resilience(dec!(0.9), t0 + chrono::Duration::seconds(1));
        // Dip again — anchor starts fresh, so MR_SUSTAIN_SECS
        // later the escalation must re-evaluate from zero.
        let t_dip_again = t0 + chrono::Duration::seconds(2);
        ks.update_market_resilience(dec!(0.1), t_dip_again);
        // Within the sustain window of the new anchor → no
        // escalation.
        ks.update_market_resilience(
            dec!(0.1),
            t_dip_again + chrono::Duration::seconds(MR_SUSTAIN_SECS - 1),
        );
        assert_eq!(ks.level(), KillLevel::Normal);
    }

    /// MR never escalates past `WidenSpreads` — hard escalation
    /// levels remain driven by PnL and position value only.
    #[test]
    fn mr_does_not_overwrite_higher_kill_level() {
        let mut ks = KillSwitch::new(KillSwitchConfig {
            daily_loss_limit: dec!(100),
            ..Default::default()
        });
        ks.update_pnl(dec!(-110));
        assert_eq!(ks.level(), KillLevel::CancelAll);
        let t0 = Utc::now();
        ks.update_market_resilience(dec!(0.1), t0);
        ks.update_market_resilience(
            dec!(0.1),
            t0 + chrono::Duration::seconds(MR_SUSTAIN_SECS + 1),
        );
        // The sustained-low-MR branch guards `current_level ==
        // Normal` so MR cannot re-escalate — and `escalate`
        // would ignore a lower level anyway.
        assert_eq!(ks.level(), KillLevel::CancelAll);
    }

    // ── Property-based tests (Epic 12) ───────────────────────

    use proptest::prelude::*;
    use proptest::sample::select;

    fn level_strat() -> impl Strategy<Value = KillLevel> {
        select(vec![
            KillLevel::Normal,
            KillLevel::WidenSpreads,
            KillLevel::StopNewOrders,
            KillLevel::CancelAll,
            KillLevel::FlattenAll,
            KillLevel::Disconnect,
        ])
    }

    prop_compose! {
        fn pnl_strat()(cents in -1_000_000i64..1_000_000i64) -> Decimal {
            Decimal::new(cents, 2)
        }
    }

    proptest! {
        /// Automatic escalation via `update_pnl` / `update_position_value`
        /// / `on_error` / `on_message_sent` only ever raises the
        /// level — never lowers it. This is the core safety
        /// invariant: a momentary recovery does not quietly roll
        /// back the kill switch. Only `reset` or `manual_trigger`
        /// can move it down.
        #[test]
        fn auto_escalation_is_monotonic(
            pnls in proptest::collection::vec(pnl_strat(), 0..30),
            errors in 0u32..100u32,
        ) {
            let mut ks = KillSwitch::new(KillSwitchConfig::default());
            let mut prev = ks.level();
            for p in pnls {
                ks.update_pnl(p);
                let now = ks.level();
                prop_assert!(now >= prev, "auto path lowered {:?} → {:?}", prev, now);
                prev = now;
            }
            for _ in 0..errors {
                ks.on_error();
                let now = ks.level();
                prop_assert!(now >= prev, "on_error lowered {:?} → {:?}", prev, now);
                prev = now;
            }
        }

        /// allow_new_orders is exactly equivalent to
        /// `level < StopNewOrders`. Any other mapping would let
        /// a CancelAll / FlattenAll escalation keep placing
        /// orders, which is the nightmare scenario the kill
        /// switch exists to prevent.
        #[test]
        fn allow_new_orders_matches_level_predicate(level in level_strat()) {
            let mut ks = KillSwitch::new(KillSwitchConfig::default());
            ks.manual_trigger(level, "proptest");
            prop_assert_eq!(ks.allow_new_orders(), level < KillLevel::StopNewOrders);
        }

        /// Manual reset from any level always returns to Normal.
        /// Operator ack path must be unconditional so a fat-
        /// finger cannot leave the switch stuck.
        #[test]
        fn reset_always_returns_to_normal(level in level_strat()) {
            let mut ks = KillSwitch::new(KillSwitchConfig::default());
            ks.manual_trigger(level, "proptest");
            ks.reset();
            prop_assert_eq!(ks.level(), KillLevel::Normal);
            prop_assert!(ks.allow_new_orders());
        }

        /// spread_multiplier and size_multiplier are non-negative
        /// and bounded. A negative multiplier would flip the
        /// strategy's spread direction inside itself — property
        /// catches a sign regression.
        #[test]
        fn multipliers_bounded(level in level_strat()) {
            let mut ks = KillSwitch::new(KillSwitchConfig::default());
            ks.manual_trigger(level, "proptest");
            let sm = ks.spread_multiplier();
            let szm = ks.size_multiplier();
            prop_assert!(sm >= dec!(0), "spread_mul {} < 0", sm);
            prop_assert!(sm <= dec!(100), "spread_mul {} > 100", sm);
            prop_assert!(szm >= dec!(0), "size_mul {} < 0", szm);
            prop_assert!(szm <= dec!(1), "size_mul {} > 1", szm);
        }

        /// `on_error` escalation is bounded: after N errors we
        /// reach at most StopNewOrders via the error path (the
        /// config's max_consecutive_errors threshold). Subsequent
        /// errors do NOT keep escalating past StopNewOrders — the
        /// error path is a one-shot trigger.
        #[test]
        fn error_path_caps_at_stop_new_orders(n_errors in 10u32..1000u32) {
            let mut ks = KillSwitch::new(KillSwitchConfig::default());
            for _ in 0..n_errors {
                ks.on_error();
            }
            // The threshold is 10 errors → StopNewOrders. More
            // errors never push past that level from this path
            // alone.
            prop_assert!(ks.level() >= KillLevel::StopNewOrders);
            prop_assert!(ks.level() <= KillLevel::StopNewOrders);
        }

        /// Fills reset the consecutive-error counter but never
        /// de-escalate the level. Catches a regression where
        /// on_fill was used to try to clear a previous error
        /// escalation.
        #[test]
        fn on_fill_never_de_escalates(
            n_errors in 0u32..30u32,
            n_fills in 1u32..30u32,
        ) {
            let mut ks = KillSwitch::new(KillSwitchConfig::default());
            for _ in 0..n_errors {
                ks.on_error();
            }
            let before = ks.level();
            for _ in 0..n_fills {
                ks.on_fill();
            }
            prop_assert_eq!(ks.level(), before);
        }
    }
}
