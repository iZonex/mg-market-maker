//! Borrow-to-short state machine — P1.3 stage-1.
//!
//! A spot market maker that starts flat in the base asset cannot
//! quote the ask side: the venue rejects the order as
//! over-balance because there is nothing to deliver. The fix that
//! production prop desks use is to pre-borrow a small base-asset
//! buffer via the venue's margin product, quote against the
//! borrowed inventory, and repay the loan when the opposing buy
//! lands. The continuous **borrow rate** the venue charges on the
//! outstanding loan is a real carry cost that has to land in the
//! ask reservation price — otherwise the captured spread silently
//! leaks into interest expense.
//!
//! This module is the stage-1 foundation: a pure state machine
//! that owns the per-asset borrow snapshot and exposes a single
//! `effective_carry_bps()` accessor that the engine threads into
//! `StrategyContext.borrow_cost_bps`. The actual loan execution
//! (`POST /sapi/v1/margin/loan`, the matching repay, and the
//! margin-mode order routing) lands in stage-2 — see ROADMAP P1.3.
//!
//! The state machine is IO-free on purpose: every venue call lives
//! on the connector trait and the engine drives the refresh cadence
//! through the same periodic-tick pattern used by `fetch_fee_tiers`.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Per-asset borrow snapshot. One instance per asset the engine is
/// quoting (typically just the base asset of the symbol).
#[derive(Debug, Clone)]
pub struct BorrowState {
    /// The asset being tracked (`"BTC"`, `"ETH"`, …).
    pub asset: String,
    /// Most recent annualised borrow rate as a fraction
    /// (`0.05` = 5 % APR). Updated on every successful refresh.
    pub rate_apr: Decimal,
    /// Hourly rate in basis points, derived from `rate_apr` so
    /// strategies do not have to repeat the conversion. Cached
    /// here because the conversion is the same across every
    /// venue.
    pub rate_bps_hourly: Decimal,
    /// When `rate_apr` was last refreshed from the venue.
    pub fetched_at: Option<DateTime<Utc>>,
}

impl BorrowState {
    pub fn new(asset: &str) -> Self {
        Self {
            asset: asset.to_string(),
            rate_apr: dec!(0),
            rate_bps_hourly: dec!(0),
            fetched_at: None,
        }
    }
}

/// Tracks borrow state across the engine's lifetime and converts
/// the venue-reported APR into an expected-carry bps surcharge
/// the strategy can bake into the ask reservation price.
///
/// The expected carry depends on how long the engine plans to
/// hold the borrowed inventory. Without a strategy-side estimator
/// we use a single operator-tunable `expected_holding_secs`
/// constant (typically the average round-trip latency) to convert
/// APR → expected-carry-bps:
///
/// ```text
/// carry_bps = APR × 10_000 × (holding_secs / seconds_per_year)
/// ```
///
/// The bps is what the engine threads into the strategy.
#[derive(Debug, Clone)]
pub struct BorrowManager {
    state: BorrowState,
    /// Operator-tuned expected holding period for one round-trip
    /// of borrowed inventory. Default 1 hour — fits the typical
    /// MM round-trip cadence and is conservative enough to cover
    /// most pair regimes.
    expected_holding_secs: u64,
    /// Maximum borrow target in base-asset units. Stage-1 uses
    /// this only for the carry-cost calculation; stage-2 will use
    /// it as the hard cap on the actual loan.
    max_borrow: Decimal,
    /// Minimum buffer the engine wants pre-borrowed at all
    /// times. Stage-2 uses this to top up the loan — stage-1
    /// only stores it.
    buffer: Decimal,
}

impl BorrowManager {
    pub fn new(asset: &str, max_borrow: Decimal, buffer: Decimal, holding_secs: u64) -> Self {
        Self {
            state: BorrowState::new(asset),
            expected_holding_secs: holding_secs,
            max_borrow,
            buffer,
        }
    }

    /// Update from a venue rate refresh. Pure mutation — no IO,
    /// no allocations beyond the timestamp on the state struct.
    pub fn apply_rate_refresh(&mut self, rate_apr: Decimal) {
        self.state.rate_apr = rate_apr;
        self.state.rate_bps_hourly = apr_to_hourly_bps(rate_apr);
        self.state.fetched_at = Some(Utc::now());
    }

    /// Read-only view of the current borrow state.
    pub fn state(&self) -> &BorrowState {
        &self.state
    }

    /// Convert the current APR into an expected-carry bps the
    /// strategy can bake into the ask reservation. Returns
    /// `Decimal::ZERO` when no rate has been refreshed yet — the
    /// strategy then behaves exactly as it did pre-P1.3.
    pub fn effective_carry_bps(&self) -> Decimal {
        if self.state.rate_apr.is_zero() {
            return Decimal::ZERO;
        }
        let seconds_per_year = Decimal::from(365u32 * 24u32 * 60u32 * 60u32);
        let holding_fraction = Decimal::from(self.expected_holding_secs) / seconds_per_year;
        self.state.rate_apr * Decimal::from(10_000u32) * holding_fraction
    }

    pub fn max_borrow(&self) -> Decimal {
        self.max_borrow
    }

    pub fn buffer(&self) -> Decimal {
        self.buffer
    }
}

/// Convert an annualised borrow rate fraction into an hourly
/// basis-points figure. Pure helper, exposed so unit tests can
/// pin the conversion against the same constant the connector
/// `BorrowRateInfo::from_apr` helper uses.
pub fn apr_to_hourly_bps(rate_apr: Decimal) -> Decimal {
    let hours_per_year = Decimal::from(8_760u32);
    rate_apr * Decimal::from(10_000u32) / hours_per_year
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5 % APR (`0.05` as a fraction) is 500 bps/year and
    /// therefore ~0.0571 bps/hour. Pin the constant so a future
    /// contributor cannot silently change the hours-per-year
    /// denominator.
    #[test]
    fn apr_to_hourly_bps_5pct_is_about_0_057_bps() {
        let bps = apr_to_hourly_bps(dec!(0.05));
        // 0.05 × 10000 / 8760 ≈ 0.05708
        assert!(bps > dec!(0.0570) && bps < dec!(0.0572), "got {bps}");
    }

    #[test]
    fn apr_to_hourly_bps_zero_is_zero() {
        assert_eq!(apr_to_hourly_bps(dec!(0)), dec!(0));
    }

    /// `apply_rate_refresh` must update both APR and the cached
    /// hourly bps so downstream readers don't have to redo the
    /// conversion.
    #[test]
    fn apply_rate_refresh_updates_apr_and_cached_hourly_bps() {
        let mut mgr = BorrowManager::new("BTC", dec!(1.0), dec!(0.01), 3600);
        mgr.apply_rate_refresh(dec!(0.10));
        assert_eq!(mgr.state().rate_apr, dec!(0.10));
        let bps = mgr.state().rate_bps_hourly;
        // 0.10 × 10000 / 8760 ≈ 0.1142 bps/hour
        assert!(bps > dec!(0.1141) && bps < dec!(0.1142), "got {bps}");
        assert!(mgr.state().fetched_at.is_some());
    }

    /// `effective_carry_bps` returns zero before any refresh has
    /// landed — the regression anchor for the "strategy behaves
    /// pre-P1.3 when borrow data is missing" invariant.
    #[test]
    fn effective_carry_bps_is_zero_before_first_refresh() {
        let mgr = BorrowManager::new("BTC", dec!(1.0), dec!(0.01), 3600);
        assert_eq!(mgr.effective_carry_bps(), dec!(0));
    }

    /// 5 % APR × 1 hour holding time should round-trip through
    /// `effective_carry_bps` to ~0.5708 bps. Pin it so the
    /// strategy ask-side surcharge is not silently inflated by a
    /// future refactor of the conversion constants.
    #[test]
    fn effective_carry_bps_for_one_hour_holding() {
        let mut mgr = BorrowManager::new("BTC", dec!(1.0), dec!(0.01), 3600);
        mgr.apply_rate_refresh(dec!(0.05));
        let bps = mgr.effective_carry_bps();
        // 0.05 × 10000 × (3600 / 31_536_000) ≈ 0.05708
        assert!(bps > dec!(0.0570) && bps < dec!(0.0572), "got {bps}");
    }

    /// Doubling the expected holding window must double the
    /// carry surcharge — the conversion is linear in the holding
    /// seconds.
    #[test]
    fn effective_carry_bps_scales_linearly_with_holding_time() {
        let mut mgr_short = BorrowManager::new("BTC", dec!(1), dec!(0.01), 1800);
        mgr_short.apply_rate_refresh(dec!(0.10));
        let bps_short = mgr_short.effective_carry_bps();

        let mut mgr_long = BorrowManager::new("BTC", dec!(1), dec!(0.01), 3600);
        mgr_long.apply_rate_refresh(dec!(0.10));
        let bps_long = mgr_long.effective_carry_bps();

        // 3600 / 1800 = 2 → bps_long ≈ 2 × bps_short.
        let ratio = bps_long / bps_short;
        assert!(ratio > dec!(1.99) && ratio < dec!(2.01), "ratio {ratio}");
    }

    /// `max_borrow` and `buffer` are stage-2 inputs — stage-1
    /// just persists them. Pin the accessors so a future stage-2
    /// pass can't silently break the contract.
    #[test]
    fn max_borrow_and_buffer_are_persisted() {
        let mgr = BorrowManager::new("BTC", dec!(0.5), dec!(0.05), 3600);
        assert_eq!(mgr.max_borrow(), dec!(0.5));
        assert_eq!(mgr.buffer(), dec!(0.05));
    }
}
