//! Protections stack — rate-limit-style guards that pause specific pairs
//! (or the whole desk) after bad things happen, without tripping the full
//! kill switch.
//!
//! Inspired by Freqtrade's protections framework but shaped for MM flow:
//! we never enter directional trades, so "stoploss" here means "hit the
//! MaxDrawdown per-pair threshold" or "cancel-on-kill-level-3 event" —
//! any event the caller chooses to count as a stop.
//!
//! Each guard is **pure sync** and is queried via `is_locked(pair, now)`
//! before the market_maker emits new orders. A locked pair simply
//! short-circuits the quote loop for that tick.
//!
//! Guards composed here:
//! - [`StoplossGuard`] — halt a pair after N stop events within a window.
//! - [`CooldownPeriod`] — mandatory pause after any stop event before
//!   the pair can re-quote.
//! - [`MaxDrawdownPause`] — equity-peak-to-trough based pause (per pair).
//! - [`LowProfitPairs`] — demote pairs whose rolling PnL under-performs.
//!
//! The [`Protections`] facade owns an instance of each guard and exposes
//! a single `is_locked(pair, now)` that returns the most restrictive
//! answer across all of them, plus a reason string for logging.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;

/// Configuration for the full protections stack. Each guard is
/// optional — a `None` disables that guard entirely.
#[derive(Debug, Clone, Default)]
pub struct ProtectionsConfig {
    pub stoploss_guard: Option<StoplossGuardConfig>,
    pub cooldown: Option<CooldownConfig>,
    pub max_drawdown: Option<MaxDrawdownConfig>,
    pub low_profit_pairs: Option<LowProfitPairsConfig>,
}

/// Short-circuit result of a protections check.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtectionStatus {
    Clear,
    Locked {
        reason: String,
        until: Option<Instant>,
    },
}

impl ProtectionStatus {
    pub fn is_locked(&self) -> bool {
        matches!(self, ProtectionStatus::Locked { .. })
    }
}

pub struct Protections {
    stoploss_guard: Option<StoplossGuard>,
    cooldown: Option<CooldownPeriod>,
    max_drawdown: Option<MaxDrawdownPause>,
    low_profit_pairs: Option<LowProfitPairs>,
}

impl Protections {
    pub fn new(config: ProtectionsConfig) -> Self {
        Self {
            stoploss_guard: config.stoploss_guard.map(StoplossGuard::new),
            cooldown: config.cooldown.map(CooldownPeriod::new),
            max_drawdown: config.max_drawdown.map(MaxDrawdownPause::new),
            low_profit_pairs: config.low_profit_pairs.map(LowProfitPairs::new),
        }
    }

    /// Record that the caller observed a "stop" event on `pair`. Feeds
    /// the StoplossGuard and CooldownPeriod counters.
    pub fn record_stop(&mut self, pair: &str, now: Instant) {
        if let Some(g) = &mut self.stoploss_guard {
            g.record_stop(pair, now);
        }
        if let Some(c) = &mut self.cooldown {
            c.trigger(pair, now);
        }
    }

    /// Record the current equity for the pair. Feeds MaxDrawdownPause.
    pub fn update_equity(&mut self, pair: &str, equity: Decimal, now: Instant) {
        if let Some(mdd) = &mut self.max_drawdown {
            mdd.update(pair, equity, now);
        }
    }

    /// Record realised PnL from a closed trade. Feeds LowProfitPairs.
    pub fn record_trade_pnl(&mut self, pair: &str, pnl: Decimal, now: Instant) {
        if let Some(lpp) = &mut self.low_profit_pairs {
            lpp.record_trade(pair, pnl, now);
        }
    }

    /// Check whether the pair is currently locked by any guard. Returns
    /// the first hit so the caller gets a stable reason string.
    pub fn is_locked(&self, pair: &str, now: Instant) -> ProtectionStatus {
        if let Some(g) = &self.stoploss_guard {
            if let Some(status) = g.status(pair, now) {
                return status;
            }
        }
        if let Some(c) = &self.cooldown {
            if let Some(status) = c.status(pair, now) {
                return status;
            }
        }
        if let Some(mdd) = &self.max_drawdown {
            if let Some(status) = mdd.status(pair, now) {
                return status;
            }
        }
        if let Some(lpp) = &self.low_profit_pairs {
            if let Some(status) = lpp.status(pair, now) {
                return status;
            }
        }
        ProtectionStatus::Clear
    }
}

// ============================================================================
// StoplossGuard
// ============================================================================

#[derive(Debug, Clone)]
pub struct StoplossGuardConfig {
    pub window: Duration,
    pub max_stops: usize,
    pub lockout: Duration,
}

pub struct StoplossGuard {
    config: StoplossGuardConfig,
    stops: HashMap<String, Vec<Instant>>,
    locked_until: HashMap<String, Instant>,
}

impl StoplossGuard {
    pub fn new(config: StoplossGuardConfig) -> Self {
        Self {
            config,
            stops: HashMap::new(),
            locked_until: HashMap::new(),
        }
    }

    pub fn record_stop(&mut self, pair: &str, now: Instant) {
        let window_start = now.checked_sub(self.config.window).unwrap_or(now);
        let entry = self.stops.entry(pair.to_string()).or_default();
        entry.retain(|t| *t >= window_start);
        entry.push(now);
        if entry.len() >= self.config.max_stops {
            self.locked_until
                .insert(pair.to_string(), now + self.config.lockout);
        }
    }

    pub fn status(&self, pair: &str, now: Instant) -> Option<ProtectionStatus> {
        if let Some(until) = self.locked_until.get(pair) {
            if now < *until {
                return Some(ProtectionStatus::Locked {
                    reason: format!(
                        "StoplossGuard: {} stops within {:?}",
                        self.config.max_stops, self.config.window
                    ),
                    until: Some(*until),
                });
            }
        }
        None
    }
}

// ============================================================================
// CooldownPeriod
// ============================================================================

#[derive(Debug, Clone)]
pub struct CooldownConfig {
    pub duration: Duration,
}

pub struct CooldownPeriod {
    config: CooldownConfig,
    cooldowns: HashMap<String, Instant>,
}

impl CooldownPeriod {
    pub fn new(config: CooldownConfig) -> Self {
        Self {
            config,
            cooldowns: HashMap::new(),
        }
    }

    pub fn trigger(&mut self, pair: &str, now: Instant) {
        self.cooldowns
            .insert(pair.to_string(), now + self.config.duration);
    }

    pub fn status(&self, pair: &str, now: Instant) -> Option<ProtectionStatus> {
        if let Some(until) = self.cooldowns.get(pair) {
            if now < *until {
                return Some(ProtectionStatus::Locked {
                    reason: format!("CooldownPeriod: active for {:?}", self.config.duration),
                    until: Some(*until),
                });
            }
        }
        None
    }
}

// ============================================================================
// MaxDrawdownPause (equity-peak mode)
// ============================================================================

#[derive(Debug, Clone)]
pub struct MaxDrawdownConfig {
    /// Drawdown in quote currency before the pair is paused.
    pub max_drawdown_quote: Decimal,
    /// How long to stay paused after the trigger.
    pub lockout: Duration,
    /// If equity recovers to this fraction of the peak during the
    /// lockout, clear the pause early. `1.0` means require full recovery.
    pub recovery_fraction: Decimal,
}

struct DrawdownState {
    peak: Decimal,
    last_equity: Decimal,
    locked_until: Option<Instant>,
}

pub struct MaxDrawdownPause {
    config: MaxDrawdownConfig,
    per_pair: HashMap<String, DrawdownState>,
}

impl MaxDrawdownPause {
    pub fn new(config: MaxDrawdownConfig) -> Self {
        Self {
            config,
            per_pair: HashMap::new(),
        }
    }

    pub fn update(&mut self, pair: &str, equity: Decimal, now: Instant) {
        let state = self
            .per_pair
            .entry(pair.to_string())
            .or_insert_with(|| DrawdownState {
                peak: equity,
                last_equity: equity,
                locked_until: None,
            });
        state.last_equity = equity;
        if equity > state.peak {
            state.peak = equity;
        }
        let drawdown = state.peak - equity;
        if drawdown >= self.config.max_drawdown_quote && state.locked_until.is_none() {
            state.locked_until = Some(now + self.config.lockout);
        }
        // Early recovery: equity back above peak × recovery_fraction.
        if let Some(until) = state.locked_until {
            if now < until && equity >= state.peak * self.config.recovery_fraction {
                state.locked_until = None;
            }
        }
    }

    pub fn status(&self, pair: &str, now: Instant) -> Option<ProtectionStatus> {
        let state = self.per_pair.get(pair)?;
        if let Some(until) = state.locked_until {
            if now < until {
                return Some(ProtectionStatus::Locked {
                    reason: format!(
                        "MaxDrawdownPause: dd >= {} (peak {}, equity {})",
                        self.config.max_drawdown_quote, state.peak, state.last_equity
                    ),
                    until: Some(until),
                });
            }
        }
        None
    }
}

// ============================================================================
// LowProfitPairs
// ============================================================================

#[derive(Debug, Clone)]
pub struct LowProfitPairsConfig {
    /// Rolling window over which we compute PnL.
    pub window: Duration,
    /// Minimum rolling PnL in quote currency to stay active. Below this,
    /// the pair is demoted for `lockout`.
    pub min_pnl_quote: Decimal,
    pub lockout: Duration,
    /// Require at least this many trades in the window before judging.
    pub min_trades: usize,
}

pub struct LowProfitPairs {
    config: LowProfitPairsConfig,
    trades: HashMap<String, Vec<(Instant, Decimal)>>,
    locked_until: HashMap<String, Instant>,
}

impl LowProfitPairs {
    pub fn new(config: LowProfitPairsConfig) -> Self {
        Self {
            config,
            trades: HashMap::new(),
            locked_until: HashMap::new(),
        }
    }

    pub fn record_trade(&mut self, pair: &str, pnl: Decimal, now: Instant) {
        let window_start = now.checked_sub(self.config.window).unwrap_or(now);
        let entry = self.trades.entry(pair.to_string()).or_default();
        entry.retain(|(t, _)| *t >= window_start);
        entry.push((now, pnl));
        if entry.len() >= self.config.min_trades {
            let sum: Decimal = entry.iter().map(|(_, p)| *p).sum();
            if sum < self.config.min_pnl_quote {
                self.locked_until
                    .insert(pair.to_string(), now + self.config.lockout);
            }
        }
    }

    pub fn status(&self, pair: &str, now: Instant) -> Option<ProtectionStatus> {
        if let Some(until) = self.locked_until.get(pair) {
            if now < *until {
                return Some(ProtectionStatus::Locked {
                    reason: format!(
                        "LowProfitPairs: rolling PnL < {} over {:?}",
                        self.config.min_pnl_quote, self.config.window
                    ),
                    until: Some(*until),
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn cfg_all() -> ProtectionsConfig {
        ProtectionsConfig {
            stoploss_guard: Some(StoplossGuardConfig {
                window: Duration::from_secs(60),
                max_stops: 3,
                lockout: Duration::from_secs(300),
            }),
            cooldown: Some(CooldownConfig {
                duration: Duration::from_secs(30),
            }),
            max_drawdown: Some(MaxDrawdownConfig {
                max_drawdown_quote: dec!(100),
                lockout: Duration::from_secs(600),
                recovery_fraction: dec!(1),
            }),
            low_profit_pairs: Some(LowProfitPairsConfig {
                window: Duration::from_secs(3600),
                min_pnl_quote: dec!(0),
                lockout: Duration::from_secs(1800),
                min_trades: 5,
            }),
        }
    }

    #[test]
    fn clear_when_no_events() {
        let p = Protections::new(cfg_all());
        assert!(!p.is_locked("BTCUSDT", Instant::now()).is_locked());
    }

    #[test]
    fn stoploss_guard_triggers_after_max_stops() {
        let mut p = Protections::new(cfg_all());
        let t0 = Instant::now();
        for i in 0..3 {
            p.record_stop("BTCUSDT", t0 + Duration::from_secs(i * 5));
        }
        // Cooldown still active from the latest stop, but stoploss
        // lockout is the dominant one (longer).
        let t = t0 + Duration::from_secs(45);
        assert!(p.is_locked("BTCUSDT", t).is_locked());
    }

    #[test]
    fn stoploss_guard_window_drops_old_events() {
        let mut cfg = cfg_all();
        cfg.cooldown = None; // isolate stoploss behaviour
        cfg.max_drawdown = None;
        cfg.low_profit_pairs = None;
        let mut p = Protections::new(cfg);
        let t0 = Instant::now();
        p.record_stop("BTCUSDT", t0);
        p.record_stop("BTCUSDT", t0 + Duration::from_secs(10));
        // A third stop 2 minutes later — first two fall out of the
        // 60s window, so the guard does NOT trigger.
        p.record_stop("BTCUSDT", t0 + Duration::from_secs(120));
        assert!(!p
            .is_locked("BTCUSDT", t0 + Duration::from_secs(121))
            .is_locked());
    }

    #[test]
    fn cooldown_is_per_pair() {
        let mut p = Protections::new(cfg_all());
        let t0 = Instant::now();
        p.record_stop("BTCUSDT", t0);
        assert!(p
            .is_locked("BTCUSDT", t0 + Duration::from_secs(5))
            .is_locked());
        assert!(!p
            .is_locked("ETHUSDT", t0 + Duration::from_secs(5))
            .is_locked());
    }

    #[test]
    fn cooldown_expires() {
        let mut p = Protections::new(cfg_all());
        let t0 = Instant::now();
        p.record_stop("BTCUSDT", t0);
        assert!(p
            .is_locked("BTCUSDT", t0 + Duration::from_secs(5))
            .is_locked());
        // After 30s cooldown + a margin, clear.
        let t_later = t0 + Duration::from_secs(31);
        // stoploss guard only triggers at 3 stops, so we have just 1 — cooldown should be the only active lock.
        assert!(!p.is_locked("BTCUSDT", t_later).is_locked());
    }

    #[test]
    fn max_drawdown_triggers_on_peak_to_trough_breach() {
        let mut p = Protections::new(cfg_all());
        let t0 = Instant::now();
        p.update_equity("BTCUSDT", dec!(1000), t0);
        p.update_equity("BTCUSDT", dec!(1100), t0 + Duration::from_secs(1));
        // Drawdown from peak 1100: -150 > -100 limit.
        p.update_equity("BTCUSDT", dec!(950), t0 + Duration::from_secs(2));
        assert!(p
            .is_locked("BTCUSDT", t0 + Duration::from_secs(3))
            .is_locked());
    }

    #[test]
    fn max_drawdown_recovers_early_if_back_to_peak() {
        let mut cfg = cfg_all();
        if let Some(ref mut mdd) = cfg.max_drawdown {
            mdd.recovery_fraction = dec!(1);
        }
        let mut p = Protections::new(cfg);
        let t0 = Instant::now();
        p.update_equity("BTCUSDT", dec!(1000), t0);
        p.update_equity("BTCUSDT", dec!(1100), t0 + Duration::from_secs(1));
        p.update_equity("BTCUSDT", dec!(950), t0 + Duration::from_secs(2)); // locked
        assert!(p
            .is_locked("BTCUSDT", t0 + Duration::from_secs(3))
            .is_locked());
        // Recovery back above peak during lockout window.
        p.update_equity("BTCUSDT", dec!(1100), t0 + Duration::from_secs(10));
        assert!(!p
            .is_locked("BTCUSDT", t0 + Duration::from_secs(11))
            .is_locked());
    }

    #[test]
    fn low_profit_pairs_demotes_after_enough_trades() {
        let mut p = Protections::new(cfg_all());
        let t0 = Instant::now();
        // Need at least 5 trades with negative rolling sum.
        for i in 0..5 {
            p.record_trade_pnl("BTCUSDT", dec!(-10), t0 + Duration::from_secs(i * 60));
        }
        assert!(p
            .is_locked("BTCUSDT", t0 + Duration::from_secs(301))
            .is_locked());
    }

    #[test]
    fn low_profit_pairs_ignores_until_min_trades_met() {
        let mut p = Protections::new(cfg_all());
        let t0 = Instant::now();
        // 4 trades, all losing — below min_trades=5, so no lock.
        for i in 0..4 {
            p.record_trade_pnl("BTCUSDT", dec!(-10), t0 + Duration::from_secs(i * 60));
        }
        assert!(!p
            .is_locked("BTCUSDT", t0 + Duration::from_secs(241))
            .is_locked());
    }

    #[test]
    fn no_guards_configured_is_always_clear() {
        let p = Protections::new(ProtectionsConfig::default());
        assert!(!p.is_locked("BTCUSDT", Instant::now()).is_locked());
    }

    // ── Property-based tests (Epic 19) ────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// CooldownPeriod::trigger sets a lockout that expires after
        /// exactly `duration`. Status is Locked for any query strictly
        /// before expiry and None afterwards. Catches an off-by-one in
        /// the `now < until` comparison.
        #[test]
        fn cooldown_locks_until_duration_elapses(
            duration_secs in 1u64..600,
            probe_secs in 0u64..1200,
        ) {
            let mut c = CooldownPeriod::new(CooldownConfig {
                duration: Duration::from_secs(duration_secs),
            });
            let t0 = Instant::now();
            c.trigger("P", t0);
            let probe = t0 + Duration::from_secs(probe_secs);
            let status = c.status("P", probe);
            if probe_secs < duration_secs {
                prop_assert!(status.is_some());
                prop_assert!(status.unwrap().is_locked());
            } else {
                prop_assert!(status.is_none());
            }
        }

        /// CooldownPeriod is per-pair: triggering one pair does not
        /// affect the other's status.
        #[test]
        fn cooldown_is_isolated_across_pairs(
            duration_secs in 1u64..600,
            probe_secs in 0u64..300,
        ) {
            let mut c = CooldownPeriod::new(CooldownConfig {
                duration: Duration::from_secs(duration_secs),
            });
            let t0 = Instant::now();
            c.trigger("A", t0);
            let probe = t0 + Duration::from_secs(probe_secs);
            prop_assert!(c.status("B", probe).is_none());
        }

        /// MaxDrawdownPause: peak equity is monotonic — update_equity
        /// never lowers it regardless of the incoming value.
        #[test]
        fn max_drawdown_peak_is_monotonic(
            equities in proptest::collection::vec(-1_000_000i64..1_000_000, 1..30),
        ) {
            let cfg = MaxDrawdownConfig {
                max_drawdown_quote: dec!(1_000_000_000),
                lockout: Duration::from_secs(60),
                recovery_fraction: dec!(1),
            };
            let mut mdd = MaxDrawdownPause::new(cfg);
            let t0 = Instant::now();
            let mut prev_peak = Decimal::from(equities[0]);
            for (i, eq) in equities.iter().enumerate() {
                let eq_dec = Decimal::from(*eq);
                mdd.update("P", eq_dec, t0 + Duration::from_secs(i as u64));
                let peak = mdd.per_pair.get("P").unwrap().peak;
                prop_assert!(peak >= prev_peak,
                    "peak regressed {} → {}", prev_peak, peak);
                prev_peak = peak;
            }
        }

        /// MaxDrawdownPause triggers once drawdown hits the threshold.
        /// The first update after peak sets the peak; a subsequent
        /// drop of exactly `max_drawdown_quote` or more must lock.
        #[test]
        fn max_drawdown_triggers_at_threshold(
            peak_val in 100_000i64..1_000_000,
            dd_above_threshold in 0i64..500_000,
        ) {
            let threshold = dec!(10_000);
            let cfg = MaxDrawdownConfig {
                max_drawdown_quote: threshold,
                lockout: Duration::from_secs(3600),
                recovery_fraction: dec!(99), // disable early recovery
            };
            let mut mdd = MaxDrawdownPause::new(cfg);
            let t0 = Instant::now();
            let peak = Decimal::from(peak_val);
            mdd.update("P", peak, t0);
            // Drop by threshold + extra. Must lock.
            let drop_amount = threshold + Decimal::from(dd_above_threshold);
            let new_eq = peak - drop_amount;
            mdd.update("P", new_eq, t0 + Duration::from_secs(1));
            let status = mdd.status("P", t0 + Duration::from_secs(2));
            prop_assert!(status.is_some());
            prop_assert!(status.unwrap().is_locked());
        }

        /// MaxDrawdownPause does NOT trigger while drawdown stays
        /// strictly below the threshold.
        #[test]
        fn max_drawdown_silent_below_threshold(
            peak_val in 100_000i64..1_000_000,
            dd_below in 0i64..9_999,
        ) {
            let threshold = dec!(10_000);
            let cfg = MaxDrawdownConfig {
                max_drawdown_quote: threshold,
                lockout: Duration::from_secs(3600),
                recovery_fraction: dec!(1),
            };
            let mut mdd = MaxDrawdownPause::new(cfg);
            let t0 = Instant::now();
            let peak = Decimal::from(peak_val);
            mdd.update("P", peak, t0);
            // Drop strictly less than threshold.
            let new_eq = peak - Decimal::from(dd_below);
            mdd.update("P", new_eq, t0 + Duration::from_secs(1));
            prop_assert!(mdd.status("P", t0 + Duration::from_secs(2)).is_none());
        }

        /// StoplossGuard: lockout activates iff `max_stops` events
        /// land inside the rolling window. Fewer = no lock; enough
        /// inside = lock.
        #[test]
        fn stoploss_guard_triggers_exactly_on_max_stops(
            max_stops in 2usize..6,
            extra_stops in 0usize..5,
        ) {
            let cfg = StoplossGuardConfig {
                window: Duration::from_secs(600),
                max_stops,
                lockout: Duration::from_secs(300),
            };
            let mut g = StoplossGuard::new(cfg);
            let t0 = Instant::now();
            // Record exactly (max_stops - 1) — must NOT lock.
            for i in 0..(max_stops - 1) {
                g.record_stop("P", t0 + Duration::from_secs(i as u64));
            }
            prop_assert!(g
                .status("P", t0 + Duration::from_secs(max_stops as u64))
                .is_none());
            // Push to at least max_stops — must lock.
            let total = max_stops + extra_stops;
            for i in (max_stops - 1)..total {
                g.record_stop("P", t0 + Duration::from_secs(i as u64));
            }
            let status = g.status("P", t0 + Duration::from_secs(total as u64 + 1));
            prop_assert!(status.is_some());
            prop_assert!(status.unwrap().is_locked());
        }

        /// ProtectionStatus::is_locked() agrees with the enum
        /// discriminant — Clear is never locked; Locked is always
        /// locked. Invariant guards against a refactor that adds a
        /// third variant without updating is_locked().
        #[test]
        fn protection_status_is_locked_matches_variant(
            locked in any::<bool>(),
            secs_ahead in 1u64..3600,
        ) {
            let s = if locked {
                ProtectionStatus::Locked {
                    reason: "x".into(),
                    until: Some(Instant::now() + Duration::from_secs(secs_ahead)),
                }
            } else {
                ProtectionStatus::Clear
            };
            prop_assert_eq!(s.is_locked(), locked);
        }
    }
}
