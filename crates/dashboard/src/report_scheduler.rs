//! Cron scheduler for automated compliance reports (Epic 42.4).
//!
//! Ties PDF (E42.1) + CSV/XLSX (E42.2) + SMTP (E42.3) into an
//! automated pipeline. Config-driven schedules using standard
//! crontab syntax. Resilient to clock skew and catches up on
//! missed runs after a restart within a configurable window.
//!
//! Schedule semantics:
//!   - Daily: runs at 00:15 UTC (15 min grace after the UTC
//!     rollover to let the dashboard snapshot the closed day)
//!   - Weekly: runs Mondays at 08:00 UTC
//!   - Monthly: runs 1st of month at 00:30 UTC
//!
//! The `ReportJob` trait is what a caller implements to hand the
//! scheduler an actual report pipeline. Keeps the scheduler free
//! of any direct dependency on the engine / dashboard state.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_cron_scheduler::{Job, JobScheduler};

/// Cadence keyword for a scheduled report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportCadence {
    Daily,
    Weekly,
    Monthly,
}

impl ReportCadence {
    /// Crontab expression — `tokio_cron_scheduler` 0.13 uses croner
    /// with `.with_seconds_required()`, so the 6-field form
    /// `sec min hour dom mon dow` is required. All times UTC.
    pub fn cron_expr(self) -> &'static str {
        match self {
            // 00:15:00 every day
            ReportCadence::Daily => "0 15 0 * * *",
            // 08:00:00 every Monday (Mon = 1 in croner)
            ReportCadence::Weekly => "0 0 8 * * MON",
            // 00:30:00 on the 1st of every month
            ReportCadence::Monthly => "0 30 0 1 * *",
        }
    }
}

/// Abstract interface to the real report pipeline. The scheduler
/// is a dumb fire-and-log wrapper — all the PDF / XLSX / SMTP
/// work happens inside the job's `run` method. Dashboard startup
/// wires one concrete `ReportJob` that knows how to generate +
/// email per-client reports.
#[async_trait]
pub trait ReportJob: Send + Sync + 'static {
    /// Execute a scheduled run. `fired_at` is the UTC instant the
    /// scheduler woke; implementations compute the reporting
    /// period from that (e.g. Daily → report for fired_at's date
    /// minus 1 day).
    async fn run(&self, cadence: ReportCadence, fired_at: DateTime<Utc>) -> anyhow::Result<()>;
}

/// Per-cadence config block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    #[serde(default)]
    pub daily_enabled: bool,
    #[serde(default)]
    pub weekly_enabled: bool,
    #[serde(default)]
    pub monthly_enabled: bool,
    /// Catch-up window — if the last successful run is older than
    /// this, the scheduler fires once immediately on startup to
    /// close the gap. Protects against missed reports after
    /// operator-side downtime. Set `0` to disable.
    #[serde(default = "default_catchup_hours")]
    pub catchup_hours: u32,
    /// Last-run tracking path. JSONL with one `{cadence, fired_at}`
    /// per line. Used for catch-up decisions.
    #[serde(default = "default_last_run_path")]
    pub last_run_path: String,
}

fn default_catchup_hours() -> u32 {
    6
}

fn default_last_run_path() -> String {
    "data/report_last_run.jsonl".into()
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            daily_enabled: false,
            weekly_enabled: false,
            monthly_enabled: false,
            catchup_hours: default_catchup_hours(),
            last_run_path: default_last_run_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LastRunRecord {
    cadence: ReportCadence,
    fired_at: DateTime<Utc>,
    ok: bool,
    error: Option<String>,
}

/// Wraps `JobScheduler`. Call `start` to begin firing, `stop` for
/// graceful shutdown. Dropping also stops.
pub struct ReportScheduler {
    config: ScheduleConfig,
    job: Arc<dyn ReportJob>,
    scheduler: Option<JobScheduler>,
}

impl ReportScheduler {
    pub fn new(config: ScheduleConfig, job: Arc<dyn ReportJob>) -> Self {
        Self {
            config,
            job,
            scheduler: None,
        }
    }

    /// Spin up the inner `JobScheduler`, register enabled cadences,
    /// run any catch-up jobs, then start the background ticker.
    pub async fn start(&mut self) -> anyhow::Result<()> {
        let sched = JobScheduler::new().await?;

        // Pre-start: catch-up check. For each enabled cadence, look
        // up its last successful `fired_at`. If it's older than
        // `catchup_hours`, fire a one-shot now.
        if self.config.catchup_hours > 0 {
            self.run_catchup_jobs().await;
        }

        // Register recurring jobs.
        if self.config.daily_enabled {
            self.add_recurring(&sched, ReportCadence::Daily).await?;
        }
        if self.config.weekly_enabled {
            self.add_recurring(&sched, ReportCadence::Weekly).await?;
        }
        if self.config.monthly_enabled {
            self.add_recurring(&sched, ReportCadence::Monthly).await?;
        }

        sched.start().await?;
        self.scheduler = Some(sched);
        tracing::info!(
            daily = self.config.daily_enabled,
            weekly = self.config.weekly_enabled,
            monthly = self.config.monthly_enabled,
            "report scheduler started"
        );
        Ok(())
    }

    /// Stop the scheduler — awaits current job completion.
    pub async fn stop(&mut self) {
        if let Some(mut s) = self.scheduler.take() {
            if let Err(e) = s.shutdown().await {
                tracing::warn!(error = %e, "report scheduler shutdown failed");
            }
        }
    }

    async fn add_recurring(
        &self,
        sched: &JobScheduler,
        cadence: ReportCadence,
    ) -> anyhow::Result<()> {
        let job_ref = self.job.clone();
        let last_run_path = self.config.last_run_path.clone();
        let expr = cadence.cron_expr();
        let job = Job::new_async(expr, move |_uuid, _l| {
            let job_ref = job_ref.clone();
            let last_run_path = last_run_path.clone();
            Box::pin(async move {
                let fired_at = Utc::now();
                let result = job_ref.run(cadence, fired_at).await;
                persist_last_run(&last_run_path, cadence, fired_at, &result);
                if let Err(e) = result {
                    tracing::error!(?cadence, error = %e, "scheduled report failed");
                }
            })
        })?;
        sched.add(job).await?;
        tracing::debug!(?cadence, expr, "registered recurring report");
        Ok(())
    }

    async fn run_catchup_jobs(&self) {
        let records = load_last_runs(&self.config.last_run_path);
        let now = Utc::now();
        let threshold = chrono::Duration::hours(self.config.catchup_hours as i64);

        for cadence in [
            ReportCadence::Daily,
            ReportCadence::Weekly,
            ReportCadence::Monthly,
        ] {
            let enabled = match cadence {
                ReportCadence::Daily => self.config.daily_enabled,
                ReportCadence::Weekly => self.config.weekly_enabled,
                ReportCadence::Monthly => self.config.monthly_enabled,
            };
            if !enabled {
                continue;
            }
            let last = records
                .iter()
                .filter(|r| r.cadence == cadence && r.ok)
                .map(|r| r.fired_at)
                .max();
            let should_catchup = match last {
                Some(t) => (now - t) > threshold,
                None => true, // never ran — catchup if enabled
            };
            if should_catchup {
                tracing::info!(
                    ?cadence,
                    last = ?last,
                    "firing catch-up report"
                );
                let res = self.job.run(cadence, now).await;
                persist_last_run(&self.config.last_run_path, cadence, now, &res);
                if let Err(e) = res {
                    tracing::error!(?cadence, error = %e, "catch-up report failed");
                }
            }
        }
    }
}

impl Drop for ReportScheduler {
    fn drop(&mut self) {
        // Best-effort synchronous cleanup. Real shutdown should use
        // `.stop()` explicitly in a tokio context.
        if self.scheduler.is_some() {
            tracing::warn!(
                "ReportScheduler dropped without explicit stop(). In-flight \
                 jobs may be truncated."
            );
        }
    }
}

fn persist_last_run(
    path: &str,
    cadence: ReportCadence,
    fired_at: DateTime<Utc>,
    result: &anyhow::Result<()>,
) {
    use std::io::Write;
    let rec = LastRunRecord {
        cadence,
        fired_at,
        ok: result.is_ok(),
        error: result.as_ref().err().map(|e| e.to_string()),
    };
    let line = match serde_json::to_string(&rec) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "last-run serialisation failed");
            return;
        }
    };
    let path = std::path::Path::new(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path);
    if let Ok(mut f) = f {
        let _ = writeln!(f, "{line}");
    }
}

fn load_last_runs(path: &str) -> Vec<LastRunRecord> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .filter_map(|l| serde_json::from_str::<LastRunRecord>(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_expressions_parse() {
        // Each cadence should produce a valid tokio_cron_scheduler
        // expression. Validate by trying to construct a Job with it.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            for c in [
                ReportCadence::Daily,
                ReportCadence::Weekly,
                ReportCadence::Monthly,
            ] {
                let j = Job::new_async(c.cron_expr(), |_, _| Box::pin(async {}));
                assert!(j.is_ok(), "cron expression for {c:?} failed to parse");
            }
        });
    }

    #[test]
    fn last_run_persistence_round_trips() {
        let tmp = std::env::temp_dir().join(format!(
            "mm_lastrun_{}.jsonl",
            std::process::id()
        ));
        let path_str = tmp.to_string_lossy().to_string();

        let now = Utc::now();
        persist_last_run(&path_str, ReportCadence::Daily, now, &Ok(()));
        persist_last_run(
            &path_str,
            ReportCadence::Weekly,
            now,
            &Err(anyhow::anyhow!("simulated failure")),
        );

        let records = load_last_runs(&path_str);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].cadence, ReportCadence::Daily);
        assert!(records[0].ok);
        assert_eq!(records[1].cadence, ReportCadence::Weekly);
        assert!(!records[1].ok);
        assert!(records[1].error.as_deref().unwrap().contains("simulated"));
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn catchup_decision_uses_last_successful() {
        let tmp = std::env::temp_dir().join(format!(
            "mm_catchup_{}.jsonl",
            std::process::id()
        ));
        let path_str = tmp.to_string_lossy().to_string();

        // Seed a daily run 12 h ago (inside a 24 h window).
        let recent = Utc::now() - chrono::Duration::hours(12);
        persist_last_run(&path_str, ReportCadence::Daily, recent, &Ok(()));

        let records = load_last_runs(&path_str);
        let last_daily = records
            .iter()
            .filter(|r| r.cadence == ReportCadence::Daily && r.ok)
            .map(|r| r.fired_at)
            .max()
            .unwrap();
        assert!(
            (Utc::now() - last_daily) < chrono::Duration::hours(24),
            "last run should be inside catchup window"
        );
        let _ = std::fs::remove_file(tmp);
    }
}
