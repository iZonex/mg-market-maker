//! Cont-Kukanov-Stoikov Order Flow Imbalance (Epic D, sub-component #1).
//!
//! Implements the L1 order-flow-imbalance process from
//! Cont, Kukanov, Stoikov — "The Price Impact of Order Book
//! Events" (*J. Financial Econometrics*, 12(1), 47–88, 2014).
//!
//! The tracker folds successive top-of-book snapshots into a
//! signed observation per event:
//!
//! ```text
//!          ⎧  Q_b'           if  P_b' > P_b
//! e_b[t] = ⎨  Q_b' − Q_b     if  P_b' = P_b
//!          ⎩  −Q_b           if  P_b' < P_b
//!
//!          ⎧  −Q_a'          if  P_a' < P_a
//! e_a[t] = ⎨  Q_a' − Q_a     if  P_a' = P_a
//!          ⎩  Q_a            if  P_a' > P_a
//!
//! OFI[t] = e_b[t] − e_a[t]
//! ```
//!
//! Positive OFI = net upward pressure (aggressive bid depth
//! arriving or aggressive ask depth departing). Negative OFI
//! = net downward pressure. Full derivation + source
//! attribution in `docs/research/signal-wave-2-formulas.md`
//! §"Sub-component #1".
//!
//! Unlike `features::TradeFlow` (which tracks *trade* volume),
//! OFI captures **passive** depth changes — a fresh limit buy
//! posted at the touch counts as upward pressure even without
//! a trade.

use rust_decimal::Decimal;

/// Stateful L1 order-flow-imbalance tracker.
///
/// Holds the previous top-of-book snapshot and emits one OFI
/// observation per new update. First call returns `None` because
/// the tracker has no prior state to diff against.
#[derive(Debug, Clone, Default)]
pub struct OfiTracker {
    prev: Option<L1Snapshot>,
}

/// Frozen top-of-book snapshot used as the "previous" reference
/// for the next OFI observation. Kept public so tests and
/// debug dumps can inspect what the tracker is comparing
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct L1Snapshot {
    pub bid_px: Decimal,
    pub bid_qty: Decimal,
    pub ask_px: Decimal,
    pub ask_qty: Decimal,
}

impl OfiTracker {
    /// Construct an empty tracker. The first [`Self::update`]
    /// call returns `None`; subsequent calls emit signed OFI
    /// observations against the previous snapshot.
    pub fn new() -> Self {
        Self { prev: None }
    }

    /// Pre-seed the tracker with a known snapshot without
    /// firing an observation. Useful when the engine has a
    /// warm book snapshot before the first feed event.
    pub fn seed(&mut self, bid_px: Decimal, bid_qty: Decimal, ask_px: Decimal, ask_qty: Decimal) {
        self.prev = Some(L1Snapshot {
            bid_px,
            bid_qty,
            ask_px,
            ask_qty,
        });
    }

    /// Fold one new L1 snapshot into the tracker and emit the
    /// signed OFI observation. Returns `None` on the very first
    /// call (no previous snapshot to diff against) and **auto-
    /// seeds** so the next call produces a real observation.
    /// Callers that already have a warm book snapshot should
    /// call [`Self::seed`] first instead — both paths reach the
    /// same steady state.
    pub fn update(
        &mut self,
        bid_px: Decimal,
        bid_qty: Decimal,
        ask_px: Decimal,
        ask_qty: Decimal,
    ) -> Option<Decimal> {
        let Some(prev) = self.prev else {
            self.prev = Some(L1Snapshot {
                bid_px,
                bid_qty,
                ask_px,
                ask_qty,
            });
            return None;
        };
        let e_b = if bid_px > prev.bid_px {
            bid_qty
        } else if bid_px == prev.bid_px {
            bid_qty - prev.bid_qty
        } else {
            -prev.bid_qty
        };
        let e_a = if ask_px < prev.ask_px {
            -ask_qty
        } else if ask_px == prev.ask_px {
            ask_qty - prev.ask_qty
        } else {
            prev.ask_qty
        };
        self.prev = Some(L1Snapshot {
            bid_px,
            bid_qty,
            ask_px,
            ask_qty,
        });
        Some(e_b - e_a)
    }

    /// Current cached snapshot, `None` until the first `seed` /
    /// `update` call.
    pub fn prev_snapshot(&self) -> Option<L1Snapshot> {
        self.prev
    }

    /// Forget the cached state. Next `update` call returns `None`.
    pub fn reset(&mut self) {
        self.prev = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn tracker_seeded() -> OfiTracker {
        let mut t = OfiTracker::new();
        // Seed at symmetric book: bid=99/10, ask=101/10.
        t.seed(dec!(99), dec!(10), dec!(101), dec!(10));
        t
    }

    #[test]
    fn first_update_returns_none_without_seed() {
        let mut t = OfiTracker::new();
        assert!(t.update(dec!(99), dec!(10), dec!(101), dec!(10)).is_none());
    }

    #[test]
    fn second_update_after_seed_emits_observation() {
        let mut t = tracker_seeded();
        let obs = t.update(dec!(99), dec!(15), dec!(101), dec!(10));
        // Bid unchanged price, qty grew by 5 → e_b = +5.
        // Ask unchanged → e_a = 0. OFI = 5 − 0 = 5.
        assert_eq!(obs, Some(dec!(5)));
    }

    #[test]
    fn bid_moves_up_contributes_new_bid_qty() {
        let mut t = tracker_seeded();
        // Bid moves from 99 → 100, new qty = 7.
        // e_b = +7 (all of the new bid qty).
        // Ask unchanged → e_a = 0.
        // OFI = 7.
        let obs = t.update(dec!(100), dec!(7), dec!(101), dec!(10));
        assert_eq!(obs, Some(dec!(7)));
    }

    #[test]
    fn bid_moves_down_contributes_negative_old_qty() {
        let mut t = tracker_seeded();
        // Bid drops 99 → 98. e_b = −10 (all of old bid disappeared).
        // Ask unchanged → e_a = 0.
        // OFI = −10.
        let obs = t.update(dec!(98), dec!(5), dec!(101), dec!(10));
        assert_eq!(obs, Some(dec!(-10)));
    }

    #[test]
    fn ask_moves_down_contributes_negative_new_ask_qty() {
        let mut t = tracker_seeded();
        // Bid unchanged → e_b = 0.
        // Ask drops 101 → 100, new qty = 8 → e_a = −8.
        // OFI = 0 − (−8) = +8 (ask falling = upward pressure).
        let obs = t.update(dec!(99), dec!(10), dec!(100), dec!(8));
        assert_eq!(obs, Some(dec!(8)));
    }

    #[test]
    fn ask_moves_up_contributes_positive_old_ask_qty() {
        let mut t = tracker_seeded();
        // Bid unchanged → e_b = 0.
        // Ask moves 101 → 102 → e_a = +10 (old qty).
        // OFI = 0 − 10 = −10 (ask rising = ask depth vanished = still
        // downward pressure per CKS sign convention).
        let obs = t.update(dec!(99), dec!(10), dec!(102), dec!(4));
        assert_eq!(obs, Some(dec!(-10)));
    }

    #[test]
    fn ask_unchanged_qty_delta_carries_sign() {
        let mut t = tracker_seeded();
        // Ask unchanged price, qty grew 10 → 15 → e_a = +5.
        // OFI = 0 − 5 = −5 (more resting ask depth = downward pressure).
        let obs = t.update(dec!(99), dec!(10), dec!(101), dec!(15));
        assert_eq!(obs, Some(dec!(-5)));
    }

    #[test]
    fn symmetric_no_change_gives_zero_ofi() {
        let mut t = tracker_seeded();
        let obs = t.update(dec!(99), dec!(10), dec!(101), dec!(10));
        assert_eq!(obs, Some(dec!(0)));
    }

    #[test]
    fn aggressive_bid_plus_ask_drop_compounds_positive() {
        let mut t = tracker_seeded();
        // Bid moves up 99 → 100, qty 12 → e_b = +12.
        // Ask moves down 101 → 100.5, qty 6 → e_a = −6.
        // OFI = 12 − (−6) = +18. Net upward pressure.
        let obs = t.update(dec!(100), dec!(12), dec!(100.5), dec!(6));
        assert_eq!(obs, Some(dec!(18)));
    }

    #[test]
    fn hand_verified_cks_fixture() {
        // Walk through a four-step book sequence and assert
        // the full OFI series matches a hand calculation.
        let mut t = OfiTracker::new();
        t.seed(dec!(100), dec!(10), dec!(101), dec!(10));

        // Event 1: bid qty grows 10 → 14 (no price move).
        // e_b = +4, e_a = 0 → OFI = +4.
        assert_eq!(
            t.update(dec!(100), dec!(14), dec!(101), dec!(10)),
            Some(dec!(4))
        );

        // Event 2: ask drops 101 → 100.5 with qty 6.
        // e_b = 0, e_a = −6 → OFI = +6.
        assert_eq!(
            t.update(dec!(100), dec!(14), dec!(100.5), dec!(6)),
            Some(dec!(6))
        );

        // Event 3: bid falls 100 → 99 with qty 5.
        // e_b = −14 (prev bid qty), e_a = 0 → OFI = −14.
        assert_eq!(
            t.update(dec!(99), dec!(5), dec!(100.5), dec!(6)),
            Some(dec!(-14))
        );

        // Event 4: ask rises 100.5 → 101 with qty 20.
        // e_b = 0, e_a = +6 (prev ask qty) → OFI = −6.
        assert_eq!(
            t.update(dec!(99), dec!(5), dec!(101), dec!(20)),
            Some(dec!(-6))
        );
    }

    #[test]
    fn prev_snapshot_accessor_tracks_state() {
        let mut t = OfiTracker::new();
        assert!(t.prev_snapshot().is_none());
        t.seed(dec!(99), dec!(10), dec!(101), dec!(10));
        assert_eq!(
            t.prev_snapshot(),
            Some(L1Snapshot {
                bid_px: dec!(99),
                bid_qty: dec!(10),
                ask_px: dec!(101),
                ask_qty: dec!(10),
            })
        );
        t.update(dec!(100), dec!(15), dec!(101), dec!(8));
        let s = t.prev_snapshot().unwrap();
        assert_eq!(s.bid_px, dec!(100));
        assert_eq!(s.bid_qty, dec!(15));
        assert_eq!(s.ask_qty, dec!(8));
    }

    #[test]
    fn reset_drops_cached_state() {
        let mut t = tracker_seeded();
        t.reset();
        assert!(t.prev_snapshot().is_none());
        assert!(t.update(dec!(99), dec!(10), dec!(101), dec!(10)).is_none());
    }
}
