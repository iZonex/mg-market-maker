//! Smart Order Router — greedy cost-minimising dispatcher
//! (Epic A sub-component #3).
//!
//! Given a target `(side, qty, urgency)` and a list of
//! [`VenueSnapshot`]s from the aggregator, walks the
//! snapshots sorted by per-venue effective cost and
//! greedily fills up to the target qty. Emits a
//! [`RouteDecision`] describing the per-venue split.
//!
//! # Algorithm
//!
//! 1. Filter out snapshots where `is_available() == false`
//!    — a venue with zero available qty or zero rate-limit
//!    budget cannot accept a leg.
//! 2. Price every remaining snapshot via the
//!    [`VenueCostModel`] at the requested `(side, urgency)`.
//!    Cache the `RouteCost` alongside the snapshot so the
//!    router does not re-price during sorting.
//! 3. Sort ascending by `effective_cost_bps`. Ties are
//!    broken by venue ordinal (`VenueId as u8`) so the
//!    router is deterministic across runs.
//! 4. Walk the sorted list, taking
//!    `min(snapshot.available_qty, remaining_target)` from
//!    each venue until the target qty is met or the list
//!    is exhausted.
//! 5. Emit `RouteDecision { legs, filled_qty, target_qty,
//!    target_side, is_complete }`. `is_complete` is true
//!    when `filled_qty >= target_qty`.
//!
//! # Maker vs taker classification
//!
//! Each leg carries an `is_taker: bool` flag derived from a
//! **single urgency threshold** of `0.5`. Above threshold =
//! take, below = post. The router does not split a single
//! leg into a maker portion + a taker portion in v1 —
//! every leg is fully one or the other. Stage-2 can refine
//! this when the cost model starts tracking per-side
//! queue position inside a venue.
//!
//! # Pure function
//!
//! Everything is synchronous. The async flavour lives on
//! the engine hook in Sprint A-4 — the router itself never
//! touches a connector.

use mm_common::types::Side;
use mm_exchange_core::connector::VenueId;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::sor::cost::{RouteCost, VenueCostModel};
use crate::sor::venue_state::VenueSnapshot;

/// One leg of a routing decision — one dispatch to one
/// venue at one side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteLeg {
    /// Venue tag the dispatcher should route to.
    pub venue: VenueId,
    /// Signed qty to dispatch — always positive; the side
    /// is carried separately on [`RouteDecision::target_side`].
    pub qty: Decimal,
    /// `true` → take against the book, `false` → post as
    /// maker. Threshold on urgency at `0.5`.
    pub is_taker: bool,
    /// Per-venue effective cost in bps that the greedy
    /// ranker used to pick this leg. Echoed through so the
    /// audit trail and the dashboard can show "why this
    /// venue?" without re-computing.
    pub expected_cost_bps: Decimal,
}

/// Full routing decision from [`GreedyRouter::route`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    /// Signed side of the target (`Buy` → we want to buy
    /// on some venue, `Sell` → sell).
    pub target_side: Side,
    /// Qty the caller asked for.
    pub target_qty: Decimal,
    /// Qty the router actually scheduled across all legs.
    /// Equal to the sum of `legs[*].qty`. Can be less than
    /// `target_qty` when the available-venue universe is
    /// too small — the caller then inspects `is_complete`.
    pub filled_qty: Decimal,
    /// `true` when every unit of `target_qty` was
    /// scheduled. `false` on a partial fill.
    pub is_complete: bool,
    /// Ordered list of per-venue legs. Deterministic sort
    /// by effective cost ascending with venue-ordinal
    /// tiebreaker.
    pub legs: Vec<RouteLeg>,
}

impl RouteDecision {
    /// Empty decision — no legs, nothing filled. Returned
    /// on a zero-qty target.
    pub fn empty(target_side: Side) -> Self {
        Self {
            target_side,
            target_qty: Decimal::ZERO,
            filled_qty: Decimal::ZERO,
            is_complete: true,
            legs: Vec::new(),
        }
    }

    /// Total expected cost across every leg, in bps × qty
    /// units. Useful for test assertions + dashboard
    /// "average route cost" rollups.
    pub fn total_expected_cost_bps(&self) -> Decimal {
        self.legs
            .iter()
            .map(|leg| leg.expected_cost_bps * leg.qty)
            .sum()
    }
}

/// The urgency threshold above which the router marks a
/// leg as a taker dispatch. Pinned in Sprint A-1.
pub const TAKER_THRESHOLD: Decimal = dec!(0.5);

/// Cost-aware greedy router. Owns the cost model — callers
/// construct once at engine startup and call `route()` per
/// refresh tick.
#[derive(Debug, Clone)]
pub struct GreedyRouter {
    pub cost_model: VenueCostModel,
}

impl GreedyRouter {
    pub fn new(cost_model: VenueCostModel) -> Self {
        Self { cost_model }
    }

    /// Run the greedy router. Pure function — no IO, no
    /// state mutation. Returns an `empty()` decision for a
    /// zero or negative `target_qty`.
    pub fn route(
        &self,
        target_side: Side,
        target_qty: Decimal,
        urgency: Decimal,
        snapshots: &[VenueSnapshot],
    ) -> RouteDecision {
        if target_qty <= Decimal::ZERO {
            return RouteDecision::empty(target_side);
        }
        let urgency_clamped = urgency.max(Decimal::ZERO).min(Decimal::ONE);
        let is_taker = urgency_clamped >= TAKER_THRESHOLD;

        // Step 1: filter + price.
        let mut priced: Vec<(VenueSnapshot, RouteCost)> = snapshots
            .iter()
            .filter(|s| s.is_available())
            .map(|s| {
                let cost = self.cost_model.price(s, target_side, urgency_clamped);
                (s.clone(), cost)
            })
            .collect();

        // Step 2: sort by effective cost ascending, venue
        // ordinal as tiebreaker.
        priced.sort_by(|a, b| {
            a.1.effective_cost_bps
                .cmp(&b.1.effective_cost_bps)
                .then_with(|| (a.0.venue as u8).cmp(&(b.0.venue as u8)))
        });

        // Step 3: greedy fill.
        let mut legs = Vec::new();
        let mut filled = Decimal::ZERO;
        for (snap, cost) in priced {
            if filled >= target_qty {
                break;
            }
            let remaining = target_qty - filled;
            let take = snap.available_qty.min(remaining);
            if take <= Decimal::ZERO {
                continue;
            }
            legs.push(RouteLeg {
                venue: snap.venue,
                qty: take,
                is_taker,
                expected_cost_bps: cost.effective_cost_bps,
            });
            filled += take;
        }

        let is_complete = filled >= target_qty;
        RouteDecision {
            target_side,
            target_qty,
            filled_qty: filled,
            is_complete,
            legs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sor::venue_state::VenueSnapshot;

    fn snap(
        venue: VenueId,
        available: Decimal,
        maker_bps: Decimal,
        taker_bps: Decimal,
        queue_wait_secs: Decimal,
    ) -> VenueSnapshot {
        VenueSnapshot {
            venue,
            symbol: "BTCUSDT".into(),
            available_qty: available,
            rate_limit_remaining: 1000,
            maker_fee_bps: maker_bps,
            taker_fee_bps: taker_bps,
            best_bid: dec!(50000),
            best_ask: dec!(50010),
            queue_wait_secs,
        }
    }

    /// Zero qty target returns an empty decision that is
    /// still marked `is_complete = true` — the router
    /// fulfilled the trivial request.
    #[test]
    fn zero_target_returns_empty_complete_decision() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let decision = router.route(Side::Buy, dec!(0), dec!(0.5), &[]);
        assert!(decision.legs.is_empty());
        assert!(decision.is_complete);
        assert_eq!(decision.filled_qty, Decimal::ZERO);
    }

    /// Single-venue universe: the full target goes to that
    /// venue when there is enough available.
    #[test]
    fn single_venue_full_fill() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let snapshots = vec![snap(VenueId::Binance, dec!(5), dec!(1), dec!(5), dec!(10))];
        let decision = router.route(Side::Buy, dec!(2), dec!(1), &snapshots);
        assert_eq!(decision.legs.len(), 1);
        assert_eq!(decision.legs[0].venue, VenueId::Binance);
        assert_eq!(decision.legs[0].qty, dec!(2));
        assert!(decision.is_complete);
    }

    /// Two-venue universe with different costs: the cheaper
    /// venue is filled first. Pins the sort-ascending rule.
    #[test]
    fn two_venues_cheaper_first() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        // Bybit cheaper: taker 2 bps vs Binance 5 bps.
        let snapshots = vec![
            snap(VenueId::Binance, dec!(10), dec!(0), dec!(5), dec!(0)),
            snap(VenueId::Bybit, dec!(10), dec!(0), dec!(2), dec!(0)),
        ];
        let decision = router.route(Side::Buy, dec!(3), dec!(1), &snapshots);
        assert_eq!(decision.legs.len(), 1);
        assert_eq!(decision.legs[0].venue, VenueId::Bybit);
    }

    /// Target exceeds a single venue's capacity: the router
    /// splits across venues in cost order.
    #[test]
    fn target_exceeding_single_venue_splits_across_venues() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let snapshots = vec![
            snap(VenueId::Binance, dec!(2), dec!(0), dec!(5), dec!(0)),
            snap(VenueId::Bybit, dec!(5), dec!(0), dec!(2), dec!(0)),
        ];
        // Target 4: Bybit is cheaper (2 bps), fills 4 of 5 first.
        // Binance gets nothing — Bybit had enough.
        let decision = router.route(Side::Buy, dec!(4), dec!(1), &snapshots);
        assert_eq!(decision.legs.len(), 1);
        assert_eq!(decision.legs[0].venue, VenueId::Bybit);
        assert_eq!(decision.legs[0].qty, dec!(4));
        assert!(decision.is_complete);
    }

    /// Target exceeds the total available across every
    /// venue: partial fill, `is_complete = false`.
    #[test]
    fn partial_fill_when_universe_too_small() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let snapshots = vec![
            snap(VenueId::Binance, dec!(1), dec!(0), dec!(5), dec!(0)),
            snap(VenueId::Bybit, dec!(2), dec!(0), dec!(2), dec!(0)),
        ];
        let decision = router.route(Side::Buy, dec!(10), dec!(1), &snapshots);
        assert_eq!(decision.legs.len(), 2);
        assert_eq!(decision.filled_qty, dec!(3));
        assert!(!decision.is_complete);
    }

    /// Unavailable venues (exhausted qty or drained rate
    /// limit) are filtered BEFORE pricing — they must not
    /// appear in the decision even if they would have been
    /// the cheapest.
    #[test]
    fn unavailable_venues_are_filtered() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let mut exhausted = snap(VenueId::Binance, dec!(0), dec!(0), dec!(1), dec!(0));
        exhausted.available_qty = dec!(0); // drained
        let snapshots = vec![
            exhausted, // cheapest but exhausted
            snap(VenueId::Bybit, dec!(5), dec!(0), dec!(10), dec!(0)),
        ];
        let decision = router.route(Side::Buy, dec!(3), dec!(1), &snapshots);
        assert_eq!(decision.legs.len(), 1);
        assert_eq!(decision.legs[0].venue, VenueId::Bybit);
    }

    /// Drained rate limit is equivalent to exhausted qty.
    #[test]
    fn rate_limited_venue_is_filtered() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let mut drained = snap(VenueId::Binance, dec!(10), dec!(0), dec!(1), dec!(0));
        drained.rate_limit_remaining = 0;
        let snapshots = vec![
            drained,
            snap(VenueId::Bybit, dec!(5), dec!(0), dec!(10), dec!(0)),
        ];
        let decision = router.route(Side::Buy, dec!(2), dec!(1), &snapshots);
        assert_eq!(decision.legs.len(), 1);
        assert_eq!(decision.legs[0].venue, VenueId::Bybit);
    }

    /// Taker/maker classification: urgency ≥ 0.5 →
    /// `is_taker = true`, below → false. Pin the threshold.
    #[test]
    fn urgency_threshold_sets_taker_flag() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let snapshots = vec![snap(VenueId::Binance, dec!(5), dec!(1), dec!(5), dec!(10))];
        let taker = router.route(Side::Buy, dec!(1), dec!(0.7), &snapshots);
        let maker = router.route(Side::Buy, dec!(1), dec!(0.3), &snapshots);
        let boundary = router.route(Side::Buy, dec!(1), dec!(0.5), &snapshots);
        assert!(taker.legs[0].is_taker);
        assert!(!maker.legs[0].is_taker);
        assert!(boundary.legs[0].is_taker); // 0.5 is inclusive
    }

    /// Determinism — the router returns the same decision
    /// for the same inputs across repeated calls, and the
    /// venue-ordinal tiebreaker picks a stable winner on
    /// ties.
    #[test]
    fn deterministic_tiebreaker_by_venue_ordinal() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        // Two venues with IDENTICAL cost — tiebreaker picks
        // the lower venue-ordinal one.
        let snapshots = vec![
            snap(VenueId::Bybit, dec!(10), dec!(0), dec!(5), dec!(0)),
            snap(VenueId::Binance, dec!(10), dec!(0), dec!(5), dec!(0)),
        ];
        let d1 = router.route(Side::Buy, dec!(3), dec!(1), &snapshots);
        let d2 = router.route(Side::Buy, dec!(3), dec!(1), &snapshots);
        assert_eq!(d1, d2);
        // Lower ordinal wins — Binance is 0 vs Bybit 1 in
        // the VenueId enum order.
        assert_eq!(d1.legs[0].venue, VenueId::Binance);
    }

    /// Maker rebate (negative fee) makes a venue strictly
    /// cheaper than a non-rebate venue with the same taker
    /// fee.
    #[test]
    fn rebate_venue_wins_over_non_rebate() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let snapshots = vec![
            // Rebate venue — -3 bps maker, 5 bps taker.
            snap(VenueId::Binance, dec!(10), dec!(-3), dec!(5), dec!(0)),
            // Non-rebate venue — 0 bps maker, 5 bps taker.
            snap(VenueId::Bybit, dec!(10), dec!(0), dec!(5), dec!(0)),
        ];
        // At zero urgency both venues produce a maker-cost
        // sort: Binance = -3, Bybit = 0. Binance wins.
        let decision = router.route(Side::Buy, dec!(2), dec!(0), &snapshots);
        assert_eq!(decision.legs[0].venue, VenueId::Binance);
        assert!(decision.legs[0].expected_cost_bps.is_sign_negative());
    }

    /// `total_expected_cost_bps` sums over every leg.
    #[test]
    fn total_expected_cost_sums_legs() {
        let decision = RouteDecision {
            target_side: Side::Buy,
            target_qty: dec!(3),
            filled_qty: dec!(3),
            is_complete: true,
            legs: vec![
                RouteLeg {
                    venue: VenueId::Binance,
                    qty: dec!(2),
                    is_taker: true,
                    expected_cost_bps: dec!(5),
                },
                RouteLeg {
                    venue: VenueId::Bybit,
                    qty: dec!(1),
                    is_taker: true,
                    expected_cost_bps: dec!(10),
                },
            ],
        };
        // 2·5 + 1·10 = 20
        assert_eq!(decision.total_expected_cost_bps(), dec!(20));
    }

    /// Property-style invariant: across a handful of
    /// random-ish scenarios, the router's filled_qty must
    /// never exceed target_qty, and no leg's qty may
    /// exceed its snapshot's available_qty.
    #[test]
    fn property_never_over_fills() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let cases: &[(Decimal, Decimal, Decimal)] = &[
            (dec!(1), dec!(5), dec!(10)),
            (dec!(2.5), dec!(3), dec!(7)),
            (dec!(100), dec!(2), dec!(3)),
            (dec!(0.1), dec!(0.5), dec!(0.2)),
            (dec!(5), dec!(5), dec!(5)),
        ];
        for (target, avail_a, avail_b) in cases {
            let snapshots = vec![
                snap(VenueId::Binance, *avail_a, dec!(0), dec!(5), dec!(0)),
                snap(VenueId::Bybit, *avail_b, dec!(0), dec!(7), dec!(0)),
            ];
            let decision = router.route(Side::Buy, *target, dec!(1), &snapshots);
            assert!(
                decision.filled_qty <= *target,
                "overfilled: target={target}, filled={}",
                decision.filled_qty
            );
            let sum: Decimal = decision.legs.iter().map(|l| l.qty).sum();
            assert_eq!(sum, decision.filled_qty);
            for leg in &decision.legs {
                let snap = snapshots.iter().find(|s| s.venue == leg.venue).unwrap();
                assert!(
                    leg.qty <= snap.available_qty,
                    "leg {} exceeds venue cap {}",
                    leg.qty,
                    snap.available_qty
                );
            }
        }
    }

    /// Empty snapshot list returns an empty decision marked
    /// `is_complete = false` (the target could not be met
    /// at all).
    #[test]
    fn empty_snapshots_returns_incomplete_empty_decision() {
        let router = GreedyRouter::new(VenueCostModel::default_v1());
        let decision = router.route(Side::Buy, dec!(5), dec!(1), &[]);
        assert!(decision.legs.is_empty());
        assert!(!decision.is_complete);
        assert_eq!(decision.filled_qty, Decimal::ZERO);
        assert_eq!(decision.target_qty, dec!(5));
    }
}
