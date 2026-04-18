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
`CheckpointSaved`, `BalanceReconciled`, Epic F `NewsRetreatActivated`, and
Epic H strategy-graph events: `StrategyGraphDeployed`,
`StrategyGraphRolledBack`, `StrategyGraphDeployRejected`,
`StrategyGraphSinkFired` (all critical → fsync before return).

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
summary.xlsx                       (HMAC manifest baked into workbook)
summary.pdf
fills.jsonl
audit.jsonl
strategy_graphs/deploys.jsonl      (Epic H — DeployRecords in range)
strategy_graphs/snapshots/{hash}.json (canonical JSON of every
                                       graph that was live — joined by
                                       hash on the audit rows above)
manifest.json                      (HMAC-SHA256)
README.txt                         (verification instructions)
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

## 11. Strategy-graph governance (Epic H)

The visual strategy builder is the operator's hot-path for changing
quoting behaviour without a redeploy. Every mutation is recorded on
the same hash-chained audit trail as order + risk events, joined on
the graph's content hash (SHA-256 of the canonical JSON form).

**Disk layout.**

```
data/strategy_graphs/
├── {name}.json                    active graph per name
├── .deploys.jsonl                 full deploy history (JSONL)
├── history/{name}/{hash}.json     immutable per-hash snapshot
└── user_templates/{name}.json     operator-saved reusable templates
```

**Endpoints.**

- `POST /api/admin/strategy/graph` — deploy. Validates, compiles,
  writes `{name}.json` + appends a `DeployRecord` + archives a
  `history/{name}/{hash}.json` snapshot, broadcasts
  `ConfigOverride::StrategyGraphSwap` to every engine whose scope
  matches. Supports `?rollback_from={prev_hash}` so the audit row
  records *intent* (rollback vs. accidental hash match).
- `POST /api/v1/strategy/validate` — same validator the deploy path
  runs, no writes. Front-end calls debounced on every canvas
  mutation so the operator sees "ready / N issues" live. Returns
  `{valid, issues[], node_count, edge_count, sink_count}`.
- `GET /api/v1/strategy/active` — folds the deploy log to one row
  per `(name, scope)` showing the latest hash + operator + timestamp.
  The Settings → Config snapshot panel renders it so an auditor sees
  what's live without opening the graph editor.
- `GET /api/v1/strategy/deploys` — full DeployRecord history.
- `GET /api/v1/strategy/graphs/{name}/history/{hash}` — fetch an
  immutable historical snapshot.
- `POST/GET/DELETE /api/v1/strategy/custom_templates[/{name}]` —
  operator-authored reusable templates on disk.

**Restricted gate.** Graphs that reference a node kind marked
`restricted: true` (pentest-only strategies) are refused unless the
runtime was started with `MM_RESTRICTED_ALLOW=1` (env-only — the
flag is deliberately absent from TOML so prod must be explicit).
Refusal emits `StrategyGraphDeployRejected` before returning 403;
the `detail` field names every offending kind.

**Audit events.**

| Event | Detail format | Critical |
|---|---|---|
| `StrategyGraphDeployed` | `graph={name} hash={sha256} scope={scope} operator={id} recipients={n}` | yes (fsync) |
| `StrategyGraphRolledBack` | `graph={name} from_hash={sha256} to_hash={sha256} operator={id}` | yes (fsync) |
| `StrategyGraphDeployRejected` | `graph={name} reason={restricted/validation/...} operator={id}` | yes (fsync) |
| `StrategyGraphSinkFired` | `action=Flatten{policy}` or `KillEscalate{level,reason}` `hash={sha256}` | yes (fsync) |

The sink-provenance row fires only on high-consequence sinks
(`Out.Flatten`, `Out.KillEscalate`) — multipliers fire every tick
and would drown the log. Regulators joining `StrategyGraphSinkFired`
rows against the bundle's `strategy_graphs/snapshots/{hash}.json`
get a closed-book reconstruction: "graph X said kill-L4 at time T
because of input Y".

**Regulator reconstruction workflow (extended).**

1. Pull the bundle for the period (as in §10).
2. In `audit.jsonl`, filter events of type
   `strategy_graph_*`. Every row carries the hash of the graph that
   was live at that moment.
3. Open `strategy_graphs/snapshots/{hash}.json` to see the authored
   graph; render it in the editor (File → Import) to visualise
   exactly what ran.
4. `strategy_graph_sink_fired` rows let you attribute any specific
   kill or flatten to the exact graph node that triggered it —
   needed for MiCA Article 17 market-abuse investigations.

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
| H-P1 | Visual strategy builder — typed DAG + evaluator + overlay sinks |
| H-P2 | Catalog expansion to 43 kinds (Waves A-D) + deploy history + rollback |
| H-P3 | Strategy-graph compliance wiring — deploy/rollback/reject/sink audit events, bundle section, restricted gate, active-graphs API |
| H-P4 | Graph-authored quoting — `Out.Quotes`, `Quote.Grid`, `Quote.Mux` + live validate endpoint + user templates + export/import |
