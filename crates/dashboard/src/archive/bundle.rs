//! On-demand compliance bundle assembler.
//!
//! `build_zip` returns a single ZIP with everything an auditor
//! needs for the period:
//!
//! ```text
//! summary.json     — MonthlyReportData
//! summary.csv      — render_csv
//! summary.xlsx     — render_xlsx (HMAC-signed manifest baked in)
//! summary.pdf      — render_pdf
//! fills.jsonl      — all fills in the period, one per line
//! audit.jsonl      — hash-chained audit events in the period
//! manifest.json    — HMAC-signed manifest over summary counts
//! README.txt       — period / client / generator / verification instructions
//! ```
//!
//! The in-memory zip stays under a few MB for typical monthly
//! bundles, so there's no streaming path yet — if exports ever
//! routinely exceed 100 MB we switch to
//! `async_zip::tokio::write::ZipFileWriter` with a multipart
//! upload.

use crate::monthly_report::build_monthly_report;
use crate::report_export::{
    build_manifest, render_csv, render_xlsx, MonthlyReportData, ReportManifest,
};
use crate::state::DashboardState;
use chrono::NaiveDate;
use std::io::{Cursor, Write};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub struct BundleRequest<'a> {
    pub state: &'a DashboardState,
    pub client_id: Option<&'a str>,
    pub client_name: &'a str,
    pub from: NaiveDate,
    pub to: NaiveDate,
}

pub struct BundleOutput {
    pub bytes: Vec<u8>,
    pub data: MonthlyReportData,
    pub manifest: ReportManifest,
}

pub fn build_zip(req: BundleRequest<'_>) -> anyhow::Result<BundleOutput> {
    let audit_path = req.state.audit_log_path();
    let data = build_monthly_report(
        req.state,
        req.client_id,
        req.client_name,
        req.from,
        req.to,
        audit_path.as_deref(),
    )?;

    let secret = req.state.report_secret();
    let formats = ["json", "csv", "xlsx", "pdf"];
    let manifest = build_manifest(&data, &formats, &secret)?;

    let mut buf = Vec::with_capacity(256 * 1024);
    {
        let cursor = Cursor::new(&mut buf);
        let mut zip = ZipWriter::new(cursor);
        let opt = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        zip.start_file("summary.json", opt)?;
        zip.write_all(serde_json::to_string_pretty(&data)?.as_bytes())?;

        zip.start_file("summary.csv", opt)?;
        zip.write_all(render_csv(&data).as_bytes())?;

        zip.start_file("summary.xlsx", opt)?;
        zip.write_all(&render_xlsx(&data, &manifest)?)?;

        zip.start_file("summary.pdf", opt)?;
        zip.write_all(&crate::pdf_report::render_pdf(&data, &manifest)?)?;

        zip.start_file("fills.jsonl", opt)?;
        for row in &data.fills {
            zip.write_all(serde_json::to_string(row)?.as_bytes())?;
            zip.write_all(b"\n")?;
        }

        zip.start_file("audit.jsonl", opt)?;
        for row in &data.audit_events {
            zip.write_all(serde_json::to_string(row)?.as_bytes())?;
            zip.write_all(b"\n")?;
        }

        // Epic H Phase 3 — strategy-graph provenance. Regulators
        // reconstructing a minute of trades need the authored graph
        // for every hash that ever went live in the period. We ship:
        //   strategy_graphs/deploys.jsonl     — DeployRecords in range
        //   strategy_graphs/snapshots/{hash}.json — canonical JSON
        // so a single bundle is enough to join audit events (which
        // carry the same hash) back to the intent.
        if let Some(store) = req.state.strategy_graph_store() {
            if let Ok(records) = store.deploys() {
                let from_ts = req.from.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc());
                let to_ts = req
                    .to
                    .succ_opt()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|dt| dt.and_utc());
                let in_range: Vec<_> = records
                    .into_iter()
                    .filter(|r| {
                        from_ts.is_some_and(|f| r.deployed_at >= f)
                            && to_ts.is_some_and(|t| r.deployed_at < t)
                    })
                    .collect();
                zip.start_file("strategy_graphs/deploys.jsonl", opt)?;
                for rec in &in_range {
                    zip.write_all(serde_json::to_string(rec)?.as_bytes())?;
                    zip.write_all(b"\n")?;
                }
                // Deduplicate hashes so the bundle is compact.
                let mut seen = std::collections::HashSet::new();
                for rec in &in_range {
                    if !seen.insert((rec.name.clone(), rec.hash.clone())) {
                        continue;
                    }
                    if let Ok(graph) = store.load_by_hash(&rec.name, &rec.hash) {
                        let body = serde_json::to_string_pretty(&graph).unwrap_or_default();
                        let path =
                            format!("strategy_graphs/snapshots/{hash}.json", hash = rec.hash);
                        zip.start_file(path, opt)?;
                        zip.write_all(body.as_bytes())?;
                    }
                }
            }
        }

        zip.start_file("manifest.json", opt)?;
        zip.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;

        zip.start_file("README.txt", opt)?;
        zip.write_all(readme_text(&data, &manifest).as_bytes())?;

        zip.finish()?;
    }

    Ok(BundleOutput {
        bytes: buf,
        data,
        manifest,
    })
}

fn readme_text(data: &MonthlyReportData, manifest: &ReportManifest) -> String {
    format!(
        "Market Maker compliance bundle\n\
         ================================\n\
         client_id      : {cid}\n\
         client_name    : {cname}\n\
         period_from    : {from}\n\
         period_to      : {to}\n\
         generated_at   : {gen}\n\
         symbols        : {n_sym}\n\
         fills          : {n_fill}\n\
         audit_events   : {n_aud}\n\
         \n\
         Integrity verification\n\
         ----------------------\n\
         manifest.json  : HMAC-SHA256 signature over canonical JSON of\n\
                          (client_id, period, counts, formats).\n\
                          Use `verify_manifest(manifest, bundle_data, secret)`\n\
                          (see crates/dashboard/src/report_export.rs) to\n\
                          re-compute and compare in constant time.\n\
         audit.jsonl    : each line is hash-chained via `prev_hash`. Verify\n\
                          by recomputing SHA-256(serialised prior line) and\n\
                          matching `prev_hash` on the next line.\n\
         \n\
         manifest.sig   : {sig}\n\
        ",
        cid = data.client_id,
        cname = data.client_name,
        from = data.period_from,
        to = data.period_to,
        gen = data.generated_at.to_rfc3339(),
        n_sym = data.summaries.len(),
        n_fill = data.fills.len(),
        n_aud = data.audit_events.len(),
        sig = manifest.sig,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DashboardState;

    #[test]
    fn bundle_is_nonempty_zip_with_expected_entries() {
        let state = DashboardState::new();
        let from = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
        let out = build_zip(BundleRequest {
            state: &state,
            client_id: None,
            client_name: "Test",
            from,
            to,
        })
        .expect("build zip");
        assert!(
            out.bytes.len() > 100,
            "zip body should not be trivially small"
        );
        // ZIP magic
        assert_eq!(&out.bytes[0..4], b"PK\x03\x04");
        assert_eq!(out.manifest.formats.len(), 4);
    }
}
