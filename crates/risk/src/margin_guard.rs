//! Pre-liquidation margin-ratio guard (Epic 40.4).
//!
//! Sits between the venue's margin endpoint and the kill switch.
//! Its only job is turning a stream of `AccountMarginInfo`
//! snapshots — plus the *projected* ratio for a pending quote —
//! into a [`KillLevel`] escalation via three configurable
//! thresholds (`widen / stop / cancel`).
//!
//! Two inputs drive the guard:
//!
//! 1. **Observed ratio** (`update`). The engine polls
//!    `connector.account_margin_info()` on the cadence set by
//!    `MarginConfig::refresh_interval_secs` and feeds the
//!    venue-reported snapshot here. If the snapshot's
//!    `reported_at_ms` is older than `max_stale_secs`, the
//!    guard treats the next `level()` call as a stale read
//!    and returns `WidenSpreads` regardless of ratio — a dark
//!    venue feed is itself a risk event.
//!
//! 2. **Projected ratio** (`projected_ratio`). The engine
//!    calls this ahead of `order_manager.place_order` with the
//!    notional delta the quote would add to the account. If
//!    the *post-fill* ratio would cross `stop_ratio`, the
//!    quote is skipped even though the *current* ratio is
//!    below the line. Prevents the "quote was OK, fill
//!    crossed the line" race that a polled-only guard cannot
//!    catch.
//!
//! The guard itself is pure state — it does not touch the
//! kill switch. The engine reads `level()` and calls
//! `kill_switch.update_margin_ratio(...)` so the monotonic
//! escalation semantics stay in one place.

use mm_common::config::MarginConfig;
use mm_exchange_core::connector::AccountMarginInfo;
use rust_decimal::Decimal;

/// Snapshot decision surfaced to the engine each tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarginGuardDecision {
    /// Under `widen_ratio` — no action.
    Normal,
    /// `widen_ratio ≤ ratio < stop_ratio` — spread multiplier up.
    WidenSpreads,
    /// `stop_ratio ≤ ratio < cancel_ratio` — no new orders.
    StopNewOrders,
    /// `ratio ≥ cancel_ratio` — cancel everything and stop.
    CancelAll,
    /// Snapshot older than `max_stale_secs` (or never received).
    /// Engine widens conservatively while the feed recovers.
    Stale,
}

impl MarginGuardDecision {
    /// The `KillLevel`-shape bucket this decision maps to.
    /// `Normal` returns `None` so the engine's combined
    /// escalation logic can ignore it without allocating a
    /// no-op hop through the kill switch.
    pub fn kill_level(self) -> Option<crate::kill_switch::KillLevel> {
        use crate::kill_switch::KillLevel;
        match self {
            MarginGuardDecision::Normal => None,
            MarginGuardDecision::WidenSpreads | MarginGuardDecision::Stale => {
                Some(KillLevel::WidenSpreads)
            }
            MarginGuardDecision::StopNewOrders => Some(KillLevel::StopNewOrders),
            MarginGuardDecision::CancelAll => Some(KillLevel::CancelAll),
        }
    }

    pub fn is_stale(self) -> bool {
        matches!(self, MarginGuardDecision::Stale)
    }
}

/// Thresholds + staleness budget owned by the guard. Cloned
/// out of the engine's [`MarginConfig`] at construction; the
/// guard does not re-read config between ticks.
#[derive(Debug, Clone)]
pub struct MarginGuardThresholds {
    pub widen_ratio: Decimal,
    pub stop_ratio: Decimal,
    pub cancel_ratio: Decimal,
    pub max_stale_secs: i64,
}

impl MarginGuardThresholds {
    pub fn from_config(cfg: &MarginConfig) -> Self {
        Self {
            widen_ratio: cfg.widen_ratio,
            stop_ratio: cfg.stop_ratio,
            cancel_ratio: cfg.cancel_ratio,
            max_stale_secs: cfg.max_stale_secs as i64,
        }
    }
}

/// The guard itself. One per engine — spot engines don't
/// construct it, so absence encodes "this venue has no margin
/// concept".
#[derive(Debug, Clone)]
pub struct MarginGuard {
    thresholds: MarginGuardThresholds,
    /// Last observed snapshot, or `None` before the first poll.
    last: Option<AccountMarginInfo>,
}

impl MarginGuard {
    pub fn new(thresholds: MarginGuardThresholds) -> Self {
        Self {
            thresholds,
            last: None,
        }
    }

    pub fn thresholds(&self) -> &MarginGuardThresholds {
        &self.thresholds
    }

    pub fn last(&self) -> Option<&AccountMarginInfo> {
        self.last.as_ref()
    }

    /// Ingest a fresh snapshot. `reported_at_ms` on the info
    /// drives the staleness check on subsequent `decide(now)`
    /// calls.
    pub fn update(&mut self, info: AccountMarginInfo) {
        self.last = Some(info);
    }

    /// What the guard would say *now*, given the last ingested
    /// snapshot and the wall-clock `now_ms`.
    pub fn decide(&self, now_ms: i64) -> MarginGuardDecision {
        match &self.last {
            None => MarginGuardDecision::Stale,
            Some(info) => {
                let age_secs = (now_ms - info.reported_at_ms) / 1000;
                if age_secs > self.thresholds.max_stale_secs {
                    return MarginGuardDecision::Stale;
                }
                Self::bucket(info.margin_ratio, &self.thresholds)
            }
        }
    }

    /// Forecast the post-fill ratio if the engine adds
    /// `notional_delta` (quote-asset) of new exposure. Lets the
    /// pre-order hook short-circuit a quote whose fill would
    /// cross `stop_ratio` even though the current snapshot is
    /// comfortably below it.
    ///
    /// Approximation: holds total maintenance margin flat (fill
    /// has not happened yet, so the venue hasn't booked any MM
    /// for it) and reduces `total_equity` by the IM component
    /// `notional_delta / leverage_est`. Since we don't always
    /// know per-position leverage here, callers pass in the
    /// effective leverage for the symbol. A conservative caller
    /// passes `leverage = 1` (treat quote notional as raw IM),
    /// which upper-bounds the projected ratio.
    ///
    /// Returns the projected ratio ∈ `[0, +∞)`. The engine
    /// compares it to `stop_ratio` itself (so the same code
    /// handles both observed + projected escalation through
    /// one path).
    pub fn projected_ratio(
        &self,
        notional_delta: Decimal,
        leverage: u32,
    ) -> Option<Decimal> {
        let info = self.last.as_ref()?;
        if info.total_equity <= Decimal::ZERO {
            // Zero/negative equity already means we're at 1.0+ —
            // the guard would already have hit `CancelAll` via
            // `decide`. Return the saturating value.
            return Some(Decimal::ONE);
        }
        let lev = Decimal::from(leverage.max(1));
        let im_needed = notional_delta / lev;
        let projected_equity = info.total_equity - im_needed;
        if projected_equity <= Decimal::ZERO {
            return Some(Decimal::ONE);
        }
        // Conservative: MM only grows (not shrinks) with added
        // exposure; treat new IM as 1:1 MM for the projection so
        // the guard errs toward refusing the quote.
        let projected_mm = info.total_maintenance_margin + im_needed;
        Some(projected_mm / projected_equity)
    }

    /// Same bucket function used for both observed + projected
    /// ratios — single source of truth for threshold mapping.
    pub fn bucket(ratio: Decimal, t: &MarginGuardThresholds) -> MarginGuardDecision {
        if ratio >= t.cancel_ratio {
            MarginGuardDecision::CancelAll
        } else if ratio >= t.stop_ratio {
            MarginGuardDecision::StopNewOrders
        } else if ratio >= t.widen_ratio {
            MarginGuardDecision::WidenSpreads
        } else {
            MarginGuardDecision::Normal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::types::Side;
    use rust_decimal_macros::dec;

    fn thresholds() -> MarginGuardThresholds {
        MarginGuardThresholds {
            widen_ratio: dec!(0.5),
            stop_ratio: dec!(0.8),
            cancel_ratio: dec!(0.9),
            max_stale_secs: 30,
        }
    }

    fn snapshot(ratio: Decimal, age_secs: i64, now_ms: i64) -> AccountMarginInfo {
        AccountMarginInfo {
            total_equity: dec!(10_000),
            total_initial_margin: dec!(2_000),
            total_maintenance_margin: ratio * dec!(10_000),
            available_balance: dec!(8_000),
            margin_ratio: ratio,
            positions: vec![],
            reported_at_ms: now_ms - age_secs * 1000,
        }
    }

    #[test]
    fn empty_guard_is_stale() {
        let g = MarginGuard::new(thresholds());
        assert_eq!(g.decide(0), MarginGuardDecision::Stale);
    }

    #[test]
    fn bucket_transitions() {
        let t = thresholds();
        assert_eq!(
            MarginGuard::bucket(dec!(0.0), &t),
            MarginGuardDecision::Normal
        );
        assert_eq!(
            MarginGuard::bucket(dec!(0.49), &t),
            MarginGuardDecision::Normal
        );
        assert_eq!(
            MarginGuard::bucket(dec!(0.5), &t),
            MarginGuardDecision::WidenSpreads
        );
        assert_eq!(
            MarginGuard::bucket(dec!(0.79), &t),
            MarginGuardDecision::WidenSpreads
        );
        assert_eq!(
            MarginGuard::bucket(dec!(0.8), &t),
            MarginGuardDecision::StopNewOrders
        );
        assert_eq!(
            MarginGuard::bucket(dec!(0.89), &t),
            MarginGuardDecision::StopNewOrders
        );
        assert_eq!(
            MarginGuard::bucket(dec!(0.9), &t),
            MarginGuardDecision::CancelAll
        );
        assert_eq!(
            MarginGuard::bucket(dec!(1.5), &t),
            MarginGuardDecision::CancelAll
        );
    }

    #[test]
    fn stale_snapshot_escalates_to_stale() {
        let mut g = MarginGuard::new(thresholds());
        let now = 1_700_000_000_000;
        g.update(snapshot(dec!(0.1), 100, now));
        assert_eq!(g.decide(now), MarginGuardDecision::Stale);
    }

    #[test]
    fn fresh_snapshot_passes_through_bucket() {
        let mut g = MarginGuard::new(thresholds());
        let now = 1_700_000_000_000;
        g.update(snapshot(dec!(0.1), 0, now));
        assert_eq!(g.decide(now), MarginGuardDecision::Normal);
        g.update(snapshot(dec!(0.55), 0, now));
        assert_eq!(g.decide(now), MarginGuardDecision::WidenSpreads);
        g.update(snapshot(dec!(0.82), 0, now));
        assert_eq!(g.decide(now), MarginGuardDecision::StopNewOrders);
        g.update(snapshot(dec!(0.95), 0, now));
        assert_eq!(g.decide(now), MarginGuardDecision::CancelAll);
    }

    #[test]
    fn decision_maps_to_kill_level() {
        use crate::kill_switch::KillLevel;
        assert_eq!(MarginGuardDecision::Normal.kill_level(), None);
        assert_eq!(
            MarginGuardDecision::WidenSpreads.kill_level(),
            Some(KillLevel::WidenSpreads)
        );
        assert_eq!(
            MarginGuardDecision::Stale.kill_level(),
            Some(KillLevel::WidenSpreads)
        );
        assert_eq!(
            MarginGuardDecision::StopNewOrders.kill_level(),
            Some(KillLevel::StopNewOrders)
        );
        assert_eq!(
            MarginGuardDecision::CancelAll.kill_level(),
            Some(KillLevel::CancelAll)
        );
    }

    #[test]
    fn projected_ratio_monotonic_in_notional() {
        let mut g = MarginGuard::new(thresholds());
        let now = 1_700_000_000_000;
        g.update(snapshot(dec!(0.4), 0, now));
        let r0 = g.projected_ratio(dec!(0), 5).unwrap();
        let r1 = g.projected_ratio(dec!(1_000), 5).unwrap();
        let r2 = g.projected_ratio(dec!(5_000), 5).unwrap();
        assert!(r0 <= r1, "r0={r0} r1={r1}");
        assert!(r1 <= r2, "r1={r1} r2={r2}");
    }

    #[test]
    fn projected_ratio_no_snapshot_returns_none() {
        let g = MarginGuard::new(thresholds());
        assert!(g.projected_ratio(dec!(100), 5).is_none());
    }

    #[test]
    fn projected_ratio_zero_equity_saturates_to_one() {
        let mut g = MarginGuard::new(thresholds());
        let now = 1_700_000_000_000;
        let mut s = snapshot(dec!(0.9), 0, now);
        s.total_equity = dec!(0);
        g.update(s);
        let r = g.projected_ratio(dec!(100), 5).unwrap();
        assert_eq!(r, Decimal::ONE);
    }

    #[test]
    fn projected_ratio_crosses_stop_when_notional_large() {
        let mut g = MarginGuard::new(thresholds());
        let now = 1_700_000_000_000;
        // equity 10k, MM 7k -> ratio 0.7 (widen but not stop).
        // add a big quote at leverage 1 -> IM = notional, both
        // reduces equity and raises MM, trivially pushes over.
        g.update(snapshot(dec!(0.7), 0, now));
        let r_small = g.projected_ratio(dec!(100), 1).unwrap();
        let r_big = g.projected_ratio(dec!(2_000), 1).unwrap();
        assert!(r_small < thresholds().stop_ratio, "r_small={r_small}");
        assert!(r_big >= thresholds().stop_ratio, "r_big={r_big}");
    }

    // Silence unused_imports on `Side` — kept for parity with
    // other risk-crate test modules.
    #[test]
    fn _keep_side_import() {
        let _ = Side::Buy;
    }
}
