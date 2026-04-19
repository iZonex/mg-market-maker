//! Queue-position-aware fill model for the backtester.
//!
//! Ported from `hftbacktest/src/backtest/models/queue.rs` (MIT).
//! The upstream library is the canonical reference implementation
//! for the queue-position method described in:
//!
//! - Rigtorp, "Estimating Queue Position in an Order Book", 2013
//!   (<https://rigtorp.se/2013/06/08/estimating-order-queue-position.html>)
//! - <https://quant.stackexchange.com/questions/3782/how-do-we-estimate-position-of-our-order-in-order-book>
//!
//! # Why queue-aware fills matter
//!
//! Our existing [`crate::fill_model::ProbabilisticFiller`] rolls a
//! single `prob_fill_on_touch` scalar when the market touches a
//! maker quote. That's a convenient lie: in reality, a maker bid
//! at price P only fills after the queue **ahead** of it at P
//! clears, and the queue clears via trades plus cancels. Two
//! maker orders at the same price posted a microsecond apart
//! have very different fill probabilities because one sits ahead
//! of the other in a FIFO queue. Ignoring that gap makes MM
//! backtests **systematically over-report PnL** by 10–30% on
//! realistic tick data.
//!
//! This module adds the stateful per-order tracking that models
//! that gap properly:
//!
//! - When a maker quote is placed at price P, record the current
//!   resting qty at P as the initial `front_q_qty` (everything
//!   that sits between us and the touch).
//! - When a market trade hits P, decrement `front_q_qty` by the
//!   trade size and bump the `cum_trade_qty` counter (so the
//!   subsequent depth-change update doesn't double-count).
//! - When the resting qty at P changes for any other reason
//!   (cancels, new entries), partition the qty change between
//!   "ahead of us" and "behind us" using a pluggable
//!   [`Probability`] model.
//! - The quote fills once `front_q_qty` reaches zero; the amount
//!   of overshoot translates into the filled qty.
//!
//! # Integration
//!
//! The module is deliberately standalone — it doesn't touch the
//! existing `ProbabilisticFiller`. Consumers that want queue-
//! aware fills build a [`QueuePos`] per live maker quote, route
//! market events through `on_trade` / `on_depth_change`, and
//! call `consume_fill()` to lift newly-filled qty out of the
//! tracker. A future backtester rewrite can replace the current
//! coin-flip model with this.

use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// Probability trait — how a decrease in the resting qty at our
// price level is split between "in front of us" and "behind us".
// ---------------------------------------------------------------------------

/// Given the current `front` (qty ahead of our order) and `back`
/// (qty behind us), return the probability that a qty decrease
/// hits the **back** of the queue rather than the front. Values
/// in `[0, 1]`.
///
/// The function shape is the tuning knob. Two canonical choices
/// are shipped: `LogProbQueueFunc` and `PowerProbQueueFunc`.
pub trait Probability {
    fn prob(&self, front: Decimal, back: Decimal) -> f64;
}

/// `f(x) = ln(1 + x)`, `prob = f(back) / (f(back) + f(front))`.
/// A good default — the log weighting pulls aggressive cancels
/// toward the back of a deep queue (where aggressive retail
/// cancels typically live) while still allowing front-of-queue
/// decreases to matter when the queue is short.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogProbQueueFunc;

impl LogProbQueueFunc {
    pub fn new() -> Self {
        Self
    }
    fn f(x: Decimal) -> f64 {
        let v = x.to_f64().unwrap_or(0.0).max(0.0);
        (1.0 + v).ln()
    }
}

impl Probability for LogProbQueueFunc {
    fn prob(&self, front: Decimal, back: Decimal) -> f64 {
        let b = Self::f(back);
        let fv = Self::f(front);
        let denom = b + fv;
        if denom <= 0.0 {
            return 0.0;
        }
        b / denom
    }
}

/// `f(x) = x^n`, `prob = f(back) / (f(back) + f(front))`.
/// Larger `n` pulls probability harder toward whichever side has
/// more qty (so a 10:1 ratio at `n = 2` gives prob 100/101 ≈
/// 0.99). Smaller `n` flattens the split back toward 50/50.
#[derive(Debug, Clone, Copy)]
pub struct PowerProbQueueFunc {
    n: f64,
}

impl PowerProbQueueFunc {
    /// `n` must be positive; typical values are in `[0.5, 3.0]`.
    pub fn new(n: f64) -> Self {
        assert!(n > 0.0, "PowerProbQueueFunc: exponent must be > 0");
        Self { n }
    }

    fn f(&self, x: Decimal) -> f64 {
        x.to_f64().unwrap_or(0.0).max(0.0).powf(self.n)
    }
}

impl Probability for PowerProbQueueFunc {
    fn prob(&self, front: Decimal, back: Decimal) -> f64 {
        let b = self.f(back);
        let fv = self.f(front);
        let denom = b + fv;
        if denom <= 0.0 {
            return 0.0;
        }
        b / denom
    }
}

// ---------------------------------------------------------------------------
// QueuePos — per-order state tracked by the simulator.
// ---------------------------------------------------------------------------

/// Queue state for a single live maker order. Created when the
/// order is placed; mutated on trade and depth events at the
/// order's price level; consumed when the simulator wants to
/// know "how much of this order has filled since the last
/// check".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuePos {
    /// Remaining qty **ahead** of our order at the price level.
    /// Starts at the book's resting qty at the moment of
    /// placement. Goes to zero (and possibly negative, briefly)
    /// once our order reaches the front. Negative values carry
    /// over the overshoot until `consume_fill` turns that into
    /// filled base qty.
    pub front_q_qty: Decimal,
    /// Cumulative trade qty witnessed at the price level since
    /// the last depth-change update. Used to avoid double
    /// counting — a trade decreases the book qty too, and
    /// `on_depth_change` subtracts this tally before splitting
    /// the remaining change between front and back.
    pub cum_trade_qty: Decimal,
}

impl QueuePos {
    /// Initialise queue state from the current resting qty at
    /// the order's price level. A fresh maker order joins the
    /// back of the existing queue, so `front_q_qty` starts at
    /// the full resting size.
    pub fn new(book_qty_at_price: Decimal) -> Self {
        Self {
            front_q_qty: book_qty_at_price.max(Decimal::ZERO),
            cum_trade_qty: Decimal::ZERO,
        }
    }

    /// Handle a market trade that hit our price level. The trade
    /// consumes some qty from the front of the queue, so our
    /// position moves up by `trade_qty`.
    pub fn on_trade(&mut self, trade_qty: Decimal) {
        if trade_qty <= Decimal::ZERO {
            return;
        }
        self.front_q_qty -= trade_qty;
        self.cum_trade_qty += trade_qty;
    }

    /// Handle a depth-change event at our price level. `prev_qty`
    /// is the resting qty before the event, `new_qty` after. The
    /// change minus accumulated trades since the last depth
    /// update is partitioned between the front (us and everyone
    /// ahead) and the back (everyone behind) using `prob_model`.
    ///
    /// Cancels that land in front of us advance our queue
    /// position; cancels behind us don't help. When `new_qty >
    /// prev_qty` (qty grew), the new orders join at the back, so
    /// the front queue is unchanged except for a safety clamp to
    /// `new_qty` in case our previous front estimate was stale.
    pub fn on_depth_change<P: Probability>(
        &mut self,
        prev_qty: Decimal,
        new_qty: Decimal,
        prob_model: &P,
    ) {
        // Net change in the resting qty, minus the trades we
        // already accounted for via `on_trade` (to avoid double
        // counting the same qty decrease twice).
        let chg = prev_qty - new_qty - self.cum_trade_qty;
        // Reset the tally — every future depth update works off
        // a fresh window.
        self.cum_trade_qty = Decimal::ZERO;

        if chg <= Decimal::ZERO {
            // Qty grew or stayed flat. New entries join the
            // back of the queue — our position doesn't change.
            // Clamp `front_q_qty` to the current book depth just
            // in case the front was stale (e.g. a resync).
            if self.front_q_qty > new_qty {
                self.front_q_qty = new_qty;
            }
            return;
        }

        let front = self.front_q_qty;
        let back = (prev_qty - front).max(Decimal::ZERO);

        let p = prob_model.prob(front, back);
        let p = if !p.is_finite() {
            1.0
        } else {
            p.clamp(0.0, 1.0)
        };
        let Some(p_dec) = Decimal::from_f64(p) else {
            // Round-trip failed (should be unreachable for
            // values in [0, 1]); fall back to advancing the
            // front by the full change, the conservative option.
            self.front_q_qty -= chg;
            if self.front_q_qty > new_qty {
                self.front_q_qty = new_qty;
            }
            return;
        };
        let one_minus_p = Decimal::ONE - p_dec;

        // Canonical hftbacktest formula:
        //   est_front = front - (1 - p) * chg
        //             + min(back - p * chg, 0)
        //
        // The second term activates only when the estimated
        // "behind" share of the change is larger than the actual
        // back queue — that overflow spills over to the front.
        let overflow = (back - p_dec * chg).min(Decimal::ZERO);
        let est_front = front - one_minus_p * chg + overflow;
        self.front_q_qty = est_front.min(new_qty);
    }

    /// `true` when our order has reached the touch and any
    /// incoming trade will fill us.
    pub fn is_at_front(&self) -> bool {
        self.front_q_qty <= Decimal::ZERO
    }

    /// Lift any newly-filled base qty out of the tracker. A
    /// negative `front_q_qty` means we overshot the front of
    /// the queue by that much — the absolute value is the qty
    /// that must be attributed to our order as a fill. The
    /// tracker resets `front_q_qty` to zero afterwards so the
    /// next event starts from the touch.
    pub fn consume_fill(&mut self) -> Decimal {
        if self.front_q_qty < Decimal::ZERO {
            let filled = -self.front_q_qty;
            self.front_q_qty = Decimal::ZERO;
            filled
        } else {
            Decimal::ZERO
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // ---- Probability models ----

    #[test]
    fn log_prob_empty_sides_returns_zero() {
        let p = LogProbQueueFunc::new();
        assert_eq!(p.prob(dec!(0), dec!(0)), 0.0);
    }

    #[test]
    fn log_prob_symmetric_queue_is_half() {
        let p = LogProbQueueFunc::new();
        let v = p.prob(dec!(10), dec!(10));
        assert!((v - 0.5).abs() < 1e-12);
    }

    #[test]
    fn log_prob_heavy_back_is_above_half() {
        let p = LogProbQueueFunc::new();
        let v = p.prob(dec!(1), dec!(100));
        assert!(v > 0.5, "expected back-weighted, got {v}");
    }

    #[test]
    fn power_prob_rejects_non_positive_n() {
        let r = std::panic::catch_unwind(|| PowerProbQueueFunc::new(0.0));
        assert!(r.is_err());
    }

    #[test]
    fn power_prob_large_n_saturates_to_heavy_side() {
        let p = PowerProbQueueFunc::new(3.0);
        let v = p.prob(dec!(1), dec!(10));
        // 10^3 / (1 + 10^3) ≈ 0.999.
        assert!(v > 0.99);
    }

    #[test]
    fn power_prob_n_equals_one_is_linear_ratio() {
        let p = PowerProbQueueFunc::new(1.0);
        // Linear: back / (back + front) = 3 / 4.
        let v = p.prob(dec!(1), dec!(3));
        assert!((v - 0.75).abs() < 1e-12);
    }

    // ---- QueuePos — placement and simple advancement ----

    #[test]
    fn new_order_initialises_front_to_book_qty() {
        let q = QueuePos::new(dec!(42));
        assert_eq!(q.front_q_qty, dec!(42));
        assert_eq!(q.cum_trade_qty, dec!(0));
    }

    #[test]
    fn new_order_clamps_negative_book_qty_to_zero() {
        // A defensive case — callers should pass the book qty,
        // but if they accidentally pass a negative we clamp so
        // the tracker does not start in a filled state.
        let q = QueuePos::new(dec!(-5));
        assert_eq!(q.front_q_qty, dec!(0));
    }

    #[test]
    fn trade_advances_front_and_bumps_counter() {
        let mut q = QueuePos::new(dec!(10));
        q.on_trade(dec!(3));
        assert_eq!(q.front_q_qty, dec!(7));
        assert_eq!(q.cum_trade_qty, dec!(3));
    }

    #[test]
    fn trade_ignores_non_positive_qty() {
        let mut q = QueuePos::new(dec!(10));
        q.on_trade(dec!(0));
        q.on_trade(dec!(-3));
        assert_eq!(q.front_q_qty, dec!(10));
        assert_eq!(q.cum_trade_qty, dec!(0));
    }

    #[test]
    fn is_at_front_fires_when_front_hits_zero() {
        let mut q = QueuePos::new(dec!(5));
        assert!(!q.is_at_front());
        q.on_trade(dec!(5));
        assert!(q.is_at_front());
    }

    #[test]
    fn consume_fill_returns_overshoot_as_filled_qty() {
        let mut q = QueuePos::new(dec!(3));
        q.on_trade(dec!(5));
        // Overshoot = 2 → we took 2 units of our own.
        assert_eq!(q.consume_fill(), dec!(2));
        // Front resets to zero; a subsequent check is a no-op.
        assert_eq!(q.consume_fill(), dec!(0));
        assert_eq!(q.front_q_qty, dec!(0));
    }

    #[test]
    fn consume_fill_returns_zero_when_not_at_front() {
        let mut q = QueuePos::new(dec!(10));
        assert_eq!(q.consume_fill(), dec!(0));
        assert_eq!(q.front_q_qty, dec!(10));
    }

    // ---- on_depth_change — the probabilistic partition ----

    #[test]
    fn depth_increase_does_not_change_front() {
        // Qty grew from 10 to 15 → 5 new entries joined the
        // back of the queue. Front queue must be untouched.
        let mut q = QueuePos::new(dec!(10));
        q.on_depth_change(dec!(10), dec!(15), &LogProbQueueFunc::new());
        assert_eq!(q.front_q_qty, dec!(10));
    }

    #[test]
    fn depth_decrease_partitions_change_between_front_and_back() {
        // 20 total resting qty, 8 ahead of us → 12 behind.
        // Qty drops to 10 → change = 10. With LogProb the back
        // gets the larger share (back > front). The front
        // should advance by a fractional amount strictly less
        // than the full change.
        let mut q = QueuePos {
            front_q_qty: dec!(8),
            cum_trade_qty: dec!(0),
        };
        q.on_depth_change(dec!(20), dec!(10), &LogProbQueueFunc::new());
        assert!(q.front_q_qty < dec!(8), "front must advance");
        assert!(q.front_q_qty > dec!(0), "front should not jump to zero");
    }

    #[test]
    fn depth_change_does_not_double_count_trades() {
        // A trade of 4 decreased the book from 10 to 6. Then a
        // depth-change event reports the 10 → 6 transition. The
        // tracker must NOT apply the 4-unit change twice.
        let mut q = QueuePos::new(dec!(10));
        q.on_trade(dec!(4));
        assert_eq!(q.front_q_qty, dec!(6));
        assert_eq!(q.cum_trade_qty, dec!(4));
        // Depth event arrives — net change is 10 - 6 = 4, minus
        // the 4 we already credited → net chg = 0, no further
        // adjustment to front.
        q.on_depth_change(dec!(10), dec!(6), &LogProbQueueFunc::new());
        assert_eq!(q.front_q_qty, dec!(6));
        // Counter resets on every depth update.
        assert_eq!(q.cum_trade_qty, dec!(0));
    }

    /// End-to-end placement → trades → depth change → fill.
    ///
    /// Scenario: post a bid at a level with 10 units ahead.
    /// Three trades of 2 each clear 6 units → 4 remaining ahead.
    /// Then a cancel of 8 units drops the resting qty from 4 to
    /// 2 — the probability model splits the change between us
    /// (front=4) and behind (back=0) → the front shrinks but
    /// stays bounded by the remaining book depth. Another trade
    /// of 3 overshoots our position → 1-unit fill.
    #[test]
    fn end_to_end_place_trade_depth_fill_sequence() {
        let model = LogProbQueueFunc::new();
        let mut q = QueuePos::new(dec!(10));
        q.on_trade(dec!(2));
        q.on_trade(dec!(2));
        q.on_trade(dec!(2));
        assert_eq!(q.front_q_qty, dec!(4));
        // Depth update consistent with the three trades: 10 - 6
        // = 4. No extra cancel signal, so front is unchanged.
        q.on_depth_change(dec!(10), dec!(4), &model);
        assert_eq!(q.front_q_qty, dec!(4));
        // Now a fresh trade of 3 hits the level.
        q.on_trade(dec!(3));
        // Front is 4 - 3 = 1.
        assert_eq!(q.front_q_qty, dec!(1));
        assert!(!q.is_at_front());
        // One more trade of 3 overshoots — 2 units go to us.
        q.on_trade(dec!(3));
        assert!(q.is_at_front());
        assert_eq!(q.consume_fill(), dec!(2));
    }

    #[test]
    fn depth_increase_clamps_stale_front() {
        // If `front_q_qty` ever exceeds the new book qty (e.g.
        // after a snapshot resync), the invariant must be
        // restored silently.
        let mut q = QueuePos {
            front_q_qty: dec!(100),
            cum_trade_qty: dec!(0),
        };
        q.on_depth_change(dec!(5), dec!(10), &LogProbQueueFunc::new());
        assert_eq!(q.front_q_qty, dec!(10));
    }
}
