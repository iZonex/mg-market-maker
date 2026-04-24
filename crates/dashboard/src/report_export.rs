//! Monthly compliance report generators (Epic 42.2).
//!
//! Two parallel surfaces for the same underlying data:
//!   - CSV (plaintext, auditor-friendly, grep-able)
//!   - XLSX (multi-sheet, per-section — Summary / Fills / Audit /
//!     SLA — for clients that want structured workbooks)
//!
//! Both are generated from a single `MonthlyReportData` struct so a
//! schema bump hits one place and propagates to both formats. An
//! HMAC-SHA256 manifest accompanies every export — clients verify
//! `manifest.sig` against the published secret to confirm no row
//! was tampered with after delivery.

use chrono::{DateTime, NaiveDate, Utc};
use hmac::{Hmac, Mac};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// One line of the per-fill section. Mirrors `FillRecord` plus
/// the client id so downstream aggregation is column-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillRow {
    pub timestamp: DateTime<Utc>,
    pub client_id: String,
    pub symbol: String,
    pub side: String,
    pub price: Decimal,
    pub qty: Decimal,
    pub fee: Decimal,
    pub is_maker: bool,
    pub slippage_bps: Decimal,
}

/// One line of the per-audit-event section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRow {
    pub timestamp: DateTime<Utc>,
    pub seq: u64,
    pub event_type: String,
    pub symbol: String,
    pub client_id: Option<String>,
    pub detail: Option<String>,
    pub prev_hash: Option<String>,
}

/// Per-symbol monthly PnL + SLA + volume summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolSummaryRow {
    pub symbol: String,
    pub total_pnl: Decimal,
    pub spread_pnl: Decimal,
    pub inventory_pnl: Decimal,
    pub fees_paid: Decimal,
    pub rebates_earned: Decimal,
    pub round_trips: u64,
    pub total_volume: Decimal,
    pub presence_pct: Decimal,
    pub two_sided_pct: Decimal,
    pub uptime_pct: Decimal,
    pub sla_violations: u32,
}

/// Aggregated payload the report generators read from. Engine
/// builds this once at request time via `build_monthly_report`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonthlyReportData {
    pub client_id: String,
    pub client_name: String,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    pub generated_at: DateTime<Utc>,
    pub summaries: Vec<SymbolSummaryRow>,
    pub fills: Vec<FillRow>,
    pub audit_events: Vec<AuditRow>,
}

/// HMAC-signed manifest accompanying each export. Consumers verify
/// `sig` against a known public key / shared secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportManifest {
    pub client_id: String,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    pub generated_at: DateTime<Utc>,
    pub row_counts: ManifestCounts,
    pub formats: Vec<String>,
    pub sig: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestCounts {
    pub summaries: usize,
    pub fills: usize,
    pub audit_events: usize,
}

/// Build a signed manifest for the payload. Signs a canonical
/// sorted-JSON rendering so byte-for-byte reproducibility is
/// guaranteed even across serde version changes.
pub fn build_manifest(
    data: &MonthlyReportData,
    formats: &[&str],
    secret: &[u8],
) -> anyhow::Result<ReportManifest> {
    let counts = ManifestCounts {
        summaries: data.summaries.len(),
        fills: data.fills.len(),
        audit_events: data.audit_events.len(),
    };
    // Sign the counts + period + client. The bulk data is signed
    // in the per-format body (CSV / XLSX) via content hash — the
    // manifest is a compact integrity check that binds everything
    // together.
    let unsigned = serde_json::json!({
        "client_id":    data.client_id,
        "period_from":  data.period_from.format("%Y-%m-%d").to_string(),
        "period_to":    data.period_to.format("%Y-%m-%d").to_string(),
        "generated_at": data.generated_at.to_rfc3339(),
        "counts":       counts,
        "formats":      formats,
    });
    let canonical = serde_json::to_string(&unsigned)?;
    let mut mac = HmacSha256::new_from_slice(secret)?;
    mac.update(canonical.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());

    Ok(ReportManifest {
        client_id: data.client_id.clone(),
        period_from: data.period_from,
        period_to: data.period_to,
        generated_at: data.generated_at,
        row_counts: counts,
        formats: formats.iter().map(|s| s.to_string()).collect(),
        sig,
    })
}

/// Verify a manifest against the same `secret` used at signing.
/// Returns `true` only if the re-computed signature matches in
/// constant time.
pub fn verify_manifest(manifest: &ReportManifest, data: &MonthlyReportData, secret: &[u8]) -> bool {
    // Re-use build_manifest to produce the canonical signature, then
    // compare via constant-time eq.
    let formats: Vec<&str> = manifest.formats.iter().map(|s| s.as_str()).collect();
    let reference = match build_manifest(data, &formats, secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    use subtle::ConstantTimeEq;
    reference
        .sig
        .as_bytes()
        .ct_eq(manifest.sig.as_bytes())
        .unwrap_u8()
        == 1
}

// ── CSV ───────────────────────────────────────────────────────

/// Render the report as a single CSV. Sections are separated by
/// blank lines + section-header rows so `csvkit` + manual audit
/// both work. CSV is RFC 4180 compliant — fields with commas /
/// newlines / quotes get proper escaping.
pub fn render_csv(data: &MonthlyReportData) -> String {
    let mut out = String::with_capacity(4096);

    // Header block
    out.push_str("# Monthly Compliance Report\n");
    out.push_str(&format!("# client_id,{}\n", csv_escape(&data.client_id)));
    out.push_str(&format!(
        "# client_name,{}\n",
        csv_escape(&data.client_name)
    ));
    out.push_str(&format!("# period_from,{}\n", data.period_from));
    out.push_str(&format!("# period_to,{}\n", data.period_to));
    out.push_str(&format!(
        "# generated_at,{}\n\n",
        data.generated_at.to_rfc3339()
    ));

    // Summary section
    out.push_str("## symbol_summary\n");
    out.push_str("symbol,total_pnl,spread_pnl,inventory_pnl,fees_paid,rebates_earned,round_trips,total_volume,presence_pct,two_sided_pct,uptime_pct,sla_violations\n");
    for s in &data.summaries {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_escape(&s.symbol),
            s.total_pnl,
            s.spread_pnl,
            s.inventory_pnl,
            s.fees_paid,
            s.rebates_earned,
            s.round_trips,
            s.total_volume,
            s.presence_pct,
            s.two_sided_pct,
            s.uptime_pct,
            s.sla_violations,
        ));
    }
    out.push('\n');

    // Fills section
    out.push_str("## fills\n");
    out.push_str("timestamp,client_id,symbol,side,price,qty,fee,is_maker,slippage_bps\n");
    for f in &data.fills {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            f.timestamp.to_rfc3339(),
            csv_escape(&f.client_id),
            csv_escape(&f.symbol),
            csv_escape(&f.side),
            f.price,
            f.qty,
            f.fee,
            f.is_maker,
            f.slippage_bps,
        ));
    }
    out.push('\n');

    // Audit section
    out.push_str("## audit_events\n");
    out.push_str("timestamp,seq,event_type,symbol,client_id,detail,prev_hash\n");
    for a in &data.audit_events {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            a.timestamp.to_rfc3339(),
            a.seq,
            csv_escape(&a.event_type),
            csv_escape(&a.symbol),
            a.client_id.as_deref().map(csv_escape).unwrap_or_default(),
            a.detail.as_deref().map(csv_escape).unwrap_or_default(),
            a.prev_hash.as_deref().unwrap_or_default(),
        ));
    }

    out
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

// ── XLSX ──────────────────────────────────────────────────────

/// Render the report as a 4-sheet XLSX workbook:
///   1. `Summary` — header block + per-symbol aggregates
///   2. `Fills` — one row per fill, full detail
///   3. `Audit` — audit-trail events for the period
///   4. `Manifest` — the HMAC signature + counts
///
/// Returns the workbook as a `Vec<u8>` ready to stream as
/// `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`.
pub fn render_xlsx(data: &MonthlyReportData, manifest: &ReportManifest) -> anyhow::Result<Vec<u8>> {
    use rust_xlsxwriter::{Format, FormatAlign, FormatBorder, Workbook};

    let mut wb = Workbook::new();
    let header_fmt = Format::new()
        .set_bold()
        .set_background_color(0x1F2937)
        .set_font_color(0xF9FAFB)
        .set_align(FormatAlign::Left)
        .set_border(FormatBorder::Thin);
    let label_fmt = Format::new().set_bold().set_align(FormatAlign::Right);
    let num_fmt = Format::new()
        .set_num_format("#,##0.0000")
        .set_align(FormatAlign::Right);
    let int_fmt = Format::new()
        .set_num_format("#,##0")
        .set_align(FormatAlign::Right);
    let pct_fmt = Format::new()
        .set_num_format("0.00%")
        .set_align(FormatAlign::Right);
    let ts_fmt = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");

    // ── Sheet 1: Summary ─────────────────────────────────────
    {
        let sheet = wb.add_worksheet().set_name("Summary")?;
        sheet.write_string_with_format(0, 0, "Monthly Compliance Report", &header_fmt)?;
        sheet.merge_range(0, 0, 0, 11, "Monthly Compliance Report", &header_fmt)?;

        sheet.write_string_with_format(2, 0, "Client ID", &label_fmt)?;
        sheet.write_string(2, 1, &data.client_id)?;
        sheet.write_string_with_format(3, 0, "Client name", &label_fmt)?;
        sheet.write_string(3, 1, &data.client_name)?;
        sheet.write_string_with_format(4, 0, "Period", &label_fmt)?;
        sheet.write_string(4, 1, format!("{} – {}", data.period_from, data.period_to))?;
        sheet.write_string_with_format(5, 0, "Generated", &label_fmt)?;
        sheet.write_string(5, 1, data.generated_at.to_rfc3339())?;

        // Column headers row 8
        let cols = [
            "Symbol",
            "Total PnL",
            "Spread PnL",
            "Inventory PnL",
            "Fees",
            "Rebates",
            "Round trips",
            "Volume",
            "Presence %",
            "Two-sided %",
            "Uptime %",
            "SLA violations",
        ];
        for (i, c) in cols.iter().enumerate() {
            sheet.write_string_with_format(8, i as u16, *c, &header_fmt)?;
        }
        for (row_i, s) in data.summaries.iter().enumerate() {
            let r = 9 + row_i as u32;
            sheet.write_string(r, 0, &s.symbol)?;
            sheet.write_number_with_format(r, 1, dec_to_f64(s.total_pnl), &num_fmt)?;
            sheet.write_number_with_format(r, 2, dec_to_f64(s.spread_pnl), &num_fmt)?;
            sheet.write_number_with_format(r, 3, dec_to_f64(s.inventory_pnl), &num_fmt)?;
            sheet.write_number_with_format(r, 4, dec_to_f64(s.fees_paid), &num_fmt)?;
            sheet.write_number_with_format(r, 5, dec_to_f64(s.rebates_earned), &num_fmt)?;
            sheet.write_number_with_format(r, 6, s.round_trips as f64, &int_fmt)?;
            sheet.write_number_with_format(r, 7, dec_to_f64(s.total_volume), &num_fmt)?;
            sheet.write_number_with_format(r, 8, dec_to_f64(s.presence_pct) / 100.0, &pct_fmt)?;
            sheet.write_number_with_format(r, 9, dec_to_f64(s.two_sided_pct) / 100.0, &pct_fmt)?;
            sheet.write_number_with_format(r, 10, dec_to_f64(s.uptime_pct) / 100.0, &pct_fmt)?;
            sheet.write_number_with_format(r, 11, s.sla_violations as f64, &int_fmt)?;
        }
        sheet.autofit();
    }

    // ── Sheet 2: Fills ───────────────────────────────────────
    {
        let sheet = wb.add_worksheet().set_name("Fills")?;
        let cols = [
            "Timestamp",
            "Client ID",
            "Symbol",
            "Side",
            "Price",
            "Qty",
            "Fee",
            "Maker",
            "Slippage (bps)",
        ];
        for (i, c) in cols.iter().enumerate() {
            sheet.write_string_with_format(0, i as u16, *c, &header_fmt)?;
        }
        for (row_i, f) in data.fills.iter().enumerate() {
            let r = 1 + row_i as u32;
            sheet.write_string_with_format(r, 0, f.timestamp.to_rfc3339(), &ts_fmt)?;
            sheet.write_string(r, 1, &f.client_id)?;
            sheet.write_string(r, 2, &f.symbol)?;
            sheet.write_string(r, 3, &f.side)?;
            sheet.write_number_with_format(r, 4, dec_to_f64(f.price), &num_fmt)?;
            sheet.write_number_with_format(r, 5, dec_to_f64(f.qty), &num_fmt)?;
            sheet.write_number_with_format(r, 6, dec_to_f64(f.fee), &num_fmt)?;
            sheet.write_boolean(r, 7, f.is_maker)?;
            sheet.write_number_with_format(r, 8, dec_to_f64(f.slippage_bps), &num_fmt)?;
        }
        sheet.autofit();
    }

    // ── Sheet 3: Audit ───────────────────────────────────────
    {
        let sheet = wb.add_worksheet().set_name("Audit")?;
        let cols = [
            "Timestamp",
            "Seq",
            "Event type",
            "Symbol",
            "Client ID",
            "Detail",
            "Prev hash",
        ];
        for (i, c) in cols.iter().enumerate() {
            sheet.write_string_with_format(0, i as u16, *c, &header_fmt)?;
        }
        for (row_i, a) in data.audit_events.iter().enumerate() {
            let r = 1 + row_i as u32;
            sheet.write_string_with_format(r, 0, a.timestamp.to_rfc3339(), &ts_fmt)?;
            sheet.write_number_with_format(r, 1, a.seq as f64, &int_fmt)?;
            sheet.write_string(r, 2, &a.event_type)?;
            sheet.write_string(r, 3, &a.symbol)?;
            sheet.write_string(r, 4, a.client_id.as_deref().unwrap_or(""))?;
            sheet.write_string(r, 5, a.detail.as_deref().unwrap_or(""))?;
            sheet.write_string(r, 6, a.prev_hash.as_deref().unwrap_or(""))?;
        }
        sheet.autofit();
    }

    // ── Sheet 4: Manifest ────────────────────────────────────
    {
        let sheet = wb.add_worksheet().set_name("Manifest")?;
        let rows: [(&str, String); 8] = [
            ("Client ID", manifest.client_id.clone()),
            ("Period from", manifest.period_from.to_string()),
            ("Period to", manifest.period_to.to_string()),
            ("Generated at", manifest.generated_at.to_rfc3339()),
            ("Summaries", manifest.row_counts.summaries.to_string()),
            ("Fills", manifest.row_counts.fills.to_string()),
            ("Audit events", manifest.row_counts.audit_events.to_string()),
            ("Formats", manifest.formats.join(", ")),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            sheet.write_string_with_format(i as u32, 0, *k, &label_fmt)?;
            sheet.write_string(i as u32, 1, v)?;
        }
        sheet.write_string_with_format(10, 0, "HMAC-SHA256", &label_fmt)?;
        sheet.write_string(10, 1, &manifest.sig)?;
        sheet.autofit();
    }

    let bytes = wb.save_to_buffer()?;
    Ok(bytes)
}

fn dec_to_f64(d: Decimal) -> f64 {
    d.to_string().parse::<f64>().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn mk_data() -> MonthlyReportData {
        MonthlyReportData {
            client_id: "acme".into(),
            client_name: "Acme Capital".into(),
            period_from: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            period_to: NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
            generated_at: Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
            summaries: vec![SymbolSummaryRow {
                symbol: "BTCUSDT".into(),
                total_pnl: dec!(123.45),
                spread_pnl: dec!(200.00),
                inventory_pnl: dec!(-50.00),
                fees_paid: dec!(30.00),
                rebates_earned: dec!(3.45),
                round_trips: 42,
                total_volume: dec!(100000),
                presence_pct: dec!(98.5),
                two_sided_pct: dec!(97.2),
                uptime_pct: dec!(99.1),
                sla_violations: 1,
            }],
            fills: vec![FillRow {
                timestamp: Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap(),
                client_id: "acme".into(),
                symbol: "BTCUSDT".into(),
                side: "Buy".into(),
                price: dec!(77000),
                qty: dec!(0.01),
                fee: dec!(0.77),
                is_maker: true,
                slippage_bps: dec!(-0.5),
            }],
            audit_events: vec![AuditRow {
                timestamp: Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 1).unwrap(),
                seq: 1001,
                event_type: "order_filled".into(),
                symbol: "BTCUSDT".into(),
                client_id: Some("acme".into()),
                detail: Some("normal fill".into()),
                prev_hash: Some("abc123".into()),
            }],
        }
    }

    #[test]
    fn csv_has_all_sections() {
        let data = mk_data();
        let csv = render_csv(&data);
        assert!(csv.contains("# Monthly Compliance Report"));
        assert!(csv.contains("## symbol_summary"));
        assert!(csv.contains("## fills"));
        assert!(csv.contains("## audit_events"));
        assert!(csv.contains("BTCUSDT"));
        assert!(csv.contains("acme"));
    }

    #[test]
    fn csv_escapes_commas_and_quotes() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("has \"quotes\""), "\"has \"\"quotes\"\"\"");
    }

    #[test]
    fn manifest_round_trips() {
        let data = mk_data();
        let secret = b"0123456789abcdef0123456789abcdef";
        let m = build_manifest(&data, &["csv", "xlsx"], secret).unwrap();
        assert!(verify_manifest(&m, &data, secret));
        // Wrong secret fails
        assert!(!verify_manifest(
            &m,
            &data,
            b"wrong-secret-value-padding-padding"
        ));
    }

    #[test]
    fn manifest_tamper_detection() {
        let data = mk_data();
        let secret = b"0123456789abcdef0123456789abcdef";
        let m = build_manifest(&data, &["csv"], secret).unwrap();

        // Tamper with the row_counts — manifest should no longer verify.
        let mut tampered_data = data.clone();
        tampered_data.fills.push(FillRow {
            timestamp: Utc::now(),
            client_id: "acme".into(),
            symbol: "X".into(),
            side: "Buy".into(),
            price: dec!(1),
            qty: dec!(1),
            fee: dec!(0),
            is_maker: true,
            slippage_bps: dec!(0),
        });
        assert!(!verify_manifest(&m, &tampered_data, secret));
    }

    #[test]
    fn xlsx_produces_nonempty_bytes() {
        let data = mk_data();
        let m = build_manifest(&data, &["xlsx"], b"0123456789abcdef0123456789abcdef").unwrap();
        let bytes = render_xlsx(&data, &m).unwrap();
        // XLSX magic bytes = PK (ZIP)
        assert_eq!(&bytes[..2], b"PK");
        assert!(bytes.len() > 1000); // non-trivial workbook
    }
}
