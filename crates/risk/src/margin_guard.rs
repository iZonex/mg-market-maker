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

use mm_common::config::{MarginConfig, MarginModeCfg};
use mm_exchange_core::connector::AccountMarginInfo;
use rust_decimal::Decimal;

/// Snapshot decision surfaced to the engine each tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarginGuardDecision {
    /// Under `widen_ratio` — no action.
    Normal,
    /// `widen_ratio ≤ ratio < reduce_ratio` — spread multiplier up.
    WidenSpreads,
    /// PERP-1: `reduce_ratio ≤ ratio < stop_ratio` — engine
    /// issues a reduce-only IoC slice to proactively lower
    /// position BEFORE the stop / cancel thresholds fire. New
    /// orders still flow (this is gentler than
    /// `StopNewOrders`), but the engine's unwind loop runs in
    /// parallel.
    Reduce,
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
            // Reduce doesn't escalate the kill switch — quoting
            // stays live, the engine just additionally unwinds.
            // Sidecar action, not escalation.
            MarginGuardDecision::Reduce => Some(KillLevel::WidenSpreads),
            MarginGuardDecision::StopNewOrders => Some(KillLevel::StopNewOrders),
            MarginGuardDecision::CancelAll => Some(KillLevel::CancelAll),
        }
    }

    /// PERP-1 — true when the guard wants a proactive reduce
    /// slice this tick.
    pub fn wants_reduce(self) -> bool {
        matches!(self, MarginGuardDecision::Reduce)
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
    /// PERP-1 — `reduce_ratio ≤ ratio < stop_ratio` triggers
    /// a proactive reduce slice. Defaults to the midpoint of
    /// `(widen_ratio, stop_ratio)` if not configured.
    pub reduce_ratio: Decimal,
    pub stop_ratio: Decimal,
    pub cancel_ratio: Decimal,
    pub max_stale_secs: i64,
    /// PERP-2 — fallback MM-as-fraction-of-notional used when
    /// the venue snapshot has no open position to derive an
    /// effective MMR from. Mirrors
    /// `MarginConfig::default_maintenance_margin_rate`.
    pub default_mmr: Decimal,
}

impl MarginGuardThresholds {
    pub fn from_config(cfg: &MarginConfig) -> Self {
        let midpoint = (cfg.widen_ratio + cfg.stop_ratio) / Decimal::from(2);
        Self {
            widen_ratio: cfg.widen_ratio,
            reduce_ratio: cfg.reduce_ratio.unwrap_or(midpoint),
            stop_ratio: cfg.stop_ratio,
            cancel_ratio: cfg.cancel_ratio,
            max_stale_secs: cfg.max_stale_secs as i64,
            default_mmr: cfg.default_maintenance_margin_rate,
        }
    }
}

/// The guard itself. One per engine — spot engines don't
/// construct it, so absence encodes "this venue has no margin
/// concept".
#[derive(Debug, Clone)]
pub struct MarginGuard {
    thresholds: MarginGuardThresholds,
    /// PERP-3 — symbol the engine instance is quoting. Isolated
    /// mode ratios are computed off *this* symbol's position
    /// only; cross mode ignores it and uses the wallet-wide
    /// figure.
    symbol: String,
    /// PERP-3 — venue-configured margin mode (isolated vs
    /// cross). Decides whether the observed + projected ratios
    /// look at per-position collateral or the whole wallet.
    mode: MarginModeCfg,
    /// Last observed snapshot, or `None` before the first poll.
    last: Option<AccountMarginInfo>,
}

impl MarginGuard {
    pub fn new(thresholds: MarginGuardThresholds) -> Self {
        Self {
            thresholds,
            symbol: String::new(),
            mode: MarginModeCfg::Cross,
            last: None,
        }
    }

    /// PERP-3 — pin this guard to a specific symbol + margin
    /// mode. Isolated-mode guards compute the observed and
    /// projected ratio off `(position_mm, isolated_margin)`
    /// for `symbol`; cross-mode guards use the venue's
    /// wallet-wide figure. Callers typically invoke this
    /// immediately after `new()` with values resolved from
    /// `MarginConfig::for_symbol(symbol)`.
    pub fn with_symbol_mode(mut self, symbol: impl Into<String>, mode: MarginModeCfg) -> Self {
        self.symbol = symbol.into();
        self.mode = mode;
        self
    }

    pub fn mode(&self) -> MarginModeCfg {
        self.mode
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

    /// PERP-3 — the observed ratio this guard actually reasons
    /// about. Cross-mode: the venue-reported wallet-wide
    /// `margin_ratio` as before. Isolated-mode: the
    /// per-position ratio for `self.symbol`, computed as
    /// `(size × mark × effective_mmr) / isolated_margin`.
    /// Falls back to the cross-mode ratio when the position is
    /// missing or `isolated_margin` is `None` (pre-anchor
    /// state, or the venue runs a cross-funded isolated bucket).
    pub fn observed_ratio(&self) -> Option<Decimal> {
        let info = self.last.as_ref()?;
        if matches!(self.mode, MarginModeCfg::Cross) {
            return Some(info.margin_ratio);
        }
        // Isolated path — find this engine's position.
        let Some(pos) = info.positions.iter().find(|p| p.symbol == self.symbol) else {
            return Some(info.margin_ratio);
        };
        let Some(iso) = pos.isolated_margin else {
            return Some(info.margin_ratio);
        };
        if iso <= Decimal::ZERO {
            return Some(Decimal::ONE);
        }
        let position_notional = pos.size.abs() * pos.mark_price;
        let position_mm = position_notional * self.effective_mmr();
        Some(position_mm / iso)
    }

    /// PERP-4 — highest ADL quantile on any of our open
    /// positions. `0`–`4` on venues that publish it (Binance
    /// `adlQuantile`, Bybit `adlRankIndicator`); `None` when
    /// either no snapshot has arrived yet or no position
    /// reports one (HyperLiquid for instance). Used to widen
    /// spreads when we're close to the front of the venue's
    /// auto-deleverage queue.
    pub fn max_adl_quantile(&self) -> Option<u8> {
        self.last
            .as_ref()?
            .positions
            .iter()
            .filter_map(|p| p.adl_quantile)
            .max()
    }

    /// PERP-4 — `true` when any of our positions has an
    /// elevated ADL rank (≥ 3, i.e. top-40% of the venue's
    /// deleverage queue). The guard forces a `WidenSpreads`
    /// decision while this is set — a venue-triggered ADL
    /// would close the position at mark, so tightening
    /// spreads in that state is the wrong bet.
    pub fn adl_elevated(&self) -> bool {
        self.max_adl_quantile().is_some_and(|q| q >= 3)
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
                let ratio = self.observed_ratio().unwrap_or(info.margin_ratio);
                let bucket = Self::bucket(ratio, &self.thresholds);
                // PERP-4 — ADL-rank override. Never DEMOTES a
                // higher-severity decision (the ratio side has
                // priority if we're already at StopNewOrders /
                // CancelAll); only lifts Normal → WidenSpreads
                // so the engine widens while the venue's
                // deleverage queue has us near the front.
                if self.adl_elevated() && matches!(bucket, MarginGuardDecision::Normal) {
                    return MarginGuardDecision::WidenSpreads;
                }
                bucket
            }
        }
    }

    /// PERP-2 — effective maintenance-margin rate inferred from
    /// the last venue snapshot:
    ///
    /// - Sum `size.abs() × mark_price` across every position.
    /// - Divide `total_maintenance_margin` by that notional to
    ///   get the venue's blended MMR for our current book.
    /// - Clamp to `[default_mmr / 10, default_mmr × 10]` so a
    ///   stale or pathological snapshot can't push the
    ///   projection to absurd values.
    ///
    /// Falls back to `thresholds.default_mmr` when no position
    /// is open (zero notional) or `last` is empty — the
    /// configured default covers the cold-start path.
    pub fn effective_mmr(&self) -> Decimal {
        let default = self.thresholds.default_mmr;
        let Some(info) = self.last.as_ref() else {
            return default;
        };
        let total_notional: Decimal = info
            .positions
            .iter()
            .map(|p| p.size.abs() * p.mark_price)
            .sum();
        if total_notional <= Decimal::ZERO {
            return default;
        }
        let inferred = info.total_maintenance_margin / total_notional;
        let floor = default / Decimal::from(10u32);
        let ceil = default * Decimal::from(10u32);
        inferred.max(floor).min(ceil)
    }

    /// Forecast the post-fill ratio if the engine adds
    /// `notional_delta` (quote-asset) of new exposure. Lets the
    /// pre-order hook short-circuit a quote whose fill would
    /// cross `stop_ratio` even though the current snapshot is
    /// comfortably below it.
    ///
    /// MM delta uses the venue-inferred effective MMR (see
    /// [`Self::effective_mmr`]) instead of the previous
    /// `notional/leverage` upper bound, which treated new IM
    /// as if it were MM and over-rejected valid quotes by
    /// 10-100×. Equity is still reduced by the IM reservation
    /// `notional / leverage` because that is what the venue
    /// actually locks from available balance on fill.
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
        let lev = Decimal::from(leverage.max(1));
        let im_needed = notional_delta / lev;
        let mm_delta = notional_delta * self.effective_mmr();

        // PERP-3 — isolated mode projects against the
        // position's own bucket. Opening exposure on the
        // isolated symbol adds IM to the isolated collateral
        // (the venue reserves from available balance into the
        // position's bucket) AND lifts the position MM. Cross
        // mode keeps the wallet-wide arithmetic.
        if matches!(self.mode, MarginModeCfg::Isolated) {
            if let Some(pos) = info.positions.iter().find(|p| p.symbol == self.symbol) {
                if let Some(iso) = pos.isolated_margin {
                    let position_mm_current =
                        pos.size.abs() * pos.mark_price * self.effective_mmr();
                    let projected_iso = iso + im_needed;
                    if projected_iso <= Decimal::ZERO {
                        return Some(Decimal::ONE);
                    }
                    return Some((position_mm_current + mm_delta) / projected_iso);
                }
            }
        }

        if info.total_equity <= Decimal::ZERO {
            // Zero/negative equity already means we're at 1.0+ —
            // the guard would already have hit `CancelAll` via
            // `decide`. Return the saturating value.
            return Some(Decimal::ONE);
        }
        let projected_equity = info.total_equity - im_needed;
        if projected_equity <= Decimal::ZERO {
            return Some(Decimal::ONE);
        }
        let projected_mm = info.total_maintenance_margin + mm_delta;
        Some(projected_mm / projected_equity)
    }

    /// Same bucket function used for both observed + projected
    /// ratios — single source of truth for threshold mapping.
    pub fn bucket(ratio: Decimal, t: &MarginGuardThresholds) -> MarginGuardDecision {
        if ratio >= t.cancel_ratio {
            MarginGuardDecision::CancelAll
        } else if ratio >= t.stop_ratio {
            MarginGuardDecision::StopNewOrders
        } else if ratio >= t.reduce_ratio {
            MarginGuardDecision::Reduce
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
            reduce_ratio: dec!(0.65),
            stop_ratio: dec!(0.8),
            cancel_ratio: dec!(0.9),
            max_stale_secs: 30,
            default_mmr: dec!(0.005),
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
            MarginGuard::bucket(dec!(0.64), &t),
            MarginGuardDecision::WidenSpreads
        );
        // PERP-1 — Reduce band starts at 0.65 in the test fixture.
        assert_eq!(
            MarginGuard::bucket(dec!(0.65), &t),
            MarginGuardDecision::Reduce
        );
        assert_eq!(
            MarginGuard::bucket(dec!(0.79), &t),
            MarginGuardDecision::Reduce
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

    // ── PERP-2 — effective MMR + richer projection ────────────

    fn snapshot_with_positions(
        total_mm: Decimal,
        positions: Vec<(Decimal, Decimal)>, // (size, mark_price)
        now_ms: i64,
    ) -> AccountMarginInfo {
        use mm_exchange_core::connector::PositionMargin;
        AccountMarginInfo {
            total_equity: dec!(10_000),
            total_initial_margin: dec!(2_000),
            total_maintenance_margin: total_mm,
            available_balance: dec!(8_000),
            margin_ratio: total_mm / dec!(10_000),
            positions: positions
                .into_iter()
                .map(|(size, mark_price)| PositionMargin {
                    symbol: "BTCUSDT".into(),
                    side: Side::Buy,
                    size,
                    entry_price: mark_price,
                    mark_price,
                    isolated_margin: None,
                    liq_price: None,
                    adl_quantile: None,
                })
                .collect(),
            reported_at_ms: now_ms,
        }
    }

    #[test]
    fn effective_mmr_defaults_without_positions() {
        let g = MarginGuard::new(thresholds());
        // No last snapshot — fall back to default MMR.
        assert_eq!(g.effective_mmr(), dec!(0.005));
    }

    #[test]
    fn effective_mmr_infers_from_venue_snapshot() {
        let mut g = MarginGuard::new(thresholds());
        // Position notional = 1 × 50_000 = 50_000; MM = 250 →
        // inferred MMR = 0.005 exactly (matches the default but
        // via the inferred path, not the fallback).
        g.update(snapshot_with_positions(
            dec!(250),
            vec![(dec!(1), dec!(50_000))],
            1_700_000_000_000,
        ));
        assert_eq!(g.effective_mmr(), dec!(0.005));

        // A venue that runs tighter brackets (MM = 100 on same
        // 50_000 notional → MMR = 0.002) — inferred value wins.
        g.update(snapshot_with_positions(
            dec!(100),
            vec![(dec!(1), dec!(50_000))],
            1_700_000_000_000,
        ));
        assert_eq!(g.effective_mmr(), dec!(0.002));
    }

    #[test]
    fn effective_mmr_clamps_pathological_values() {
        let mut g = MarginGuard::new(thresholds());
        // Degenerate snapshot with MM 5000 on 50_000 notional
        // (MMR = 0.1) — ten times the default. Guard clamps
        // to default × 10 = 0.05.
        g.update(snapshot_with_positions(
            dec!(5_000),
            vec![(dec!(1), dec!(50_000))],
            1_700_000_000_000,
        ));
        assert_eq!(g.effective_mmr(), dec!(0.05));

        // Same idea on the low side: MM 10 on 50_000 notional
        // (MMR = 0.0002, 1/25 of default) clamps up to
        // default / 10 = 0.0005.
        g.update(snapshot_with_positions(
            dec!(10),
            vec![(dec!(1), dec!(50_000))],
            1_700_000_000_000,
        ));
        assert_eq!(g.effective_mmr(), dec!(0.0005));
    }

    // ── PERP-3 — cross vs isolated margin mode ────────────────

    fn isolated_snapshot(
        symbol: &str,
        size: Decimal,
        mark: Decimal,
        isolated_margin: Decimal,
        now_ms: i64,
    ) -> AccountMarginInfo {
        use mm_exchange_core::connector::PositionMargin;
        AccountMarginInfo {
            total_equity: dec!(10_000),
            total_initial_margin: dec!(2_000),
            // Venue-reported wallet-wide MM — keep it BELOW
            // the widen_ratio (0.5) so any isolated-vs-cross
            // difference in tests comes from the guard's
            // isolation logic, not the wallet figure.
            total_maintenance_margin: dec!(1_000),
            available_balance: dec!(8_000),
            margin_ratio: dec!(0.1),
            positions: vec![PositionMargin {
                symbol: symbol.into(),
                side: Side::Buy,
                size,
                entry_price: mark,
                mark_price: mark,
                isolated_margin: Some(isolated_margin),
                liq_price: None,
                adl_quantile: None,
            }],
            reported_at_ms: now_ms,
        }
    }

    #[test]
    fn isolated_mode_uses_per_position_ratio_not_wallet() {
        let mut g = MarginGuard::new(thresholds())
            .with_symbol_mode("BTCUSDT", MarginModeCfg::Isolated);
        let now = 1_700_000_000_000;
        // Position: 1 BTC × 50_000 = 50_000 notional. Isolated
        // margin = 200. MMR default = 0.005 (no other position
        // data to infer from) → position MM = 250. Ratio =
        // 250 / 200 = 1.25. Wallet-wide ratio is 0.1 (from
        // `margin_ratio` field on the snapshot) — if the guard
        // used that it would return `Normal`. We expect
        // `CancelAll` via the per-position path.
        g.update(isolated_snapshot(
            "BTCUSDT",
            dec!(1),
            dec!(50_000),
            dec!(200),
            now,
        ));
        let ratio = g.observed_ratio().unwrap();
        assert!(ratio >= dec!(1.0), "isolated ratio should reflect position bucket, got {ratio}");
        assert_eq!(g.decide(now), MarginGuardDecision::CancelAll);
    }

    #[test]
    fn cross_mode_ignores_symbol_and_uses_wallet_ratio() {
        let mut g = MarginGuard::new(thresholds())
            .with_symbol_mode("BTCUSDT", MarginModeCfg::Cross);
        let now = 1_700_000_000_000;
        // Same snapshot as isolated test — but in Cross mode
        // the guard uses `margin_ratio` = 0.1 → Normal, even
        // though the per-position bucket is deeply underwater.
        // This is the correct reading: under cross, the
        // position draws collateral from the whole wallet.
        g.update(isolated_snapshot(
            "BTCUSDT",
            dec!(1),
            dec!(50_000),
            dec!(200),
            now,
        ));
        assert_eq!(g.observed_ratio(), Some(dec!(0.1)));
        assert_eq!(g.decide(now), MarginGuardDecision::Normal);
    }

    #[test]
    fn isolated_projected_ratio_uses_bucket_not_wallet() {
        let mut g = MarginGuard::new(thresholds())
            .with_symbol_mode("BTCUSDT", MarginModeCfg::Isolated);
        let now = 1_700_000_000_000;
        // Healthy bucket: 50_000 notional, 5_000 isolated
        // margin. Using the fixture snapshot's total_mm of
        // 1_000 and position notional 50_000, the inferred
        // effective MMR is 0.02 (venue running a tighter
        // bracket than the 0.005 default).
        g.update(isolated_snapshot(
            "BTCUSDT",
            dec!(1),
            dec!(50_000),
            dec!(5_000),
            now,
        ));
        assert_eq!(g.effective_mmr(), dec!(0.02));
        // Add a new 10_000 notional at leverage 5. IM delta =
        // 2_000 (reserved into the isolated bucket). MM delta
        // = 10_000 × 0.02 = 200.
        // position_mm_current = 50_000 × 0.02 = 1_000.
        // projected_iso = 5_000 + 2_000 = 7_000
        // projected_pos_mm = 1_000 + 200 = 1_200
        // projected_ratio ≈ 0.1714
        let r = g.projected_ratio(dec!(10_000), 5).unwrap();
        let expected = dec!(1_200) / dec!(7_000);
        assert_eq!(r, expected);
        // Sanity: the wallet-wide cross projection would have
        // given a very different answer — this test proves the
        // isolated path is live.
        assert!(r > dec!(0.1) && r < dec!(0.25));
    }

    // ── PERP-4 — ADL awareness ───────────────────────────────

    fn snapshot_with_adl(adl: Option<u8>, ratio: Decimal, now_ms: i64) -> AccountMarginInfo {
        use mm_exchange_core::connector::PositionMargin;
        AccountMarginInfo {
            total_equity: dec!(10_000),
            total_initial_margin: dec!(2_000),
            total_maintenance_margin: ratio * dec!(10_000),
            available_balance: dec!(8_000),
            margin_ratio: ratio,
            positions: vec![PositionMargin {
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                size: dec!(1),
                entry_price: dec!(50_000),
                mark_price: dec!(50_000),
                isolated_margin: None,
                liq_price: None,
                adl_quantile: adl,
            }],
            reported_at_ms: now_ms,
        }
    }

    #[test]
    fn adl_none_on_empty_or_missing_snapshot() {
        let g = MarginGuard::new(thresholds());
        assert_eq!(g.max_adl_quantile(), None);
        assert!(!g.adl_elevated());
    }

    #[test]
    fn adl_low_rank_does_not_trigger() {
        let mut g = MarginGuard::new(thresholds());
        let now = 1_700_000_000_000;
        g.update(snapshot_with_adl(Some(1), dec!(0.1), now));
        assert_eq!(g.max_adl_quantile(), Some(1));
        assert!(!g.adl_elevated());
        // No ADL bump on a healthy ratio + low rank.
        assert_eq!(g.decide(now), MarginGuardDecision::Normal);
    }

    #[test]
    fn adl_elevated_lifts_normal_to_widen() {
        let mut g = MarginGuard::new(thresholds());
        let now = 1_700_000_000_000;
        // Healthy wallet ratio (0.1 = Normal) but high ADL
        // quantile (4 = next in line) → guard widens.
        g.update(snapshot_with_adl(Some(4), dec!(0.1), now));
        assert!(g.adl_elevated());
        assert_eq!(g.decide(now), MarginGuardDecision::WidenSpreads);
    }

    #[test]
    fn adl_does_not_demote_higher_severity() {
        let mut g = MarginGuard::new(thresholds());
        let now = 1_700_000_000_000;
        // Cancel-worthy ratio (0.95) plus elevated ADL —
        // must stay at CancelAll, not get demoted to
        // WidenSpreads by the ADL override.
        g.update(snapshot_with_adl(Some(4), dec!(0.95), now));
        assert_eq!(g.decide(now), MarginGuardDecision::CancelAll);
    }

    #[test]
    fn isolated_falls_back_to_cross_when_no_position_for_symbol() {
        let mut g = MarginGuard::new(thresholds())
            .with_symbol_mode("BTCUSDT", MarginModeCfg::Isolated);
        let now = 1_700_000_000_000;
        // Position is for ETHUSDT, not our engine's symbol.
        g.update(isolated_snapshot(
            "ETHUSDT",
            dec!(1),
            dec!(3_000),
            dec!(100),
            now,
        ));
        // Falls back to wallet ratio = 0.1.
        assert_eq!(g.observed_ratio(), Some(dec!(0.1)));
    }

    #[test]
    fn projected_ratio_honours_inferred_mmr_not_im_leverage() {
        // Crucial PERP-2 assertion: the previous implementation
        // treated new IM as 1:1 MM, inflating the projected
        // MM delta by ~100× for a 2x-leverage quote. The new
        // implementation multiplies notional by the inferred
        // MMR (~0.005), which is what the venue will actually
        // book.
        let mut g = MarginGuard::new(thresholds());
        g.update(snapshot_with_positions(
            dec!(250),
            vec![(dec!(1), dec!(50_000))], // inferred MMR = 0.005
            1_700_000_000_000,
        ));
        // Add 2_000 notional at leverage 5 → IM locks 400 but
        // MM only rises by 2_000 × 0.005 = 10.
        let r = g.projected_ratio(dec!(2_000), 5).unwrap();
        // projected_equity = 10_000 - 400 = 9_600
        // projected_mm     = 250 + 10 = 260
        // ratio ≈ 0.02708
        let expected = dec!(260) / dec!(9_600);
        assert_eq!(r, expected);
        // Sanity: nowhere near the 2.0+ the old formula would
        // have produced (250 + 400) / 9_600 = 0.0677 — the old
        // output for this exact case. PERP-2 fixes the 2.5×
        // over-rejection that the old formula created.
        assert!(r < dec!(0.03));
    }
}
