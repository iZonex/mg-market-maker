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

/// Epic A stage-2 #4 — convex-cost router with iterative
/// water-filling allocation.
///
/// The [`GreedyRouter`] is optimal when per-venue cost is
/// linear in qty (constant marginal cost). Once the cost
/// model charges a **convex** slippage term (Stoikov 2018,
/// Obizhaeva–Wang 2013: slippage ≈ `α · q²` for some α
/// derived from book depth), greedy can over-fill the
/// cheapest venue past the point where its marginal cost
/// exceeds a pricier venue's, giving sub-optimal total cost.
///
/// The water-filling allocation solves the Lagrangian KKT
/// stationarity condition `∂Cost/∂q_i = λ` for every venue
/// with positive allocation:
///
/// ```text
/// minimize   Σ_i (fee_i · q_i + 0.5 · slip_i · q_i²)
/// subject to Σ_i q_i = target,  0 ≤ q_i ≤ cap_i
/// ```
///
/// Stationarity: `fee_i + slip_i · q_i = λ`, so
/// `q_i(λ) = clamp(0, cap_i, (λ − fee_i) / slip_i)`.
/// Bisection on λ ∈ [min(fee), max(fee + slip·cap)] until
/// `Σ q_i(λ) = target`. The one-dim root-find converges in
/// O(log(1/ε)) iterations; each iteration is O(N) over the
/// venue list. N is bounded by a handful of venues in
/// practice, so runtime is dominated by bisection.
///
/// When `slippage_bps_per_unit == 0` for every venue this
/// router degenerates to the greedy solution — verified by
/// a regression test.
#[derive(Debug, Clone)]
pub struct ConvexRouter {
    pub cost_model: VenueCostModel,
    /// Bisection tolerance on total allocated qty vs target,
    /// in base-asset units. Smaller = tighter match at the
    /// cost of more iterations. Default `1e-8` — generously
    /// sub-pip on any venue we target.
    pub bisect_tol: Decimal,
    /// Max bisection iterations. 50 is plenty given the
    /// tol above; the guard exists so a pathological input
    /// cannot spin forever.
    pub max_iter: usize,
}

impl ConvexRouter {
    pub fn new(cost_model: VenueCostModel) -> Self {
        Self {
            cost_model,
            bisect_tol: dec!(0.00000001),
            max_iter: 50,
        }
    }

    /// Run the water-filling allocation. Same signature as
    /// [`GreedyRouter::route`] so operators swap routers via
    /// config without changing call sites.
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
        let urgency_c = urgency.max(Decimal::ZERO).min(Decimal::ONE);
        let is_taker = urgency_c >= TAKER_THRESHOLD;

        // Gather (snapshot, linear_fee_bps, slip_coef_bps) triples
        // for every available venue. Skip exhausted venues up
        // front — they do not enter the KKT system.
        let mut rows: Vec<(VenueSnapshot, Decimal, Decimal)> = snapshots
            .iter()
            .filter(|s| s.is_available())
            .map(|s| {
                let cost = self.cost_model.price(s, target_side, urgency_c);
                (s.clone(), cost.effective_cost_bps, s.slippage_bps_per_unit)
            })
            .collect();
        if rows.is_empty() {
            // No available venues for a non-zero target —
            // return a non-complete decision that preserves
            // `target_qty` so callers can distinguish this
            // from the trivial zero-target case.
            return RouteDecision {
                target_side,
                target_qty,
                filled_qty: Decimal::ZERO,
                is_complete: false,
                legs: Vec::new(),
            };
        }

        // Pathological cases: every row has zero slippage
        // coef → the problem is a pure LP where greedy is
        // already optimal. Delegate so we don't spin in
        // bisection with a zero-denominator in the stationarity
        // formula.
        let all_linear = rows.iter().all(|(_, _, slip)| *slip <= Decimal::ZERO);
        if all_linear {
            let greedy = GreedyRouter::new(self.cost_model.clone());
            return greedy.route(target_side, target_qty, urgency, snapshots);
        }

        let total_cap: Decimal = rows.iter().map(|r| r.0.available_qty).sum();
        let effective_target = target_qty.min(total_cap);

        // λ-bounds: at λ = min(fee), every q_i ≤ 0 so total
        // allocation is 0. At λ = max(fee + slip · cap),
        // every venue is at its cap. Bisect between.
        let mut lo = rows
            .iter()
            .map(|(_, fee, _)| *fee)
            .fold(Decimal::MAX, |acc, v| if v < acc { v } else { acc });
        let mut hi = rows
            .iter()
            .map(|(s, fee, slip)| *fee + *slip * s.available_qty)
            .fold(Decimal::MIN, |acc, v| if v > acc { v } else { acc });

        // Guard against a degenerate equality (all fees equal,
        // all caps equal) — stretch the bracket by 1 so the
        // sign-change exists.
        if hi <= lo {
            hi = lo + Decimal::ONE;
        }

        for _ in 0..self.max_iter {
            let mid = (lo + hi) / Decimal::from(2u32);
            let total = self.allocation_total(&rows, mid);
            if (total - effective_target).abs() <= self.bisect_tol {
                lo = mid;
                hi = mid;
                break;
            }
            if total > effective_target {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let lambda = (lo + hi) / Decimal::from(2u32);

        // Build legs. Sort by ascending fee for deterministic
        // ordering — `effective_cost_bps` on the output carries
        // the final marginal cost at this allocation, same
        // semantics as GreedyRouter.
        rows.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| (a.0.venue as u8).cmp(&(b.0.venue as u8)))
        });
        let mut legs = Vec::new();
        let mut filled = Decimal::ZERO;
        for (snap, fee, slip) in &rows {
            if filled >= effective_target {
                break;
            }
            let q = Self::qty_at_lambda(*fee, *slip, snap.available_qty, lambda);
            if q <= Decimal::ZERO {
                continue;
            }
            let take = q.min(effective_target - filled);
            if take <= Decimal::ZERO {
                continue;
            }
            // `expected_cost_bps` reports the AVERAGE cost per
            // unit so `leg.qty × expected_cost_bps` is the
            // integrated cost of this leg under the convex
            // model: C(q) = fee·q + 0.5·slip·q². Matches the
            // greedy router's semantics where the product is
            // the leg's total cost.
            let avg_cost = *fee + *slip * take / Decimal::from(2u32);
            legs.push(RouteLeg {
                venue: snap.venue,
                qty: take,
                is_taker,
                expected_cost_bps: avg_cost,
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

    fn allocation_total(
        &self,
        rows: &[(VenueSnapshot, Decimal, Decimal)],
        lambda: Decimal,
    ) -> Decimal {
        rows.iter()
            .map(|(s, fee, slip)| Self::qty_at_lambda(*fee, *slip, s.available_qty, lambda))
            .sum()
    }

    /// Closed-form stationarity solution for one venue given
    /// the Lagrange multiplier `λ`. Clamps to `[0, cap]`.
    fn qty_at_lambda(fee: Decimal, slip: Decimal, cap: Decimal, lambda: Decimal) -> Decimal {
        if slip <= Decimal::ZERO {
            // Linear venue: either take the full cap when
            // cheaper than λ, or skip entirely. Matches the
            // LP solution at equal-cost tie-break.
            if fee < lambda {
                cap
            } else {
                Decimal::ZERO
            }
        } else {
            let raw = (lambda - fee) / slip;
            raw.max(Decimal::ZERO).min(cap)
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
            slippage_bps_per_unit: Decimal::ZERO,
        }
    }

    /// Variant that carries a non-zero slippage coefficient —
    /// used by the convex-router tests.
    fn snap_with_slip(
        venue: VenueId,
        available: Decimal,
        maker_bps: Decimal,
        taker_bps: Decimal,
        queue_wait_secs: Decimal,
        slippage_bps_per_unit: Decimal,
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
            slippage_bps_per_unit,
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

    // ---------------------------------------------------------
    // Epic A stage-2 #4 — ConvexRouter (water-filling)
    // ---------------------------------------------------------

    fn zero_cost_model() -> VenueCostModel {
        VenueCostModel::new(Decimal::ZERO)
    }

    /// Degenerate case: every venue has zero slippage coef →
    /// convex router delegates to greedy, so allocation is
    /// identical to the greedy path.
    #[test]
    fn convex_router_matches_greedy_when_no_slippage() {
        let snaps = vec![
            snap(VenueId::Binance, dec!(10), dec!(1), dec!(5), Decimal::ZERO),
            snap(VenueId::Bybit, dec!(10), dec!(2), dec!(6), Decimal::ZERO),
        ];
        let greedy = GreedyRouter::new(zero_cost_model()).route(
            Side::Buy,
            dec!(7),
            dec!(1),
            &snaps,
        );
        let convex = ConvexRouter::new(zero_cost_model()).route(
            Side::Buy,
            dec!(7),
            dec!(1),
            &snaps,
        );
        assert_eq!(greedy.legs.len(), convex.legs.len());
        for (g, c) in greedy.legs.iter().zip(convex.legs.iter()) {
            assert_eq!(g.venue, c.venue);
            assert_eq!(g.qty, c.qty);
        }
        assert_eq!(greedy.filled_qty, convex.filled_qty);
    }

    /// Positive slippage forces the router to SPREAD qty
    /// across venues instead of sending it all to the cheapest
    /// — the key win over greedy. Two venues with equal fees
    /// but different slippage coefficients: the router should
    /// favour the thicker book but still allocate some to the
    /// thinner one when target > single-venue slippage-optimal.
    #[test]
    fn convex_router_spreads_across_venues_under_slippage() {
        // Both venues fee = 5 bps, but Binance has 10× the
        // depth → 10× less slippage per unit. The convex router
        // should still put most qty on Binance, but NOT 100%
        // — the marginal cost on Binance rises with allocation.
        let snaps = vec![
            snap_with_slip(
                VenueId::Binance,
                dec!(100),
                dec!(5),
                dec!(5),
                Decimal::ZERO,
                dec!(0.1),
            ),
            snap_with_slip(
                VenueId::Bybit,
                dec!(100),
                dec!(5),
                dec!(5),
                Decimal::ZERO,
                dec!(1.0),
            ),
        ];
        let convex = ConvexRouter::new(zero_cost_model()).route(
            Side::Buy,
            dec!(10),
            dec!(1),
            &snaps,
        );
        assert_eq!(convex.legs.len(), 2,
            "convex should spread across both venues when slippage differs");
        let bin_leg = convex.legs.iter().find(|l| l.venue == VenueId::Binance).unwrap();
        let byb_leg = convex.legs.iter().find(|l| l.venue == VenueId::Bybit).unwrap();
        // At equilibrium: fee_bin + slip_bin·q_bin = fee_byb + slip_byb·q_byb
        //   5 + 0.1·q_bin = 5 + 1.0·q_byb  →  q_bin = 10·q_byb
        // q_bin + q_byb = 10  →  q_byb ≈ 0.909, q_bin ≈ 9.09.
        assert!(bin_leg.qty > byb_leg.qty,
            "thicker book should take the bigger slice: bin={}, byb={}",
            bin_leg.qty, byb_leg.qty);
        // Sanity: total fills target.
        assert_eq!(convex.filled_qty, dec!(10));
        assert!(convex.is_complete);
    }

    /// Degenerate greedy suboptimality: one cheap thin venue
    /// along with one pricey thick venue. Evaluate both
    /// routers' allocations against the same integrated
    /// convex-cost function `C_i(q) = fee_i·q + 0.5·slip_i·q²`
    /// so the convex router's allocation has strictly lower
    /// (or equal) integrated cost.
    #[test]
    fn convex_router_beats_greedy_total_cost_under_slippage() {
        let snaps = vec![
            // Cheap fee but thin book → heavy slippage penalty
            // at large qty.
            snap_with_slip(
                VenueId::Binance,
                dec!(10),
                dec!(1),
                dec!(1),
                Decimal::ZERO,
                dec!(2.0),
            ),
            // Pricey fee but thick book → flat slippage. Greedy
            // ignores this (fee dominates) but convex uses it
            // at the margin.
            snap_with_slip(
                VenueId::Bybit,
                dec!(10),
                dec!(10),
                dec!(10),
                Decimal::ZERO,
                dec!(0.01),
            ),
        ];
        let greedy = GreedyRouter::new(zero_cost_model()).route(
            Side::Buy,
            dec!(8),
            dec!(1),
            &snaps,
        );
        let convex = ConvexRouter::new(zero_cost_model()).route(
            Side::Buy,
            dec!(8),
            dec!(1),
            &snaps,
        );
        // Integrated cost function: for qty q on venue with
        // fee f and slip s → f·q + 0.5·s·q².
        let cost_under_convex = |legs: &[RouteLeg]| -> Decimal {
            legs.iter()
                .map(|leg| {
                    let s = snaps.iter().find(|s| s.venue == leg.venue).unwrap();
                    let fee = s.maker_fee_bps; // urgency=1 → taker; but both fees equal here.
                    let fee = if s.taker_fee_bps != fee { s.taker_fee_bps } else { fee };
                    fee * leg.qty
                        + s.slippage_bps_per_unit * leg.qty * leg.qty / Decimal::from(2u32)
                })
                .sum()
        };
        let greedy_cost = cost_under_convex(&greedy.legs);
        let convex_cost = cost_under_convex(&convex.legs);
        assert!(
            convex_cost <= greedy_cost,
            "convex integrated cost {convex_cost} must not exceed greedy {greedy_cost}"
        );
    }

    /// Zero target returns an empty, complete decision.
    #[test]
    fn convex_router_zero_target_is_empty_complete() {
        let convex = ConvexRouter::new(zero_cost_model()).route(
            Side::Buy,
            Decimal::ZERO,
            dec!(1),
            &[],
        );
        assert!(convex.legs.is_empty());
        assert_eq!(convex.target_qty, Decimal::ZERO);
    }

    /// No available venues → empty decision, not-complete.
    #[test]
    fn convex_router_no_available_venues_returns_empty_incomplete() {
        let mut s = snap_with_slip(
            VenueId::Binance,
            dec!(0),
            dec!(1),
            dec!(5),
            Decimal::ZERO,
            dec!(0.1),
        );
        s.available_qty = Decimal::ZERO;
        let convex = ConvexRouter::new(zero_cost_model()).route(
            Side::Buy,
            dec!(5),
            dec!(1),
            &[s],
        );
        assert!(convex.legs.is_empty());
        assert!(!convex.is_complete);
    }
}
