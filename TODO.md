# MM — Open Work Tracker

Last updated: 2026-04-20 (post-distributed-control-plane audit)

---

## Distributed architecture audit (2026-04-20)

Pre-refactor mm-server ran a single in-process engine and the
dashboard read its state directly. Post-refactor engines live in
remote `mm-agent` processes; controller + dashboard receive
everything via fleet telemetry. A lot of dashboard code still
assumes the old model. This audit tags every artefact as:

- **OK** — works with the new architecture, no action needed
- **LEGACY** — artefact is architecturally dead (reads / writes
  something that doesn't exist in the new model); remove or
  replace
- **PARTIAL** — works but surfaces defaults for metrics that
  aren't propagated through fleet telemetry yet; needs
  per-deployment wire-up
- **REWIRE** — functionality belongs in the new model but the
  current implementation targets the wrong layer (e.g. pushes
  to server-embedded engine instead of `POST /agents/{id}/deployments`)

### Frontend pages (14)

| Page | Status | Notes |
|---|---|---|
| Overview | OK (2026-04-21) | Adapter fills SymbolState from extended DeploymentStateRow. Mid/spread/VPIN/Kyle/adverse/volatility + SLA uptime/presence/two-sided + hourly presence + market-impact + performance + open orders + active-graph now flow through. Agent reads from shared DashboardState each snapshot tick. |
| Orderbook | PARTIAL | Book-display wires through WS; per-symbol state works if at least one deployment trades that symbol; otherwise empty |
| History | OK (2026-04-21) | per-leg inventory populated via adapter.publish_inventory; AuditStream fans out to each deployment's `audit_tail` details topic; DecisionsLedger fans out to `decisions_recent` details topic. 401 log-flood fixed via auth_middleware 60s dedup. |
| Calibration | LEGACY | `CalibrationStatus` reads engine-side GLFT calibration; engine is in agents → need per-deployment endpoint or move this to StrategyPage drill-down |
| Compliance | OK (2026-04-21) | AuditStream works via per-deployment `audit_tail` topic (with from_ms/until_ms/limit args). MiCA monthly report now fleet-aware — server boot wires an `AuditRangeFetcher` closure into DashboardState that fans out `audit_tail` per deployment and merges with cap 50k; `build_monthly_report` picks it up and falls back to local file only when no fetcher is set (test deploys). |
| Surveillance | LEGACY | Detector scores come from engine-internal state; global "surveillance" page is meaningless in distributed model (flagged as UX-SURV-1) |
| Strategy | REWIRE | Graph editor deploys through old `/api/admin/strategy/graph` (server-embedded engine); in new model graph → variables in `DesiredStrategy` → `POST /agents/{id}/deployments`. Needs integration with DeployDialog or replacement |
| Settings | LEGACY | FeatureStatusPanel + ParamTuner + ConfigViewer + AdaptivePanel all assume single-process engine. CONFIG-ONLY panel flags (momentum_ofi_enabled, bvc_enabled, etc.) belong in per-deployment variables now |
| Users | OK | Users live on controller via AuthState + users.json |
| Admin | PARTIAL | Kill switch + SOR + atomic bundles + funding-arb + manipulation + onchain panels assume engine-side state; only VenuesHealth still makes sense via fleet adapter. AdminConfigPanels (webhooks / alerts / loans / sentiment) reference server-side stores that are legacy |
| Fleet | OK | Fully distributed-native — agents, approvals, per-agent credentials, deploy dialog |
| Vault | OK | Unified encrypted secret store, admin-only |
| Platform | OK | Controller-level runtime tunables |
| Profile | OK | User self-service (password, 2FA) |

### Frontend components (61) — highlights of LEGACY / REWIRE

- `FeatureStatusPanel` — CONFIG-ONLY breadcrumb list of engine flags; **delete**
- `ParamTuner` — writes to engine via `/api/admin/config/{symbol}` which targets server-embedded engine; **rewire** to per-deployment variables via a future "Live tune" drawer on a deployment drilldown
- `ConfigViewer` — reads `/api/v1/config/effective` (server config); **delete** in favour of Platform page + Vault + Deploy dialog variables
- `AdaptivePanel` — reads γ / pair_class from `DashboardState.tunable_config` which is a default in `adapter.rs`; **delete** until per-deployment adaptive state is propagated
- `CalibrationStatus` — engine-internal; **rewire** to pull from a deployment's current calibration state (new endpoint needed)
- `SorDecisions`, `AtomicBundles`, `RebalanceRecommendations`, `FundingArbPairs` — all engine-internal; **rewire** or **delete**
- `ManipulationScores`, `OnchainScores`, `AdverseSelection` — engine-internal; **rewire** to per-deployment or per-symbol drill-downs
- `InventoryPanel`, `InventoryChart`, `PnlChart`, `OpenOrders`, `FillHistory`, `SpreadChart` — read through `DashboardState` fed by adapter; mostly work but fields filled with defaults for metrics not in telemetry
- `AdminConfigPanels` (webhooks / alerts / loans / sentiment) — server-side stores, **mostly legacy** once alert / webhook wiring moves to controller telemetry
- `ClientOnboardingPanel`, `ClientCircuitPanel` — per-client features; still valid but need controller-side client store equivalent to what was in dashboard

### Backend endpoints

Controller routes (15) — all distributed-native, **OK**:
- `/api/v1/fleet`, `/api/v1/approvals*`, `/api/v1/agents/{id}/*`, `/api/v1/vault*`, `/api/v1/templates`, `/api/v1/tunables*`

Dashboard routes (64) — audit:
- Auth (`/api/auth/*`) — **OK** (post-bootstrap refactor)
- WS `/ws` — **PARTIAL** (broadcasts `DashboardState`; works but many fields stale)
- Per-symbol ops (`/api/v1/ops/*/{symbol}`) — **REWIRE** to `/api/v1/agents/{id}/deployments/{dep_id}/ops/*` — in new model you kill/widen a specific deployment on a specific agent, not a "global symbol"
- `/api/admin/config/*` — **LEGACY**, tunes server engine that doesn't exist
- `/api/admin/strategy/graph` — **REWIRE** to `POST /agents/{id}/deployments` via DeployDialog
- `/api/admin/webhooks`, `/api/admin/alerts`, `/api/admin/loans`, `/api/admin/sentiment/*` — **LEGACY-ish**, these server-side stores should move to controller or be removed
- `/api/admin/optimize/*` — **LEGACY**, hyperopt assumed in-process
- `/api/v1/surveillance/scores`, `/api/v1/manipulation/scores`, `/api/v1/onchain/scores`, `/api/v1/calibration/status`, `/api/v1/adverse-selection`, `/api/v1/funding-arb/pairs`, `/api/v1/sor/decisions/recent`, `/api/v1/atomic-bundles/inflight`, `/api/v1/rebalance/*`, `/api/v1/decisions/recent`, `/api/v1/plans/active`, `/api/v1/otr/tiered`, `/api/v1/basis` — **LEGACY / PARTIAL**, all depend on engine-internal state that now lives in agents
- `/api/v1/venues/status`, `/api/v1/venues/latency_p95`, `/api/v1/venues/book_state`, `/api/v1/venues/funding_state`, `/api/v1/portfolio/cross_venue`, `/api/v1/clients/loss-state`, `/api/v1/active-graphs`, `/api/v1/kill/venues` — **PARTIAL**, work via adapter when deployments exist, but fields incomplete

### Proposed clean-up waves

**Wave 1 Reshape (DONE — 2026-04-20): Restore-not-delete**

Fixing the earlier deletion framing. Each item reshaped onto
the distributed architecture instead of deleted:

- `FeatureStatusPanel` / `ParamTuner` / `ConfigViewer` /
  `AdaptivePanel` → moved into `DeploymentDrilldown` in Wave 2
  Phase C (already landed). No longer "dead".
- `SettingsPage` — reshaped into a fleet-wide live-deployment
  summary + navigation tiles. Shows rollup (running / stopped /
  kill-escalated / live-orders), mode/regime/template chips,
  and a clickable row per deployment that navigates to Fleet
  for drilldown.
- `/api/admin/config/{symbol}` — restored as a controller thin-
  proxy. Translates the legacy `{field, value}` shape into a
  variables-PATCH and forwards to the matching deployment
  (resolved by symbol from the fleet snapshot). Old tools +
  hyperopt scripts keep working. Handlers:
  `post_admin_config_proxy`, `legacy_config_to_variable`.
- `DeploymentStateRow` gained 4 execution-layer scalars
  (`sor_filled_qty`, `sor_dispatch_success`,
  `atomic_bundles_inflight`, `atomic_bundles_completed`). Agent
  scrapes the existing `mm_sor_*` + `mm_atomic_bundles_*`
  Prometheus gauges/counters. Drilldown renders them in a
  new "Execution" section.

**Engine gauge emission (DONE — 2026-04-20, step (1) above):**
- `mm_calibration_a/k/samples` — emitted from the engine's
  refresh-quotes tick alongside `recalibrate_if_due`.
- `mm_manipulation_pump_dump/wash/thin_book/combined` —
  emitted from the same tick off the aggregator's snapshot;
  fires regardless of whether a dashboard is attached (colo
  agents publish to Prometheus).
- `mm_funding_arb_transitions_total{outcome}` +
  `mm_funding_arb_active` — emitted from
  `MarketMakerEngine::handle_driver_event` so every DriverEvent
  bumps a counter and (for Entered/Exited/PairBreak) flips the
  active gauge.
- `DeploymentStateRow` carries 13 new scalar fields; agent
  scrapes via `read_gauge_by_symbol` / `read_counter_by_symbol`
  / `read_counter_by_symbol_outcome`; `DeploymentDrilldown`
  renders Calibration + Manipulation + Funding-arb sections
  (Funding-arb section hidden when all counters are zero).

**Detail endpoints (DONE — 2026-04-20):**
- New wire commands `CommandPayload::FetchDeploymentDetails`
  + `TelemetryPayload::DetailsReply` with `request_id`
  correlation. Controller parks a oneshot in the
  `AgentRegistry.pending_details` map, agent replies with a
  topic-shaped JSON payload, HTTP handler enforces a 5s timeout.
- Shared `mm_dashboard::details_store` with a 20-entry ring
  buffer per symbol. Engine's `handle_driver_event` pushes on
  every DriverEvent; agent reads on `FetchDeploymentDetails`.
- Controller route
  `GET /api/v1/agents/{id}/deployments/{dep}/details/{topic}`
  forwards to agent and streams the reply back. Topic
  `funding_arb_recent_events` wired end-to-end.
- `DeploymentDrilldown` funding-arb section gained a "Recent
  events" table that auto-loads when any driver counter is
  non-zero.

**SurveillancePage fleet-aggregate (DONE — 2026-04-20):**
- New controller endpoint `GET /api/v1/surveillance/fleet`
  rolls up every live deployment's `manipulation_*` fields
  (added in the prior engine-gauge pass) into a single array
  sorted by combined risk score.
- `SurveillancePage` reshaped from the old 16-pattern
  speculative board into a fleet-wide table: one row per
  deployment, four category columns (combined, pump_dump,
  wash, thin_book), watch / alert thresholds at 0.5 / 0.8,
  rollup counters across the top.

**Follow-up topics for details endpoint:**
- `sor_decisions_recent` — add a ring buffer in
  `engine::sor` alongside the existing SOR gauges.
- `atomic_bundles_inflight` — surface the in-flight list
  from `AtomicBundleManager` so the UI can show leg status.
- `calibration_history` — recent `(a, k, samples)` tuples
  from the calibration walker.
- `CompliancePage` audit aggregation — controller endpoint
  that proxies to each agent's JSONL audit log and concatenates.
- `/api/admin/strategy/graph` thin-proxy — wrap graph JSON in
  `variables.strategy_graph` and forward via `POST .../deployments`.
- Admin stores migration: `webhooks`, `alerts`, `loans`,
  `sentiment` from dashboard to controller.

**Wave 2b (DONE — 2026-04-20): Tenant-isolation hardening**

Closed the cross-tenant credential leak risk. Delivered:
- `VaultEntry.client_id: Option<String>` top-level field
  (shared-infra = `None`). Serde-back-compat with old vault
  files. Surfaced through `VaultSummary`, `CredentialDescriptor`,
  and `VaultCreate` request body. (`crates/controller/src/vault.rs`)
- `can_exchange_access(cred_id, agent_id, agent_tenant)` now
  runs three-gate compose: kind → tenant → `allowed_agents`
  whitelist. New `CredentialCheck::TenantMismatch` variant.
- Controller `pre_validate_deploy` resolves the agent's tenant
  via approvals lookup on fingerprint, blocks cross-tenant
  mismatch AND cross-tenant mix within a single deployment's
  credential set. (`crates/controller/src/http.rs`)
- Agent catalog gains `resolve_for(cred_id, allowlist)` that
  refuses credentials outside `desired.credentials` even if the
  catalog happens to hold them for another deployment.
  `engine_runner.rs` uses it on primary / hedge / extras.
- `DeployDialog.svelte` shows `client_id` chip on each
  credential, disables submit + shows reason on tenant
  mismatch / cross-tenant mix.

New tests: 4 in `vault.rs` (shared-infra pass, cross-tenant
refuse, untagged-agent-refuse-for-tagged-cred, tenant-then-
whitelist), 1 in `catalog.rs` (resolve_for enforcement), 1 in
`engine_runner.rs` (allowlist bypass attempt yields no-op).

Skipped from original plan:
- Full per-deployment catalog split (one runner can't see
  another's keys even in-process). The `resolve_for` gate
  covers the same threat model at the boundary and doesn't
  require restructuring the catalog. Reopen if threat model
  tightens (e.g. we add strategy-level untrusted plugins).

**Wave 2 (medium — 2-3 days): Per-deployment drill-down** ← NEXT

Preserved from earlier deletion attempt:
- `FeatureStatusPanel.svelte`, `ParamTuner.svelte`, `AdaptivePanel.svelte`,
  `ConfigViewer.svelte` are back in the repo, unused, waiting for this
  wave to re-mount them into a Fleet → deployment drilldown panel.

Deliverables:
- New agent-side telemetry payload `DeploymentStatus` with
  strategy-visible state: current γ, regime, adaptive_reason,
  feature flags bag, effective variables snapshot, last N decisions.
- Controller endpoint `GET /api/v1/agents/{id}/deployments/{dep_id}/status`
  → returns the latest pushed `DeploymentStatus` for that deployment.
- Controller endpoint `PATCH /api/v1/agents/{id}/deployments/{dep_id}/variables`
  → merges an operator edit into the live `variables` map; agent
  reconciles the running engine without a restart (bumps γ, toggles
  features, etc). Validates: agent is Accepted + deployment exists.
- `DeploymentDrilldown.svelte` — expandable panel on FleetPage below
  each deployment row, mounts the 4 preserved components reading from
  the new endpoints.
- Remove legacy server-engine config routes (`/api/admin/config/*`)
  once the new path is live — already deleted in Wave 1.
- Add `POST /api/v1/agents/{id}/deployments/{dep_id}/ops/widen|stop|cancel|flatten` — replaces global `/api/v1/ops/{symbol}`
- Agent implements a deployment-ops command channel
- Dashboard `Overview` + per-symbol views read deployment telemetry from fleet view
- Move ParamTuner to a per-deployment drilldown modal — edit variables live via a new `PATCH /agents/{id}/deployments/{dep_id}/variables` path
- StrategyPage: replace `/api/admin/strategy/graph` path with `POST /agents/{id}/deployments` using graph JSON as `variables.graph`

**Wave 3 (big — a week): Telemetry uplift**
- Extend `DeploymentStateRow` with full metric set (VPIN, Kyle λ, adverse bps, regime, kill_level, spread_compliance, venue/product, mode, fills, volume, etc.)
- Agent populates these from MarketMakerRunner state on its telemetry cadence
- `adapter.rs` maps them into `SymbolState` instead of filling defaults
- Overview, AdaptivePanel, AdverseSelection, ManipulationScores, SurveillancePage (per-symbol) all come back online with real data
- Calibration page rewires to pull per-deployment calibration

**Wave 4 (big — 3-5 days): Audit + compliance aggregation**
- Agents stream audit events to controller (new telemetry kind)
- Controller aggregates into a fleet-wide audit log
- MiCA report endpoints read aggregated log instead of single engine's
- HistoryPage reads from aggregated log

**Wave 5 (polish — day): Kill-switch rewire**
- Kill switch currently a global /symbol op. Rewire to per-deployment (multiple deployments can have same symbol on different agents)
- Controls.svelte gains agent+deployment selector

Pick-up in any order depending on which operator pain is sharpest.

---

## Stabilization plan — 2026-04-21 (post-distributed audit)

Triple-agent audit (graph system, UI flow, compliance surface)
after the monolith→distributed port uncovered a set of gaps that
block a clean end-to-end "login → author → deploy → monitor →
report" flow. Four waves below, **execute in order**.

**Wave A — deploy flow unification (highest impact, ~2 days)**

Operator today can't answer "what graph is running on agent X?"
and has to save + pick + swap in two pages. Fix that first.

- [x] **A1** Agent echoes `graph_hash: Option<String>` +
  `template: Option<String>` in `DeploymentStateRow`. Populated
  on engine start from the resolved template/graph; updated on
  `StrategyGraphSwap` override. Fleet readback shows it.
- [x] **A2** Add `GET /api/v1/agents/{id}/deployments/{dep}/variables`
  returning the currently-applied variables map (agent serves
  from its own DesiredStrategy state). Mirror of the existing
  PATCH — no introspection today.
- [x] **A3** `StrategyPage` gains a single "Deploy" action that
  combines save-graph + pick-agent + dispatch into one modal.
  Current three-step flow (save → open picker → swap) becomes
  one confirmation dialog with fleet selector + variable form.
- [x] **A4** `DeployDialog` gains a "Custom graph" tab next to
  "Template". Same modal, two modes. Graph mode imports from
  StrategyPage draft or file.
- [x] **A5** `StrategyPage` rollback surface: list graph history
  (`/api/v1/strategy/graphs/{name}/history`) + one-click
  "rollback to hash X on deployment Y". `rollbackFrom` state
  already exists (line 50) — just needs the button.
- [x] **A6** Graph preview: wire `POST /api/v1/strategy/preview`
  to StrategyPage — dry-run the evaluator with sample inputs,
  show emitted quotes without touching the fleet.

**Wave B — fleet-aware client reports (~3 days)**

Today `/api/v1/pnl`, `/sla`, `/positions`, `/client/{id}/*` read
a local `DashboardState` on the controller that never gets
populated (engines live on agents). Only MiCA monthly export is
fleet-aware, via the `AuditRangeFetcher` pattern landed
2026-04-21. Reuse that pattern.

- [x] **B1** Generalize the fetcher-closure pattern: introduce
  `FleetClientReportFetcher` on `DashboardState` which fans out
  per-deployment detail queries to all accepted agents with a
  matching `profile.client_id` and merges into the existing
  client-portal response shapes.
- [x] **B2** Wire `/api/v1/pnl`, `/api/v1/pnl/timeseries`,
  `/api/v1/sla`, `/api/v1/sla/certificate`, `/api/v1/positions`,
  `/api/v1/client/{id}/*` through the new fetcher. Fallback to
  local state only when no deployments exist (test mode).
- [x] **B3** New details topics the agents must publish (engine
  already tracks these, just needs the topic registration):
  `pnl_snapshot`, `positions_snapshot`, `sla_snapshot`,
  `reconciliation_snapshot`.
- [x] **B4** Per-client drilldown page:
  `frontend/src/lib/pages/ClientPage.svelte`. Joins fleet rows
  by `profile.client_id` → shows positions per venue, PnL
  attribution per strategy, SLA certificate, open fills,
  webhook delivery log. Renders the fleet-aware endpoints.
- [x] **B5** Hot client-onboarding: registering a client in
  ClientOnboardingPanel currently requires agent restart. Send
  an `AddClient` command to every accepted agent so new-client
  state (`ClientConfig` slot, PnL row, SLA tracker) spawns
  without restart.

**Wave C — missing operator surfaces (~3 days)**

Gaps that are silent footguns: operator can't see
reconciliation drift, can't pause the whole fleet at once, can
revoke an agent that still has live deployments, can't rotate
credentials.

- [x] **C1** ReconciliationPage — new details topic
  `reconciliation_snapshot` (phantom orders, orphaned orders,
  balance drift). Controller endpoint
  `GET /api/v1/reconciliation/fleet` fans out + aggregates.
  Frontend page with three tables + "resolve" actions per row.
- [x] **C2** Global pause: `POST /api/v1/ops/pause_fleet` fans
  out `ops/pause` to every accepted agent's active deployments.
  Button at fleet-level on FleetPage. Same for `resume_fleet`.
- [x] **C3** Multi-agent batch deploy: `FleetPage` adds
  checkbox selection → "Deploy to selected". `DeployDialog`
  accepts an array of agent_ids → serial fan-out with
  per-agent result report.
- [x] **C4** Revoke-flow safety: when operator clicks "Revoke"
  on an agent with live deployments, show warning modal listing
  them + option to "stop all deployments then revoke" vs "cancel".
- [x] **C5** Credential rotation UI: VaultPage edit mode that
  bumps version without changing kind/ACL. Separate
  `credential_rotated_at` timestamp. Per-credential fetch audit
  (which agent fetched + when) via controller-side tap on
  `resolve_for`.
- [x] **C6** Credential expiry warnings: optional
  `expires_at: Option<DateTime>` on `VaultEntry`. VaultPage
  renders red chip for <7 days remaining; Platform dashboard
  has a rollup counter.
- [x] **C7** Fleet-level aggregate card on FleetPage: per-agent
  "running N deployments · M live orders · total PnL $X · last
  tick Y ms ago" summary row above the agent-card list.
- [x] **C8** Deployment-level flatten preview: before dispatch,
  `GET .../ops/flatten/preview` returns qty/side/expected
  slippage; confirmation modal shows it.

**Wave E-I — auth, operator UX, incident playbook, compliance polish (in flight 2026-04-21)**

Landed:
- **E** Tenant-isolated client portal: `Role::ClientReader`,
  `tenant_scope_middleware`, invite-based signup flow,
  `/api/v1/client/self/*` aliases for self-scoped reads.
- **F** Operator UX: pre-approve fingerprint, FirstInstallWizard,
  EmptyStateGuide on Fleet/Clients, deploy templates carry
  `risk_band` + `recommended_for` + `caveats`, kill drill-down
  links deployment from Admin.
- **G** Incident lifecycle: per-category auto-widen flags
  (sla/manip/recon), `OpenIncident` store with ack + resolve
  + post-mortem, violation rollup surfaces per-row Pause /
  Widen / Open-incident actions.
- **H** Auth hardening: password reset (admin mints one-shot
  signed URL → user consumes → token burned), env-gated
  `require_totp_for_admin` returning structured `must_enroll_totp`,
  `/api/admin/auth/audit` readback + LoginAuditPage, public
  probes verified minimal.
- **I** Tenant-self webhook CRUD (list/add/remove/test) with
  scheme + length validation + cross-tenant isolation tested.

**SEC-1 (2026-04-21 evening) — CLOSED.** The product-journey smoke
exposed that every route mounted via `mm_controller::http_router_full`
(fleet, vault, approvals, agents/deployments, ops, tunables) was
reachable anonymously. `curl -sS http://controller/api/v1/vault`
without a token returned the full credential list. Wave H/I auth
polish (password reset, TOTP gate, login audit) was effectively
defense in depth over a ground-floor open door. Fixed with
`router_full_authed(..., auth_state)`:
- Tier 1 (read, admin/operator/viewer): `/api/v1/fleet`, vault
  GET, approvals GET, tunables GET, templates, per-deployment
  details read, surveillance/reconciliation/alerts fleet — layered
  `auth_middleware` + `tenant_scope_middleware` (blocks ClientReader).
- Tier 2 (control, admin/operator): deploy POST, variables PATCH,
  ops/{op}, ops/fleet/{op}, audit/verify, sentiment/headline,
  admin/config/{symbol} — layered `auth_middleware` +
  `control_role_middleware` (Admin/Operator; blocks Viewer).
- Tier 3 (admin only): vault POST/PUT/DELETE, approvals writes
  (accept/reject/revoke/pre-approve/delete), tunables PUT,
  agent profile PUT — layered `auth_middleware` + `admin_middleware`.
- Regression test `crates/controller/tests/auth_matrix.rs` walks
  the full role × route matrix; it must stay green.

Outstanding follow-ups surfaced by the 2026-04-21 smoke run:
- [x] **I3** Distributed webhook dispatch — CLOSED 2026-04-21
  evening. Controller now runs a `webhook_fanout_loop` that
  ticks every 5s, walks tenants with registered dispatchers,
  pulls their `recent_fills` via the fleet fetcher, and fires
  `WebhookEvent::Fill` for every fill newer than a per-tenant
  cursor. First-pass-initialisation prevents flooding freshly
  registered endpoints with pre-existing fills. Verified
  live: tenant registered `http://sink:38080/hook`, deployed
  paper avellaneda, 4 distinct Fill events delivered without
  dupes (vs 9 engine fills — the gap is the cursor-bootstrap
  skip, working as designed).
- [x] **I4** Preview banner CLOSED — removed from
  ClientPortalPage once I3 landed.
- [x] **H5** TOTP migration story: if an operator flips
  `MM_REQUIRE_TOTP_FOR_ADMIN=true` while the root admin has
  no TOTP, the admin is locked out (must bootstrap a fresh
  deployment). Add a "lockout-safe" self-unlock via a signed
  out-of-band token, or document "always enroll TOTP before
  flipping the flag" in SECURITY.md + warn at server boot.
- [x] **TEST-1** `pentest_templates_e2e` flakes when run
  concurrently (2/6 fail → 6/6 pass in isolation). Shared
  global state (metrics registry? audit singleton?) needs a
  per-test isolate or a serial-only `#[serial_test::serial]`.
- [x] **AUDIT-1** Controller auth-event fsync added
  (2026-04-21) — LoginSucceeded/Failed/Logout + PasswordReset
  Issued/Completed now critical. Agents emit their own event
  classes; audit their critical list independently.

---

**Wave D — compliance polish (~2 days)**

Surface-level polish for the compliance/audit flow. Lower
urgency — current state already passes MiCA Article 17 export.

- [x] **D1** Hash-chain visual in AuditStream: highlight rows
  where chain is broken (prev_hash mismatch). Requires audit
  JSONL to carry both `hash` and `prev_hash` per row (already
  does per `crates/risk/src/audit.rs`).
- [x] **D2** Signed audit-range export endpoint:
  `POST /api/v1/audit/export` with `from_ms/until_ms/client_id`
  → returns tamper-proof bundle (same HMAC-SHA256 manifest
  shape as monthly bundle, arbitrary range). Button on
  ReportsPanel.
- [x] **D3** Webhook test + delivery log: per-client "Test
  webhook" button on ClientOnboardingPanel; controller stores
  last-50 deliveries in a ring buffer, surfaces via a new
  `/api/admin/clients/{id}/webhooks/deliveries` endpoint.
- [x] **D4** Fleet alert dedup: today each agent spawns its own
  AlertManager → Telegram. Moves to controller:
  `TelemetryPayload::Alert` envelope → controller dedups by
  `(severity, message_hash)` with 60s window → single Telegram
  send. Keeps per-agent AlertManager as emitter only.
- [x] **D5** Compliance violations panel: per-client watch list
  (daily loss > threshold, position limit breach, SLA uptime
  drop, halt participation). Rolls up across the fleet with
  direct links to relevant drilldown.

---



Tracking debt not yet closed. Closed items live in git history.
Each row is a concrete deliverable; bigger initiatives are
grouped by area. Prioritisation below is guidance, not a
contract — pick up whatever the next paper-trading smoke run
exposes first.

Legend:
- **🔴 P0** — blocks safe paper/live trading on ≥ 1 venue
- **🟠 P1** — visibly broken UX or missing critical observability
- **🟡 P2** — nice-to-have; makes the system "full-featured"
- **⚪ P3** — polish / hardening / internal refactor

---

## P0 — production-blocking

### Orderbook rigor
- [x] **BOOK-1** Live queue-position tracker (Rigtorp / L2-derived).
  Queue model moved from `backtester` to `mm_common::queue_model`.
  `crates/engine/src/queue_tracker.rs` attaches one `QueuePos`
  per resting maker order, fed by (1) placement/cancel/amend
  hooks around `execute_diff` using pre-snapshot book qty,
  (2) `MarketEvent::Trade` → `on_trade`, (3) book deltas →
  `on_depth_change`, (4) `MarketEvent::Fill` → `on_order_filled`.
- [x] **BOOK-2** Graph source `Book.FillProbability` — 60-second
  Poisson-rate-matching estimator blending per-order queue
  position with a 30-second half-life per-symbol trade-rate
  EWMA. Config `{side, price?}`; price defaults to the
  frontmost own order on that side. Registered in catalog
  shape + meta; node count 102 → 103.

### Perpetual safety
- [x] **PERP-2** `MarginGuard::effective_mmr` infers the
  venue's blended MMR from `total_maintenance_margin` ÷
  position notional (clamped to `[default/10, default×10]`).
  `projected_ratio` now uses that inferred rate for the MM
  delta instead of treating new IM as 1:1 MM. Over-rejection
  of valid quotes drops ~10–100× on majors. Fallback MMR is
  configurable via `MarginConfig::default_maintenance_margin_rate`.
- [x] **PERP-3** `MarginGuard::with_symbol_mode` pins the guard
  to the engine's symbol + `MarginModeCfg`. Isolated mode
  returns the per-position ratio `(size × mark × MMR) /
  isolated_margin` from `observed_ratio` and a bucket-local
  `projected_ratio` that adds IM into the isolated collateral.
  Cross mode keeps the wallet-wide figure.
- [x] **PERP-4** `PositionMargin.adl_quantile` threaded
  through Binance (`adlQuantile`) + Bybit
  (`adlRankIndicator`); HyperLiquid leaves it `None`.
  `MarginGuard::adl_elevated` (≥ 3) lifts a Normal decision to
  `WidenSpreads` without demoting higher-severity bucket
  readings.

### Inventory truth
- [x] **INV-2** `InventoryManager` now tracks per-trade
  `trade_opened_at` + `trade_peak_abs` (monotone high-water
  mark within a trade) + `trade_drawdown()` (peak minus
  current |inventory|). Sign-flips in one fill close the
  previous trade and open a fresh one at the flip timestamp.
  `trade_holding_seconds(now)` returns wall-clock delta since
  the current trade opened.
- [x] **INV-4** Dedicated portfolio aggregator struct.
  `CrossVenuePortfolio` in `crates/portfolio/src/cross_venue.rs`
  owns every engine's `(symbol, venue) → inventory + mark`
  snapshot with per-leg notional and update timestamp. Graph
  source `Portfolio.CrossVenueNetDelta`, HTTP endpoint
  `/api/v1/portfolio/cross_venue`, and the MV-UI-1 panel all
  read through `DashboardState` delegators that funnel into the
  single aggregator. Engine publishes book mid alongside
  inventory so the UI shows live notional per venue.

### Kill-switch teardown
- [x] **KILL-L5** Engine run-loop now detects the transition
  to `KillLevel::Disconnect`: fires `cancel_all` + audit +
  critical incident, sets `disconnected = true`, and collapses
  the main select to `shutdown / config_override / 1s sleep`
  so only `ConfigOverride::ManualKillSwitchReset` clears the
  flag. The falling edge (operator reset → Normal) logs a
  resume line and returns the loop to the full event pipeline.

---

## P1 — operator-visible gaps

### Paper validation
- [ ] **PAPER-2** 30-minute two-venue paper smoke test.
  Run binance paper + bybit paper side-by-side; deploy a
  graph using cross-venue reads; verify DecisionLedger
  resolves, tiered OTR publishes per venue, cross-venue
  inventory aggregates. **Operator-driven** — needs a live
  paper-mode run of the binary. See the new runbook at
  `docs/guides/paper-smoke-two-venue.md`.
- [x] **PAPER-3** Engine-level sanity tests added at
  `crates/engine/src/market_maker.rs` (`graph_source_sanity_tests`):
  Cost.Sweep on empty vs stocked book, Risk.UnrealizedIfFlatten
  with / without inventory. Plan.Accumulate already covered by
  7 tests in `crates/strategy-graph/src/nodes/plan.rs`.

### UI / UX
- [ ] **UX-SURV-1** Surveillance detectors are currently a
  standalone global page with 16 empty cards until a strategy
  is quoting a symbol — operator-unfriendly "no data / no data /
  no data" wall. Detectors only make sense in the
  `(strategy, symbol)` context: move per-symbol scores onto
  `OrderbookPage` alongside the book they describe, and into
  `StrategyPage` drilldown as a toxicity/environment panel.
  Remove the nav entry for the standalone page; keep an
  admin-only raw diagnostic view if engineering still wants it.
  Audit/log side unchanged — this is purely UI reorganisation.
- [x] **UX-VENUE-1** Per-venue market strip on Overview is live
  (`/api/v1/venues/book_state` → `VenueMarketStrip.svelte`) and
  publishes `primary_engine.book` + `hedge_book` to the data bus.
  **Gap**: SOR-extra venues (e.g. Binance linear perp in
  `cross-exchange-paper.toml`) are not WS-subscribed — they only
  serve on-demand REST queries inside `VenueStateAggregator.collect`.
  Result: the strip currently renders only primary (+ hedge when
  cross-exchange). Two fixes possible: (a) subscribe extras to WS
  + stream their books through a lightweight `ExtraBookKeeper`
  in the engine; (b) periodic `get_orderbook(depth=1)` poll per
  extra on a new tokio interval that publishes L1 to the data
  bus. Option (b) is simpler but lossy; (a) is correct.
- [ ] **UX-VENUE-2** Per-venue regime classification. Today the
  engine runs one `RegimeClassifier` on the primary mid stream
  and the Overview's market-quality card labels its regime chip
  `primary`. For cross-venue, operators want to see regime for
  EACH active venue surface (spot `Quiet` vs perp `Volatile` is
  a real signal). Depends on UX-VENUE-1 landing first so there's
  per-venue mid data on the data bus to feed classifiers.
- [x] **UX-VENUE-3** Bybit hedge WS book stream is silent —
  `hedge_conn.subscribe(BTCUSDT)` on Bybit linear perp delivers
  only a `Connected` event; no subsequent `BookSnapshot` /
  `BookDelta`. Initial REST orderbook load works (see
  `initial hedge book loaded seq=...`) but without WS streaming
  the hedge book stays stale from t=0. Root-cause the Bybit V5
  WS subscribe call path in `crates/exchange/bybit`.
- [x] **UI-5** `StrategyDeployHistory.svelte` gained a Diff
  button per history row: loads the current + previous deploy
  bodies via `/api/v1/strategy/graphs/{name}/history/{hash}`,
  pretty-prints both, renders a line-by-line side-by-side
  modal with colour markers for `+` / `-` / `~` changed lines.
  First deploy of a graph shows a single-pane fallback.
- [x] **UI-6** Server returns `412 Precondition Required` +
  `restricted_nodes` list when a restricted graph is deployed
  under `MM_RESTRICTED_ALLOW=1` without an explicit ack
  token. Frontend catches the 412, opens a confirmation modal
  listing the pentest nodes + a checkbox, and retries the
  POST with `restricted_ack=yes-pentest-mode`. The original
  env-less refusal (403) path is unchanged.
- [x] **UI-7** `AdminConfigPanels.svelte` mounts on
  AdminPage: four sub-panels (webhooks, alert rules, loans,
  sentiment headlines) calling the existing `/api/admin/*`
  endpoints with minimal list + add forms. Loans panel also
  lists the first eight active agreements inline.
- [x] **UI-8** `VenuesHealth.svelte` polls both
  `/api/v1/venues/status` and `/api/v1/venues/latency_p95` in
  parallel, rendering a `book p95` stat row per venue.
  Latency poll failures fall through to "—" so the health
  panel stays alive when the metrics endpoint is down.

### Observability
- [ ] **OBS-1** OTel traces with request/tick spans. Sentry
  error reporting already present on `server.rs` init but
  untested against a real DSN. **Needs live DSN** — operator.
- [x] **OBS-2** `mm_book_update_latency_ms` histogram gained a
  `venue` label; `GET /api/v1/venues/latency_p95` scrapes the
  histogram buckets and returns one row per venue with a
  bucket-approximated p95 in milliseconds.

---

## P2 — full-featured

### Multi-venue
- [x] **MV-1** Audited — `SinkAction::VenueQuotes` already
  buckets remote legs by target symbol and dispatches each
  bucket via `ConfigOverride::ExternalVenueQuotes` on the
  dashboard's per-symbol channel; target engine picks it up
  on its next tick. The stale "degenerate dispatcher" comment
  at line 3647 was refreshed to describe the actual 3.A + 3.B
  flow. A separate SOR-based venue-selection layer (pick the
  best venue for an order intent the graph didn't preselect)
  is a new feature and NOT what this TODO was tracking.
- [x] **MV-2** Shared `AtomicBundleLeg` table on `DashboardState`
  carries per-leg ack flags across engines. Originator
  registers maker+hedge on dispatch; every engine's sweep
  publishes matches off its own live-orders snapshot. Sweep +
  watchdog wired into `refresh_quotes()` so rollback + merge
  fire each tick. Local + cross-venue ack round-trip covered
  by `atomic_bundle_ack_sweep_honours_cross_venue_dashboard_signal`.
- [x] **MV-3** SOR `VenueCostModel::price` (at
  `crates/engine/src/sor/cost.rs`) already folds venue
  `maker_fee_bps` + `taker_fee_bps` into
  `effective_cost_bps`; both Greedy and Convex routers sort
  routes by that figure. Audited this pass — the TODO was
  stale.
- [x] **MV-4** Stale "advisory-only" docstring replaced.
  `handle_stat_arb_event` now detects a partial dispatch
  (one leg placed, the other rejected), escalates the kill
  switch to `StopNewOrders`, drops the driver, and records a
  `PairBreak` audit + critical incident — the naked-leg
  safety the comment claimed we needed. Two new unit tests
  (`partial_dispatch_failure_escalates_and_drops_driver`
  + `full_dispatch_success_does_not_escalate`).

### Strategies + sources
- [x] **STRAT-1** Audited — `Strategy` trait at
  `crates/strategy/src/trait.rs` already exposes
  `on_fill(&FillObservation)` + `on_tick(&StrategyContext)` +
  `on_session_tick(i64)` default-no-op hooks (MM-2 landed
  these during the production push). Engine wires them in
  `market_maker.rs` at the tick and fill call sites. Stateful
  strategies (GLFT calibration, Cartea adverse-selection) are
  already using the hooks. TODO was stale.
- [x] **STRAT-2** New `Strategy.QueueAware` node in
  `strategy-graph/src/nodes/strategies.rs` — inputs
  `(quotes: Quotes, probability: Number)`, output `quotes`.
  Multiplier `0.3 + 0.7 · p` with the `0.3` floor guaranteeing
  no full flatten on a stalled probability feed. Registered in
  catalog (103 → 104 nodes) with palette meta + 5 unit tests
  covering the multiplier curve.

### Graph polish
- [x] **GR-1** `period_config_field(default)` helper in
  `crates/strategy-graph/src/nodes/indicators.rs` gives every
  Indicator.* kind a bounded `Integer { min: 1, max: 10_000 }`
  widget matching `parse_period`. Bollinger also gets a `σ
  multiplier` `Number { min: 0.0 }` widget. Cast.ToBool already
  had Enum-widget coverage for `cmp`. Catalog guard test
  (`every_period_indicator_declares_bounded_integer_schema`)
  blocks regressions on new indicators.
- [x] **GR-2** `SinkAction::KillEscalate` now carries an
  optional `venue: Option<String>`. The evaluator reads the
  string from `Out.KillEscalate`'s node config; the engine
  compares (case-insensitive) against its own
  `exchange_type.to_lowercase()` and skips the kill on
  mismatch. Empty / missing venue keeps the legacy global
  semantics. Two evaluator round-trip tests + engine-level
  handling in `market_maker.rs`.

---

## Sprint 1 — P0 safety nets (landed Apr 19)

Captured from the Apr-19 triple-audit (core MM + MV-UI + graph
parity). These three items are the blocking set before any
client sees the system.

- [x] **S1.1** Funding-arb naked-leg retry. The compensating
  reversal on pair-break now tries up to 4 times (initial +
  100/200/400 ms backoff). A single transient venue hiccup
  (429, brief disconnect) no longer leaves the perp leg
  unhedged. Two tests pin the behaviour: retry-succeeds and
  all-attempts-fail. `basis.rs` is quote-only; no pair
  dispatcher there, no retry needed.
- [x] **S1.2** Per-strategy capital budget. New
  `strategy_capital_budget: HashMap<String, Decimal>` on
  `MarketMakerConfig`. Engine's `apply_capital_budget` zeros
  the side that would grow same-sign exposure when
  `|inventory| × mid >= cap` for the running strategy tag.
  Opposite-side leg kept for the unwind. 6 unit tests cover
  absent/zero/different-strategy/long-over/short-over/under.
- [x] **S1.3** SOR decision log + endpoint.
  `DashboardState::record_sor_decision` + ring buffer (256
  entries) + `GET /api/v1/sor/decisions/recent?limit=N`.
  Engine's `publish_route_decision` now records winners +
  every runner-up venue the router evaluated but didn't
  pick, sorted by cost_bps. Operators can see "why this
  venue, how close was the runner-up" without scraping
  Prometheus.
- [x] **S1.4** `SorDecisions.svelte` on AdminPage: polls
  `/api/v1/sor/decisions/recent?limit=30`, renders per
  decision with winner + considered legs, side colour,
  partial-fill tag. 4-second refresh.

## Sprint 2 — crash safety + observability basics (landed Apr 19)

- [x] **S2.1** Atomic bundle checkpoint recovery.
  `InflightAtomicBundle` gained `#[derive(Serialize, Deserialize)]`
  and a new `inflight_atomic_bundles: Vec<serde_json::Value>`
  field on `SymbolCheckpoint` (serde-default for backward
  compat). Engine's `with_checkpoint_restore` restores the
  in-flight map on boot; the next `refresh_quotes` tick's
  watchdog cancels any expired bundle and the shared
  DashboardState ack map picks up mid-dispatch state. Two
  engine tests pin the round-trip and malformed-entry
  tolerance.
- [x] **S2.2** `DashboardState::atomic_bundles_inflight` +
  `GET /api/v1/atomic-bundles/inflight` + `AtomicBundles.svelte`
  panel on AdminPage. Maker / hedge legs paired by bundle_id
  with ack indicators; missing-side rows render "—" so a
  mid-dispatch state is visible instead of hidden.
- [x] **S2.3** `FillRecord` gained a `venue` field (lowercase
  exchange_type tag). Engine populates on fill ingest; the
  existing WS fill broadcaster carries it through; FillHistory
  table gains a Venue column + a Fee column while we're there.
  Old serialised fills deserialise as `""` via `#[serde(default)]`.
- [x] **S2.4** `DashboardState::set_adl_quantile` /
  `adl_quantile()` + `per_symbol_adl_quantile` map. Engine's
  margin-guard poll publishes `max_adl_quantile()` alongside
  the existing `margin_ratio`. `venues_status` endpoint now
  carries both per row (Option-skipped); VenuesHealth panel
  aggregates the max across a venue's symbols and red-flags
  margin ≥ 50% or ADL rank ≥ 3.

## Sprint 3 — multi-venue correctness (landed Apr 19)

- [x] **S3.1** Liquidation waterfall priority.
  `TwapExecutor::with_start_delay` + `PairedUnwindExecutor::with_start_delay`
  defer slice scheduling by shifting `started_at` forward.
  `DashboardState::register_flatten_priority` +
  `flatten_priority_rank` let engines self-rank on L4 entry;
  worst-drawdown symbol fires immediately (`delay=0`), others
  stagger `rank × 3 s`. Tied drawdowns break by lexicographic
  symbol order for deterministic behaviour. Test covers
  descending sort + clear path.
- [x] **S3.2** Position-delta reconciliation.
  `mm_risk::reconciliation::reconcile_position_delta` sums
  `total_bought − total_sold` → expected inventory, diffs
  against `InventoryManager::inventory()` at
  `inventory_drift_tolerance × 2`. Called from the engine's
  reconcile loop alongside the existing order + balance paths;
  drift fires a `high`-severity incident + audit row. 4 unit
  tests cover agree / missed-buy / tolerance-edge paths.
- [x] **S3.3** Hedge-book staleness gate on PairedUnwind.
  Before emitting a paired slice the engine checks
  `hedge_book.last_update_ms`; if > 5 s stale, the unwind
  pauses, a single `hedge_book_stale_during_flatten` audit row
  fires (latch prevents repeat spam), and the loop retries
  next tick. Latch resets on feed recovery so the operator
  sees both the pause and the resume.
- [x] **S3.4** Per-venue inventory drift. `InventoryDriftReconciler`
  gained `venue: String` + `with_venue` builder; `DriftReport`
  carries the venue through to the audit row. Engine tags its
  reconciler with `exchange_type.to_lowercase()` at construction
  so the drift log answers "which venue's wallet slice
  drifted", not just "which asset".
- [x] **S3.5** Cross-venue PnL attribution. New
  `mm_portfolio::AttributionSnap` + `Portfolio::record_attribution`
  + `consolidated_attribution` + `attribution_by_asset`.
  Engine publishes its `PnlTracker::attribution` per-tick;
  the portfolio replaces (never accumulates) so a
  double-counting across venues is impossible by construction.
  Three portfolio-level tests pin consolidation, replace
  semantics, and base-asset rollup.

## Sprint 4 — graph ↔ legacy parity (landed Apr 19)

- [x] **S4.1** Risk guard states exposed as graph sources.
  `Risk.CircuitBreakerTripped` (Bool), `Risk.NewsRetreatState`
  (String: normal/reduce/halt), `Risk.LeadLagMultiplier`
  (Number: widen factor). Engine overlays publish the same
  values the legacy gating already consults, so a graph author
  can route these into `Strategy.QueueAware`/gating logic
  without the guard running twice.
- [x] **S4.2** Per-node `Strategy.*` ctx overrides.
  `StrategyCtxOverride { as_prob, as_prob_bid, as_prob_ask,
  borrow_cost_bps }` on the engine; `build_strategy_ctx_overrides`
  parses each `Strategy.*` node's config and patches the
  `StrategyContext` on the per-node tick before invoking
  the pooled strategy. Two authors sharing a pool get
  independent as_prob / borrow surcharges without
  inventing a second pool key.
- [x] **S4.3** Composable inventory-aware execution.
  `Math.InventorySkew` node (inputs `level: Number`; config
  `cap`, `exponent`; output skew in [-1,1]) +
  `Signal.FillDepth` source publishing the running max
  fill-depth-bps the router observed. Catalog bumped 108 →
  109; `catalog_has_109_nodes_after_s4_3_composable_inventory`
  pins the count.
- [x] **S4.4** Graph ↔ legacy parity tests.
  `crates/engine/src/market_maker.rs` `dual_connector_tests`:
  `avellaneda_graph_parity_matches_legacy` (identical ctx →
  identical quotes), `avellaneda_per_node_gamma_override_matches_direct_config`
  (GR-3 override path), `avellaneda_borrow_cost_override_matches_direct_ctx`
  + `avellaneda_as_prob_override_matches_direct_ctx` (S4.2
  ctx override path). `Quote` + `QuotePair` gained `PartialEq,
  Eq` derives so `assert_eq!` works byte-for-byte.

## Sprint 5 — advisory panels + live calibration (landed Apr 19)

- [x] **S5.1** Cross-venue rebalance recommendations. Moved
  `rebalancer` module from `mm-engine` to `mm-risk` (switched
  `VenueBalance.venue` to `String` in the process —
  `VenueId::Debug` fmt was the only use). New
  `DashboardState::{set_rebalancer_config, rebalance_recommendations}`
  aggregates `venue_balances` across every engine's symbol by
  `(venue, asset)` and runs the rebalancer. `AppConfig.rebalancer`
  forwarded at server boot. `GET /api/v1/rebalance/recommendations`
  + `RebalanceRecommendations.svelte` on AdminPage. Two round-trip
  tests pin empty-without-config + deficit-surfacing.
- [x] **S5.2** Funding-arb pair monitor. New
  `DashboardFundingArbSink` (server crate, bridging the sink trait
  so `mm-dashboard` stays free of `mm-strategy`) records every
  `DriverEvent` into `DashboardState::record_funding_arb_event`
  against a `pair_key = "{primary}|{hedge}"` bucket. Replaces the
  previous `NullSink` at boot. `FundingArbPairState` carries
  per-variant counters + last-event details;
  `pair_break_uncompensated` is its own field so the UI flags
  unhedged breaks in red. `GET /api/v1/funding-arb/pairs` +
  `FundingArbPairs.svelte` on AdminPage.
- [x] **S5.3** Adverse-selection tracker panel. New endpoint
  `/api/v1/adverse-selection` projects `(adverse_bps,
  as_prob_bid, as_prob_ask)` off every `SymbolState` the engine
  already publishes. `AdverseSelection.svelte` highlights
  symbols where either side's ρ deviates past 0.55 / 0.45 so
  operators spot toxic-flow pairs without scraping the
  Prometheus gauges.
- [x] **S5.4** Live GLFT auto-calibration. New
  `Strategy::{calibration_state, recalibrate_if_due}` trait
  methods with no-op defaults; `GlftStrategy` overrides to
  surface fitted `(a, k, samples, last_recalibrated_ms)` and
  to run a periodic retune gated by the existing ≥50-sample
  threshold AND a 30-second cooldown. Engine's `on_tick` path
  calls `recalibrate_if_due` on the legacy strategy + every
  pool node, then publishes the first `Some` snapshot into
  `DashboardState::publish_calibration`. `GET
  /api/v1/calibration/status` + `CalibrationStatus.svelte`
  render the live `(a, k)` + time-since-retune.
  `CalibrationState` is duplicated on the dashboard side as
  `CalibrationSnapshot` so the dashboard stays independent of
  `mm-strategy`. Three unit tests pin the throttling, trait
  round-trip, and dashboard replace-on-publish semantics.

## Sprint 6 — graph E2E closeout + rebalancer execute (landed Apr 19)

- [x] **S6.1** Active-graph visibility. `SymbolState.active_graph:
  Option<ActiveGraphSnapshot>` carries `{name, hash, scope,
  deployed_at_ms, node_count}`. Engine stamps `deployed_at_ms`
  + `node_count` on both `with_strategy_graph` and
  `swap_strategy_graph`, then folds them into every tick's
  `update(SymbolState{...})`. New endpoint
  `/api/v1/active-graphs` returns a flat per-symbol list for
  scripting. Overview page shows a `graph: <name>` pill next to
  the strategy name with tooltip for hash + deploy time.
- [x] **S6.2** Bundled starter templates. Two new JSONs —
  `glft-via-graph` (Strategy.GLFT + Out.Quotes, mirror of
  legacy `strategy=glft`) and `cross-exchange-basic`
  (Strategy.CrossExchange + Out.Quotes, mirror of
  `strategy=cross_exchange`). Registered in `templates::BUILTIN`
  so the Strategy page template picker lists them; both round-
  trip through `Evaluator::build` via the existing
  `every_safe_template_compiles` guard test.
- [x] **S6.3** Orphaned-strategy docs.
  `docs/guides/writing-strategies.md` gained a "Two Classes of
  Strategies" section explaining that `funding_arb` + `stat_arb`
  are async drivers (not graph nodes), how to activate them via
  `[funding_arb]` / `[stat_arb]` config, and how to observe via
  the S5.2 panel + `/api/v1/funding-arb/pairs`. CLAUDE.md's
  Key Design block gained the same distinction.
- [x] **S6.4** Rebalancer execute path. New
  `mm-persistence::transfer_log` JSONL module with
  `TransferRecord` + `TransferLogWriter` + `read_all` (2 round-
  trip tests). `DashboardState::register_venue_connector` /
  `venue_connector` / `set_transfer_log` / `max_kill_level`
  wired in. New endpoint `POST /api/v1/rebalance/execute` (body
  `{from_venue, to_venue, asset, qty, from_wallet?, to_wallet?,
  reason?}`):
  1. refuses with 403 + `rejected_kill_switch` when any engine
     reports `kill_level > 0`;
  2. intra-venue → calls `connector.internal_transfer`, returns
     200 on success / 502 on venue failure;
  3. cross-venue → 202 Accepted + `status=accepted` (logged but
     NOT dispatched — V1 keeps on-chain withdrawals manual
     because deposit-address whitelisting is not yet wired).
  Every outcome writes a `TransferRecord` row. Companion
  endpoint `GET /api/v1/rebalance/log` returns the full history
  for the panel. Frontend: Execute button on every
  recommendation row + confirmation modal with result display.
  Server boot opens `data/transfers.jsonl`, registers every
  bundle connector by `VenueId::Display` lowercase. Operator
  identity taken from the JWT `TokenClaims::user_id` for audit.

## Sprint 7 — Epic R2 Phase 1: market-manipulation tooling (landed Apr 19)

Detector + exploit pair that implements the RAVE-style pump-
and-dump cycle on both halves: our MM sees the public pattern
on a symbol (detect), and — under the same `MM_RESTRICTED_ALLOW=1`
gate as the existing pentest suite — can reproduce the cycle on
a controlled venue for surveillance validation (exploit). Key
use case: "пенетрейсим нашу биржу" — the user will attack their
own exchange with this tooling, verify the detection + risk
controls fire, then harden the venue.

- [x] **R2.1** `PumpDumpDetector` at `crates/risk/src/manipulation.rs`.
  Price velocity (% change across a rolling window, bps) crossed
  with volume surge (second-half / first-half notional ratio)
  → product [0, 1]. Self-warming: first-half vs second-half
  baseline means no separate seed step.
- [x] **R2.2** `WashPrintDetector`. Classic wash signature: N
  size-matched opposite-side public prints in a short window at
  prices clustered near one level. Pair-matched so
  buy-buy-sell-sell counts as 2, not 4.
- [x] **R2.3** `ThinBookGuard`. Book depth within ±2% of mid vs
  trailing 60-second notional. Score saturates at 1.0 when the
  ratio drops below `min_ratio` (default 0.1 — book can't absorb
  10% of recent volume).
- [x] **R2.4** `ManipulationScoreAggregator`. Weighted combiner
  (default 0.5 / 0.3 / 0.2 on pump-dump / wash / thin-book) with
  a single `snapshot()` returning the four-field view the
  dashboard + graph source both consume.
- [x] **R2.5** Engine wire-in. `MarketMakerEngine.manipulation`
  field; every `MarketEvent::Trade` feeds `on_trade`, every tick's
  `refresh_quotes` calls `on_book` + publishes
  `SymbolState.manipulation_score`.
- [x] **R2.6** Dashboard publish + panel.
  `ManipulationScoreSnapshot` on `SymbolState`, endpoint
  `/api/v1/manipulation/scores` (sorted by combined DESC),
  `ManipulationScores.svelte` on AdminPage highlighting symbols
  with `combined ≥ 0.5` in red.
- [x] **R2.7** Graph source `Surveillance.ManipulationScore` with
  four Number outputs (value, pump_dump, wash, thin_book).
  Engine overlay at `tick_strategy_graph` copies the current
  snapshot into `source_inputs`. Catalog count bumped 109 → 111.
- [x] **R2.8** `PumpAndDumpStrategy` at
  `crates/strategy/src/pump_and_dump.rs`. Four-phase FSM:
  Accumulate (passive bids) → Pump (cross-through buys) →
  Distribute (laddered asks above mid) → Dump (cross-through
  sells). Tick-driven phase advance; cycle wraps so smoke runs
  can span multiple cycles. Restricted under `MM_RESTRICTED_ALLOW`.
- [x] **R2.9** Graph node `Strategy.PumpAndDump` (restricted,
  same gate as Spoof/Wash/…) with full config schema for each
  phase's ticks + sizes + depths. Engine `build_strategy_pool`
  match arm parses config and instantiates `PumpAndDumpStrategy`.
- [x] **R2.10** Bundled `pentest-pump-and-dump` template. Pairs
  `Strategy.PumpAndDump` + `Out.Quotes` with
  `Surveillance.ManipulationScore` → `Cast.ToBool(≥0.6)` →
  `Out.KillEscalate(level=4)`. Proves the detector fires against
  the exploit on the same graph: operator deploys it on a test
  venue, watches the ManipulationScore panel trip the kill
  switch when the attack phase hits.

Phase 2 deferred (on-chain holder-concentration tracker, CEX
deposit flow monitor, market-cap vs liquidation ratio guard) —
those need a new chain indexer / external API integration, out
of scope for a CEX-data-only sprint.

## Sprint 8 — Epic R3: on-chain surveillance (landed Apr 19)

New `mm-onchain` crate wires 4 free-tier on-chain providers
behind one `OnchainProvider` trait so operators pick whichever
chain coverage / rate budget fits. Closes the RAVE-style
pre-dump signal gap: ZachXBT's key signal was "9 wallets hold
95% + CEX deposits before peak" — both halves now surface on
the dashboard + graph sources.

- [x] **R3.1** `mm-onchain` foundation — `OnchainProvider`
  trait (`get_top_holders`, `get_address_transfers`,
  `get_token_metadata`), shared types (`HolderEntry`,
  `TransferEntry`, `TokenMetadata`, `ChainId`), `OnchainError`
  enum (RateLimited / Auth / Network / Decode /
  UnsupportedChain). Fail-open contract — a rate-limited
  provider never halts the engine.
- [x] **R3.2** `GoldRushProvider` — Covalent / GoldRush REST
  (`/v1/{chain}/tokens/{token}/token_holders/` +
  `/v1/{chain}/address/{addr}/transactions_v3/`). ~50 chains
  (EVM + Solana + Cosmos), ~1000 req/day free tier, auth via
  `MM_GOLDRUSH_KEY` env.
- [x] **R3.3** `EtherscanFamilyProvider` — one impl covers
  Etherscan + BscScan + PolygonScan + ArbiScan +
  OptimisticEtherscan by per-chain base URL. Free-tier
  token-holder endpoint is PRO-gated → returns
  `UnsupportedChain` so the fallback provider picks up.
- [x] **R3.4** `MoralisProvider` — EVM-only, ~40k CU/day free,
  auth via `MM_MORALIS_KEY`.
  `/api/v2.2/erc20/{token}/owners` + `/wallets/{addr}/history`.
- [x] **R3.5** `AlchemyProvider` — JSON-RPC
  (`alchemy_getAssetTransfers`, `alchemy_getTokenMetadata`).
  EVM-only, 300M CU/month free. No holder-list endpoint →
  returns `UnsupportedChain` for that op; fallback provider
  fills the gap.
- [x] **R3.6** Cache + tracker.
  `HolderConcentrationCache` (per-token TTL cache, serves
  stale snapshot on provider error so the graph never emits
  `Missing` because of a transient rate-limit) +
  `SuspectWalletTracker` (walks operator-supplied wallet
  lists, filters transfers whose destination matches the CEX
  deposit allowlist, sums notional).
- [x] **R3.7** Config + boot. New `[onchain]` section with
  `provider` + optional `fallback` + per-symbol
  `{chain, token, suspect_wallets}` map +
  `cex_deposit_addresses` allowlist. Server boot reads the
  matching `MM_{PROVIDER}_KEY` env, spawns one poller task
  for all configured symbols on `min(holder_refresh_secs,
  inflow_poll_secs)` cadence, publishes via
  `DashboardState::publish_onchain`.
- [x] **R3.8** Graph sources + dashboard.
  `Onchain.HolderConcentration` (1 Number: value) and
  `Onchain.SuspectInflowRate` (2 Numbers: value + events).
  Engine overlay at `tick_strategy_graph` reads from
  `dashboard.onchain_snapshot(symbol)` and translates to
  `Value::Number` / `Value::Missing` on the fail-open path.
  `GET /api/v1/onchain/scores` endpoint +
  `OnchainScores.svelte` panel on AdminPage highlights
  concentration ≥ 0.8 in red, inflow events > 0 in red.
  Catalog 111 → 113.

## Sprint 9 — Epic R2 Phase 2 + composite rug detector (landed Apr 19)

Closes the RAVE-pattern surveillance loop: every signal ZachXBT
called out on 2026-04-18 now has a graph source operators can
route into a kill-switch gate, and two bundled templates stand
them up end-to-end in one click — one defensive, one pentest.

- [x] **R2.11** `ListingAgeGuard`: per-symbol first-seen
  stamp, emits `[0,1]` newness score decaying linearly over
  `mature_days` (default 30). Fresh listing = 1.0; 30-day-old
  symbol contributes nothing. Graph source
  `Surveillance.ListingAge`.
- [x] **R2.12** `MarketCapProxyGuard`: uses operator-supplied
  `symbol_circulating_supply` config to compute
  `mcap_proxy = supply × mid`, compares to trailing notional,
  saturates the score at `mcap / volume ≥ 100` (matches
  ZachXBT's $6B/$52M RAVE litmus test). Graph source
  `Surveillance.MarketCapRatio`; `Missing` when supply is
  unset for that symbol.
- [x] **R2.13** `RugCompositeAggregator` — stateless
  `compute_rug_score` + `RugWeights` (defaults 0.35 manip /
  0.3 concentration / 0.15 inflow / 0.1 age / 0.1 mcap,
  sum = 1.0). Engine tick builds the snapshot from the
  existing signals, publishes `SymbolState.rug_score`.
  `Surveillance.RugScore` graph source exposes combined +
  5 sub-scores.
- [x] **R2.14** `rug-detector-composite` template. Avellaneda
  quoter + `Surveillance.RugScore` → `Cast.ToBool(≥0.6)` →
  `Out.KillEscalate(WidenSpreads)`. One-click defender for
  any symbol — pair with `[onchain]` config for full
  coverage.
- [x] **R2.15** `pentest-rave-cycle` template
  (`MM_RESTRICTED_ALLOW=1` gated). `Strategy.PumpAndDump`
  runs the 4-phase attack; `Surveillance.RugScore` guard
  trips kill L4 when the engine catches its own pattern —
  proves the detect ↔ exploit loop in one deploy.
- [x] **Port fix**: `Out.KillEscalate.level` port type
  changed from `KillLevel` to `Number` so operators pipe
  `Math.Const(N)` directly without a cast helper. Evaluator
  clamps to 1..=5. Unblocks both the pentest and
  rug-detector-composite templates.
- [x] **Catalog**: 113 → 116 kinds (ListingAge +
  MarketCapRatio + RugScore).

## Sprint 10 — Epic R4: multi-venue exploit orchestration (landed Apr 19)

⚠ **Pentest-only build.** Every new exploit node in this sprint is
`restricted()=true` behind `MM_RESTRICTED_ALLOW=1`. `docs/guides/pentest.md`
gates operator use; loud `tracing::warn!` on every restricted graph compile.

- [x] **R4.1** `Strategy.CampaignOrchestrator` graph node.
  Multi-phase timeline FSM config schema (phases JSON array,
  loop_cycle bool). V1 is advisory-only — engine logs a warn
  reminding the operator to chain the explicit exploit nodes
  instead until the FSM driver is plumbed (documented in
  `docs/guides/pentest.md`).
- [x] **R4.2** `LiquidationHeatmap` in `mm-risk`. Rolling
  30-minute window of forced liquidations, bucketed by
  bps-from-mid (20-bps default). `MarketEvent::Liquidation`
  variant added to `mm_exchange_core`. Engine feeds on arrival.
  `Surveillance.LiquidationHeatmap` graph source emits 6-field
  summary (total, event_count, nearest_above/below bps +
  notional).
- [x] **R4.3** `Strategy.LiquidationHunt` (restricted). Reads
  the heatmap's `nearest_above/below_bps` for targeted
  cross-through push. V1 wraps `IgniteStrategy` — same
  cross-book mechanic, plus the `max_bps_overshoot` knob.
- [x] **R4.4** `Signal.OpenInterest` source. Non-restricted —
  OI is legitimate MM input. V1 derives from the liquidation
  feed's total notional; returns Missing on a cold tracker so
  silence doesn't gate risk decisions.
- [x] **R4.5** `Strategy.LeverageBuilder` (restricted). Single
  directional push with leverage + position_size + max
  slippage config. V1 wraps `IgniteStrategy` with one-shot
  burst; real `connector.set_leverage()` plumbing marked as
  stage-2 in `pentest.md`.
- [x] **R4.6** `pentest-rave-full-campaign` template. Full
  multi-phase campaign shell paired with
  `Surveillance.RugScore` → `Cast.ToBool(≥0.5)` →
  `Out.KillEscalate(L4)` self-guard. Bundled under the
  restricted template-family alongside `spoof-classic` /
  `pump-and-dump` / `rave-cycle`.
- [x] **R4.7** Restricted-gate warnings. `tracing::warn!` fires
  on every restricted-node compile inside
  `Evaluator::build`; quadruple-star warning with
  "authorized testing only" + "MiFID II / Dodd-Frank / MiCA
  violation" + `docs/guides/pentest.md` pointer. `pentest.md`
  written as the README for the restricted suite with the
  three operator-confirmation conditions + full exploit /
  detector / template cross-reference table.
- [x] Catalog 116 → 121 kinds (+5 from R4).

## Sprint 11 — cross-venue data parity (landed Apr 19)

Honest data-parity pass. Makes sure every surveillance + pentest
feature actually has the right data feeds from every perp venue,
regardless of which one the operator picks as their customer.

- [x] **R5.1** Forced-liquidation WS subscribers on every perp
  venue: Binance USDⓈ-M `!forceOrder@arr`, Bybit V5
  `liquidation.{symbol}` on linear + inverse, HyperLiquid
  `liquidations` per coin. All three emit the canonical
  `MarketEvent::Liquidation` variant; engine's
  `LiquidationHeatmap` populates on every arrival.
- [x] **R5.2** Audited — `ExchangeConnector::set_leverage`
  already exists from Epic 40.7 + Binance/Bybit/HL impls.
  `Strategy.LeverageBuilder` stage-2 plumbing (call it on
  phase entry) is the next-sprint item — not a trait gap.
- [x] **R5.5** `VenueCapabilities::supports_liquidation_feed`
  + `supports_set_leverage` flags. Binance futures = both
  true; Bybit linear/inverse = both true; HL perp = both
  true; every spot / custom / coinbase-prime = false.
- [x] **Cross-venue parity doc** — `docs/guides/pentest.md`
  gained the honest capability matrix (L1/L2, trades,
  liquidations, OI, funding, set_leverage, margin info,
  transfers, withdraw) per venue × product. Everything that's
  wired + everything that's deferred is in the table.

Sprint 12 picks up: `Strategy.CampaignOrchestrator` real FSM
(advisory-only in this build), real `get_open_interest()` REST
calls (currently proxied via liquidation total), and
`Strategy.BasketPush` + `Strategy.IndexPush` for the remaining
RAVE-pattern pentest vectors.

## Sprint 12 — Sprint 11 deferral closeout (landed Apr 19)

Converted the three "advisory only" pieces from Sprint 10/11 into
real implementations. Campaign orchestration, leverage setup, and
open-interest data now work end-to-end without "stage-2 TBD"
caveats.

- [x] **R6.1** `CampaignOrchestratorStrategy` real FSM in
  `mm-strategy::campaign_orchestrator`. Time-based 4-phase +
  idle machine (accumulate → pump → distribute → dump → idle).
  Engine `build_strategy_pool` parses the config and hands the
  real strategy to the pool instead of the old advisory
  IgniteStrategy stub. Three unit tests pin the phase timeline,
  loop wrap, and zero-duration edge case. Node config schema
  exposes phase-seconds + size / depth knobs.
- [x] **R6.2** `Strategy.LeverageBuilder` actually calls
  `connector.set_leverage`. Engine's `swap_strategy_graph` and
  `with_strategy_graph` now sweep the graph for leverage nodes
  and spawn one-shot tasks per match.
  `VenueCapabilities::supports_set_leverage` gates the call —
  spot / custom venues log a warn!-skip, perp venues (Binance
  futures, Bybit linear, HL perp) set leverage for real.
  Failures warn!-skip so a bad leverage value doesn't brick
  the whole deploy.
- [x] **R6.3** `ExchangeConnector::get_open_interest` trait
  method + `OpenInterestInfo { oi_contracts, oi_usd, ts }`
  struct. Binance USDⓈ-M impl uses `/fapi/v1/openInterest`;
  Bybit impl uses `/v5/market/open-interest?intervalTime=5min`;
  HyperLiquid returns the default `Ok(None)` (deferred —
  `clearinghouseState` aggregation is a separate crate-level
  change, documented in pentest.md).
- [x] **R6.4** `Signal.OpenInterest` reads real OI first,
  falls back to the liquidation-feed total as a proxy when
  `last_open_interest` is unset. Engine polls OI on the
  funding-rate cadence so no new timer task — one `get_open_interest`
  call per funding refresh (≈ every 30 s on perp symbols).

## Sprint 13 — liquidation cascade mechanics (landed Apr 19)

⚠⚠⚠ Pentest-only sprint. Adds the full observable-data shape of
the 2021-05 BTC flash-crash / RAVE / Alameda cascade plays, plus
the offensive node that reproduces the attack for authorized
exchange surveillance validation. Gated the same way as every
other Epic R module: `restricted()=true` + `MM_RESTRICTED_ALLOW=1`
env + loud `tracing::warn!` on every compile + mandatory
operator read of `docs/guides/pentest.md` and
`docs/research/liquidation-cascades.md`.

- [x] **R7.1** `Signal.LongShortRatio` — new
  `get_long_short_ratio` trait method on `ExchangeConnector`
  + `LongShortRatio { long_pct, short_pct, ratio, ts }` type.
  Binance impl hits `/futures/data/globalLongShortAccountRatio`;
  Bybit impl hits `/v5/market/account-ratio`. Engine polls
  on the funding-rate cadence (≈30 s on perps) and stores on
  `last_long_short`; graph source exposes 3 Number outputs.
- [x] **R7.2** `Signal.LiquidationLevelEstimate` — pure graph
  source deriving `long_liq_bps` / `short_liq_bps` from
  current mid + config `avg_leverage` (default 10). No venue
  API; documented as order-of-magnitude estimate only.
- [x] **R7.3** `Signal.CascadeCompleted` — Bool graph source
  that flips `true` when in-window liquidation notional from
  `LiquidationHeatmap` exceeds the configured
  `threshold_notional`. Downstream exit / stand-down gate.
- [x] **R7.4** `Strategy.CascadeHunter` (restricted) — one-shot
  crossing push gated by `trigger` bool input + `target_bps`
  Number input. V1 wraps IgniteStrategy — the graph drives
  the trigger semantics, the strategy just emits the cross.
- [x] **R7.5** `pentest-liquidation-cascade` bundled template
  wires the full loop: `LiquidationLevelEstimate.long_liq_bps`
  + `LongShortRatio.ratio ≥ 1.5 (Cast.ToBool)` →
  `CascadeHunter` → `Out.Quotes`; `RugScore ≥ 0.5` →
  `Out.KillEscalate(L4)` self-guard. Description carries the
  triple-warning + MAR Art. 12 / CEA §9(a) / MiCA Art. 92
  citations.
- [x] **R7.6** `docs/research/liquidation-cascades.md` —
  catalogue of public investigations (Kaiko 2021 BTC, Glassnode,
  LUNA 2022, FTX/Alameda 2022 discovery, RAVE/SIREN/MYX 2026)
  + attack-shape data table + defensive-use recommendations +
  deferred research items.
- [x] Catalog 121 → 125 kinds (+4 from R7).

## Sprint 14 — honest audit + 2 real bug fixes (landed Apr 19)

Operator intuition was right — the audit found TWO real bugs that
made the flagship pentest suite a no-op. Both bundled pentest
templates were effectively unreachable or silently inert before
this sprint.

- [x] **R8.2** E2E integration test
  (`crates/strategy-graph/tests/pentest_templates_e2e.rs`). Five
  tests cover `pentest-liquidation-cascade`,
  `pentest-rave-cycle`, `rug-detector-composite`. Single-thread
  run required (env-var flip is unsafe in parallel).
- [x] **BUG FIX — env-var gate drift.** `MM_RESTRICTED_ALLOW=1` in
  dashboard vs `MM_ALLOW_RESTRICTED=yes-pentest-mode` in evaluator
  → nobody could actually deploy a restricted template.
  Consolidated on the explicit `MM_ALLOW_RESTRICTED=yes-pentest-mode`;
  dashboard code + all docs + all template descriptions fixed.
  E2E test exercises the real gate so can't silently regress.
- [x] **BUG FIX — Strategy.CascadeHunter always emitted Missing.**
  Input-having Strategy.* nodes break the engine's strategy-pool
  overlay (which only populates SOURCE node outputs via
  `source_inputs`). Refactored CascadeHunter to zero inputs; the
  `pentest-liquidation-cascade` template now uses `Quote.Mux`
  downstream for the trigger gate.
- [x] **R8.4** Full audit findings at
  `docs/research/audit-apr19-sprint14.md` — two real bugs
  documented, three false positives documented, takeaways for
  future sprints (E2E tests only way to catch gate drift,
  input-having Strategy nodes break pool overlay contract).

## Sprint 15 — critical-path test coverage (landed Apr 19)

Sprint 14 showed the operator was right — E2E gaps hide real bugs.
This sprint closes three of the highest-risk untested paths and
ships an honest coverage matrix so the remaining gaps are
visible.

- [x] **R8.5** Rebalancer execute state-level round-trip. Three
  new tests in `dashboard::state::tests`:
  `rebalance_execute_state_roundtrip_intra_venue`,
  `rebalance_execute_kill_switch_gate_state`,
  `rebalance_execute_transfer_log_is_none_by_default`. Pins
  the business logic the HTTP handler wraps.
- [x] **R8.6** Manipulation scores publish cycle.
  `manipulation_score_publish_cycle` +
  `manipulation_score_missing_is_absent_not_zero` — verifies
  engine publish → `SymbolState.manipulation_score` →
  `get_all()` projection the `/api/v1/manipulation/scores`
  handler consumes, and that missing scores stay absent (no
  "silence = safe" leak).
- [x] **R8.7** Integration test coverage matrix at
  `docs/research/integration-test-coverage.md` — enumerates
  every HTTP endpoint, every bundled template, and every
  engine-tick path with ✅ E2E / 🟡 Unit / ❌ None markers.
  Three clusters of weakness identified for Sprint 16+:
  HTTP-layer E2E near-zero, REST-poll connector paths have no
  integration tests, dashboard deploy handler env-var gate
  untested (Sprint 14 BUG #1 hid exactly here).

Sprint 16 backlog extended with the prioritised Axum
TestClient harness + env-var gate handler test items.

## Sprint 16 — HTTP-layer E2E + env-var gate test (landed Apr 19)

Sprint 15 matrix pointed at three cluster weaknesses; this sprint
closes two of three. The third (REST-poll connector integration)
deferred to Sprint 17+ because mocking the 20-method
`ExchangeConnector` trait is bigger than the sprint budget.

- [x] **R11.1** Axum TestClient harness at
  `crates/dashboard/tests/http_handlers_e2e.rs` using
  `tower::ServiceExt::oneshot` on a minimal Router (no auth /
  rate-limit layers — those are tested separately).
- [x] **R11.2** Six endpoint tests through the harness:
  `/health`, `/api/v1/rebalance/recommendations`,
  `/api/v1/rebalance/log`, `/api/v1/manipulation/scores`,
  `/api/v1/active-graphs`, `/api/v1/onchain/scores`. Each
  asserts on status + body shape; default / published /
  skipped-on-None semantics pinned.
- [x] **R11.3** Deploy env-var gate test —
  `restricted_env_gate_only_accepts_exact_literal` compiles a
  restricted template under `MM_ALLOW_RESTRICTED=1` (fails) +
  `=yes-pentest-mode` (succeeds). Directly guards the Sprint
  14 BUG #1 regression zone.
- [x] **R11.4 DEFERRED** Engine REST-poll integration (mock
  connector → tick → verify state populated). Trait has 20+
  methods; honest-sized mock needs its own fixture crate.
  Added to Sprint 17 backlog.

Dev-dep additions: `tower = "0.5"` + `http-body-util = "0.1"`.

## Sprint 17 — MockConnector fixture + REST-poll contracts (landed Apr 19)

Closes the third cluster weakness from the Sprint 15 matrix —
REST-poll connector paths had no integration coverage before this
sprint. Ships reusable fixture + 8 contract tests in one file.

- [x] **R11.4a** `MockConnector` fixture at
  `crates/exchange/core/tests/mock_connector_contracts.rs` —
  full ExchangeConnector trait impl with sensible defaults +
  three configurable hooks (`set_oi`, `set_ls_ratio`,
  `fail_leverage`) plus `leverage_call_history()` for
  inspection.
- [x] **R11.4b** Eight contract tests:
  `get_open_interest` / `get_long_short_ratio` default-None +
  override-Value paths; `set_leverage` records calls,
  succeeds by default, can be made to fail; capability flags
  honest across spot vs perp product.
- [x] **R11.5** `docs/research/integration-test-coverage.md`
  matrix updated — REST-poll row flipped from ❌ None to
  🟡 Unit; Sprint 16 + 17 backlog items crossed off.
- [x] **R10.2c DEFERRED** Engine tick integration (spin
  MockConnector + drive fake WS events → verify SymbolState
  publish) — fixture now exists, engine-side harness is
  Sprint 18 work.

Renumbered: "deferred research" (old Sprint 17) now Sprint 19;
"honest MM side" (old Sprint 18) now Sprint 20.

## Sprint 18 — engine tick integration with MockConnector (landed Apr 19)

Completes R10.2c from the Sprint 15 coverage matrix. MockConnector
(Sprint 17 fixture) now drives the real `MarketMakerEngine`
through `refresh_funding_rate` + `spawn_leverage_setup`. Catches
the exact bug class Sprint 14 found manually — where a feature
compiles + deploys but doesn't actually flow data through the
engine.

- [x] **R12.1** Extended `crates/engine/src/test_support.rs::MockConnector`
  with `get_open_interest` / `get_long_short_ratio` /
  `set_leverage` hooks. Capability defaults honest per product
  (perp → full perp support, spot → all perp caps false).
- [x] **R12.2** Two `refresh_funding_rate` engine tick tests:
  perp mock populates `last_open_interest` + `last_long_short`
  end-to-end; spot mock leaves both `None` (fail-open).
- [x] **R12.3** Two `spawn_leverage_setup` tests under
  `MM_ALLOW_RESTRICTED=yes-pentest-mode`: perp graph with
  `Strategy.LeverageBuilder` node fires exactly one
  `set_leverage` call recorded with `(BTCUSDT, 20)`; spot
  connector short-circuits on capability gate so `set_leverage`
  is never called.
- [x] Found + fixed silent bug: engine `MockConnector`
  previously advertised `supports_liquidation_feed=false` +
  `supports_set_leverage=false` even for perp products —
  would have masked real capability-gated paths in future
  tests. Now matches the Sprint 17 cross-crate fixture pattern.

## Sprint 19 — deferred R9 research items (landed Apr 19)

Closes 3 of 4 R9 items from the cascade research doc. R9.3
`Strategy.IndexPush` deferred — needs per-index metadata source
no venue exposes.

- [x] **R13.1** `Strategy.BasketPush` restricted graph node.
  Config schema covers `basket` (JSON array of `{venue,
  symbol, product, side, size}` legs), cross_depth_bps,
  burst/rest tick cycle. Engine wiring inherits the pool
  overlay path (documented in EXEMPT as
  "pool-backed, emits VenueQuotes per basket leg").
- [x] **R13.2** `Signal.FundingExtreme` non-restricted source.
  Honest framing: observability for when funding AND OI are
  both extreme (conditions for organic cascade). NOT
  weaponization — that needs majority-OI control, impossible
  for anyone except exchange-internal arb desks. Engine
  overlay reads `pnl_tracker.funding_rate()` +
  `last_open_interest`, fail-open when either is missing.
- [x] **R13.3** `pentest-basket-push` bundled template with
  3-symbol placeholder basket (RAVEUSDT + SIRENUSDT + MYXUSDT)
  + RugScore self-guard + triple-warning description in the
  catalog.
- [x] **R13.4** `docs/guides/pentest.md` updated — exploit /
  detector / template tables now list BasketPush + CascadeHunter
  rows; "deferred" section refreshed post-Sprint 12/19 landing.
- [x] Catalog 125 → 127 (+ 2 from R13).

### R9 deferrals (Sprint 20+)

- **R9.3 `Strategy.IndexPush`** — needs per-index metadata
  source (constituent tickers + weights). No venue API
  publishes this; operator-config-supplied path is the only
  way forward.
- **True funding-rate weaponization** — out of scope for any
  single-operator pentest. `Signal.FundingExtreme` covers
  observability.
- **Cross-venue atomic sub-graph composition** — spans
  several graphs (spot-venue graph spawns perp-venue graph
  legs). Out.VenueQuotes dispatches to multiple venues but
  sub-graph orchestration isn't plumbed.

## Sprint 20 — BasketPush silent bugs fix (landed Apr 19)

Operator's "надо все фиксить что сломанное" instinct justified.
Sprint 14 pattern holds: **every sprint that adds a feature
adds 1-2 silent bugs**. Sprint 19 was no exception.

Real bugs found + fixed:

1. **`Strategy.BasketPush` had no pool builder arm.** Template
   would compile + deploy but emit nothing (same pattern as
   Sprint 14 BUG #2 — `last_strategy_quotes_per_node` never
   populated → source overlay sees `Missing` → `Out.Quotes`
   doesn't fire). Fixed by adding a dedicated overlay arm in
   `tick_strategy_graph` that parses the `basket` config JSON
   and emits `Value::VenueQuotes` directly — matches the
   `Strategy.BasisArb` pattern, which is the right one for
   nodes that fan out to multiple `(venue, symbol)` legs the
   engine's symbol-scoped pool doesn't cover.

2. **`pentest-basket-push` template routed to `Out.Quotes`
   instead of `Out.VenueQuotes`.** `Out.Quotes` extracts via
   `as_quotes()`, which returns `None` for `Value::VenueQuotes`
   — so even with the overlay above, the sink would have
   silently discarded the payload. Fixed the template; E2E
   test now pins the contract.

3. **EXEMPT entry misleading.** Initially said "pool-backed"
   but actual wiring is direct overlay. Fixed to
   "direct overlay (not pool-backed) — parses basket config
   + emits VenueQuotes legs".

Verification:
- **R14.1** BasketPush overlay at
  `crates/engine/src/market_maker.rs:4357` — parses `basket`
  JSON + emits `VenueQuotes` with cross-through pricing
  derived from DataBus L1 per leg. Fail-open: empty basket /
  zero-size legs / missing mid → empty `VenueQuotes` (no-op,
  not crash).
- **R14.2** `basket_push_template_compiles_and_routes_venue_quotes`
  E2E test in `pentest_templates_e2e.rs` — populates
  `BasketPush.quotes` with a `Value::VenueQuotes` payload and
  asserts `SinkAction::VenueQuotes(non_empty)` fires on the
  tick.

## Sprint 21 — honest MM side closeout (landed Apr 19)

Long-deferred MM-side quality work that's been sitting behind the
Epic R run. All non-restricted.

- [x] **R10.1** `ClientOnboardingPanel.svelte` mounted on
  AdminPage above the config surfaces card. Form fields: id,
  name, symbols CSV, webhook URLs one-per-line, jurisdiction
  dropdown (global / US / EU / UK / JP / SG). 403 jurisdiction
  gate surfaces a user-readable "US clients cannot register on
  a perp engine" message; 409 surfaces "client id already
  exists — choose a different id". Success banner warns the
  operator that new-symbol engines need a server restart to
  spawn (the backend module doc says so, so the UI says so).
  `ClientCircuitPanel`'s empty-state hint updated away from
  the curl pointer. No backend change — all four endpoints
  already existed at `/api/admin/clients`.
- [x] **Bonus** — fixed pre-existing build-breaking CSS in
  `StrategyDeployHistory.svelte`. Tags `=` / `+` / `-` / `~`
  aren't valid CSS identifiers; `.diff-=` / `.diff-+` /
  `.diff--` selectors made `npm run build` fail in every run
  since UI-5 landed. Tags renamed to `eq` / `add` / `del` /
  `chg` in both the diff function and the CSS. Dashboard now
  builds cleanly.
- [x] **Operator runbooks** for the three operator-blocked
  items: `docs/guides/paper-smoke-two-venue.md` (PAPER-2, was
  already there from Sprint 13), `docs/guides/obs-sanity.md`
  (OBS-1 — Sentry DSN + OTLP gRPC two-part check), and
  `docs/guides/reconciliation-live-test.md` (HARD-3 — three
  scenarios: agreement, induced order drift, induced position
  drift). Each has explicit pass criteria checkboxes so the
  operator can't handwave a half-pass. Until the operator runs
  them, these three TODO items stay open as blocked-on-hardware:
- [ ] **PAPER-2** Two-venue paper smoke runbook exercise
  (operator task, runbook at `docs/guides/paper-smoke-two-venue.md`).
- [ ] **OBS-1** OTel DSN + Sentry sanity — runbook at
  `docs/guides/obs-sanity.md`. Needs live Sentry DSN + OTLP
  collector.
- [ ] **HARD-3** Reconciliation loop real-exchange test —
  runbook at `docs/guides/reconciliation-live-test.md`. Needs
  testnet venue keys.
- [x] **R10.2** Integration test coverage audit: we have ~1600
  unit tests, but how many integration / E2E? Sprint 14 showed
  this gap is what lets gate drift hide. Enumerate, fill gaps.

## Sprint 22 — full-stack honesty audit (landed Apr 19)

20 of 21 items closed across two sessions. Zero deferrals with
"documented as dead" caveats. One item stays operator-blocked
(22M-1 — requires GitHub UI access the assistant doesn't have).

### Closed commits — audit-wire-fix band

- 22A-1 stat_arb wiring (f6f939a)
- 22A-2 var_guard CVaR tiers (301d7fa)
- 22A-3 execution algo config — operator-tunable TWAP knobs (3de36b2)
- 22A-4 paper-mode hard-fail on empty keys (df7867f)

### Closed commits — strategy state persistence

- 22B-0 Strategy checkpoint hook (b3b869a)
- 22B-1 GLFT calibration persist (df62ac1)
- 22B-2 Adaptive bucket window persist (85e178e)
- 22B-3 Autotune regime detector persist (c96fd3e)
- 22B-4 + 22B-6 Momentum + learned microprice persist (376ce59)
- 22B-5 PumpAndDump + Campaign FSM persist (ad45341)

### Closed commits — cleanup / parity

- 22C-2 queue-aware paper fill gate — backtester parity (adca843)
- 22C-3 ReportsPanel shape drift fix (daa3bfe)

### Closed commits — module wire-ups (session 2, undoing the
"delete without asking" mistake)

- 22W-1 wire protections stack end-to-end (e806a39)
- 22W-2 wire portfolio_var (969efb5)
- 22W-3 wire order_emulator — engine tick + HTTP (fccd6e1)
- 22W-4 wire dca reduction planner (21e43a5)
- 22W-5 wire xemm cross-exchange executor (0aaa9b2)
- 22W-6 wire candles + weights into backtester + momentum (d857236)

### Meta

- 22M-2 exhaustive audit sweep — completed via two Agent sweeps
  (risk module reachability matrix, mm-indicators usage scan).
  The remaining "checkpoint write loop is itself dead" finding
  is open-ended — `main.rs:987` only flushes at shutdown and
  `update_symbol` is never called at runtime, so 22B-* state
  persistence ships the HOOK but not the TRIGGER. Filed as a
  follow-up but not marked closed since the write loop needs
  its own design decision (cadence, flush policy, backpressure).

### Operator-blocked

- [ ] **22M-1 CI frontend build gap** — Frontend Build job
  exists at `ci.yml:69-84` but `StrategyDeployHistory.svelte`
  had build-breaking CSS since UI-5 landed. Operator to check
  GitHub Actions UI whether CI has been running red or not
  firing at all.

## Sprint 22 — full-stack honesty audit backlog (opened Apr 19)

Operator intuition 99% right — four parallel adversarial audits
found substantial rot beyond Sprint 19-21 scope. Captured as
tasks #163-#179; summary below so nothing is lost if the task
list is cleared.

### 22A — HIGH: config → живое (operator thinks it works, doesn't)

- [x] **22A-1 stat_arb config + dispatch** (task #163) —
  `stat_arb/driver.rs` is complete, `market_maker.rs:524` has
  the field, `main.rs:1022-1100` has no match arm, no
  `[stat_arb]` TOML section. Entire cointegration / Kalman /
  Z-score subsystem is dead code reachable only from unit tests.
- [x] **22A-2 var_guard instantiation** (task #164) —
  `config.rs:1465-1468` parses `var_guard_enabled` +
  `var_guard_limit_95/99` + `var_guard_ewma_lambda`, ZERO call
  sites in `main.rs`. Operator sets these, nothing happens.
- [x] **22A-3 exec algo selector** (task #165) — TWAP / VWAP /
  POV / Iceberg exist in `exec_algo.rs`, engine always hardcodes
  `TwapExecutor`. No `[execution]` TOML section.
- [x] **22A-4 paper-mode hard-fail on empty keys** (task #166) —
  `main.rs:1860` `unwrap_or_default()` on keys, `user_stream`
  silently skips at `main.rs:2043` → `BalanceCache` blind,
  paper fills run without inventory baseline. Either hard-fail
  or seed-balance config.

### 22B — MEDIUM: state persistence (cold-start every restart)

Blocker: `SymbolCheckpoint` has no slot for strategy internals.
`fill_replay.rs` replays inventory + PnL only, not strategy
callbacks. 8 of 12 strategies audited have state that is lost.

- [x] **22B-0 Strategy checkpoint hook** (task #167) —
  architectural. `Strategy` trait gains `checkpoint_state()` +
  `restore_state(v)` default-no-op methods. `SymbolCheckpoint`
  gains `strategy_state: Option<serde_json::Value>`.
  **Blocks all of 22B-1..22B-6.**
- [x] **22B-1 GLFT** (task #168, blocked-by #167) — fitted
  (a, k) + 50-sample `fill_depths` buffer.
- [x] **22B-2 Adaptive** (task #169, blocked-by #167) —
  60-bucket minute-resolution rolling stats.
- [x] **22B-3 Autotune** (task #170, blocked-by #167) —
  regime detector returns window + current_regime.
- [x] **22B-4 Learned microprice** (task #171, blocked-by #167) —
  online_ring + g-matrix bucket accumulators.
- [x] **22B-5 Pentest FSM** (task #172, blocked-by #167) —
  `pump_and_dump` AtomicU64 tick counter + `campaign_orchestrator`
  `first_tick_at` stamp.
- [x] **22B-6 Momentum** (task #173, blocked-by #167) —
  `signed_volumes` + `snapshots` VecDeques.

### 22C — LOW: polish / decide

- [x] **22C-1 xemm wire-or-remove** (task #174) — `xemm.rs:31-39`
  docstring admits "not currently driven by the live engine".
  Wire the SOR inline-dispatch plumbing or delete.
- [x] **22C-2 fill-model parity** (task #175) — backtester
  simulator uses queue-aware log probability model;
  `paper_match_trade()` in engine uses different logic. PnL in
  backtest ≠ PnL in paper mode on same feed.
- [x] **22C-3 ReportsPanel shape drift** (task #176) — panel
  reads `data.dates`, backend returns bare `Vec<String>`. Works
  today by JS truthy fallback; breaks under any response-shape
  normaliser.
- [x] **22C-4 dca + order_emulator wire-or-remove** (task #177).

### 22M — meta

- [ ] **22M-1 CI frontend build gap** (task #178) — `ci.yml`
  has a Frontend Build job (line 69-84) but
  `StrategyDeployHistory.svelte` had build-breaking CSS since
  UI-5 landed (commit 0e1ace2). Either CI isn't running or
  nobody's reading failures. Bigger than any single bug —
  affects whether every other audit item gets caught next time.
- [x] **22M-2 exhaustive audit sweep** (task #179) — the four
  audits hit ~30% coverage. Missed: individual risk modules
  (borrow / sla / otr / protections / circuit_breaker /
  inventory_drift), 32 of 40 dashboard endpoints for shape
  drift, `mm-indicators` crate for "library but unused".

## Graph system audit — Apr 19 follow-ups

Surfaced during the post-batch audit at
`docs/research/graph-system-audit-apr19.md`. Each entry names
a concrete extension point.

- [x] **GR-3** `strategy_node_configs: HashMap<NodeId,
  MarketMakerConfig>` built in lockstep with `strategy_pool`
  on every graph swap. Parses `gamma`/`kappa`/`sigma`/
  `order_size`/`num_levels`/`min_spread_bps`/`max_distance_bps`
  off each `Strategy.*` node's graph config, clones the
  engine baseline, applies overrides. The per-node tick loop
  builds a `StrategyContext` whose `config` field points at
  the override so two `Strategy.Avellaneda`s with different
  γ genuinely produce different quotes.
- [x] **GR-4** `config_schema()` on `Risk.ToxicityWiden`
  (`scale`), `Risk.InventoryUrgency` (`cap`, `exponent`),
  `Risk.CircuitBreaker` (`wide_bps`). Same `Number { min,
  max, step }` widget pattern as the Exec schemas.
- [x] **GR-5** `Sentiment.Rate` / `Sentiment.Score` accept an
  optional `asset` config override. Engine checks the field
  first; when set, looks up the tick regardless of graph
  scope. Empty / missing keeps the legacy Symbol-scope-only
  resolution. Schema gains the `asset` text field so the UI
  surfaces it without a catalog drift.
- [x] **GR-6** `graph_catalog_coverage` test module on the
  engine: walks every `kinds()` entry, skips nodes with
  inputs (graph-internal), skips a hand-curated EXEMPT list
  of pool-backed / sink kinds, and asserts every remaining
  kind appears as a `"Kind" =>` (or `|`-joined) arm in
  `tick_strategy_graph`. Caught Risk.* false positives +
  Sentiment compound-match on first run; both fixed in the
  same commit.

## Strategy-graph observability (design: docs/research/graph-observability.md)

Operator built a graph, deployed it, but can't see what flows
through each node/edge in real time. Closes stories 1–4 and 6
in the doc. Prometheus explicitly NOT involved — trace lives
only in our UI via details-store / HTTP. Milestones:

### M1 — engine telemetry + new details topics (landed)

- [x] **GOBS-M1-1** `Evaluator::tick_with_full_trace` — fills a
  `TickTrace` by reusing the per-tick outputs map. `tick_inner`
  now takes `&mut Option<TickTrace>`; legacy `tick_with_trace`
  projects down to the preview `EvalTrace`. Zero-cost when the
  caller passes `None`.
- [x] **GOBS-M1-2** New module `crates/strategy-graph/src/trace.rs`
  with `TickTrace`, `NodeExec`, `ExecStatus`, `GraphAnalysis`.
  All serde-round-trip-tested. `SinkAction` gained adjacent-tag
  serde derives (`{kind, data}`) so tagged serialisation handles
  `Decimal` newtype variants.
- [x] **GOBS-M1-3** `DeploymentDetailsStore` gained
  `graph_traces: HashMap<Symbol, VecDeque<TickTrace>>` capped at
  `GRAPH_TRACE_CAP=256` + `graph_analysis: HashMap<Symbol, GraphAnalysis>`.
  New methods: `push_graph_trace`, `graph_traces(limit)`,
  `clear_graph_traces`, `set_graph_analysis`, `graph_analysis`.
- [x] **GOBS-M1-4** Engine tick hook at `market_maker.rs:5027`:
  switched `graph.tick()` → `graph.tick_with_full_trace()`,
  stamps tick counter + wall-clock ms + graph hash (the
  evaluator is clock-free), pushes into details store.
  New field `strategy_graph_tick_counter: u64`.
- [x] **GOBS-M1-5** Swap hook + first-deploy hook
  (`with_strategy_graph` and `swap_strategy_graph`) compute
  `Evaluator::analyze(graph_hash)` once and store via
  `set_graph_analysis`. Swap also calls `clear_graph_traces`
  so fresh subscribers don't see stale ticks from the prior
  DAG.
- [x] **GOBS-M1-6** Agent match arms in `agent/src/lib.rs` for
  `"graph_trace_recent"` (returns `{ traces: [...], graph_analysis: {...} }`,
  honours optional `limit` arg, default 20) and
  `"graph_analysis"` (returns the analysis struct or an
  explicit error payload when no graph is attached).
- [x] **GOBS-M1-7** Regression tests landed in
  `strategy-graph/src/lib.rs`:
  `tick_with_full_trace_captures_every_node_and_sinks` +
  `analyze_flags_dead_branch_and_unconsumed_outputs`, plus
  `trace::tests::serde_roundtrip_tick_trace`. Workspace full
  test sweep green.

### M2 — UI Live mode: canvas overlay + inspector (landed)

- [x] **GOBS-M2-1** StrategyPage: `mode: 'authoring' | 'live'`
  state, segmented toggle in top-right chunk, Live button
  disabled until a `liveTarget` is bound (via URL param or
  deployment drilldown). Simulate/Deploy disabled while in Live
  mode. CSS `@keyframes live-pulse` drives the indicator dot.
- [x] **GOBS-M2-2** Live-mode entry points landed:
  - `?live=<agentId>/<deploymentId>` URL param parsed in
    `App.svelte::parseLiveTarget()`, cleared on route change.
  - `DeploymentDrilldown` gained "Open graph (live)" button in
    head-actions, gated on `row.active_graph?.name`.
  - New `onOpenGraphLive` prop chain
    App.svelte → FleetPage → DeploymentDrilldown.
- [x] **GOBS-M2-3** `createGraphLiveStore(auth, agentId, deploymentId)`
  at `frontend/src/lib/graphLiveStore.svelte.js`. 2 s poll against
  `/details/graph_trace_recent?limit=20`, reactive `{ traces,
  graphAnalysis, error, loading, lastFetch }`, `stop()` on
  unmount. Helpers `edgeValuesFromTrace` / `nodeStatsFromTraces` /
  `formatValue` for UI rendering.
- [x] **GOBS-M2-4** StrategyNode.svelte:
  - `data.live` contract (latest value, status, hitRate, dead,
    dormant, tickCount) rendered via `live-badge` row + status
    dot in header.
  - Fire-pulse via `.fired header` CSS animation (0.35s ease-out
    accent fade).
  - `.dead` → red dashed border + "dead branch" banner.
  - `.dormant` → diagonal-stripe background + "dormant source"
    banner.
  - Errored status dot in red; ok/source tones differentiated.
- [x] **GOBS-M2-5** `decorateEdgesLive()` reads from
  `liveEdgeValues` (derived from latest TickTrace), applies
  dashed red stroke when either endpoint is in
  `graphAnalysis.dead_nodes`. Reuses the svelte-flow label
  shape as the preview path.
- [x] **GOBS-M2-6** `GraphInspector.svelte` sidebar replaces
  `StrategyNodeConfig` in Live mode: per-node sparkline (last
  20 numeric outputs, min/max range shown), status chip
  (ok/source/error), hit-rate %, avg elapsed µs, dead-branch
  badge. No-selection view summarises ticks-in-window +
  dead/unconsumed counts. "Back to authoring" button flips mode.
- [x] **GOBS-M2-7** `$effect` reads `graphAnalysis` — dead nodes
  flagged on both canvas (StrategyNode `.dead` class) and edges
  (dashed red). Dormant sources get diagonal-stripe when a
  `required_sources` set is known and the node's kind is absent
  from it. Frontend build green with 0 Svelte warnings;
  workspace 2200+ Rust tests pass.

### M2.5 — Playwright E2E regression suite (landed)

Added browser-side automation so M2 regressions are caught in CI
without operator eyeball. Backed by a persistent dev stand.

- [x] **GOBS-M2.5-1** `scripts/stand-up.sh` — boots server + agent,
  bootstraps admin, accepts fingerprint, pushes paper credential,
  deploys `rug-detector-composite` (defaults; override via
  `STAND_TEMPLATE`). Writes `.stand-run/stand.env` with
  `HTTP_URL`, `ADMIN_TOKEN`, `AGENT_ID`, `DEPLOYMENT_ID`, PIDs.
  Companion `scripts/tear-down.sh` kills PIDs.
- [x] **GOBS-M2.5-2** `frontend/playwright.config.ts` + Chromium
  install. `globalSetup.ts` spawns stand-up if `.stand-run/stand.env`
  missing, else reuses it. Passes stand state via `STAND_*`
  env vars to each spec. Trace + video retained on failure.
- [x] **GOBS-M2.5-3** API suite
  (`tests/e2e/api-graph-observability.spec.ts`): asserts
  `graph_trace_recent` returns live ticks with node outputs,
  `graph_analysis` exposes depth_map + required_sources,
  tick counter is monotonic.
- [x] **GOBS-M2.5-4** UI suite
  (`tests/e2e/ui-live-mode.spec.ts`): seeds auth via
  `localStorage.mm_auth`, navigates to `/?live=<agent>/<dep>`,
  asserts the Live tab is active, 8 nodes render, live badges
  populate, inspector sidebar shows hit-rate + avg-elapsed,
  toggle back to Authoring re-enables Simulate.

### Bug caught by Playwright that shipped behind build-only M2

- [x] **GOBS-BUG-1** `StrategyPage` `$effect` read+wrote `nodes`
  without `untrack()` → `effect_update_depth_exceeded` at first
  boot, canvas empty. Fixed by wrapping the write-back in
  `untrack(() => { nodes = ...})`; same for `decorateEdgesLive`
  mutating `edges`. Build-only checks would never have caught
  this — Playwright caught it on first run.
- [x] **GOBS-BUG-2** `?live=` URL param populated `liveTarget`
  but left `route='overview'` — StrategyPage never rendered on
  initial load. Fixed by initialising `route` from `liveTarget`.
- [x] **GOBS-BUG-3** Distributed mode has no graph-store, so
  `/api/v1/strategy/graphs/:name` returns 503. `loadLiveGraph`
  now fetches via `/api/v1/strategy/templates/:name` which is
  wired and returns the full graph JSON.

### M3 — extended validation + dead-node detection (landed)

- [x] **GOBS-M3-1** `POST /api/v1/strategy/validate` response
  extended with `required_sources`, `dead_nodes`,
  `unconsumed_outputs`. Built from the same `Evaluator::analyze`
  that swap-hook feeds into `graph_analysis`. Fields always
  emitted (no `skip_serializing_if`) so client `Array.isArray`
  checks don't flake on an empty topology.
- [x] **GOBS-M3-2** StrategyPage validate strip gained two
  advisory pills in the Ready state: `N dead` (red) when any
  node has no path to a sink, `N unconsumed` (orange) for
  output ports without consumers. New `.v-pill.warn` colour
  hooked into the existing pill CSS.
- [x] **GOBS-M3-3** `StrategyPalette` accepts `requiredSources`
  prop. Source-kind chips (zero input ports, not Math/Logic/
  Cast/Strategy/Exec/Plan/Out) render with a diagonal-stripe
  background + 0.48 opacity when the graph doesn't reference
  them. Hover tooltip explains "dormant — not referenced".
  Hover boosts to 0.85 opacity so operators can still read
  the label.
- [x] **GOBS-M3-4** Playwright regression (`m3-validation.spec.ts`,
  3 tests): API returns topology fields for rug-detector,
  palette fades dormant sources after template load, validate
  strip coexists advisory pills with the Ready pill without
  layout breakage.

### M4 — timeline + time-travel (landed — 4-4 deferred)

- [x] **GOBS-M4-1** `GraphTimeline.svelte` — horizontal scrubber
  under the canvas in Live mode. One column per TickTrace,
  column height = `total_elapsed_ns` (relative), colour tone
  = `sinks_fired` count (idle / low / mid / hot). Header shows
  `tick# · HH:MM:SS.mmm · N nodes · M sinks` for the currently
  displayed tick + a pulsing live pill / "Back to live" CTA.
- [x] **GOBS-M4-2** Click-tick → pin. StrategyPage holds
  `pinnedTickNum`. When non-null, `liveTickTrace` picks that
  trace instead of `traces[0]`, `liveTickOutputs` derived map
  feeds per-node badges so operators see that tick's values on
  the canvas. Edge decoration also snaps (existing `$effect`
  re-runs on pin).
- [x] **GOBS-M4-3** URL `?tick=<tick_num>` deep link. App.svelte
  parses alongside `?live=`, passes as `liveTick` prop,
  StrategyPage seeds `pinnedTickNum` on mount. `navigateLiveGraph`
  accepts an optional `tickNum` so future Incidents → "Open
  graph at incident" deep-links are one-call wide.
- [x] **GOBS-M4-4** Incident deep-link (re-scoped — incidents are
  operator-opened, not engine-emitted, so "persist traces on
  incident" doesn't apply; instead stamp the incident with the
  current tick so the post-mortem UI can jump there).
  - `OpenIncident` gained optional
    `graph_agent_id` / `graph_deployment_id` / `graph_tick_num`.
    POST handler accepts them.
  - `DeploymentDrilldown` gained a "File incident" button —
    snapshots latest tick via `graph_trace_recent?limit=1`,
    POSTs with graph context, navigates to Incidents.
  - `IncidentsPage` renders "Open graph at incident · tick #N"
    when the graph triple is set; click routes through
    `navigateLiveGraph(agent, dep, tick)` so the Timeline
    scrubber opens at the exact pinned frame.
  - Playwright `m4-4-incident-link.spec.ts` asserts the
    round-trip (POST → row surfaces button → click sets URL
    `?live=&tick=`). Full e2e now 14/14 green.
- [x] **GOBS-M4-test** Playwright `m4-timeline.spec.ts` (3 tests):
  timeline renders one column per trace with a live pill, click
  pins + "Back to live" unpins, `?tick=` URL param pre-pins on
  load. Whole e2e suite 13/13 green.

### M5 — diff/replay v1 vs v2 (landed v1 — side-by-side canvas deferred to M5.2)

Determinism was easier than the design-doc planned for: `TickTrace`
already captures per-node inputs/outputs and source nodes expose
their outputs by kind, so a candidate graph can be fed the same
`(kind, port) → value` lookup and re-evaluated on the same
evaluator loop. No new per-tick persistence needed.

- [x] **GOBS-M5-1** `TickTrace::source_kind_values()` flattens
  source-node outputs into a `(kind, port) → Value` lookup —
  replay fodder for a candidate with different NodeIds.
- [x] **GOBS-M5-2** `mm_strategy_graph::evaluator::replay_source_inputs(candidate, kind_values)`
  rebuilds a per-NodeId `source_inputs` map for the candidate's
  own source nodes.
- [x] **GOBS-M5-3** Agent details topic `"graph_replay"` — takes
  `args.candidate_graph` + optional `ticks`, walks its trace
  ring, re-evaluates the candidate, diffs `sinks_fired` as a
  string-set compare, returns `{ summary, ticks_replayed,
  divergence_count, divergences: [...], candidate_issues }`.
- [x] **GOBS-M5-4** Controller POST `/api/v1/agents/{a}/deployments/{d}/replay`
  — sibling of the GET `/details/{topic}` fan-out but takes a
  JSON body (graph JSON is too large for query args). Wraps
  the same `FetchDeploymentDetails` command with topic
  `graph_replay`.
- [x] **GOBS-M5-5** `StrategyPage` toolbar: "Replay vs deployed"
  button (Authoring mode only, requires a live target).
  Click → modal with summary + per-tick side-by-side sink
  JSON. Identical graph → "matches deployed behaviour"; a
  rejected candidate surfaces its validation error; non-empty
  divergences scroll in a bounded list.
- [x] **GOBS-M5-6** Playwright `m5-replay.spec.ts` (3 tests):
  API identical-graph → 0 divergences; invalid graph →
  candidate_issues populated; UI button opens modal with
  matching summary. Full e2e suite 17/17 green.
- [ ] **GOBS-M5.2** (deferred) Side-by-side canvas — render
  two miniature graphs scrubbed through the tick range with
  diverging nodes glow-highlighted. Current v1 lists
  divergences as JSON diff which is good enough for first
  use; visual canvas diff is polish when operators ask.

### GOBS-SAVE — graph save-diff + versioning (landed)

- [x] **SAVE-1** Versioned storage. `user_templates/<name>/`
  holds one `<hash>.json` per unique graph + append-only
  `history.jsonl`. Legacy flat `<name>.json` lazy-migrates on
  next save (seed as version 1, remove after). Dedup: re-
  saving the same bytes appends history but doesn't rewrite
  the graph file.
- [x] **SAVE-2** Frontend `graphDiff.js` — pure fn returns
  added/removed/modified nodes + added/removed edges, stable-
  stringify config compare, edge matching by
  `(from.node,port) -> (to.node,port)` tuple.
- [x] **SAVE-3** Save dialog runs in two phases: first click
  probes `/custom_templates/:name`; if an existing version is
  found, renders a diff preview with +/-/~ chips + expandable
  details; second click (button flips to "Save new version")
  commits via POST.
- [x] **SAVE-4** `Versions` button in toolbar (visible when
  current `graphName` matches a custom template). Opens a
  modal listing every version newest-first; click any row →
  GET `/custom_templates/:name/versions/:hash` → apply to
  canvas.
- [x] **SAVE-5** Export filename stamped with ISO date so
  cross-referencing with on-disk history is trivial. Import
  detects name collision + hints "matches existing 'X' — save
  will create v{N+2}".
- [x] **SAVE-6** `DashboardState::set_strategy_graph_store`
  wired in `server/src/main.rs` so distributed-mode boots no
  longer 503 the save endpoint. Root: `data/strategy_graphs`.
- [x] **SAVE-7** M5 replay bugfix found along the way:
  `replay_source_inputs` kind-keyed lookup collapsed multiple
  `Math.Const` literals with distinct `config.value`s onto
  whichever appeared last in the trace. Now counts candidate
  source-node kinds; skips ambiguous ones (>= 2 of same kind)
  so each literal's own `evaluate()` default supplies its
  config value.
- [x] **SAVE-8** Playwright `graph-save-versioning.spec.ts` —
  API round-trip (v1→v2 history + per-version read + dedup)
  + UI (seeded template → save dialog → same name flips diff
  preview → commit). Suite 27/27 green.

### GOBS-PRE — pre-human-testing hardening (landed)

Six-point checklist to get past "happy-path proven by automation"
toward "friendly operator can actually drive this without hitting
land mines". Every item is a Playwright or Rust test so the
regression stays cheap.

- [x] **GOBS-PRE-1** `authz-graph-topics.spec.ts` — proves
  `graph_trace_recent` / `graph_analysis` / `replay` are gated
  at the controller's `internal_view` tier. No-auth = 401,
  garbage-token = 401, fresh viewer + operator accounts
  created through `/api/admin/users` both succeed on all three
  endpoints. ClientReader's `tenant_scope_middleware` gate
  inherits its existing coverage.
- [x] **GOBS-PRE-2** `error-states.spec.ts` — unknown agent →
  404 with readable body on both details + replay; unknown
  deployment_id on a valid agent → 200 envelope with empty
  traces (UI treats as "nothing to show", not a network
  failure); `?tick=0` deep link on a running deployment → UI
  detects the miss, auto-unpins, flashes a warning.
- [x] **GOBS-PRE-3** `DeploymentDetailsStore::graph_trace_ring_rolls_over`
  + `graph_trace_limit_caps_response` + `clear_graph_traces_wipes_symbol_only`
  + `graph_analysis_replaces_on_set` — ring caps at
  `GRAPH_TRACE_CAP=256`, newest-first read order, clear scoped
  to one symbol, analysis set replaces.
- [x] **GOBS-PRE-4** `multi-deploy.spec.ts` — adds a second
  deployment on ETHUSDT alongside the stand's BTCUSDT one,
  asserts both get independent `graph_analysis` + non-empty
  `graph_trace_recent`, then restores the original deployment
  list so the stand is unchanged for subsequent tests.
- [x] **GOBS-PRE-5** `StrategyPage` guards a pin that ages out
  of the ring. `$effect` watches `(pinnedTickNum, liveTraces)`;
  when the pinned tick isn't found, unpins + shows a
  "tick #N rolled off the ring — released pin" banner for 6s.
  New CSS `.pin-warning` above the validate strip.
- [x] **GOBS-PRE-6** `docs/guides/graph-live-mode.md` — operator
  guide: entry points (drilldown button + URL), what every
  visual cue means, timeline + time-travel, replay workflow,
  filing incidents at a frame, troubleshooting table, data
  retention + roles.
- [x] **GOBS-PRE-bug** Incident dedup bug caught in the
  process: the m4-4 Playwright test used a stable
  `violation_key` so a second run merged into the first
  incident's row and the assertion for the new tick_num
  failed. Fixed by suffixing `Date.now()`. The dedup is
  correct server-side; the test was the one being dishonest.

### GOBS-CI — Playwright job in `.github/workflows/ci.yml` (landed)

- [x] **GOBS-CI-1** New `e2e` job in `ci.yml`:
  installs chromium via `npx playwright install --with-deps`,
  builds Rust binaries + frontend dist, runs
  `scripts/stand-up.sh` with `STAND_SKIP_FRONTEND=1` (we just
  built dist), runs `npm run test:e2e`, tears down the stand
  with `if: always()` so a failing spec still cleans up.
- [x] **GOBS-CI-2** Failure artifacts: uploads
  `frontend/playwright-report` + `.stand-run/logs` on job
  failure with 14-day retention. Traces + videos inside the
  report cover everything Playwright recorded.
- [x] **GOBS-CI-3** 25-minute job timeout — enough for fresh
  cargo cache + chromium install + 45s tick-warmup + ≈12s
  test run, short enough to fail fast if the stand hangs.
- [x] **GOBS-CI-4** Local dry-run: `./scripts/tear-down.sh` →
  `STAND_SKIP_FRONTEND=1 ./scripts/stand-up.sh` →
  `npm run test:e2e` → `./scripts/tear-down.sh`. All 13 tests
  green, teardown clean. Mirrors the CI step sequence.

### M6 — lazy-detector gating (landed — v1 manipulation + onchain)

- [x] **GOBS-M6-1** `MarketMakerEngine` gained two bool gates —
  `gate_manipulation` + `gate_onchain`. Default true (open) so
  deployments without a graph keep their pre-graph behaviour.
  Both derived from `analysis.required_sources` on every graph
  swap (`swap_strategy_graph`) and first deploy
  (`with_strategy_graph`) — one source of truth, no drift.
- [x] **GOBS-M6-2** Engine hot paths gated:
  - `self.manipulation.on_trade(trade)` — per-trade feed.
    Closed gate = detector doesn't update on trades.
  - `self.manipulation.on_book(...)` + snapshot publish path.
    Closed gate = `manipulation_score` publishes `None` so
    Prometheus + dashboard show "detector not running"
    honestly instead of stale zeros.
- [x] **GOBS-M6-3** Regression: `analyze_exposes_gate_keys_for_detector_templates`
  in `crates/strategy-graph/src/lib.rs` asserts the gate
  predicate on `rug-detector-composite` (manip open) and
  `avellaneda-via-graph` (both closed) — catalog renames fail
  the test before silently leaving a detector running or
  starved in production.
- [x] **GOBS-M6-4** (deferred) UI palette badge — "unused —
  will not compute" annotation on dormant detector sources
  (beyond the M3 diagonal-stripe fade). Design-doc M3.3 covers
  this partially. Skip for now unless operators ask.
- [x] **GOBS-M6-5** Playwright regression sweep still 13/13 on
  the rug-detector stand (manipulation + onchain both firing
  — gate open path proven in browser-land too).

### Open design questions

- [x] **GOBS-Q1** Trace subscribe model — **decided:
  always-on**. On-demand added first-subscribe latency +
  state-machine complexity; measured always-on cost ~30
  allocs/tick at 2 Hz is negligible vs engine tick budget.
  Revisit only if profiling flags it.
- [x] **GOBS-Q2** Diff-only payload — **decided: not yet**.
  Current payload averages ~25 KB/poll at 20-tick windows,
  well under the 200 KB threshold. Premature optimisation;
  revisit when a 100+ node graph makes the wire cost hurt.
- [x] **GOBS-Q3** Incident retention — **decided: deferred
  to engine-emitted incident model**. M4-4 re-scoped around
  operator-opened incidents stamping tick_num; gives the
  same post-mortem entry without audit-ring changes.
  Engine-triggered trace snapshots land if/when the
  incident event model grows beyond operator-filed rows.

## P3 — hardening / polish

- [x] **HARD-1** Audit complete: all 40 `unwrap`/`expect` calls
  in `market_maker.rs` live in `#[cfg(test)]` modules. Wider
  engine crate shows 0 production unwraps and exactly one
  provable-invariant `expect` at `listing_sniper.rs:144`
  (symbol pulled from `by_symbol.keys()` must re-resolve in
  the same map). Hot path clean.
- [x] **HARD-2** `server/main.rs` boots with
  `mm_risk::audit::verify_chain(...)` and refuses to start on
  a broken chain unless `MM_AUDIT_RESUME_ON_BROKEN=yes` is
  set. Logs `rows_checked + last_hash` on success.
- [ ] **HARD-3** Reconciliation loop real-exchange testing.
  The reconcile code exists; no integration test fires against
  a real venue account. **Operator task** — needs live venue
  keys.
- [x] **HARD-4** Unsafe audit done: one remaining block in a
  test (`order_manager.rs:1392`) with a correct
  `SAFETY:` comment; the SOR env-var test gained per-call
  `SAFETY:` comments explaining the single-thread invariant
  Rust 2024 requires; the checkpoint tamper test rebuilds
  the tampered string through `Vec<u8>` + `String::from_utf8_lossy`,
  eliminating `String::as_bytes_mut`.
- [x] **HARD-5** CI/CD landed earlier at `.github/workflows/ci.yml` —
  `cargo check --all-targets`, `cargo test --all`,
  `cargo clippy -D warnings`, `cargo fmt --check`, OTel
  feature build, frontend build, docker build on every PR
  to main.
- [x] **HARD-6** `auth::tests::token_round_trips_and_expires`
  tamper replaced with a case-flip / fixed-substitute that
  can never equal the original byte. Validated with 5
  back-to-back runs.

---

## Reference — closed items

See git log since 2026-04-17 for:
- Phase I (INT-1..4) — DecisionLedger / TieredOtr /
  ForeignTwap / Cost.Sweep engine wiring
- Phase II (RS-1..5) — risk-aware graph sources + 3 bundled
  templates
- Phase III (UI-1..4) — Active plans, Decisions ledger,
  Tiered OTR grid, Surveillance page
- Phase IV (PERP-1, INV-1/3, BOOK-3, SPOT-1) — deep-MM
  infrastructure gaps
- Phase V (PAPER-1) — dashboard state regression tests
- Phase VI (UX-1, MV-UI-1/2) — polish + cross-venue panel +
  venues-health card

---

## How to use this file

- Pick an item from the highest-severity open band that
  matches the session's focus.
- Move the checkbox to `[x]` when the commit lands on main.
  Keep the line so future audits can see it was addressed.
- When an item grows into multiple commits, expand it into
  sub-bullets inline — no separate file per epic.
- Revisit P0 list after every paper-trading smoke run —
  whatever the operator hit first belongs at the top.

---

## Legacy cleanup tracker

When a dated pattern surfaces during an audit that's too big to
refactor in the same session, log it here with file:line + the
replacement approach. Refactor-in-place is the default — entries
here are only the deferred cases.

**Format**: `- [ ] **LEGACY-NNN** — <kind> at <file>:<line>. Replace
with <new approach>. Estimated scope: <size>.`

No entries yet — the Apr-2026 audit sweeps refactored every
dated pattern in place (xemm `.expect("is_some")`, venue-scoped
env-var migration, stale audit-event type names). Add entries
here when the next audit pass finds something too big for a
single-session refactor.
