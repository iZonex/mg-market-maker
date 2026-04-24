//! Block B — concrete `ReportJob` implementation.
//!
//! The `report_scheduler` crate ships the cron loop but defers
//! "what to generate" to an abstract trait so the core library
//! stays free of the dashboard's state + exporter deps. This
//! module is the one workspace impl of that trait: it walks
//! `DashboardState`, builds a `MonthlyReportData` for the
//! target window, and renders the bundle to disk in the formats
//! the operator asked for.
//!
//! Layout on disk:
//! ```text
//! data/reports/daily/2026-04-18/summary.{json,csv,xlsx,pdf}
//! data/reports/weekly/2026-W16/summary.{json,csv,xlsx,pdf}
//! data/reports/monthly/2026-04/summary.{json,csv,xlsx,pdf}
//! ```
//!
//! The archive shipper (Block C) uploads `data/reports/daily/*`
//! to S3 on its next tick, so subscribing to `ship_daily_reports
//! = true` is the hand-off contract: the scheduler writes, the
//! shipper uploads.

use crate::monthly_report::build_monthly_report;
use crate::report_export::{build_manifest, render_csv, render_xlsx};
use crate::report_scheduler::{ReportCadence, ReportJob};
use crate::state::DashboardState;
use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use std::path::PathBuf;

pub struct BuiltinReportJob {
    state: DashboardState,
    /// Base output directory. Typically `data/reports`.
    root: PathBuf,
    /// Default client id applied when the deployment does not
    /// configure multi-tenant clients. Matches the synthetic
    /// legacy default used across the rest of the codebase.
    default_client: String,
}

impl BuiltinReportJob {
    pub fn new(state: DashboardState, root: PathBuf) -> Self {
        Self {
            state,
            root,
            default_client: "default".into(),
        }
    }

    fn period_for(
        cadence: ReportCadence,
        fired_at: DateTime<Utc>,
    ) -> (NaiveDate, NaiveDate, String) {
        let today = fired_at.date_naive();
        match cadence {
            // Daily for the closed UTC day (fired_at is
            // 00:15 UTC, so we report on `today - 1`).
            ReportCadence::Daily => {
                let d = today - Duration::days(1);
                (d, d, d.to_string())
            }
            // Weekly: previous Mon..Sun (ISO week).
            ReportCadence::Weekly => {
                // Scheduler fires Monday 08:00 UTC. Previous week
                // = Mon..Sun preceding that.
                let this_mon =
                    today - Duration::days(today.weekday().num_days_from_monday() as i64);
                let prev_mon = this_mon - Duration::days(7);
                let prev_sun = this_mon - Duration::days(1);
                let iso = prev_mon.iso_week();
                let folder = format!("{}-W{:02}", iso.year(), iso.week());
                (prev_mon, prev_sun, folder)
            }
            // Monthly: previous full calendar month.
            ReportCadence::Monthly => {
                let first_this =
                    NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
                let last_prev = first_this - Duration::days(1);
                let first_prev = NaiveDate::from_ymd_opt(last_prev.year(), last_prev.month(), 1)
                    .unwrap_or(last_prev);
                let folder = format!("{:04}-{:02}", first_prev.year(), first_prev.month());
                (first_prev, last_prev, folder)
            }
        }
    }

    fn cadence_folder(cadence: ReportCadence) -> &'static str {
        match cadence {
            ReportCadence::Daily => "daily",
            ReportCadence::Weekly => "weekly",
            ReportCadence::Monthly => "monthly",
        }
    }
}

#[async_trait]
impl ReportJob for BuiltinReportJob {
    async fn run(&self, cadence: ReportCadence, fired_at: DateTime<Utc>) -> anyhow::Result<()> {
        let label = Self::cadence_folder(cadence);
        crate::metrics::SCHEDULER_RUNS_TOTAL
            .with_label_values(&[label])
            .inc();

        match self.run_inner(cadence, fired_at).await {
            Ok(()) => {
                crate::metrics::SCHEDULER_LAST_SUCCESS_TS
                    .with_label_values(&[label])
                    .set(chrono::Utc::now().timestamp() as f64);
                Ok(())
            }
            Err(e) => {
                crate::metrics::SCHEDULER_FAILURES_TOTAL
                    .with_label_values(&[label])
                    .inc();
                Err(e)
            }
        }
    }
}

impl BuiltinReportJob {
    async fn run_inner(
        &self,
        cadence: ReportCadence,
        fired_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let (from, to, folder) = Self::period_for(cadence, fired_at);
        let out_dir = self.root.join(Self::cadence_folder(cadence)).join(&folder);
        tokio::fs::create_dir_all(&out_dir).await?;

        let audit_path = self.state.audit_log_path();
        let data = build_monthly_report(
            &self.state,
            None,
            &self.default_client,
            from,
            to,
            audit_path.as_deref(),
        )?;
        let secret = self.state.report_secret();
        let manifest = build_manifest(&data, &["json", "csv", "xlsx", "pdf"], &secret)?;

        tokio::fs::write(
            out_dir.join("summary.json"),
            serde_json::to_string_pretty(&data)?,
        )
        .await?;
        tokio::fs::write(out_dir.join("summary.csv"), render_csv(&data)).await?;
        tokio::fs::write(out_dir.join("summary.xlsx"), render_xlsx(&data, &manifest)?).await?;
        tokio::fs::write(
            out_dir.join("summary.pdf"),
            crate::pdf_report::render_pdf(&data, &manifest)?,
        )
        .await?;
        tokio::fs::write(
            out_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)?,
        )
        .await?;

        tracing::info!(
            ?cadence,
            ?from,
            ?to,
            dir = ?out_dir,
            "scheduled report generated"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monthly_period_is_previous_calendar_month() {
        let fired = chrono::NaiveDate::from_ymd_opt(2026, 5, 1)
            .unwrap()
            .and_hms_opt(0, 30, 0)
            .unwrap()
            .and_utc();
        let (from, to, folder) = BuiltinReportJob::period_for(ReportCadence::Monthly, fired);
        assert_eq!(from, chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap());
        assert_eq!(to, chrono::NaiveDate::from_ymd_opt(2026, 4, 30).unwrap());
        assert_eq!(folder, "2026-04");
    }

    #[test]
    fn daily_period_is_previous_utc_day() {
        let fired = chrono::NaiveDate::from_ymd_opt(2026, 4, 18)
            .unwrap()
            .and_hms_opt(0, 15, 0)
            .unwrap()
            .and_utc();
        let (from, to, folder) = BuiltinReportJob::period_for(ReportCadence::Daily, fired);
        assert_eq!(from, chrono::NaiveDate::from_ymd_opt(2026, 4, 17).unwrap());
        assert_eq!(to, chrono::NaiveDate::from_ymd_opt(2026, 4, 17).unwrap());
        assert_eq!(folder, "2026-04-17");
    }
}
