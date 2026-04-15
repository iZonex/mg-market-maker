//! Pair lifecycle automation — P2.3 stage-1.
//!
//! Pre-P2.3 the engine fetched `ProductSpec` exactly once at
//! startup. New listings, delistings, trading-status transitions
//! (PRE_TRADING, HALT, BREAK), tick/lot updates — all required a
//! restart to pick up. For venues listing 10+ new pairs per week
//! that is a manual operational burden; halt handling is
//! particularly dangerous because venues sometimes send fills
//! *after* a halt and the MM has no state to reject them.
//!
//! `PairLifecycleManager` closes the gap. The engine's periodic
//! `pair_lifecycle_interval` arm calls
//! `connector.get_product_spec(symbol)` on a slow cadence, hands
//! the freshly-fetched spec to [`PairLifecycleManager::diff`],
//! and routes the resulting [`PairLifecycleEvent`]s into the
//! audit trail + the engine's `lifecycle_paused` flag. Halt
//! and delisting events flip the flag to `true` so
//! `refresh_quotes` returns early before the next book event
//! lands.
//!
//! Stage-1 is **single-symbol** — the manager owns the snapshot
//! for one product and the engine owns one manager per
//! per-symbol task. Stage-2 will lift the snapshot to a
//! cross-engine `Arc<Mutex<>>` so a multi-symbol deployment can
//! auto-onboard new listings into a probation engine without a
//! process restart.

use mm_common::types::{ProductSpec, TradingStatus};
use rust_decimal::Decimal;

/// One transition the lifecycle manager observed against its
/// in-memory snapshot. The engine consumes the events to drive
/// audit + the paused flag; tests pin the diff function purely
/// against this enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairLifecycleEvent {
    /// First time the manager has ever seen this symbol — fires
    /// on the first poll after engine startup.
    Listed,
    /// Venue removed the symbol entirely (or
    /// `get_product_spec` returned "symbol not found"). Engine
    /// cancels every order and refuses to ever requote for the
    /// rest of the process lifetime.
    Delisted,
    /// `trading_status` flipped from `Trading` to `Halted`,
    /// `Break`, or `PreTrading`. Engine cancels every order and
    /// pauses quoting until the next `Resumed` event.
    Halted {
        from: TradingStatus,
        to: TradingStatus,
    },
    /// `trading_status` flipped back to `Trading` from a
    /// non-trading state. Engine clears the paused flag.
    Resumed { from: TradingStatus },
    /// `tick_size` and / or `lot_size` changed without a
    /// status flip. Engine updates `self.product` in place and
    /// re-rounds the next quote refresh against the new values.
    TickLotChanged {
        old_tick: Decimal,
        new_tick: Decimal,
        old_lot: Decimal,
        new_lot: Decimal,
    },
    /// `min_notional` changed without a status or tick/lot
    /// flip. Surfaced separately from `TickLotChanged` so the
    /// audit trail records exactly what moved.
    MinNotionalChanged { old: Decimal, new: Decimal },
}

/// Per-symbol lifecycle state machine. Owns the most recent
/// `ProductSpec` snapshot and exposes a single
/// [`Self::diff`] method that the engine calls on each
/// refresh tick.
#[derive(Debug, Clone)]
pub struct PairLifecycleManager {
    last_spec: Option<ProductSpec>,
    /// Latched once the manager observes a `Delisted` event.
    /// A delisted symbol cannot be un-delisted — only a
    /// process restart lifts the latch — so the manager
    /// ignores any subsequent successful `get_product_spec`
    /// for safety.
    delisted_latched: bool,
}

impl PairLifecycleManager {
    pub fn new() -> Self {
        Self {
            last_spec: None,
            delisted_latched: false,
        }
    }

    /// Diff a freshly-fetched `ProductSpec` against the manager's
    /// snapshot and return the resulting events. Pure mutation
    /// — no IO. Returns an empty vector when nothing changed.
    pub fn diff(&mut self, fresh: ProductSpec) -> Vec<PairLifecycleEvent> {
        if self.delisted_latched {
            // Refuse to un-delist. Operator must restart the
            // process to recover from a delisted state.
            return Vec::new();
        }
        let mut events = Vec::new();
        match self.last_spec.as_ref() {
            None => {
                events.push(PairLifecycleEvent::Listed);
            }
            Some(prev) => {
                if prev.trading_status != fresh.trading_status {
                    if fresh.trading_status == TradingStatus::Trading {
                        events.push(PairLifecycleEvent::Resumed {
                            from: prev.trading_status,
                        });
                    } else {
                        events.push(PairLifecycleEvent::Halted {
                            from: prev.trading_status,
                            to: fresh.trading_status,
                        });
                    }
                }
                if prev.tick_size != fresh.tick_size || prev.lot_size != fresh.lot_size {
                    events.push(PairLifecycleEvent::TickLotChanged {
                        old_tick: prev.tick_size,
                        new_tick: fresh.tick_size,
                        old_lot: prev.lot_size,
                        new_lot: fresh.lot_size,
                    });
                }
                if prev.min_notional != fresh.min_notional {
                    events.push(PairLifecycleEvent::MinNotionalChanged {
                        old: prev.min_notional,
                        new: fresh.min_notional,
                    });
                }
            }
        }
        self.last_spec = Some(fresh);
        events
    }

    /// Fold a connector error path into a `Delisted` event when
    /// the venue reports the symbol no longer exists. Engine
    /// calls this with the "symbol not found" branch instead
    /// of [`Self::diff`]. Latches the manager so subsequent
    /// `diff` calls become no-ops.
    pub fn on_delisted(&mut self) -> Vec<PairLifecycleEvent> {
        if self.delisted_latched {
            return Vec::new();
        }
        self.delisted_latched = true;
        vec![PairLifecycleEvent::Delisted]
    }

    /// Read-only access to the current snapshot, primarily for
    /// dashboard exposure.
    pub fn current(&self) -> Option<&ProductSpec> {
        self.last_spec.as_ref()
    }

    /// True after a `Delisted` event has been latched.
    pub fn is_delisted(&self) -> bool {
        self.delisted_latched
    }
}

impl Default for PairLifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn spec(
        tick: Decimal,
        lot: Decimal,
        min_notional: Decimal,
        status: TradingStatus,
    ) -> ProductSpec {
        ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: tick,
            lot_size: lot,
            min_notional,
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.001),
            trading_status: status,
        }
    }

    /// First poll on a fresh manager always emits `Listed`.
    #[test]
    fn first_diff_emits_listed() {
        let mut mgr = PairLifecycleManager::new();
        let events = mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Trading,
        ));
        assert_eq!(events, vec![PairLifecycleEvent::Listed]);
        assert!(mgr.current().is_some());
    }

    /// A second poll with an identical spec emits no events.
    #[test]
    fn no_op_diff_emits_nothing() {
        let mut mgr = PairLifecycleManager::new();
        let s = spec(dec!(0.01), dec!(0.001), dec!(10), TradingStatus::Trading);
        mgr.diff(s.clone());
        let events = mgr.diff(s);
        assert!(events.is_empty());
    }

    /// Trading → Halted emits a `Halted` with the correct
    /// from/to pair.
    #[test]
    fn trading_to_halted_emits_halted() {
        let mut mgr = PairLifecycleManager::new();
        mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Trading,
        ));
        let events = mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Halted,
        ));
        assert_eq!(
            events,
            vec![PairLifecycleEvent::Halted {
                from: TradingStatus::Trading,
                to: TradingStatus::Halted
            }]
        );
    }

    /// Halted → Trading emits `Resumed`.
    #[test]
    fn halted_to_trading_emits_resumed() {
        let mut mgr = PairLifecycleManager::new();
        mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Halted,
        ));
        let events = mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Trading,
        ));
        assert_eq!(
            events,
            vec![PairLifecycleEvent::Resumed {
                from: TradingStatus::Halted
            }]
        );
    }

    /// Tick / lot drift emits exactly `TickLotChanged` with the
    /// before/after values. Multiple changes in the same poll
    /// collapse into one event.
    #[test]
    fn tick_lot_drift_emits_tick_lot_changed() {
        let mut mgr = PairLifecycleManager::new();
        mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Trading,
        ));
        let events = mgr.diff(spec(
            dec!(0.001),
            dec!(0.0001),
            dec!(10),
            TradingStatus::Trading,
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            PairLifecycleEvent::TickLotChanged {
                old_tick: dec!(0.01),
                new_tick: dec!(0.001),
                old_lot: dec!(0.001),
                new_lot: dec!(0.0001),
            }
        );
    }

    /// `min_notional` drift surfaces separately from
    /// `TickLotChanged` — auditors want to see exactly what
    /// moved.
    #[test]
    fn min_notional_drift_emits_min_notional_changed() {
        let mut mgr = PairLifecycleManager::new();
        mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Trading,
        ));
        let events = mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(20),
            TradingStatus::Trading,
        ));
        assert_eq!(
            events,
            vec![PairLifecycleEvent::MinNotionalChanged {
                old: dec!(10),
                new: dec!(20),
            }]
        );
    }

    /// A poll that flips status AND mutates tick/lot AND
    /// changes min_notional emits all three events in
    /// deterministic order: status first (highest priority),
    /// then tick/lot, then min_notional.
    #[test]
    fn multi_field_drift_emits_events_in_priority_order() {
        let mut mgr = PairLifecycleManager::new();
        mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Trading,
        ));
        let events = mgr.diff(spec(
            dec!(0.001),
            dec!(0.0001),
            dec!(20),
            TradingStatus::Halted,
        ));
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], PairLifecycleEvent::Halted { .. }));
        assert!(matches!(
            events[1],
            PairLifecycleEvent::TickLotChanged { .. }
        ));
        assert!(matches!(
            events[2],
            PairLifecycleEvent::MinNotionalChanged { .. }
        ));
    }

    /// `on_delisted` latches the manager — subsequent `diff`
    /// calls become no-ops even when the venue starts
    /// returning the symbol again.
    #[test]
    fn delisted_is_latched() {
        let mut mgr = PairLifecycleManager::new();
        mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Trading,
        ));
        let events = mgr.on_delisted();
        assert_eq!(events, vec![PairLifecycleEvent::Delisted]);
        assert!(mgr.is_delisted());

        // Even a recovery refresh produces nothing.
        let post = mgr.diff(spec(
            dec!(0.01),
            dec!(0.001),
            dec!(10),
            TradingStatus::Trading,
        ));
        assert!(post.is_empty());
        // Repeated `on_delisted` is idempotent.
        let again = mgr.on_delisted();
        assert!(again.is_empty());
    }
}
