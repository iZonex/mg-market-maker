//! Loan utilization tracking and return schedule management
//! (Epic 2: Token Lending, items 2.2 + 2.3).
//!
//! Tracks how much of each loan's inventory is currently deployed,
//! alerts when utilization approaches limits, and surfaces upcoming
//! and overdue return installments.

use chrono::{NaiveDate, Utc};
use mm_persistence::loan::{InstallmentStatus, LoanAgreement, LoanStatus};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use std::collections::HashMap;

/// Alert fired when loan utilization approaches the limit.
#[derive(Debug, Clone, Serialize)]
pub struct UtilizationAlert {
    pub symbol: String,
    pub loan_id: String,
    pub current_position: Decimal,
    pub loan_total: Decimal,
    pub utilization_pct: Decimal,
}

/// Upcoming return info.
#[derive(Debug, Clone, Serialize)]
pub struct UpcomingReturn {
    pub symbol: String,
    pub loan_id: String,
    pub installment_idx: usize,
    pub due_date: NaiveDate,
    pub qty: Decimal,
    pub days_until_due: i64,
}

/// Overdue return info.
#[derive(Debug, Clone, Serialize)]
pub struct OverdueReturn {
    pub symbol: String,
    pub loan_id: String,
    pub installment_idx: usize,
    pub due_date: NaiveDate,
    pub qty: Decimal,
    pub days_overdue: i64,
}

/// Tracks loan utilization across all agreements.
pub struct LoanUtilizationTracker {
    agreements: HashMap<String, LoanAgreement>,
    /// Alert fires when utilization exceeds this % of total loan.
    alert_threshold_pct: Decimal,
}

impl LoanUtilizationTracker {
    pub fn new(alert_threshold_pct: Decimal) -> Self {
        Self {
            agreements: HashMap::new(),
            alert_threshold_pct,
        }
    }

    /// Register or update a loan agreement.
    pub fn upsert_agreement(&mut self, agreement: LoanAgreement) {
        self.agreements.insert(agreement.id.clone(), agreement);
    }

    /// Remove a loan agreement.
    pub fn remove_agreement(&mut self, loan_id: &str) {
        self.agreements.remove(loan_id);
    }

    /// Get all tracked agreements.
    pub fn agreements(&self) -> Vec<&LoanAgreement> {
        self.agreements.values().collect()
    }

    /// Compute utilization for a symbol. `current_position` is
    /// the absolute inventory in base asset. Returns `None` if
    /// no active loan exists for the symbol.
    pub fn utilization_pct(&self, symbol: &str, current_position: Decimal) -> Option<Decimal> {
        let agreement = self
            .agreements
            .values()
            .find(|a| a.symbol == symbol && a.status != LoanStatus::Returned)?;
        if agreement.terms.total_qty.is_zero() {
            return Some(dec!(100));
        }
        Some((current_position.abs() / agreement.terms.total_qty) * dec!(100))
    }

    /// Check all loans for utilization alerts. Returns alerts
    /// for loans where utilization exceeds the threshold.
    pub fn check_alerts(&self, positions: &HashMap<String, Decimal>) -> Vec<UtilizationAlert> {
        let mut alerts = Vec::new();
        for agreement in self.agreements.values() {
            if agreement.status == LoanStatus::Returned {
                continue;
            }
            let position = positions
                .get(&agreement.symbol)
                .copied()
                .unwrap_or(Decimal::ZERO)
                .abs();
            if agreement.terms.total_qty.is_zero() {
                continue;
            }
            let util = (position / agreement.terms.total_qty) * dec!(100);
            if util >= self.alert_threshold_pct {
                alerts.push(UtilizationAlert {
                    symbol: agreement.symbol.clone(),
                    loan_id: agreement.id.clone(),
                    current_position: position,
                    loan_total: agreement.terms.total_qty,
                    utilization_pct: util,
                });
            }
        }
        alerts
    }

    /// Returns installments due within `within_days` from today.
    pub fn upcoming_returns(&self, within_days: i64) -> Vec<UpcomingReturn> {
        let today = Utc::now().date_naive();
        let mut results = Vec::new();
        for agreement in self.agreements.values() {
            if agreement.status == LoanStatus::Returned {
                continue;
            }
            for (idx, inst) in agreement.schedule.installments.iter().enumerate() {
                if inst.status != InstallmentStatus::Pending {
                    continue;
                }
                let days = (inst.due_date - today).num_days();
                if days >= 0 && days <= within_days {
                    results.push(UpcomingReturn {
                        symbol: agreement.symbol.clone(),
                        loan_id: agreement.id.clone(),
                        installment_idx: idx,
                        due_date: inst.due_date,
                        qty: inst.qty,
                        days_until_due: days,
                    });
                }
            }
        }
        results.sort_by_key(|r| r.days_until_due);
        results
    }

    /// Returns all overdue installments.
    pub fn overdue_returns(&self) -> Vec<OverdueReturn> {
        let today = Utc::now().date_naive();
        let mut results = Vec::new();
        for agreement in self.agreements.values() {
            if agreement.status == LoanStatus::Returned {
                continue;
            }
            for (idx, inst) in agreement.schedule.installments.iter().enumerate() {
                if inst.status == InstallmentStatus::Overdue
                    || (inst.status == InstallmentStatus::Pending && inst.due_date < today)
                {
                    let days = (today - inst.due_date).num_days();
                    results.push(OverdueReturn {
                        symbol: agreement.symbol.clone(),
                        loan_id: agreement.id.clone(),
                        installment_idx: idx,
                        due_date: inst.due_date,
                        qty: inst.qty,
                        days_overdue: days,
                    });
                }
            }
        }
        results.sort_by(|a, b| b.days_overdue.cmp(&a.days_overdue));
        results
    }

    /// Mark a specific installment as completed.
    pub fn mark_return_completed(&mut self, loan_id: &str, installment_idx: usize) -> bool {
        if let Some(agreement) = self.agreements.get_mut(loan_id) {
            agreement.complete_installment(installment_idx)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_persistence::loan::{LoanTerms, ReturnInstallment, ReturnSchedule};

    fn make_agreement(
        id: &str,
        symbol: &str,
        qty: Decimal,
        due_days_from_now: i64,
    ) -> LoanAgreement {
        let today = Utc::now().date_naive();
        LoanAgreement {
            id: id.into(),
            symbol: symbol.into(),
            client_id: None,
            terms: LoanTerms {
                total_qty: qty,
                cost_basis_per_token: dec!(50000),
                annual_rate_pct: dec!(10),
                option_strike: None,
                option_expiry: None,
                start_date: today,
                end_date: today + chrono::Duration::days(365),
                counterparty: "TestProject".into(),
            },
            schedule: ReturnSchedule {
                installments: vec![ReturnInstallment {
                    due_date: today + chrono::Duration::days(due_days_from_now),
                    qty,
                    status: InstallmentStatus::Pending,
                    completed_at: None,
                }],
            },
            status: LoanStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn utilization_pct_basic() {
        let mut tracker = LoanUtilizationTracker::new(dec!(90));
        tracker.upsert_agreement(make_agreement("L1", "BTCUSDT", dec!(10), 30));
        let util = tracker.utilization_pct("BTCUSDT", dec!(5)).unwrap();
        assert_eq!(util, dec!(50));
    }

    #[test]
    fn utilization_pct_unknown_symbol_returns_none() {
        let tracker = LoanUtilizationTracker::new(dec!(90));
        assert!(tracker.utilization_pct("UNKNOWN", dec!(5)).is_none());
    }

    #[test]
    fn alert_fires_when_above_threshold() {
        let mut tracker = LoanUtilizationTracker::new(dec!(80));
        tracker.upsert_agreement(make_agreement("L1", "BTCUSDT", dec!(10), 30));
        let mut positions = HashMap::new();
        positions.insert("BTCUSDT".into(), dec!(9)); // 90% > 80%
        let alerts = tracker.check_alerts(&positions);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].utilization_pct, dec!(90));
    }

    #[test]
    fn no_alert_when_under_threshold() {
        let mut tracker = LoanUtilizationTracker::new(dec!(90));
        tracker.upsert_agreement(make_agreement("L1", "BTCUSDT", dec!(10), 30));
        let mut positions = HashMap::new();
        positions.insert("BTCUSDT".into(), dec!(5)); // 50% < 90%
        let alerts = tracker.check_alerts(&positions);
        assert!(alerts.is_empty());
    }

    #[test]
    fn upcoming_returns_within_window() {
        let mut tracker = LoanUtilizationTracker::new(dec!(90));
        tracker.upsert_agreement(make_agreement("L1", "BTCUSDT", dec!(10), 7));
        tracker.upsert_agreement(make_agreement("L2", "ETHUSDT", dec!(100), 60));
        let upcoming = tracker.upcoming_returns(30);
        assert_eq!(upcoming.len(), 1);
        assert_eq!(upcoming[0].symbol, "BTCUSDT");
    }

    #[test]
    fn overdue_returns_detected() {
        let mut tracker = LoanUtilizationTracker::new(dec!(90));
        tracker.upsert_agreement(make_agreement("L1", "BTCUSDT", dec!(10), -5));
        let overdue = tracker.overdue_returns();
        assert_eq!(overdue.len(), 1);
        assert_eq!(overdue[0].days_overdue, 5);
    }

    #[test]
    fn mark_return_completed_updates_status() {
        let mut tracker = LoanUtilizationTracker::new(dec!(90));
        tracker.upsert_agreement(make_agreement("L1", "BTCUSDT", dec!(10), 30));
        assert!(tracker.mark_return_completed("L1", 0));
        let agreement = tracker.agreements.get("L1").unwrap();
        assert_eq!(agreement.status, LoanStatus::Returned);
    }

    #[test]
    fn mark_return_nonexistent_returns_false() {
        let mut tracker = LoanUtilizationTracker::new(dec!(90));
        assert!(!tracker.mark_return_completed("NOPE", 0));
    }

    #[test]
    fn returned_loans_excluded_from_alerts() {
        let mut tracker = LoanUtilizationTracker::new(dec!(50));
        let mut a = make_agreement("L1", "BTCUSDT", dec!(10), 30);
        a.status = LoanStatus::Returned;
        tracker.upsert_agreement(a);
        let mut positions = HashMap::new();
        positions.insert("BTCUSDT".into(), dec!(10));
        let alerts = tracker.check_alerts(&positions);
        assert!(alerts.is_empty());
    }
}
