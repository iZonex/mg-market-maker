//! Cross-exchange market-making executor (xemm).
//!
//! The existing `cross_exchange` module has the *strategy* — "quote
//! on venue A and hedge on venue B" — but no dedicated **executor**
//! that tracks the hedge leg, enforces a slippage band, and keeps
//! per-leg inventory accounting clean. Hummingbot's V2 `xemm_executor`
//! controller is the reference design; this is our Rust equivalent.
//!
//! ## Shape
//!
//! - Maker quotes rest on the **primary** venue.
//! - When a maker order fills, the executor emits a **hedge market
//!   order** on the **hedge** venue with the opposite side and equal
//!   qty.
//! - Before emitting the hedge, the executor re-checks the hedge
//!   venue's current bid/ask. If the implied hedge price would fall
//!   outside a configured **slippage band** (expressed in basis
//!   points of the maker fill price), the hedge is rejected and the
//!   caller handles the inventory imbalance.
//! - Per-leg inventory is tracked so the caller can audit whether
//!   the two legs are in sync.
//!
//! The executor is a pure sync state machine: you call
//! [`XemmExecutor::on_maker_fill`] with the maker leg's fill and it
//! returns an `XemmDecision`. The engine turns that into a real
//! venue call.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use mm_common::types::{PriceLevel, Side};

use crate::features::market_impact;

/// Side convention: `Primary` is the maker venue, `Hedge` is where
/// we offload the resulting inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    Primary,
    Hedge,
}

#[derive(Debug, Clone)]
pub struct XemmConfig {
    /// Maximum allowed adverse slippage on the hedge, in basis
    /// points of the maker fill price. A maker buy at 100 with
    /// `max_slippage_bps = 20` will reject a hedge sell below
    /// 99.80.
    pub max_slippage_bps: Decimal,
    /// Minimum expected edge, in bps. If the implied hedge price is
    /// *better* than `maker_fill_price × (1 - edge)`, the executor
    /// still hedges; otherwise it flags an unfavourable cross.
    pub min_edge_bps: Decimal,
}

impl Default for XemmConfig {
    fn default() -> Self {
        Self {
            max_slippage_bps: dec!(20),
            min_edge_bps: dec!(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum XemmDecision {
    /// Send this hedge order to the hedge venue immediately.
    Hedge {
        side: Side,
        qty: Decimal,
        expected_price: Decimal,
    },
    /// Hedge rejected because slippage exceeds the band. Caller must
    /// decide what to do with the open inventory (retry later, cancel
    /// maker leg, widen maker quote, etc.).
    RejectSlippage {
        reason: String,
        adverse_bps: Decimal,
    },
    /// The implied cross would not meet `min_edge_bps`. Still hedge —
    /// this is informational so the operator can log the miss — but
    /// flag it.
    HedgeWithWarning {
        side: Side,
        qty: Decimal,
        expected_price: Decimal,
        edge_bps: Decimal,
    },
}

pub struct XemmExecutor {
    config: XemmConfig,
    primary_inventory: Decimal,
    hedge_inventory: Decimal,
}

impl XemmExecutor {
    pub fn new(config: XemmConfig) -> Self {
        Self {
            config,
            primary_inventory: Decimal::ZERO,
            hedge_inventory: Decimal::ZERO,
        }
    }

    /// Called when the maker leg fills.
    ///
    /// - `maker_side` is the side of the maker fill (a buy fill
    ///   means we received base asset and need to sell it on the
    ///   hedge).
    /// - `maker_qty` is the absolute fill quantity.
    /// - `maker_price` is the fill price.
    /// - `hedge_best_bid` / `hedge_best_ask` are the current top of
    ///   book on the hedge venue.
    pub fn on_maker_fill(
        &mut self,
        maker_side: Side,
        maker_qty: Decimal,
        maker_price: Decimal,
        hedge_best_bid: Decimal,
        hedge_best_ask: Decimal,
    ) -> XemmDecision {
        // Update primary-leg inventory.
        self.primary_inventory += match maker_side {
            Side::Buy => maker_qty,
            Side::Sell => -maker_qty,
        };

        // Hedge is always the opposite side.
        let hedge_side = match maker_side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };

        // Price we'd actually get on a market hedge.
        let hedge_price = match hedge_side {
            Side::Sell => hedge_best_bid,
            Side::Buy => hedge_best_ask,
        };

        // Adverse slippage (in bps) is the signed distance from
        // the maker fill in the direction that hurts us.
        // - Maker buy → hedge sell → adverse when hedge_price < maker_price
        // - Maker sell → hedge buy → adverse when hedge_price > maker_price
        let adverse = match maker_side {
            Side::Buy => maker_price - hedge_price,
            Side::Sell => hedge_price - maker_price,
        };
        if maker_price.is_zero() {
            return XemmDecision::RejectSlippage {
                reason: "maker_price is zero".into(),
                adverse_bps: Decimal::ZERO,
            };
        }
        let adverse_bps = adverse / maker_price * dec!(10000);

        if adverse_bps > self.config.max_slippage_bps {
            return XemmDecision::RejectSlippage {
                reason: format!(
                    "adverse slippage {adverse_bps} bps > limit {}",
                    self.config.max_slippage_bps
                ),
                adverse_bps,
            };
        }

        // Emit the hedge and update hedge-leg inventory optimistically.
        self.hedge_inventory += match hedge_side {
            Side::Buy => maker_qty,
            Side::Sell => -maker_qty,
        };

        // Edge is the inverse of the adverse; a positive edge means
        // the hedge is BETTER than the maker price (profitable cross).
        let edge_bps = -adverse_bps;
        if edge_bps < self.config.min_edge_bps {
            return XemmDecision::HedgeWithWarning {
                side: hedge_side,
                qty: maker_qty,
                expected_price: hedge_price,
                edge_bps,
            };
        }

        XemmDecision::Hedge {
            side: hedge_side,
            qty: maker_qty,
            expected_price: hedge_price,
        }
    }

    /// Same as [`on_maker_fill`] but computes the expected
    /// hedge price from the **full book depth** via
    /// [`crate::features::market_impact`], not just top-of-book.
    /// Use this when the hedge qty is large enough that a
    /// single-level bid/ask quote under-estimates the real
    /// slippage — the VWAP over multiple levels is the price we
    /// will actually get.
    ///
    /// `hedge_bids` / `hedge_asks` must be ordered best-first
    /// (highest bid first, lowest ask first). `LocalOrderBook`
    /// already yields levels in that order.
    ///
    /// When the book side is empty or cannot cover `maker_qty`
    /// at all, returns `RejectSlippage` with `partial_book` in
    /// the reason — the caller should treat this as an
    /// inventory-stuck signal and escalate.
    pub fn on_maker_fill_with_book(
        &mut self,
        maker_side: Side,
        maker_qty: Decimal,
        maker_price: Decimal,
        hedge_bids: &[PriceLevel],
        hedge_asks: &[PriceLevel],
    ) -> XemmDecision {
        // Update primary inventory optimistically, same as
        // `on_maker_fill`.
        self.primary_inventory += match maker_side {
            Side::Buy => maker_qty,
            Side::Sell => -maker_qty,
        };

        // The hedge side is always opposite of the maker side.
        let hedge_side = match maker_side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };

        let levels = match hedge_side {
            // Hedge sell walks the bid side; hedge buy walks
            // the ask side.
            Side::Sell => hedge_bids,
            Side::Buy => hedge_asks,
        };

        // `features::market_impact` uses the same "positive bps =
        // unfavourable" convention as the simple top-of-book
        // path below, so the slippage comparison reads the
        // same way.
        let Some(impact) = market_impact(levels, hedge_side, maker_qty, maker_price) else {
            // Empty book on the hedge side — can't price the
            // hedge at all. Treat as a hard reject so the
            // caller surfaces it immediately.
            return XemmDecision::RejectSlippage {
                reason: "hedge book side empty".to_string(),
                adverse_bps: Decimal::ZERO,
            };
        };

        if impact.partial {
            return XemmDecision::RejectSlippage {
                reason: format!(
                    "hedge book too thin: only {} / {} fills; VWAP {} vs maker {}",
                    impact.filled_qty, maker_qty, impact.vwap, maker_price
                ),
                adverse_bps: impact.impact_bps,
            };
        }

        let hedge_price = impact.vwap;
        let adverse_bps = impact.impact_bps;

        if adverse_bps > self.config.max_slippage_bps {
            return XemmDecision::RejectSlippage {
                reason: format!(
                    "adverse slippage (full-book VWAP) {} bps > limit {}",
                    adverse_bps, self.config.max_slippage_bps
                ),
                adverse_bps,
            };
        }

        // Emit the hedge and bump the hedge-leg inventory
        // optimistically.
        self.hedge_inventory += match hedge_side {
            Side::Buy => maker_qty,
            Side::Sell => -maker_qty,
        };

        let edge_bps = -adverse_bps;
        if edge_bps < self.config.min_edge_bps {
            return XemmDecision::HedgeWithWarning {
                side: hedge_side,
                qty: maker_qty,
                expected_price: hedge_price,
                edge_bps,
            };
        }

        XemmDecision::Hedge {
            side: hedge_side,
            qty: maker_qty,
            expected_price: hedge_price,
        }
    }

    /// Primary venue's net base-asset position.
    pub fn primary_inventory(&self) -> Decimal {
        self.primary_inventory
    }

    /// Hedge venue's net base-asset position. Should be the negation
    /// of `primary_inventory` after every hedge completes; any drift
    /// is a bug (missed hedge, rejected hedge, reconciliation error).
    pub fn hedge_inventory(&self) -> Decimal {
        self.hedge_inventory
    }

    /// Cross-leg imbalance — a non-zero value means the two legs
    /// have drifted and manual intervention may be required.
    pub fn leg_imbalance(&self) -> Decimal {
        self.primary_inventory + self.hedge_inventory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(max_slip: Decimal, min_edge: Decimal) -> XemmExecutor {
        XemmExecutor::new(XemmConfig {
            max_slippage_bps: max_slip,
            min_edge_bps: min_edge,
        })
    }

    #[test]
    fn maker_buy_emits_hedge_sell_at_hedge_bid() {
        let mut e = exec(dec!(50), dec!(0));
        let d = e.on_maker_fill(Side::Buy, dec!(1), dec!(100), dec!(100.5), dec!(100.6));
        match d {
            XemmDecision::Hedge {
                side,
                qty,
                expected_price,
            } => {
                assert_eq!(side, Side::Sell);
                assert_eq!(qty, dec!(1));
                assert_eq!(expected_price, dec!(100.5));
            }
            other => panic!("expected Hedge, got {other:?}"),
        }
    }

    #[test]
    fn maker_sell_emits_hedge_buy_at_hedge_ask() {
        let mut e = exec(dec!(50), dec!(0));
        let d = e.on_maker_fill(Side::Sell, dec!(2), dec!(100), dec!(99.5), dec!(99.6));
        match d {
            XemmDecision::Hedge {
                side,
                expected_price,
                qty,
            } => {
                assert_eq!(side, Side::Buy);
                assert_eq!(expected_price, dec!(99.6));
                assert_eq!(qty, dec!(2));
            }
            other => panic!("expected Hedge, got {other:?}"),
        }
    }

    #[test]
    fn slippage_outside_band_rejects() {
        // Max slippage 10 bps; we fake a 30 bps adverse move.
        // Maker buy at 100, hedge sell at 99.7 → 30 bps adverse.
        let mut e = exec(dec!(10), dec!(0));
        let d = e.on_maker_fill(Side::Buy, dec!(1), dec!(100), dec!(99.7), dec!(99.8));
        match d {
            XemmDecision::RejectSlippage { adverse_bps, .. } => {
                assert_eq!(adverse_bps, dec!(30));
            }
            other => panic!("expected reject, got {other:?}"),
        }
        // Inventory state: primary updated, hedge NOT (rejected).
        assert_eq!(e.primary_inventory(), dec!(1));
        assert_eq!(e.hedge_inventory(), dec!(0));
        assert_eq!(e.leg_imbalance(), dec!(1));
    }

    #[test]
    fn profitable_cross_emits_clean_hedge() {
        // Maker sells at 100, hedge buys at 99.5 → positive edge 50 bps.
        let mut e = exec(dec!(50), dec!(0));
        let d = e.on_maker_fill(Side::Sell, dec!(1), dec!(100), dec!(99.4), dec!(99.5));
        assert!(matches!(d, XemmDecision::Hedge { .. }));
        assert_eq!(e.leg_imbalance(), dec!(0));
    }

    #[test]
    fn low_edge_emits_warning_not_reject() {
        // Maker buys at 100, hedge sells at 99.95 → 5 bps adverse.
        // max_slippage 20 bps (accepted), min_edge 10 bps (failed).
        let mut e = exec(dec!(20), dec!(10));
        let d = e.on_maker_fill(Side::Buy, dec!(1), dec!(100), dec!(99.95), dec!(100));
        match d {
            XemmDecision::HedgeWithWarning { edge_bps, .. } => {
                assert_eq!(edge_bps, dec!(-5));
            }
            other => panic!("expected HedgeWithWarning, got {other:?}"),
        }
    }

    #[test]
    fn balanced_after_hedge_completes() {
        let mut e = exec(dec!(100), dec!(0));
        e.on_maker_fill(Side::Buy, dec!(1), dec!(100), dec!(100), dec!(100));
        e.on_maker_fill(Side::Sell, dec!(1), dec!(100), dec!(100), dec!(100));
        assert_eq!(e.primary_inventory(), dec!(0));
        assert_eq!(e.hedge_inventory(), dec!(0));
        assert_eq!(e.leg_imbalance(), dec!(0));
    }

    #[test]
    fn leg_imbalance_flags_drift_after_reject() {
        let mut e = exec(dec!(5), dec!(0));
        // Reject because slippage > 5 bps.
        e.on_maker_fill(Side::Buy, dec!(1), dec!(100), dec!(99.5), dec!(100));
        assert_ne!(e.leg_imbalance(), dec!(0));
    }

    /// Pin the adverse-bps formula directly: maker buy fills at
    /// `P_m = 100`, hedge ask (the price we would hit on a market
    /// sell) is `P_h = 99.80`. The textbook adverse cost is
    /// `(P_m - P_h) / P_m × 10_000 = 0.20 / 100 × 10_000 = 20 bps`.
    /// The reject branch triggers when adverse > `max_slippage_bps`,
    /// so with a 10-bps max and 20 bps adverse we expect a reject.
    #[test]
    fn adverse_bps_canonical_maker_buy_example() {
        // 10 bps cap, so 20 bps adverse must reject.
        let mut e = exec(dec!(10), dec!(0));
        let d = e.on_maker_fill(
            Side::Buy,
            dec!(1),
            dec!(100),
            dec!(99.80), // hedge bid = the price we'd hit on a market sell
            dec!(99.90),
        );
        match d {
            XemmDecision::RejectSlippage { adverse_bps, .. } => {
                assert_eq!(adverse_bps, dec!(20));
            }
            other => panic!("expected reject with adverse=20, got {other:?}"),
        }
    }

    #[test]
    fn zero_maker_price_is_rejected_defensively() {
        let mut e = exec(dec!(100), dec!(0));
        let d = e.on_maker_fill(Side::Buy, dec!(1), dec!(0), dec!(0), dec!(0));
        assert!(matches!(d, XemmDecision::RejectSlippage { .. }));
    }

    // ---- on_maker_fill_with_book (market-impact path) ----

    fn level(price: Decimal, qty: Decimal) -> PriceLevel {
        PriceLevel { price, qty }
    }

    #[test]
    fn with_book_matches_top_of_book_when_qty_fits_level_one() {
        // Single-level fill: the VWAP equals the touch, so the
        // full-book path must produce the same answer as the
        // legacy top-of-book path.
        let mut e1 = exec(dec!(100), dec!(0));
        let top = e1.on_maker_fill(Side::Buy, dec!(1), dec!(100), dec!(99.9), dec!(100.1));

        let mut e2 = exec(dec!(100), dec!(0));
        let book_bids = vec![level(dec!(99.9), dec!(5)), level(dec!(99.8), dec!(10))];
        let book_asks = vec![level(dec!(100.1), dec!(5)), level(dec!(100.2), dec!(10))];
        let deep =
            e2.on_maker_fill_with_book(Side::Buy, dec!(1), dec!(100), &book_bids, &book_asks);
        assert_eq!(top, deep);
    }

    #[test]
    fn with_book_rejects_thin_book_via_partial_flag() {
        // Maker bought 10 BTC on the primary leg, but the hedge
        // book only has 3 BTC total — partial fill, must reject.
        let mut e = exec(dec!(20), dec!(0));
        let hedge_bids = vec![level(dec!(99.9), dec!(3))];
        let hedge_asks = vec![level(dec!(100.1), dec!(50))];
        let d = e.on_maker_fill_with_book(Side::Buy, dec!(10), dec!(100), &hedge_bids, &hedge_asks);
        assert!(matches!(d, XemmDecision::RejectSlippage { .. }));
    }

    #[test]
    fn with_book_rejects_when_deep_vwap_exceeds_slippage_band() {
        // Top-of-book is 99.95 (within a 20 bps band from 100),
        // but the second level is far enough that the full-fill
        // VWAP blows past the limit. The top-of-book path would
        // accept; the full-book path must reject.
        let mut e_top = exec(dec!(20), dec!(0));
        let top = e_top.on_maker_fill(Side::Buy, dec!(10), dec!(100), dec!(99.95), dec!(100.05));
        assert!(
            matches!(
                top,
                XemmDecision::Hedge { .. } | XemmDecision::HedgeWithWarning { .. }
            ),
            "top-of-book path should accept, got {top:?}"
        );

        let mut e_deep = exec(dec!(20), dec!(0));
        // 1 BTC at 99.95 (touch), 9 BTC at 99.00 — full-fill
        // VWAP is ~99.095, i.e. ~90 bps below maker. Exceeds
        // 20 bps band → reject.
        let hedge_bids = vec![level(dec!(99.95), dec!(1)), level(dec!(99.00), dec!(9))];
        let hedge_asks = vec![level(dec!(100.05), dec!(50))];
        let deep = e_deep.on_maker_fill_with_book(
            Side::Buy,
            dec!(10),
            dec!(100),
            &hedge_bids,
            &hedge_asks,
        );
        assert!(
            matches!(deep, XemmDecision::RejectSlippage { .. }),
            "deep book path should reject, got {deep:?}"
        );
    }

    #[test]
    fn with_book_fires_hedge_when_vwap_is_inside_band() {
        // Deeper VWAP is slightly worse than the touch but still
        // inside the 20 bps slippage band — hedge should fire
        // with `expected_price == vwap` (not touch).
        let mut e = exec(dec!(20), dec!(0));
        let hedge_bids = vec![level(dec!(99.99), dec!(5)), level(dec!(99.98), dec!(10))];
        let hedge_asks = vec![level(dec!(100.01), dec!(10))];
        let d = e.on_maker_fill_with_book(Side::Buy, dec!(6), dec!(100), &hedge_bids, &hedge_asks);
        match d {
            XemmDecision::HedgeWithWarning { expected_price, .. }
            | XemmDecision::Hedge { expected_price, .. } => {
                // VWAP = (5 * 99.99 + 1 * 99.98) / 6 = 99.9883...
                // which is strictly below the top-of-book bid.
                assert!(expected_price < dec!(99.99));
                assert!(expected_price > dec!(99.98));
            }
            other => panic!("expected hedge dispatch, got {other:?}"),
        }
    }

    #[test]
    fn with_book_inventory_tracking_matches_top_of_book_path() {
        // After a successful hedge via the full-book path the
        // executor's inventory accounting must match what the
        // legacy path would produce.
        let mut e = exec(dec!(100), dec!(0));
        let hedge_bids = vec![level(dec!(99.99), dec!(10))];
        let hedge_asks = vec![level(dec!(100.01), dec!(10))];
        let _ = e.on_maker_fill_with_book(Side::Buy, dec!(1), dec!(100), &hedge_bids, &hedge_asks);
        assert_eq!(e.primary_inventory(), dec!(1));
        assert_eq!(e.hedge_inventory(), dec!(-1));
    }
}
