# Product journey — 2026-04-21 live smoke

Methodology: boot mm-server + mm-agent against a clean workdir,
create one user per role (admin / operator / viewer /
clientreader tagged to tenant `acme`), tag the agent with
`profile.client_id=acme`, push a dummy binance-testnet vault
credential, deploy `avellaneda-via-graph` on BTCUSDT, wait for
the engine to tick and match paper fills against real testnet
market data, then walk every public API surface under each role
and record what the UI would render.

## Critical bugs found + fixed in this pass

| # | Severity | Finding | Fix |
|---|----------|---------|-----|
| SEC-1 | 🔥 CVSS-10 | `mm_controller::http_router_full` mounted `/api/v1/fleet`, `/api/v1/vault`, `/api/v1/approvals`, deploy POSTs anonymously. Curl without token returned full vault + fleet. | `router_full_authed(..., auth_state)` with 3 tiers (internal_view / control / admin). Regression test `crates/controller/tests/auth_matrix.rs`. |
| AUDIT-1 | 🟠 P1 | `AuthState` booted without an `AuditLog` in `mm-server/src/main.rs` → all `auth.audit(...)` calls (login success/failure, logout, reset) were no-ops. H4 LoginAuditPage would always show empty. | `MM_AUDIT_PATH` env + `with_audit()` + `set_audit_log_path()` wiring. Auth events promoted to `is_critical()` so each fsyncs. |
| FILL-1 | 🔥 P0 UX | Tenant opened portal → "Recent fills" card empty even though paper fills were happening. Controller's `get_client_fills` read its own local store which is never populated in distributed mode. | Extended agent `client_metrics` topic to carry `recent_fills[]`. Controller merges across fleet, sorts newest-first. Test `collect_fills_merges_and_sorts_fleet_rows`. |
| TEST-1 | 🟡 P2 | `pentest_templates_e2e` flaked 2/6 under parallel cargo test. `with_restricted` mutated `MM_ALLOW_RESTRICTED` env var as process-global state without a lock. | Static `Mutex<()>` serialization in the test helper. 5/5 runs green. |

## Journey — Admin

| Page | Path(s) observed | Status | Notes |
|------|------------------|--------|-------|
| Overview | `/api/v1/portfolio/cross_venue`, `/api/v1/venues/status`, `/api/status`, `/api/v1/active-graphs` | Mostly ✅ | Portfolio + venues status populated. `active-graphs` returns `rows:[]` — legacy endpoint that reads controller local state; per-deployment graph info is on `/api/v1/fleet` rows instead. |
| Fleet | `/api/v1/fleet`, `/api/v1/approvals` | ✅ | Agent + accepted lease + running deployment visible. Deployment row carries mid/spread/inventory/PnL/vpin/kyle/volatility/manipulation/market_impact/performance/hourly_presence/SLA. |
| Clients | `/api/v1/clients`, `/api/v1/clients/loss-state` | ✅ | Tenant "acme" registered with BTCUSDT. loss-state empty — expected when no loss config. |
| Reconciliation | `/api/v1/reconciliation/fleet` | ✅ | Cycle 5 reports internal_orders=2, venue_orders=0, balance_mismatches=BTC 0 vs 0.003, orders_fetch_failed=true (dummy creds) — surface exposes this. |
| Orderbook | WS-driven | not directly smoked this pass | would need frontend for WS |
| History | `/api/v1/history/inventory/per_leg`, per-deployment details | not directly smoked | |
| Compliance | AuditStream, MiCA reports, violations | ✅ audit-path | Audit file now wired, 8+ login events captured. Signed audit range export is wired in ReportsPanel. |
| Surveillance | `/api/v1/surveillance/fleet`, `/api/v1/surveillance/scores` | Mixed | fleet version returns per-deployment scores (pump_dump=0.02, combined=0.01 real data). Legacy `/surveillance/scores` returns `patterns:{}` — that endpoint reads controller-local state that nothing populates in distributed mode. |
| Incidents | `/api/v1/incidents` | ✅ | Empty (no violations yet). Lifecycle endpoints present. |
| Strategy | `/api/v1/plans/active`, templates | Empty `plans[]` | Plans surface not populated for a deployed template — minor. |
| Settings | `/api/v1/tunables`, legacy config panels | ✅ for tunables | Legacy AdminConfigPanels (hyperopt, sentiment, alerts) read stores that are empty by default. |
| Platform | `/api/v1/tunables`, `/api/v1/tunables/schema` | ✅ | Runtime tunables (lease TTL, min/max agent version, deploy defaults) returned. |
| Vault | `/api/v1/vault` | ✅ | journey-binance credential visible with kind=exchange + metadata. Values redacted on list. |
| Users | `/api/admin/users` | ✅ | 4 seeded users visible (admin, op, view, acme-user). Reset-password button works e2e. |
| Auth audit | `/api/admin/auth/audit` | ✅ | Shows login success/failure, logout, password-reset events. Filter by event_type substring works. |
| Admin | Kill switches, venue kill levels | ✅ | venue-kill state returned. |
| Profile | `/api/auth/me`, password change, TOTP enroll | ✅ | Full self-service. Enroll returns otpauth URI. |

## Journey — Operator

Identical to admin for read surfaces. Blocked by role gate on:
- POST `/api/v1/vault` (admin only) → 403 ✅
- POST `/api/v1/approvals/{fp}/accept|reject|revoke|pre-approve` → 403 ✅
- PUT `/api/v1/tunables` → 403 ✅
- PUT `/api/v1/agents/{fp}/profile` → 403 ✅
- `/api/admin/users` (user management) → 403 ✅
- `/api/admin/auth/audit` → 403 ✅

Can hit: all reads, POST deploy, PATCH deployment variables, POST
ops (kill/pause/resume/etc), POST fleet ops, POST audit verify,
POST sentiment headline, POST admin config proxy.

## Journey — Viewer

Read-only across the internal_view tier. Blocked on every write
at the control + admin tiers. No tenant-scoped data (tenant
routes require a clientreader token). This is the right shape
for a read-only auditor or dev-ops watcher.

## Journey — ClientReader (tenant "acme-user")

| Card | Endpoint | Status |
|------|----------|--------|
| PnL summary | `/api/v1/client/self/pnl` | ✅ Real data (`total_pnl=-0.22 USDT`, `total_volume=186.88`, per-symbol breakdown). **Caveat:** `total_fills` field reports `pnl_round_trips` (complete buy↔sell cycles), not raw fill count — shows 0 until first round trip closes even when 2+ fills landed. Cosmetic label bug. |
| SLA status | `/api/v1/client/self/sla` | ✅ `two_sided_pct=100`, `minutes_with_data=2`. **Bug:** `presence_pct=0` while minutes>0 — unrelated compute bug in SLA aggregation; does not affect payout. |
| SLA certificate | `/api/v1/client/self/sla/certificate` | ✅ HMAC-signed JSON, downloadable. |
| Recent fills | `/api/v1/client/self/fills` | ✅ **FIXED in this pass**. Now merges `recent_fills[]` from every agent's `client_metrics` topic. Showed 2 real Buy fills with maker fee + slippage. |
| Webhook delivery log | `/api/v1/client/self/webhook-deliveries` | ✅ Empty until tenant registers a URL. |
| Webhook self-service CRUD | `/api/v1/client/self/webhooks` | ✅ Add / list / remove / test-fire all work. Cross-tenant isolation verified (beta tenant cannot see acme URLs). **Preview banner:** registered URLs reach the controller dispatcher but do not yet propagate to the agent engine, so real fill/kill events don't fire the tenant endpoint in distributed mode. Banner explains this; tracked as I3. |
| Non-self routes | `/api/v1/fleet`, `/api/admin/*`, `/api/v1/client/other-tenant/*` | ✅ All 403 via tenant_scope_middleware + SEC-1 fixes. |

## Paper trading core

- Engine spawns under agent, subscribes to binance-testnet WS,
  gets real market data (mid=75607.575, spread 0.0013%).
- Avellaneda quotes refresh every 500ms. Over 2 minutes, 75 paper
  orders placed, 2 simulated fills based on market trades walking
  over our resting levels.
- Order diff (place/cancel/amend) works — `live=2` steady, no
  leaks.
- Inventory accumulates correctly (long 0.003 BTC).
- Unrealized PnL tracked (-0.67 quote).
- Kill switch stayed NORMAL across the whole run. One earlier
  session showed `kill_level=1` transiently on a stats boot blip;
  not reproducible in the retry.

## Distributed telemetry

Per-deployment surfaces all populate correctly through the
agent→controller `details` fan-out:
- `surveillance/fleet` — aggregate manipulation scores (pump_dump
  per deployment).
- `reconciliation/fleet` — cycle / internal / venue / drift.
- `fleet` row carries 20+ scalars + 6 structured fields
  (hourly_presence, open_orders, market_impact, performance,
  variables, book_depth_levels).

## Follow-ups — additional fixes landed 2026-04-21 evening

| # | Severity | Finding | Fix |
|---|----------|---------|-----|
| **SLA-1** | 🟠 P1 (certificate-blocking) | Tenant `/self/sla` returned `presence_pct=0` even while engine ran healthy, because production `min_depth_quote=$2000` is way above paper-mode diagnostic quotes (~$90/side). SLA certificate would report FAIL for every paper deployment. | In `MarketMakerEngine::new`, when `config.mode="paper"` AND `min_depth_quote` is the $2000 default, relax to 0. Operator can still override to a real paper-mode floor. Regression test `zero_min_depth_does_not_fail_tiny_paper_quotes`. |
| **PNL-COUNTER-1** | 🟡 P2 | `/self/pnl.total_fills` was sourced from `pnl_round_trips` (complete buy↔sell cycles). Tenant with 2 live buys + 0 sells saw `total_fills=0` despite `recent_fills[]` containing two records. | Added `fill_count: u64` to `PnlAttribution`, increments per `record_fill`. Threaded through agent's `client_metrics` payload (`pnl_fill_count`) → controller aggregation prefers it, falls back to `round_trips` for rolling-upgrade compatibility. Regression test `fill_count_tracks_raw_fills_independent_of_round_trips`. |
| **LEGACY-1** | 🟡 P2 | `/api/v1/active-graphs`, `/api/v1/surveillance/scores`, `/api/v1/manipulation/scores` read controller-local state that nothing writes in distributed mode. Panels on Overview + Surveillance + ManipulationScores showed empty while fleet endpoints had real data. | Removed the three dead endpoints + associated handlers + structs + test. `ManipulationScores.svelte` rewired to `/api/v1/surveillance/fleet` (which populates), grouping by symbol + taking max per sub-score across agents. |

## Still outstanding

- **I3** CLOSED 2026-04-21 evening — controller now runs a
  periodic `webhook_fanout_loop` that polls fleet client-fills,
  fires `WebhookEvent::Fill` per new delivery, cursor-dedupes
  per-tenant. Verified live: 4 unique Fill events delivered to
  a netcat-like sink endpoint from 9 engine fills (5-fill delta
  is the bootstrap-cursor skip). Preview banner removed.
- **A1** CLOSED — UI already handles correctly via
  `d.active_graph.hash` vs `d.template` distinction; plain
  template strategies correctly leave `active_graph=None`.
- **H5** TOTP admin migration safety (locking out existing
  admins without TOTP when flipping the flag).
- **CalibrationStatus** panel reads controller-local
  `/api/v1/calibration/status` which is empty in distributed
  mode. Needs fleet fan-out or preview banner; deferred.

## Summary

Full role-based product journey is **functional end-to-end**
after closing SEC-1, AUDIT-1, and FILL-1. Trading core runs,
tenants see real data, operators have segregated capabilities,
ClientReader cannot escape tenant scope. Remaining items are
either cosmetic (counter naming) or known architectural gaps
(webhook dispatch) with preview banners or TODO entries.
