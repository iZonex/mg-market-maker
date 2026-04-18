//! Token loan agreement persistence (Epic 2: Token Lending).
//!
//! Tracks loan agreements between MM firms and token projects.
//! Agreements are persisted as append-only JSONL so the full
//! loan history is reconstructable for MiCA compliance.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use tracing::info;

/// A token loan agreement between the MM and a token project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoanAgreement {
    /// Unique loan ID (UUID).
    pub id: String,
    /// Trading pair symbol (e.g., "BTCUSDT").
    pub symbol: String,
    /// Owning client ID (Epic 1 linkage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Loan terms.
    pub terms: LoanTerms,
    /// Return schedule with installments.
    pub schedule: ReturnSchedule,
    /// Current loan status.
    pub status: LoanStatus,
    /// When the agreement was created.
    pub created_at: DateTime<Utc>,
    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Loan terms — what was borrowed and at what cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoanTerms {
    /// Total tokens loaned in base asset.
    pub total_qty: Decimal,
    /// Cost basis per token for PnL attribution.
    #[serde(default)]
    pub cost_basis_per_token: Decimal,
    /// Annual cost rate (%) — amortized daily into PnL.
    #[serde(default)]
    pub annual_rate_pct: Decimal,
    /// Optional call option strike price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_strike: Option<Decimal>,
    /// Optional option expiry date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_expiry: Option<NaiveDate>,
    /// Loan start date.
    pub start_date: NaiveDate,
    /// Loan end date.
    pub end_date: NaiveDate,
    /// Token project / counterparty name.
    #[serde(default)]
    pub counterparty: String,
}

/// Lifecycle status of a loan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoanStatus {
    Active,
    PartiallyReturned,
    Returned,
    Defaulted,
}

/// Return schedule — sequence of installments.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReturnSchedule {
    pub installments: Vec<ReturnInstallment>,
}

/// A single return installment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReturnInstallment {
    /// Due date for this installment.
    pub due_date: NaiveDate,
    /// Quantity to return in base asset.
    pub qty: Decimal,
    /// Status of this installment.
    pub status: InstallmentStatus,
    /// When this installment was completed (if done).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// Status of a single installment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallmentStatus {
    Pending,
    Completed,
    Overdue,
}

impl LoanAgreement {
    /// Total quantity already returned across all installments.
    pub fn returned_qty(&self) -> Decimal {
        self.schedule
            .installments
            .iter()
            .filter(|i| i.status == InstallmentStatus::Completed)
            .map(|i| i.qty)
            .sum()
    }

    /// Remaining quantity to be returned.
    pub fn remaining_qty(&self) -> Decimal {
        self.terms.total_qty - self.returned_qty()
    }

    /// Utilization percentage: `returned / total * 100`.
    /// Returns 0% for a fully active (nothing returned) loan.
    pub fn return_progress_pct(&self) -> Decimal {
        if self.terms.total_qty.is_zero() {
            return dec!(100);
        }
        (self.returned_qty() / self.terms.total_qty) * dec!(100)
    }

    /// Daily amortization cost for PnL attribution.
    pub fn daily_cost(&self) -> Decimal {
        if self.terms.annual_rate_pct.is_zero() {
            return Decimal::ZERO;
        }
        let loan_value = self.terms.total_qty * self.terms.cost_basis_per_token;
        loan_value * self.terms.annual_rate_pct / dec!(100) / dec!(365)
    }

    /// Mark an installment as completed.
    pub fn complete_installment(&mut self, idx: usize) -> bool {
        if let Some(inst) = self.schedule.installments.get_mut(idx) {
            inst.status = InstallmentStatus::Completed;
            inst.completed_at = Some(Utc::now());
            self.updated_at = Utc::now();
            // Update loan status.
            let all_done = self
                .schedule
                .installments
                .iter()
                .all(|i| i.status == InstallmentStatus::Completed);
            if all_done {
                self.status = LoanStatus::Returned;
            } else {
                self.status = LoanStatus::PartiallyReturned;
            }
            true
        } else {
            false
        }
    }

    /// Check and mark overdue installments based on current date.
    pub fn check_overdue(&mut self, today: NaiveDate) {
        for inst in &mut self.schedule.installments {
            if inst.status == InstallmentStatus::Pending && inst.due_date < today {
                inst.status = InstallmentStatus::Overdue;
            }
        }
    }
}

/// JSONL-based loan store. Append-only for creates, atomic
/// rewrite for updates. Consistent with the audit/fills pattern.
pub struct LoanStore;

impl LoanStore {
    /// Load all loan agreements from a JSONL file.
    pub fn load(path: &Path) -> Vec<LoanAgreement> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let mut agreements = HashMap::new();
        for line in content.lines() {
            if let Ok(agreement) = serde_json::from_str::<LoanAgreement>(line) {
                // Last-write-wins for same ID (updates overwrite).
                agreements.insert(agreement.id.clone(), agreement);
            }
        }
        agreements.into_values().collect()
    }

    /// Append a loan agreement to the JSONL file.
    pub fn append(path: &Path, agreement: &LoanAgreement) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let json = serde_json::to_string(agreement).map_err(std::io::Error::other)?;
        writeln!(file, "{}", json)?;
        info!(id = %agreement.id, symbol = %agreement.symbol, "loan agreement persisted");
        Ok(())
    }

    /// Atomic rewrite: saves all agreements, replacing file content.
    pub fn save_all(path: &Path, agreements: &[LoanAgreement]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut file = std::fs::File::create(&tmp)?;
            for agreement in agreements {
                let json = serde_json::to_string(agreement).map_err(std::io::Error::other)?;
                writeln!(file, "{}", json)?;
            }
            file.flush()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mm_test_loan_{name}_{}.jsonl", std::process::id()))
    }

    struct CleanupFile(PathBuf);
    impl Drop for CleanupFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn sample_agreement() -> LoanAgreement {
        let now = Utc::now();
        LoanAgreement {
            id: "loan-001".into(),
            symbol: "BTCUSDT".into(),
            client_id: Some("acme".into()),
            terms: LoanTerms {
                total_qty: dec!(100),
                cost_basis_per_token: dec!(50000),
                annual_rate_pct: dec!(10),
                option_strike: None,
                option_expiry: None,
                start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
                counterparty: "TokenProject".into(),
            },
            schedule: ReturnSchedule {
                installments: vec![
                    ReturnInstallment {
                        due_date: NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
                        qty: dec!(50),
                        status: InstallmentStatus::Pending,
                        completed_at: None,
                    },
                    ReturnInstallment {
                        due_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
                        qty: dec!(50),
                        status: InstallmentStatus::Pending,
                        completed_at: None,
                    },
                ],
            },
            status: LoanStatus::Active,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn serde_roundtrip() {
        let agreement = sample_agreement();
        let json = serde_json::to_string(&agreement).unwrap();
        let parsed: LoanAgreement = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "loan-001");
        assert_eq!(parsed.terms.total_qty, dec!(100));
        assert_eq!(parsed.schedule.installments.len(), 2);
    }

    #[test]
    fn jsonl_store_roundtrip() {
        let p = tmp_path("roundtrip");
        let _c = CleanupFile(p.clone());

        let a = sample_agreement();
        LoanStore::append(&p, &a).unwrap();

        let loaded = LoanStore::load(&p);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "loan-001");
    }

    #[test]
    fn load_nonexistent_file_returns_empty() {
        let p = tmp_path("nonexistent");
        let loaded = LoanStore::load(&p);
        assert!(loaded.is_empty());
    }

    #[test]
    fn last_write_wins_on_duplicate_id() {
        let p = tmp_path("duplicate");
        let _c = CleanupFile(p.clone());

        let mut a = sample_agreement();
        LoanStore::append(&p, &a).unwrap();
        a.status = LoanStatus::PartiallyReturned;
        LoanStore::append(&p, &a).unwrap();

        let loaded = LoanStore::load(&p);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status, LoanStatus::PartiallyReturned);
    }

    #[test]
    fn complete_installment_updates_status() {
        let mut a = sample_agreement();
        assert_eq!(a.status, LoanStatus::Active);

        a.complete_installment(0);
        assert_eq!(a.status, LoanStatus::PartiallyReturned);
        assert_eq!(a.returned_qty(), dec!(50));

        a.complete_installment(1);
        assert_eq!(a.status, LoanStatus::Returned);
        assert_eq!(a.remaining_qty(), dec!(0));
    }

    #[test]
    fn daily_cost_calculation() {
        let a = sample_agreement();
        let daily = a.daily_cost();
        // 100 BTC * 50000 USDT * 10% / 365 ≈ 1369.86
        assert!(daily > dec!(1369) && daily < dec!(1370));
    }

    #[test]
    fn check_overdue_marks_past_installments() {
        let mut a = sample_agreement();
        let future = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
        a.check_overdue(future);
        assert_eq!(
            a.schedule.installments[0].status,
            InstallmentStatus::Overdue
        );
        assert_eq!(
            a.schedule.installments[1].status,
            InstallmentStatus::Pending
        );
    }

    #[test]
    fn atomic_save_all_replaces_content() {
        let p = tmp_path("save_all");
        let _c = CleanupFile(p.clone());

        let a1 = sample_agreement();
        let mut a2 = sample_agreement();
        a2.id = "loan-002".into();
        LoanStore::save_all(&p, &[a1, a2]).unwrap();

        let loaded = LoanStore::load(&p);
        assert_eq!(loaded.len(), 2);
    }

    // ── Property-based tests (Epic 19) ───────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn qty_strat()(raw in 1i64..1_000_000i64) -> Decimal {
            Decimal::new(raw, 4)
        }
    }
    prop_compose! {
        fn rate_strat()(pct in 1i64..10_000i64) -> Decimal {
            Decimal::new(pct, 2)
        }
    }

    fn mk_loan(installments: Vec<Decimal>, annual_rate: Decimal) -> LoanAgreement {
        let now = Utc::now();
        let total: Decimal = installments.iter().copied().sum();
        LoanAgreement {
            id: "x".into(),
            symbol: "T".into(),
            client_id: None,
            terms: LoanTerms {
                total_qty: total,
                cost_basis_per_token: dec!(100),
                annual_rate_pct: annual_rate,
                option_strike: None,
                option_expiry: None,
                start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
                counterparty: "TP".into(),
            },
            schedule: ReturnSchedule {
                installments: installments
                    .into_iter()
                    .map(|q| ReturnInstallment {
                        due_date: NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
                        qty: q,
                        status: InstallmentStatus::Pending,
                        completed_at: None,
                    })
                    .collect(),
            },
            status: LoanStatus::Active,
            created_at: now,
            updated_at: now,
        }
    }

    proptest! {
        /// returned_qty + remaining_qty == total_qty at every
        /// point — no phantom balance between the two.
        #[test]
        fn returned_plus_remaining_equals_total(
            qtys in proptest::collection::vec(qty_strat(), 1..8),
            completed_mask in proptest::collection::vec(proptest::bool::ANY, 1..8),
        ) {
            let n = qtys.len().min(completed_mask.len());
            let mut loan = mk_loan(qtys[..n].to_vec(), dec!(10));
            for (i, &done) in completed_mask[..n].iter().enumerate() {
                if done {
                    loan.complete_installment(i);
                }
            }
            prop_assert_eq!(
                loan.returned_qty() + loan.remaining_qty(),
                loan.terms.total_qty
            );
        }

        /// return_progress_pct ∈ [0, 100] for every valid state.
        #[test]
        fn return_progress_pct_bounded(
            qtys in proptest::collection::vec(qty_strat(), 1..8),
            complete_all in proptest::bool::ANY,
        ) {
            let n = qtys.len();
            let mut loan = mk_loan(qtys, dec!(10));
            if complete_all {
                for i in 0..n {
                    loan.complete_installment(i);
                }
            }
            let pct = loan.return_progress_pct();
            prop_assert!(pct >= dec!(0));
            prop_assert!(pct <= dec!(100));
        }

        /// Completing every installment flips status to Returned.
        /// Catches a regression where partial completion rules
        /// leak into the full-return branch.
        #[test]
        fn complete_all_yields_returned_status(
            qtys in proptest::collection::vec(qty_strat(), 1..10),
        ) {
            let n = qtys.len();
            let mut loan = mk_loan(qtys, dec!(10));
            for i in 0..n {
                prop_assert!(loan.complete_installment(i));
            }
            prop_assert_eq!(loan.status, LoanStatus::Returned);
            prop_assert_eq!(loan.returned_qty(), loan.terms.total_qty);
            prop_assert_eq!(loan.remaining_qty(), dec!(0));
        }

        /// daily_cost is linear in total_qty * cost_basis * rate
        /// / (100 * 365). Catches a constant drift.
        #[test]
        fn daily_cost_formula_holds(
            qtys in proptest::collection::vec(qty_strat(), 1..5),
            rate in rate_strat(),
        ) {
            let loan = mk_loan(qtys, rate);
            let expected = loan.terms.total_qty * loan.terms.cost_basis_per_token
                * rate / dec!(100) / dec!(365);
            prop_assert_eq!(loan.daily_cost(), expected);
        }

        /// check_overdue marks every PENDING installment whose
        /// due_date is before `today` as Overdue; Completed
        /// installments are never touched.
        #[test]
        fn check_overdue_marks_stale_pending_only(
            qtys in proptest::collection::vec(qty_strat(), 1..5),
            completed_mask in proptest::collection::vec(proptest::bool::ANY, 1..5),
        ) {
            let n = qtys.len().min(completed_mask.len());
            let mut loan = mk_loan(qtys[..n].to_vec(), dec!(10));
            for (i, &done) in completed_mask[..n].iter().enumerate() {
                if done { loan.complete_installment(i); }
            }
            // Fast-forward past every due_date.
            loan.check_overdue(NaiveDate::from_ymd_opt(2030, 1, 1).unwrap());
            for (i, inst) in loan.schedule.installments.iter().enumerate() {
                if completed_mask.get(i).copied().unwrap_or(false) {
                    prop_assert_eq!(inst.status, InstallmentStatus::Completed);
                } else {
                    prop_assert_eq!(inst.status, InstallmentStatus::Overdue);
                }
            }
        }
    }
}
