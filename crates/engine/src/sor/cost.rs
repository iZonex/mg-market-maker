//! Smart Order Router — per-venue cost model (Epic A
//! sub-component #1).
//!
//! Given a `VenueSnapshot` (the output of
//! [`crate::sor::venue_state::VenueStateAggregator`]) plus a
//! `(side, urgency)` target, produces a single `RouteCost`
//! scalar in basis points that the router
//! ([`crate::sor::router::GreedyRouter`]) uses as the
//! per-venue cost-per-unit sort key.
//!
//! # Formula
//!
//! The v1 cost model is a two-term linear combination:
//!
//! ```text
//! taker_cost_bps  = venue.taker_fee_bps
//! maker_cost_bps  = config.queue_wait_bps_per_sec × venue.queue_wait_secs
//!                 + venue.maker_fee_bps
//! effective_cost_bps
//!     = urgency     · taker_cost_bps
//!     + (1 − urgency) · maker_cost_bps
//! ```
//!
//! - `urgency = 0` → pure maker cost (queue wait + maker fee).
//!   This is the "patient hedge" regime.
//! - `urgency = 1` → pure taker cost (taker fee).
//!   This is the "crash out fast" regime.
//! - `urgency = 0.5` → average of the two.
//!
//! Negative `maker_fee_bps` (a rebate) is preserved
//! end-to-end: the router treats a venue with a rebate-paying
//! maker schedule as strictly cheaper than one with a flat
//! fee, all else equal, and the effective cost for that
//! venue can legitimately go negative for low-urgency
//! targets.
//!
//! # Pure function
//!
//! Everything is synchronous `Decimal` arithmetic. No IO, no
//! clocks, no randomness. The router calls `price()` once
//! per `(venue, side, urgency)` tuple on every refresh and
//! throws the result away — no caching, no state.
//!
//! Sprint C-1 of Epic A pinned the formula and the
//! `queue_wait_bps_per_sec = 1.0` default; see
//! `docs/sprints/epic-a-cross-venue-sor.md` for the Sprint
//! A-1 audit notes.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::sor::venue_state::VenueSnapshot;
use mm_common::types::Side;
use mm_exchange_core::connector::VenueId;

/// Per-venue cost breakdown for one candidate route leg.
/// Emitted by [`VenueCostModel::price`] and consumed by the
/// greedy router's sort key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteCost {
    /// Which venue this cost belongs to — echoed verbatim
    /// from the input snapshot for traceability.
    pub venue: VenueId,
    /// Taker fee in basis points. Always non-negative.
    pub taker_cost_bps: Decimal,
    /// Maker cost including queue-wait opportunity cost and
    /// the maker fee. Can go **negative** when the venue
    /// pays a rebate that more than compensates for the
    /// queue wait.
    pub maker_cost_bps: Decimal,
    /// Urgency-weighted blend of taker and maker costs.
    /// This is the single-number sort key the router uses.
    pub effective_cost_bps: Decimal,
}

/// Configuration + behaviour for the per-venue cost model.
/// Holds the operator-tuned conversion constants; every
/// per-tick input comes through [`Self::price`].
#[derive(Debug, Clone)]
pub struct VenueCostModel {
    /// How many basis points of opportunity cost the model
    /// charges per second of expected queue wait on the
    /// maker side. v1 uses a single constant per operator;
    /// stage-2 will thread a real trade-rate estimate from
    /// `TradeFlow` per symbol.
    pub queue_wait_bps_per_sec: Decimal,
}

impl VenueCostModel {
    /// Construct with the supplied queue-wait conversion.
    /// Negative values are clamped to zero — a negative
    /// queue wait cost would reward long queue waits, which
    /// is not what the formulation captures.
    pub fn new(queue_wait_bps_per_sec: Decimal) -> Self {
        Self {
            queue_wait_bps_per_sec: queue_wait_bps_per_sec.max(Decimal::ZERO),
        }
    }

    /// Default cost model with `1.0` bps per second of queue
    /// wait. Pinned in Sprint A-1 as the v1 baseline.
    pub fn default_v1() -> Self {
        Self::new(dec!(1))
    }

    /// Price a candidate route leg on one venue. Signature
    /// takes `side` so a future refactor can charge
    /// different rates on bid vs ask (some venues
    /// differentiate), but v1 does not actually branch on
    /// side — both legs see the same fee schedule and
    /// queue model. `urgency` is clamped to `[0, 1]`
    /// internally so callers can pass raw operator input
    /// without a pre-sanitising step.
    pub fn price(&self, snapshot: &VenueSnapshot, _side: Side, urgency: Decimal) -> RouteCost {
        let urgency = urgency.max(Decimal::ZERO).min(Decimal::ONE);
        let taker_cost_bps = snapshot.taker_fee_bps;
        let queue_wait_cost = self.queue_wait_bps_per_sec * snapshot.queue_wait_secs;
        let maker_cost_bps = snapshot.maker_fee_bps + queue_wait_cost;
        let effective_cost_bps =
            urgency * taker_cost_bps + (Decimal::ONE - urgency) * maker_cost_bps;
        RouteCost {
            venue: snapshot.venue,
            taker_cost_bps,
            maker_cost_bps,
            effective_cost_bps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sor::venue_state::VenueSnapshot;

    fn snapshot(
        venue: VenueId,
        maker_fee_bps: Decimal,
        taker_fee_bps: Decimal,
        queue_wait_secs: Decimal,
    ) -> VenueSnapshot {
        VenueSnapshot {
            venue,
            symbol: "BTCUSDT".into(),
            available_qty: dec!(10),
            rate_limit_remaining: 1000,
            maker_fee_bps,
            taker_fee_bps,
            best_bid: dec!(50000),
            best_ask: dec!(50010),
            queue_wait_secs,
            slippage_bps_per_unit: Decimal::ZERO,
        }
    }

    /// Pure-taker urgency reduces the effective cost to the
    /// taker fee verbatim. Pin the zero-maker-contribution
    /// path.
    #[test]
    fn urgency_one_is_pure_taker_cost() {
        let model = VenueCostModel::default_v1();
        let snap = snapshot(VenueId::Binance, dec!(2), dec!(5), dec!(10));
        let cost = model.price(&snap, Side::Buy, dec!(1));
        assert_eq!(cost.effective_cost_bps, dec!(5));
        assert_eq!(cost.taker_cost_bps, dec!(5));
    }

    /// Pure-maker urgency reduces the effective cost to
    /// `maker_fee + queue_wait_bps_per_sec × queue_wait_secs`.
    #[test]
    fn urgency_zero_is_pure_maker_cost() {
        let model = VenueCostModel::default_v1();
        let snap = snapshot(VenueId::Binance, dec!(2), dec!(5), dec!(10));
        let cost = model.price(&snap, Side::Buy, dec!(0));
        // maker = 2 + 1 · 10 = 12
        assert_eq!(cost.effective_cost_bps, dec!(12));
        assert_eq!(cost.maker_cost_bps, dec!(12));
    }

    /// Half urgency is the arithmetic average of the two
    /// endpoints. Catches the regression where the
    /// interpolation weight flips direction.
    #[test]
    fn urgency_half_is_arithmetic_mean() {
        let model = VenueCostModel::default_v1();
        let snap = snapshot(VenueId::Binance, dec!(2), dec!(8), dec!(10));
        let cost = model.price(&snap, Side::Buy, dec!(0.5));
        // taker = 8, maker = 2 + 10 = 12, mean = 10
        assert_eq!(cost.effective_cost_bps, dec!(10));
    }

    /// Maker rebate (negative fee) carries through end-to-end:
    /// the effective cost at zero urgency can legitimately
    /// go negative when the rebate exceeds the queue wait
    /// cost. This is the "VIP 9 on Binance" state and MUST
    /// sort ahead of every non-rebate venue in the router.
    #[test]
    fn maker_rebate_produces_negative_effective_cost() {
        let model = VenueCostModel::default_v1();
        let snap = snapshot(VenueId::Binance, dec!(-5), dec!(10), dec!(2));
        let cost = model.price(&snap, Side::Buy, dec!(0));
        // maker = -5 + 1·2 = -3, effective = -3
        assert_eq!(cost.effective_cost_bps, dec!(-3));
        assert!(cost.maker_cost_bps.is_sign_negative());
    }

    /// Queue wait cost scales linearly with both the
    /// `queue_wait_secs` input and the configured
    /// `queue_wait_bps_per_sec`. Pins both sides of the
    /// multiplication so a refactor can't silently divide
    /// one by the other.
    #[test]
    fn queue_wait_cost_is_linear_in_both_inputs() {
        let model_slow = VenueCostModel::new(dec!(1));
        let model_fast = VenueCostModel::new(dec!(3));
        let snap = snapshot(VenueId::Binance, dec!(0), dec!(5), dec!(10));
        let cost_slow = model_slow.price(&snap, Side::Buy, dec!(0));
        let cost_fast = model_fast.price(&snap, Side::Buy, dec!(0));
        // slow = 10 bps (1·10), fast = 30 bps (3·10)
        assert_eq!(cost_slow.effective_cost_bps, dec!(10));
        assert_eq!(cost_fast.effective_cost_bps, dec!(30));
    }

    /// Urgency values outside `[0, 1]` are clamped, not
    /// rejected. A caller that passes `urgency = 2` gets the
    /// pure-taker output, and `urgency = -5` gets the
    /// pure-maker output.
    #[test]
    fn urgency_is_clamped_to_unit_interval() {
        let model = VenueCostModel::default_v1();
        let snap = snapshot(VenueId::Binance, dec!(2), dec!(8), dec!(10));
        let above = model.price(&snap, Side::Buy, dec!(5));
        let below = model.price(&snap, Side::Buy, dec!(-5));
        // Above → pure taker = 8
        assert_eq!(above.effective_cost_bps, dec!(8));
        // Below → pure maker = 12
        assert_eq!(below.effective_cost_bps, dec!(12));
    }

    /// Negative `queue_wait_bps_per_sec` is clamped to zero
    /// in the constructor — a negative wait cost would
    /// reward long queues, which is not the semantic.
    #[test]
    fn negative_queue_wait_rate_clamped_to_zero() {
        let model = VenueCostModel::new(dec!(-5));
        assert_eq!(model.queue_wait_bps_per_sec, dec!(0));
    }

    /// Zero queue wait falls through to the bare maker fee —
    /// the wait-cost term contributes nothing. Regression
    /// anchor for a venue that hasn't seeded its queue
    /// estimate yet.
    #[test]
    fn zero_queue_wait_falls_through_to_maker_fee() {
        let model = VenueCostModel::default_v1();
        let snap = snapshot(VenueId::Binance, dec!(2), dec!(8), dec!(0));
        let cost = model.price(&snap, Side::Buy, dec!(0));
        assert_eq!(cost.effective_cost_bps, dec!(2));
    }

    /// Cost struct's venue tag echoes the snapshot's venue
    /// verbatim — the router needs this for logging and
    /// audit attribution.
    #[test]
    fn cost_carries_source_venue() {
        let model = VenueCostModel::default_v1();
        let snap = snapshot(VenueId::Bybit, dec!(0), dec!(0), dec!(0));
        let cost = model.price(&snap, Side::Buy, dec!(0.5));
        assert_eq!(cost.venue, VenueId::Bybit);
    }

    /// Side does not affect the v1 cost — bid and ask see
    /// the same fee schedule and queue estimate. Pin the
    /// symmetry so a future side-aware refactor doesn't
    /// silently change the existing router output.
    #[test]
    fn v1_cost_is_symmetric_across_sides() {
        let model = VenueCostModel::default_v1();
        let snap = snapshot(VenueId::Binance, dec!(2), dec!(8), dec!(10));
        let buy = model.price(&snap, Side::Buy, dec!(0.3));
        let sell = model.price(&snap, Side::Sell, dec!(0.3));
        assert_eq!(buy.effective_cost_bps, sell.effective_cost_bps);
    }
}
