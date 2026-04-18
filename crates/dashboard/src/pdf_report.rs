//! PDF daily + monthly report generator (Epic 42.1).
//!
//! Uses `printpdf`'s `PdfDocument::from_html` pipeline — the report
//! body lives as an HTML template so layout / styling can be
//! iterated by copy-editing without touching Rust, and the same
//! template round-trips through the browser during development.
//! No external tooling (wkhtmltopdf / chromium) — pure Rust.
//!
//! The generator takes the `MonthlyReportData` that already feeds
//! CSV/XLSX (Epic 42.2) so there is a single-source-of-truth for
//! every rendered format.

use printpdf::{GeneratePdfOptions, PdfDocument, PdfSaveOptions};
use std::collections::BTreeMap;

use crate::report_export::{MonthlyReportData, ReportManifest};

/// Render a daily or monthly report as a PDF byte blob.
///
/// Contract:
///   - Returns a complete PDF starting with the `%PDF-` magic
///     so the HTTP layer can stream it verbatim with
///     `Content-Type: application/pdf`.
///   - Includes the HMAC signature from `manifest.sig` in the
///     footer — clients verify against the published secret.
///   - Silently drops stylesheet-compilation warnings (the
///     top-level printpdf API doesn't separate hard errors from
///     layout hints; a log line at trace level surfaces them
///     without failing the request).
pub fn render_pdf(
    data: &MonthlyReportData,
    manifest: &ReportManifest,
) -> anyhow::Result<Vec<u8>> {
    let html = build_html(data, manifest);

    let mut doc = PdfDocument::new(&format!(
        "MG Market Maker — Compliance Report — {} — {}",
        data.client_id, data.period_from
    ));

    let mut warnings = Vec::new();
    let pages = PdfDocument::from_html(
        &html,
        &BTreeMap::new(),     // images map
        &BTreeMap::new(),     // fonts map (use built-in)
        &GeneratePdfOptions::default(),
        &mut warnings,
    )
    .map_err(|e| anyhow::anyhow!("PDF from_html failed: {e:?}"))?;

    for w in &warnings {
        tracing::trace!("pdf render warning: {w:?}");
    }

    doc.with_pages(pages.pages);
    let mut save_warnings = Vec::new();
    let bytes = doc.save(&PdfSaveOptions::default(), &mut save_warnings);
    for w in &save_warnings {
        tracing::trace!("pdf save warning: {w:?}");
    }
    Ok(bytes)
}

fn build_html(data: &MonthlyReportData, manifest: &ReportManifest) -> String {
    // Summary rows
    let mut summary_rows = String::new();
    for s in &data.summaries {
        summary_rows.push_str(&format!(
            "<tr>\
              <td class=\"sym\">{sym}</td>\
              <td class=\"num pos-neg\">{total}</td>\
              <td class=\"num\">{spread}</td>\
              <td class=\"num\">{inv}</td>\
              <td class=\"num neg\">−{fees}</td>\
              <td class=\"num pos\">{rebates}</td>\
              <td class=\"num\">{trips}</td>\
              <td class=\"num\">{vol}</td>\
              <td class=\"num\">{pres}%</td>\
              <td class=\"num\">{two}%</td>\
              <td class=\"num\">{up}%</td>\
             </tr>",
            sym = esc(&s.symbol),
            total = fmt4(s.total_pnl),
            spread = fmt4(s.spread_pnl),
            inv = fmt4(s.inventory_pnl),
            fees = fmt4(s.fees_paid),
            rebates = fmt4(s.rebates_earned),
            trips = s.round_trips,
            vol = fmt2(s.total_volume),
            pres = fmt2(s.presence_pct),
            two = fmt2(s.two_sided_pct),
            up = fmt2(s.uptime_pct),
        ));
    }

    // Recent fills — cap to 100 so a monthly report with 100 k fills
    // doesn't blow the PDF up. Full fill ledger is in the CSV/XLSX.
    let fills_shown = data.fills.len().min(100);
    let mut fills_rows = String::new();
    for f in data.fills.iter().take(fills_shown) {
        fills_rows.push_str(&format!(
            "<tr>\
              <td class=\"ts\">{ts}</td>\
              <td class=\"sym\">{sym}</td>\
              <td class=\"side side-{side_lc}\">{side}</td>\
              <td class=\"num\">{px}</td>\
              <td class=\"num\">{qty}</td>\
              <td class=\"num\">{fee}</td>\
              <td>{maker}</td>\
             </tr>",
            ts = f.timestamp.format("%Y-%m-%d %H:%M:%S"),
            sym = esc(&f.symbol),
            side_lc = f.side.to_ascii_lowercase(),
            side = esc(&f.side),
            px = fmt2(f.price),
            qty = fmt4(f.qty),
            fee = fmt4(f.fee),
            maker = if f.is_maker { "M" } else { "T" },
        ));
    }

    let fills_overflow_note = if data.fills.len() > fills_shown {
        format!(
            "<p class=\"note\">Showing {} of {} fills. Full ledger \
            in the CSV / XLSX attachment.</p>",
            fills_shown,
            data.fills.len()
        )
    } else {
        String::new()
    };

    format!(
        r#"<!DOCTYPE html>
<html title="MG Market Maker Compliance Report">
<head>
<style>
  body {{ font-family: 'Helvetica', sans-serif; font-size: 9pt; color: #111; padding: 15mm; }}
  h1 {{ font-size: 18pt; margin: 0 0 4pt; color: #05110e; }}
  h2 {{ font-size: 12pt; margin: 18pt 0 6pt; color: #05110e; border-bottom: 1pt solid #ccc; padding-bottom: 2pt; }}
  .brand-bar {{ border-bottom: 2pt solid #00D1B2; padding-bottom: 8pt; margin-bottom: 16pt; }}
  .mg {{ color: #00D1B2; font-weight: bold; }}
  .meta-row {{ color: #555; font-size: 9pt; margin-top: 4pt; }}
  .meta-label {{ color: #666; font-weight: bold; }}
  .meta-value {{ color: #111; margin-right: 12pt; }}
  table {{ width: 100%; border-collapse: collapse; font-size: 8.5pt; margin: 6pt 0; }}
  th {{ background-color: #05110e; color: #00D1B2; padding: 4pt 6pt; text-align: left; font-weight: bold; font-size: 7.5pt; }}
  td {{ padding: 3pt 6pt; border-bottom: 1px solid #e6e6e6; }}
  td.num {{ text-align: right; font-family: 'Courier New', monospace; }}
  td.sym {{ font-weight: bold; }}
  td.ts {{ font-family: 'Courier New', monospace; color: #666; font-size: 7.5pt; }}
  td.side-buy {{ color: #2ea043; font-weight: bold; }}
  td.side-sell {{ color: #cf222e; font-weight: bold; }}
  td.pos {{ color: #2ea043; }}
  td.neg {{ color: #cf222e; }}
  p.note {{ font-style: italic; color: #888; font-size: 8pt; }}
  div.sig {{
    margin-top: 24pt; padding: 8pt; background-color: #f6f8fa;
    border-left: 3pt solid #00D1B2;
    font-family: 'Courier New', monospace; font-size: 7pt;
  }}
  div.sig-label {{ font-size: 7pt; color: #666; margin-bottom: 4pt; font-weight: bold; }}
  p.disclaimer {{ margin-top: 12pt; font-size: 7.5pt; color: #666; line-height: 1.4; }}
  div.footer {{ margin-top: 28pt; border-top: 1px solid #ccc; padding-top: 8pt; font-size: 7pt; color: #888; text-align: center; }}
</style>
</head>
<body>

<div class="brand-bar">
  <h1><span class="mg">MG|</span> Market Maker Compliance Report</h1>
  <div class="meta-row">
    <span class="meta-label">Client:</span>
    <span class="meta-value">{client_name} ({client_id})</span>
    <span class="meta-label">Period:</span>
    <span class="meta-value">{period_from} – {period_to}</span>
    <span class="meta-label">Generated:</span>
    <span class="meta-value">{gen}</span>
  </div>
</div>

<h2>Per-symbol summary</h2>
<table>
<thead><tr>
  <th>Symbol</th><th>Total PnL</th><th>Spread</th><th>Inventory</th>
  <th>Fees</th><th>Rebates</th><th>Trips</th><th>Volume</th>
  <th>Presence</th><th>2-sided</th><th>Uptime</th>
</tr></thead>
<tbody>{summary_rows}</tbody>
</table>

<h2>Recent fills (latest {fills_shown})</h2>
<table>
<thead><tr>
  <th>Timestamp (UTC)</th><th>Symbol</th><th>Side</th>
  <th>Price</th><th>Qty</th><th>Fee</th><th>Role</th>
</tr></thead>
<tbody>{fills_rows}</tbody>
</table>
{fills_overflow_note}

<h2>Compliance signature</h2>
<div class="sig">
  <div class="sig-label">HMAC-SHA256 · row counts: S={n_sum} F={n_fills} A={n_audit}</div>
  {sig}
</div>

<p class="disclaimer">
This report was generated by MG Market Maker (MiCA Article 17 compliant
operator). The HMAC signature above binds the client identifier,
reporting period, row counts, and delivery formats declared in the
accompanying CSV / XLSX manifest. Tampering with the CSV / XLSX row
ledger invalidates the signature. Contact compliance@… for the
shared secret verification procedure.
</p>

<div class="footer">Document generated {gen} · SIG {sig_short}</div>

</body>
</html>"#,
        client_name = esc(&data.client_name),
        client_id = esc(&data.client_id),
        period_from = data.period_from,
        period_to = data.period_to,
        gen = data.generated_at.format("%Y-%m-%d %H:%M:%S UTC"),
        summary_rows = summary_rows,
        fills_rows = fills_rows,
        fills_shown = fills_shown,
        fills_overflow_note = fills_overflow_note,
        sig = manifest.sig,
        sig_short = &manifest.sig[..manifest.sig.len().min(16)],
        n_sum = manifest.row_counts.summaries,
        n_fills = manifest.row_counts.fills,
        n_audit = manifest.row_counts.audit_events,
    )
}

fn fmt4(d: rust_decimal::Decimal) -> String {
    let f: f64 = d.to_string().parse().unwrap_or(0.0);
    format!("{:.4}", f)
}

fn fmt2(d: rust_decimal::Decimal) -> String {
    let f: f64 = d.to_string().parse().unwrap_or(0.0);
    format!("{:.2}", f)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_export::{
        build_manifest, AuditRow, FillRow, MonthlyReportData, SymbolSummaryRow,
    };
    use chrono::{NaiveDate, TimeZone, Utc};
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
    fn html_contains_all_sections() {
        let data = mk_data();
        let m = build_manifest(&data, &["pdf"], b"0123456789abcdef0123456789abcdef").unwrap();
        let html = build_html(&data, &m);
        assert!(html.contains("BTCUSDT"));
        assert!(html.contains("Acme Capital"));
        assert!(html.contains("Per-symbol summary"));
        assert!(html.contains("Recent fills"));
        assert!(html.contains("HMAC-SHA256"));
        assert!(html.contains(&m.sig));
    }

    #[test]
    fn html_escapes_special_chars() {
        let mut data = mk_data();
        data.client_name = "Acme & <Co>".into();
        let m = build_manifest(&data, &["pdf"], b"0123456789abcdef0123456789abcdef").unwrap();
        let html = build_html(&data, &m);
        assert!(html.contains("Acme &amp; &lt;Co&gt;"));
        assert!(!html.contains("Acme & <Co>"));
    }

    #[test]
    fn pdf_produces_valid_magic_bytes() {
        let data = mk_data();
        let m = build_manifest(&data, &["pdf"], b"0123456789abcdef0123456789abcdef").unwrap();
        let bytes = render_pdf(&data, &m).expect("pdf render");
        // PDF magic header.
        assert_eq!(&bytes[..5], b"%PDF-");
        // Non-trivial size (empty PDFs are ~500 bytes, with tables
        // we should comfortably exceed 2 kB).
        assert!(
            bytes.len() > 2000,
            "PDF suspiciously small: {} bytes",
            bytes.len()
        );
    }
}
