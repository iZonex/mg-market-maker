//! Inventory-vs-wallet drift reconciliation.
//!
//! `InventoryManager` tracks the **net position delta** since
//! the engine started by accumulating `on_fill` callbacks.
//! The ground truth for a spot position, however, lives in the
//! wallet balance — every fill settles into a real
//! transfer of base asset between wallets. If the fill stream
//! drops a message (listen-key reconnect, WS frame loss, a
//! parser bug), `InventoryManager.inventory()` silently drifts
//! from the wallet balance and stays wrong until operator
//! intervention.
//!
//! This module detects that drift by snapshotting the wallet
//! balance at first reconcile as a baseline and comparing the
//! **wallet delta** against the **tracked inventory delta** on
//! every subsequent reconcile cycle. A mismatch greater than a
//! configurable tolerance produces a [`DriftReport`] that the
//! engine routes into the audit trail and (optionally) into a
//! force-correction hook on `InventoryManager`.
//!
//! See `ROADMAP.md` P0.2 for the design rationale and the
//! pre-conditions the engine-level wiring assumes.

use rust_decimal::Decimal;

/// A single drift check outcome. `None`-returning calls (before
/// the baseline is established, or when drift is inside
/// tolerance) are represented by the absence of the report;
/// this struct is only produced when there *is* a mismatch to
/// surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftReport {
    /// Base asset of the symbol being checked (e.g. `"BTC"`).
    pub asset: String,
    /// Wallet total at the baseline reconcile. Held so the
    /// report is self-contained when emitted into audit JSONL.
    pub baseline_wallet: Decimal,
    /// Wallet total at the current reconcile.
    pub current_wallet: Decimal,
    /// `current_wallet - baseline_wallet`. What the ground
    /// truth says we should be holding relative to startup.
    pub expected_inventory: Decimal,
    /// `InventoryManager.inventory()` at this reconcile cycle.
    pub tracked_inventory: Decimal,
    /// `expected_inventory - tracked_inventory`. A positive
    /// drift means the wallet has more than the tracker thinks
    /// — we missed a BUY fill (or an external deposit). A
    /// negative drift means the wallet has less — we missed a
    /// SELL fill (or an external withdrawal).
    pub drift: Decimal,
    /// `true` if the reconciler was configured with
    /// `auto_correct = true` and the caller should now call
    /// `InventoryManager::force_reset_inventory_to(expected)`.
    /// When `false` the drift is advisory — alert only.
    pub corrected: bool,
}

/// Drift reconciler — one instance per engine / symbol.
///
/// On the first `check` call the reconciler stores the wallet
/// total as the **baseline**. Every subsequent call computes
/// drift as
///
/// ```text
/// drift = (wallet_now - wallet_baseline) − inventory_tracked
/// ```
///
/// and emits a [`DriftReport`] iff `|drift| > tolerance`.
///
/// The tolerance is absolute (not relative): it should be at
/// least a few lot sizes to absorb rounding noise from fees
/// paid in the base asset. A sensible default is
/// `max(lot_size * 5, 1e-6)`.
#[derive(Debug, Clone)]
pub struct InventoryDriftReconciler {
    asset: String,
    baseline_wallet: Option<Decimal>,
    tolerance: Decimal,
    auto_correct: bool,
}

impl InventoryDriftReconciler {
    /// Create a new reconciler for the given base `asset`, with
    /// an absolute `tolerance` (same units as the asset — e.g.
    /// for `"BTC"` the tolerance is in BTC).
    ///
    /// `auto_correct = false` is the safe default: drift
    /// reports are emitted but the caller only alerts, it does
    /// not silently rewrite inventory state. Operators enable
    /// `auto_correct = true` once they trust the drift source
    /// and want the system to self-heal.
    ///
    /// # Panics
    /// Panics if `tolerance < 0`.
    pub fn new(asset: impl Into<String>, tolerance: Decimal, auto_correct: bool) -> Self {
        assert!(
            tolerance >= Decimal::ZERO,
            "drift tolerance must be non-negative"
        );
        Self {
            asset: asset.into(),
            baseline_wallet: None,
            tolerance,
            auto_correct,
        }
    }

    /// Asset this reconciler watches.
    pub fn asset(&self) -> &str {
        &self.asset
    }

    /// Current tolerance.
    pub fn tolerance(&self) -> Decimal {
        self.tolerance
    }

    /// Is auto-correction enabled?
    pub fn auto_correct(&self) -> bool {
        self.auto_correct
    }

    /// Baseline wallet total captured on the first `check`
    /// call. `None` before the first check.
    pub fn baseline_wallet(&self) -> Option<Decimal> {
        self.baseline_wallet
    }

    /// Run one drift check.
    ///
    /// - `current_wallet` is the wallet total (free + locked)
    ///   for the base asset in the spot wallet at this tick.
    /// - `tracked_inventory` is `InventoryManager::inventory()`.
    ///
    /// Returns `None` if this is the first call (baseline
    /// established, nothing to compare yet) or if the drift is
    /// within tolerance.
    ///
    /// Returns `Some(DriftReport)` when the absolute drift
    /// exceeds the configured tolerance.
    pub fn check(
        &mut self,
        current_wallet: Decimal,
        tracked_inventory: Decimal,
    ) -> Option<DriftReport> {
        let baseline = match self.baseline_wallet {
            Some(b) => b,
            None => {
                // First call — capture baseline, nothing to
                // compare against yet.
                self.baseline_wallet = Some(current_wallet);
                return None;
            }
        };
        let expected_inventory = current_wallet - baseline;
        let drift = expected_inventory - tracked_inventory;
        if drift.abs() <= self.tolerance {
            return None;
        }
        Some(DriftReport {
            asset: self.asset.clone(),
            baseline_wallet: baseline,
            current_wallet,
            expected_inventory,
            tracked_inventory,
            drift,
            corrected: self.auto_correct,
        })
    }

    /// Reset the baseline. Useful after a deliberate manual
    /// operator intervention (deposit, withdrawal, transfer)
    /// that should **not** count as drift.
    pub fn reset_baseline(&mut self, new_baseline: Decimal) {
        self.baseline_wallet = Some(new_baseline);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn reconciler() -> InventoryDriftReconciler {
        InventoryDriftReconciler::new("BTC", dec!(0.0001), false)
    }

    /// First call always returns `None` and captures the
    /// baseline.
    #[test]
    fn first_call_captures_baseline_and_returns_none() {
        let mut r = reconciler();
        assert!(r.check(dec!(1), dec!(0)).is_none());
        assert_eq!(r.baseline_wallet(), Some(dec!(1)));
    }

    /// A perfectly-tracked sequence of fills produces no drift.
    #[test]
    fn in_sync_wallet_and_tracker_emit_no_report() {
        let mut r = reconciler();
        assert!(r.check(dec!(1), dec!(0)).is_none());
        // Bought 0.3 → wallet = 1.3, tracker = 0.3.
        assert!(r.check(dec!(1.3), dec!(0.3)).is_none());
        // Sold 0.1 → wallet = 1.2, tracker = 0.2.
        assert!(r.check(dec!(1.2), dec!(0.2)).is_none());
    }

    /// A positive drift (missed BUY fill) surfaces a report
    /// with a positive `drift` value.
    #[test]
    fn missed_buy_produces_positive_drift() {
        let mut r = reconciler();
        r.check(dec!(1), dec!(0));
        // Wallet says 1.2 (real state) but tracker thinks 0.1
        // because a 0.1 BTC buy fill was missed.
        let report = r.check(dec!(1.2), dec!(0.1)).expect("drift expected");
        assert_eq!(report.asset, "BTC");
        assert_eq!(report.baseline_wallet, dec!(1));
        assert_eq!(report.current_wallet, dec!(1.2));
        assert_eq!(report.expected_inventory, dec!(0.2));
        assert_eq!(report.tracked_inventory, dec!(0.1));
        assert_eq!(report.drift, dec!(0.1));
        assert!(!report.corrected);
    }

    /// A negative drift (missed SELL fill) surfaces a negative
    /// `drift` value.
    #[test]
    fn missed_sell_produces_negative_drift() {
        let mut r = reconciler();
        r.check(dec!(1), dec!(0));
        // Wallet says 0.8 (real state) but tracker thinks 0.1
        // because a 0.3 BTC sell fill was missed (0.1 − 0.3 =
        // −0.2 net, wallet = 1 − 0.2 = 0.8).
        let report = r.check(dec!(0.8), dec!(0.1)).expect("drift expected");
        assert_eq!(report.drift, dec!(-0.3));
    }

    /// Drift within tolerance is absorbed silently — fee noise
    /// (fee paid in base) should not trip the detector.
    #[test]
    fn drift_within_tolerance_does_not_alert() {
        let mut r = InventoryDriftReconciler::new("BTC", dec!(0.001), false);
        r.check(dec!(1), dec!(0));
        // 0.0005 BTC drift (half the tolerance) — silent.
        assert!(r.check(dec!(1.2995), dec!(0.3)).is_none());
    }

    /// `auto_correct = true` flips the `corrected` flag on the
    /// emitted report so the caller knows to force-reset the
    /// tracker.
    #[test]
    fn auto_correct_flag_propagates_into_report() {
        let mut r = InventoryDriftReconciler::new("BTC", dec!(0.0001), true);
        r.check(dec!(1), dec!(0));
        let report = r.check(dec!(1.2), dec!(0.1)).expect("drift expected");
        assert!(report.corrected);
    }

    /// Resetting the baseline shifts the comparison point —
    /// the caller is expected to reset the tracker
    /// concurrently (or to pass a baseline that matches the
    /// current tracker state). After the reset the next
    /// check compares against the new baseline.
    #[test]
    fn reset_baseline_shifts_comparison_point() {
        let mut r = reconciler();
        r.check(dec!(1), dec!(0));
        let _ = r.check(dec!(1.5), dec!(0.5));
        // Operator intervened: deposited 0.2 BTC externally.
        // New wallet = 1.7, tracker still = 0.5 (unchanged by
        // the external deposit). Baseline gets shifted up by
        // 0.2 so the next check sees no drift.
        r.reset_baseline(dec!(1.2));
        assert!(r.check(dec!(1.7), dec!(0.5)).is_none());
    }

    /// Negative tolerance panics — contract check.
    #[test]
    #[should_panic]
    fn negative_tolerance_panics() {
        let _ = InventoryDriftReconciler::new("BTC", dec!(-0.1), false);
    }

    /// Zero tolerance is allowed and produces reports on any
    /// non-zero drift.
    #[test]
    fn zero_tolerance_catches_every_drift() {
        let mut r = InventoryDriftReconciler::new("BTC", Decimal::ZERO, false);
        r.check(dec!(1), dec!(0));
        assert!(r.check(dec!(1.0001), dec!(0)).is_some());
    }
}
