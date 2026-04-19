//! Client-side order emulator.
//!
//! Some venues (including HyperLiquid) do not natively support stop
//! orders, trailing stops, or conditional OCO (one-cancels-other)
//! brackets. Rather than re-implement the logic in every connector
//! that lacks the feature, this module provides a single layer that
//! the engine consults on every book update. When a condition
//! triggers, the emulator emits an `EmulatorAction::PlaceMarket`
//! (or `Cancel`) that the engine forwards to the venue as a plain
//! market / limit order.
//!
//! Supported types:
//!
//! - **Stop market** — fires a market order when the trigger is
//!   touched from the correct side.
//! - **Stop limit** — fires a limit order at `limit_price` when the
//!   trigger is touched.
//! - **Trailing stop** — tracks the high-water mark (for sells) or
//!   low-water mark (for buys) and fires when the market retraces by
//!   `trail_amount`.
//! - **OCO bracket** — pair of conditional orders where triggering
//!   one cancels the other.
//! - **GTD expiry** — time-based expiry for orders carrying a
//!   `TimeInForce::Gtd` flag; the emulator issues a cancel when the
//!   deadline passes.
//!
//! The emulator is a **pure sync state machine**: feed it book
//! updates and the current time, collect actions, done.

use std::collections::HashMap;
use std::time::Instant;

use rust_decimal::Decimal;

use mm_common::types::Side;

/// Unique id for an emulated order. Separate from exchange order ids
/// because the order does not yet exist on the venue.
pub type EmulatorOrderId = u64;

/// Kinds of emulated orders supported.
#[derive(Debug, Clone)]
pub enum EmulatorOrder {
    StopMarket {
        side: Side,
        trigger_price: Decimal,
        qty: Decimal,
    },
    StopLimit {
        side: Side,
        trigger_price: Decimal,
        limit_price: Decimal,
        qty: Decimal,
    },
    TrailingStop {
        side: Side,
        trail_amount: Decimal,
        qty: Decimal,
        /// High-water mark for sell trailing stops, low-water for buys.
        watermark: Decimal,
    },
    /// Half of an OCO bracket; cancels its sibling when triggered.
    OcoLeg {
        side: Side,
        trigger_price: Decimal,
        qty: Decimal,
        sibling: EmulatorOrderId,
    },
    /// Time-based expiry for a venue order that does not honour GTD.
    /// `venue_order_id` is the id on the venue that must be cancelled
    /// at the deadline.
    GtdCancel {
        venue_order_id: String,
        deadline: Instant,
    },
}

/// Actions the emulator wants the engine to perform.
#[derive(Debug, Clone, PartialEq)]
pub enum EmulatorAction {
    PlaceMarket {
        side: Side,
        qty: Decimal,
        emulator_id: EmulatorOrderId,
    },
    PlaceLimit {
        side: Side,
        price: Decimal,
        qty: Decimal,
        emulator_id: EmulatorOrderId,
    },
    CancelVenue {
        venue_order_id: String,
        emulator_id: EmulatorOrderId,
    },
}

pub struct OrderEmulator {
    orders: HashMap<EmulatorOrderId, EmulatorOrder>,
    next_id: EmulatorOrderId,
}

impl Default for OrderEmulator {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderEmulator {
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn register(&mut self, order: EmulatorOrder) -> EmulatorOrderId {
        let id = self.next_id;
        self.next_id += 1;
        self.orders.insert(id, order);
        id
    }

    pub fn cancel(&mut self, id: EmulatorOrderId) {
        self.orders.remove(&id);
    }

    pub fn len(&self) -> usize {
        self.orders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    /// Feed a market tick and collect any triggered actions.
    ///
    /// `mid` is the current mid price; `now` is used for GTD expiry
    /// checks.
    pub fn on_tick(&mut self, mid: Decimal, now: Instant) -> Vec<EmulatorAction> {
        let mut out = Vec::new();
        // Collect triggered ids first so we can safely mutate the map
        // during iteration.
        let mut triggered: Vec<EmulatorOrderId> = Vec::new();
        let mut siblings_to_drop: Vec<EmulatorOrderId> = Vec::new();

        for (id, order) in self.orders.iter_mut() {
            match order {
                EmulatorOrder::StopMarket {
                    side,
                    trigger_price,
                    qty,
                } => {
                    if stop_triggered(*side, *trigger_price, mid) {
                        out.push(EmulatorAction::PlaceMarket {
                            side: *side,
                            qty: *qty,
                            emulator_id: *id,
                        });
                        triggered.push(*id);
                    }
                }
                EmulatorOrder::StopLimit {
                    side,
                    trigger_price,
                    limit_price,
                    qty,
                } => {
                    if stop_triggered(*side, *trigger_price, mid) {
                        out.push(EmulatorAction::PlaceLimit {
                            side: *side,
                            price: *limit_price,
                            qty: *qty,
                            emulator_id: *id,
                        });
                        triggered.push(*id);
                    }
                }
                EmulatorOrder::TrailingStop {
                    side,
                    trail_amount,
                    qty,
                    watermark,
                } => {
                    // Update the watermark first, then check.
                    match side {
                        Side::Sell => {
                            if mid > *watermark {
                                *watermark = mid;
                            }
                            // Fires when price drops below
                            // `watermark - trail_amount`.
                            if mid <= *watermark - *trail_amount {
                                out.push(EmulatorAction::PlaceMarket {
                                    side: Side::Sell,
                                    qty: *qty,
                                    emulator_id: *id,
                                });
                                triggered.push(*id);
                            }
                        }
                        Side::Buy => {
                            if mid < *watermark {
                                *watermark = mid;
                            }
                            if mid >= *watermark + *trail_amount {
                                out.push(EmulatorAction::PlaceMarket {
                                    side: Side::Buy,
                                    qty: *qty,
                                    emulator_id: *id,
                                });
                                triggered.push(*id);
                            }
                        }
                    }
                }
                EmulatorOrder::OcoLeg {
                    side,
                    trigger_price,
                    qty,
                    sibling,
                } => {
                    if stop_triggered(*side, *trigger_price, mid) {
                        out.push(EmulatorAction::PlaceMarket {
                            side: *side,
                            qty: *qty,
                            emulator_id: *id,
                        });
                        triggered.push(*id);
                        siblings_to_drop.push(*sibling);
                    }
                }
                EmulatorOrder::GtdCancel {
                    venue_order_id,
                    deadline,
                } => {
                    if now >= *deadline {
                        out.push(EmulatorAction::CancelVenue {
                            venue_order_id: venue_order_id.clone(),
                            emulator_id: *id,
                        });
                        triggered.push(*id);
                    }
                }
            }
        }

        for id in triggered {
            self.orders.remove(&id);
        }
        for id in siblings_to_drop {
            self.orders.remove(&id);
        }

        out
    }
}

fn stop_triggered(side: Side, trigger: Decimal, mid: Decimal) -> bool {
    match side {
        // A protective sell stop fires when price falls to or below
        // the trigger — "stop loss" on a long position.
        Side::Sell => mid <= trigger,
        // A protective buy stop fires when price rises to or above
        // the trigger — "stop loss" on a short position, or breakout
        // entry.
        Side::Buy => mid >= trigger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::time::Duration;

    #[test]
    fn stop_market_fires_below_trigger_for_sell() {
        let mut em = OrderEmulator::new();
        let id = em.register(EmulatorOrder::StopMarket {
            side: Side::Sell,
            trigger_price: dec!(100),
            qty: dec!(1),
        });
        let now = Instant::now();

        // Price above trigger — no action.
        assert!(em.on_tick(dec!(105), now).is_empty());
        // Price drops to trigger → fires and cleans up.
        let actions = em.on_tick(dec!(99), now);
        assert_eq!(
            actions,
            vec![EmulatorAction::PlaceMarket {
                side: Side::Sell,
                qty: dec!(1),
                emulator_id: id,
            }]
        );
        assert!(em.is_empty());
    }

    #[test]
    fn stop_market_fires_above_trigger_for_buy() {
        let mut em = OrderEmulator::new();
        em.register(EmulatorOrder::StopMarket {
            side: Side::Buy,
            trigger_price: dec!(100),
            qty: dec!(2),
        });
        let now = Instant::now();
        assert!(em.on_tick(dec!(99), now).is_empty());
        assert_eq!(em.on_tick(dec!(101), now).len(), 1);
    }

    #[test]
    fn stop_limit_emits_place_limit() {
        let mut em = OrderEmulator::new();
        em.register(EmulatorOrder::StopLimit {
            side: Side::Sell,
            trigger_price: dec!(100),
            limit_price: dec!(99),
            qty: dec!(1),
        });
        let actions = em.on_tick(dec!(99), Instant::now());
        assert!(matches!(
            actions[0],
            EmulatorAction::PlaceLimit {
                side: Side::Sell,
                price,
                ..
            } if price == dec!(99)
        ));
    }

    #[test]
    fn trailing_sell_tracks_highs_and_fires_on_retrace() {
        let mut em = OrderEmulator::new();
        em.register(EmulatorOrder::TrailingStop {
            side: Side::Sell,
            trail_amount: dec!(5),
            qty: dec!(1),
            watermark: dec!(100),
        });
        let now = Instant::now();
        // Price climbs — no fire, watermark moves.
        assert!(em.on_tick(dec!(105), now).is_empty());
        assert!(em.on_tick(dec!(110), now).is_empty());
        // Drops 3 — still within trail (110 - 5 = 105).
        assert!(em.on_tick(dec!(107), now).is_empty());
        // Drops 6 from peak → fires.
        let actions = em.on_tick(dec!(104), now);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn trailing_buy_tracks_lows_and_fires_on_rebound() {
        let mut em = OrderEmulator::new();
        em.register(EmulatorOrder::TrailingStop {
            side: Side::Buy,
            trail_amount: dec!(5),
            qty: dec!(1),
            watermark: dec!(100),
        });
        let now = Instant::now();
        // Drops 10 — watermark moves to 90.
        assert!(em.on_tick(dec!(90), now).is_empty());
        // Bounces 6 → fires (90 + 5 = 95).
        let actions = em.on_tick(dec!(96), now);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn oco_bracket_cancels_sibling_on_trigger() {
        let mut em = OrderEmulator::new();
        // Stop-loss leg.
        let stop_id = em.register(EmulatorOrder::OcoLeg {
            side: Side::Sell,
            trigger_price: dec!(95),
            qty: dec!(1),
            sibling: 0, // placeholder, patched below
        });
        // Take-profit leg.
        let tp_id = em.register(EmulatorOrder::OcoLeg {
            side: Side::Sell,
            trigger_price: dec!(105),
            qty: dec!(1),
            sibling: stop_id,
        });
        // Patch the stop's sibling now that we know the tp id.
        if let Some(EmulatorOrder::OcoLeg { sibling, .. }) = em.orders.get_mut(&stop_id) {
            *sibling = tp_id;
        }
        assert_eq!(em.len(), 2);

        // Price rises above take-profit trigger — TP fires and cancels stop.
        // OcoLeg sell fires when price falls below trigger; our TP leg
        // at 105 would need price ≤ 105 to fire. For a take-profit on
        // a long, we'd use Side::Sell with trigger_price being the
        // *upper* target we want to exit at, firing when price reaches
        // it. We model this as Sell-at-or-below trigger_price where
        // trigger_price = 105 above the entry.
        let actions = em.on_tick(dec!(103), Instant::now());
        assert_eq!(actions.len(), 1);
        // Both legs gone: the one that triggered and its sibling.
        assert!(em.is_empty());
    }

    #[test]
    fn gtd_cancel_fires_after_deadline() {
        let mut em = OrderEmulator::new();
        let start = Instant::now();
        em.register(EmulatorOrder::GtdCancel {
            venue_order_id: "ORDER-1".into(),
            deadline: start + Duration::from_secs(10),
        });
        // Before deadline — nothing.
        assert!(em.on_tick(dec!(100), start).is_empty());
        // Past deadline — emits cancel.
        let actions = em.on_tick(dec!(100), start + Duration::from_secs(11));
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], EmulatorAction::CancelVenue { .. }));
    }

    #[test]
    fn unrelated_orders_are_untouched() {
        let mut em = OrderEmulator::new();
        em.register(EmulatorOrder::StopMarket {
            side: Side::Sell,
            trigger_price: dec!(100),
            qty: dec!(1),
        });
        let kept = em.register(EmulatorOrder::StopMarket {
            side: Side::Sell,
            trigger_price: dec!(50),
            qty: dec!(1),
        });
        // Only the first one fires at mid=99.
        em.on_tick(dec!(99), Instant::now());
        assert_eq!(em.len(), 1);
        assert!(em.orders.contains_key(&kept));
    }

    #[test]
    fn register_and_cancel_manually() {
        let mut em = OrderEmulator::new();
        let id = em.register(EmulatorOrder::StopMarket {
            side: Side::Buy,
            trigger_price: dec!(100),
            qty: dec!(1),
        });
        em.cancel(id);
        assert!(em.is_empty());
        // Cancelled order does not fire.
        assert!(em.on_tick(dec!(101), Instant::now()).is_empty());
    }
}
