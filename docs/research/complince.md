# Compliance reference

Consolidated map of the compliance surface shipped in the market-maker. The goal
of this doc is so a regulator / client / auditor can answer four questions in
five minutes:

1. **What is recorded?** (audit trail + fills + config + reports)
2. **How is it tamper-evident?** (HMAC, hash chain, SHA-256 of prior event)
3. **Where does it live?** (local JSONL now, S3 when archive is on)
4. **How is it handed over?** (bundle ZIP, monthly export, presigned URL)

It is an index over implementation — not a rationale document. For *why* each
layer exists, see the epic commits listed at the bottom.

---

## 1. Audit trail (`crates/risk/src/audit.rs`)

**Format.** Append-only JSONL at `data/audit.jsonl`. Each record is a
`AuditEvent { seq, timestamp, event_type, symbol, client_id?, order_id?, side?,
price?, qty?, detail?, prev_hash? }`.

**Integrity.** `prev_hash` is the SHA-256 of the serialised previous record.
Insertion / deletion / modification of any row breaks the chain at that
point and every subsequent row. A linear verifier (re-hash each line, compare
to `prev_hash` of the next) is the canonical check.

**Events recorded.** `OrderPlaced`, `OrderCancelled`, `OrderAmended`,
`OrderFilled`, `OrderRejected`, `CircuitBreakerTripped`, `KillSwitchEscalated`,
`KillSwitchReset`, `InventoryLimitHit`, `ExchangeConnected/Disconnected/
Reconnected`, `BookResync`, `EngineStarted/Shutdown`, `ConfigLoaded`,
`CheckpointSaved`, `BalanceReconciled`, and Epic F `NewsRetreatActivated`.

**Date-range export.** `mm_risk::audit_reader::read_audit_range(path, from,
to) -> Vec<AuditEvent>`. HMAC-signable via `export_signed(events, from, to,
secret) -> SignedAuditExport { signature, … }` (`Hmac<Sha256>`).

---

## 2. Per-fill log (`crates/dashboard/src/state.rs`)

`FillRecord { timestamp, symbol, client_id?, side, price, qty, is_maker, fee,
nbbo_bid, nbbo_ask, slippage_bps }`. Persisted as JSONL via
`DashboardState::enable_fill_log(path)`. NBBO captured at fill time — MiFID II
execution-quality attestations need this.

---

## 3. Daily report

- `GET /api/v1/report/daily` — operator-scope JSON summary (symbols, PnL,
  volume, spread, uptime, presence, two-sided).
- `GET /api/v1/report/daily/csv` — same data as CSV.
- `GET /api/v1/report/history` + `/api/v1/report/history/{YYYY-MM-DD}` —
  archived snapshots (90-day in-memory cap).
- `GET /api/v1/client/report/daily` — per-client scoped variant.

Rendered via `pdf_report::render_pdf` + `report_export::render_csv/render_xlsx`.

---

## 4. Monthly MiCA export

Endpoints (`crates/dashboard/src/client_api.rs`):

- `GET /api/v1/report/monthly.json?from=…&to=…&client_id=…`
- `GET /api/v1/report/monthly.csv?…`  (text/csv)
- `GET /api/v1/report/monthly.xlsx?…` (multi-sheet: Summary, Fills, Audit, SLA)
- `GET /api/v1/report/monthly.pdf?…`
- `GET /api/v1/report/monthly.manifest?…` — HMAC-SHA256 signed manifest.

`MonthlyReportData` built by `monthly_report::build_monthly_report(state,
client_id, name, from, to, audit_path)` — joins `DashboardState` summaries with
per-fill + hash-chained audit rows.

**Manifest.** Signs canonical JSON of `{client_id, period, counts, formats}`.
Verifier: `report_export::verify_manifest(manifest, data, secret) -> bool`
(constant-time compare).

---

## 5. Automated scheduler (`crates/dashboard/src/report_scheduler.rs`)

Cron-driven report generation. Cadences (UTC):

- Daily — 00:15 (closed-day reports)
- Weekly — Monday 08:00
- Monthly — 1st 00:30

Concrete job: `BuiltinReportJob` (Epic B wiring). Writes bundles to
`data/reports/{cadence}/{folder}/summary.{json,csv,xlsx,pdf}` +
`manifest.json`. Missed runs close via `catchup_hours` (default 6h) so
operator-side downtime never loses a reporting day.

---

## 6. Compliance bundle (`crates/dashboard/src/archive/bundle.rs`)

`GET /api/v1/export/bundle?from=&to=&client_id=` returns a ZIP:

```
summary.json
summary.csv
summary.xlsx   (HMAC manifest baked into workbook)
summary.pdf
fills.jsonl
audit.jsonl
manifest.json  (HMAC-SHA256)
README.txt     (verification instructions)
```

One click = one hand-off. Intended for regulator / client portals.

---

## 7. S3 archive pipeline (`crates/dashboard/src/archive/`)

Opt-in via `[archive]` config. Uploads to any S3-compatible bucket (AWS S3,
MinIO, Cloudflare R2, Backblaze B2 — `s3_endpoint_url` overrides hostname,
`force_path_style` auto-enabled for non-AWS).

**Encryption.** SSE-S3 (AES-256) default; SSE-KMS when `encrypt_kms_key` is
set.

**Retention.** `retention_days` (default 2555 = 7 years, covers MiFID II and
exceeds MiCA's 5-year bar). Enforcement lives on the bucket (Object Lock +
lifecycle); shipper records the claim so auditors can verify.

**Shipper.** `data/audit.jsonl`, `data/fills.jsonl`, `data/reports/daily/*`
shipped on `shipper_interval_secs` cadence (default 1h). Byte-offset
checkpoints in `data/archive_offsets.json` — restart never re-uploads.

**Health probe.** `GET /api/v1/archive/health` hits `head_bucket` — operators
verify creds + endpoint at boot, not at 01:00 when the first shipper tick
fires.

**Credentials.** AWS default provider chain (env / IAM role / SSO / profile).
Never in TOML.

---

## 8. Configuration snapshot

`GET /api/v1/config/snapshot` — read-only serialised `AppConfig` so operators
see which compliance knobs are configured. Secrets never land in `AppConfig`
(env-only), so exposing the whole struct is safe.

Frontend viewer: **Settings → Config snapshot** (`frontend/src/lib/components/
ConfigViewer.svelte`) — shows runtime flags + wired/off chips for each
optional subsystem including archive, schedule, sentiment.

---

## 9. Observability

Prometheus series (scraped at `/metrics`):

```
mm_archive_uploads_total{stream}             # audit / fills / daily
mm_archive_upload_bytes_total{stream}
mm_archive_upload_errors_total{stream}
mm_archive_last_success_ts{stream}           # alert key
mm_scheduler_runs_total{cadence}
mm_scheduler_failures_total{cadence}
mm_scheduler_last_success_ts{cadence}
```

Alert pattern: `time() - mm_archive_last_success_ts{stream="audit"} >
shipper_interval_secs * 2` → "archive pipeline stalled".

---

## 10. Regulator hand-off workflow

Repeatable operator runbook:

1. Determine period + client (e.g. 2026-03-01 … 2026-03-31, client `foo`).
2. `GET /api/v1/report/monthly.manifest?...` → save `manifest.json`.
3. `GET /api/v1/export/bundle?...` → save `bundle.zip`.
4. Hand both artefacts plus the HMAC signing secret (out-of-band) to the
   recipient.
5. Recipient verifies: recompute manifest HMAC, walk `audit.jsonl` rechaining
   via `prev_hash`. Any mismatch means the bundle was altered after signing.

For regulator API-direct access (MiCA inspection, CFTC swap-data):
presigned S3 URL via `ArchiveClient::presign_get(key, ttl)`. Time-limited, no
long-lived IAM creds handed out.

---

## Epic commit trail

| Epic | Scope |
|---|---|
| 36.3 | Audit log fsync + SHA-256 hash chain |
| 36.4 | MiCA HMAC full-body scope |
| 42.1 | PDF daily / monthly report generator |
| 42.2 | CSV / XLSX monthly export |
| 42.3 | SMTP email delivery |
| 42.4 | Cron scheduler for reports |
| A1   | MiCA monthly on-demand HTTP |
| B (wire) | BuiltinReportJob impl + scheduler spawn at boot |
| C    | S3 archive shipper + bundle endpoint |
| D    | Observability — Prometheus + S3 health probe |
| UX-5 | Config snapshot viewer |
| UX-6 | Reports panel + audit stream on CompliancePage |
