# UI / UX audit — 2026-04-22

Systematic walk of the operator-facing UI through the strategy
lifecycle. Twenty-one pages, sixty-two components. Audit focus:
can an operator author, deploy, monitor, tune, and retire a
strategy without dropping to curl? What's a first-class UI
action vs. a half-finished stub?

## Pages inventory

| Path | Primary purpose | Operator uses it for |
|------|-----------------|---------------------|
| Overview | Landing dashboard | Portfolio glance, recent alerts, quick symbol view |
| Fleet | Agent fleet + admission | Accept / revoke / profile-edit; **deploy** strategy (DeployDialog); per-deployment drilldown |
| Strategy | Visual graph builder (svelte-flow) | Author custom strategy graphs, save to catalog, deploy / rollback |
| Clients | Tenant management | Admin-create clients, view loss circuit, onboarding invites |
| Compliance | Audit + reports + alerts | MiCA monthly export, signed audit range, violation rollup |
| Surveillance | Manipulation detector scores fleet-aggregated | Watch own-fleet surveillance |
| Incidents | Open / acked / resolved lifecycle | Ack + post-mortem |
| Reconciliation | Order + balance drift per deployment | Spot ghost / phantom orders, balance mismatches |
| Orderbook | Live L2 view + imbalance | Sanity-check feed |
| History | Inventory + fills + decisions ledger | Historical investigation |
| Calibration | GLFT calibration state | (was empty pre-apr22 fix — now fan-out) |
| Settings | Aggregated engine knobs | Central read-only view (pre-distributed legacy-shaped) |
| Platform | Controller tunables | Lease TTL, version pinning, deploy defaults |
| Vault | Unified secret store | Credential CRUD (admin-only writes) |
| Users | Auth user management | Create / list / reset-password |
| Auth audit | Login / reset event stream | Security review |
| Admin | Kill switches + legacy config panels | Mixed — some legacy, some live |
| Profile | Self-service (password, 2FA) | User account |
| ClientPortal | Tenant-scoped self-view | ClientReader sees PnL / SLA / fills / webhooks |
| ClientSignup | Invite consumption | First-time tenant setup |
| PasswordReset | Admin-issued reset | Password recovery flow |
| LoginAudit | Login event browser | `/api/admin/auth/audit` reader |

## Findings

| # | Severity | Finding | Fix |
|---|----------|---------|-----|
| **UI-DEPLOY-1** | 🔥 P0 | `DeployDialog` POSTed a single-element `strategies` array. Controller's `SetDesiredStrategies` is replace-by-set, so deploying a second strategy silently stopped every sibling deployment on that agent. Operator's mental model ("I'm adding one") didn't match the wire semantics. | Dialog now fetches the agent's current `/deployments`, preserves every row except the one matching the new `deployment_id`, then POSTs the union. Echoing `credentials` on `DeploymentStateRow` is the enabler — without it the merge couldn't reconstruct the allow-list. Live-verified: added ETH GLFT after BTC Avellaneda — both keep running, no more silent stop. |
| **UI-STOP-1** | 🟠 P1 | There was **no UI action** to retire a single deployment. DeployDialog only adds; drilldown's kill ladder stops quoting but leaves the row in the desired set so reconcile re-spawns it. Genuine retire needed the same union-fetch pattern, minus the new row. | Added "Retire" button per deployment row on FleetPage. Same fetch-filter-POST flow as the deploy dialog. Confirmation prompt explains that sibling deployments stay running. |
| **UI-TEMPLATE-1** | 🟡 P2 (doc) | Catalog lists 13 templates; `avellaneda-via-graph` and `glft-via-graph` are the only ones with a real strategy type mapping. Others (cost-gated-quoter, meme-spot-guarded, cross-asset-regime, liquidity-burn-guard, rug-detector-composite, cross-exchange-basic, xemm-reactive, basis-carry-spot-perp, funding-aware-quoter, stat_arb) all get their distinguishing behaviour ONLY via the bundled graph JSON loaded at engine start (R1-TEMPLATE-3 fix). Operators picking "rug-detector-composite" do NOT get a plain-strategy rug detector — they get AvellanedaStoikov with a rug-gate graph attached. | Worth a catalog footnote + UI tooltip linking the template card to the JSON it loads. Deferred. |

## Strategy lifecycle coverage matrix

| Phase | Surface | Coverage |
|-------|---------|----------|
| Author (custom) | StrategyPage (svelte-flow visual builder) | ✅ drag/drop palette, live validate, preview tick, save to catalog, deploy history, rollback by hash |
| Author (template pick) | Catalog → DeployDialog | ✅ with a caveat — only 4 template types have distinct plain-strategy mappings; the rest rely on graph JSON |
| Test | Backtester (not in UI), Preview tick in StrategyPage | 🟡 preview = one-tick sim only; no "run 100 ticks with synthetic data" sandbox |
| Deploy (single agent) | Fleet → Deploy button → DeployDialog | ✅ (post UI-DEPLOY-1 fix — union merge) |
| Deploy (batch) | Fleet → checkbox multi-select → DeployDialog | ✅ per-agent merge still works via `mergedBodyFor(agentId)` |
| Monitor overview | Overview page + Fleet table | ✅ |
| Monitor per-deployment | Drilldown modal (kill ladder, features, variables, execution scalars) | ✅ |
| Tune variables live | Drilldown → ParamTuner (PATCH /variables) | ✅ |
| Kill switch L1-L5 | Drilldown → ops buttons (widen / stop / cancel-all / flatten / disconnect / reset) | ✅ |
| Pause / Resume | Drilldown aux-ops row | ✅ |
| Retire deployment | Fleet row Retire button (post UI-STOP-1 fix) | ✅ |
| Review audit | Compliance page + AuthAudit admin page | ✅ |

## Verdict

Operator can now do the full lifecycle without curl. The two
critical holes (UI-DEPLOY-1 silent-stop + UI-STOP-1 no-retire)
both closed. Remaining gaps are mostly documentation (template
disambiguation) and test tooling (no multi-tick preview
sandbox) — not blocking for production UX.

## Per-page walk

Systematic per-page read after the lifecycle fixes. Endpoint
list captured from `/tmp/mm-journey/ui_probe.py` against a
live controller+agent. Every page hits 200 OK for ADMIN and CR
roles respectively.

### Live group

| Page | Endpoints | Renders | Empty/loading | Notes |
|------|-----------|---------|---------------|-------|
| Overview | indirect via sub-components (HeroKpis, VenueMarketStrip, CrossVenuePortfolio, BasisMonitor, FundingPanel, SignalsPanel, InventoryPanel, PerLeg*, PnlChart, SpreadChart, OrderBook) + WS state | 3-column dashboard + "Market quality" KV card | placeholders "—" for null; FirstInstallWizard for new admins | Pure layout; if WS stale everything reads empty — no page-level skeleton |
| Fleet | `/fleet`, `/approvals`, `/agents/{id}/credentials`, `/agents/{id}/deployments`, `/approvals/{fp}/{accept\|reject\|revoke}`, `/ops/fleet/{pause\|resume}`, PUT `/agents/{fp}/profile` | pending approvals, agent cards with profile+deployments, drilldown modal, revoke modal, pre-approve modal, Retire button per row | EmptyStateGuide when zero agents; per-row busy flags | Retire (UI-STOP-1) and merge-deploy (UI-DEPLOY-1) both landed; profile PUT endpoint verified correct (backend uses `{fingerprint}`) |
| Clients | `/admin/clients`, `/client/{id}/pnl`, `/client/{id}/sla`, `/client/{id}/sla/certificate`, `/admin/clients/{id}/webhooks/deliveries`, `/admin/clients/{id}/webhooks/test`, `/admin/clients/{id}/invite`, `/fleet`, `/approvals` | client list + PnL / SLA / invite / webhooks / tenant-agents cards | EmptyStateGuide; section-level "No data yet" | SLA thresholds hardcoded (99% / 95%) without legend; webhook `attempted=0` tone reads as warn but often just means no URLs configured |
| Reconciliation | `/reconciliation/fleet` | summary KVs, drift table, collapsible clean-rows section | "Clean — no drift detected" | clean rows collapsed by default — low discovery; ghost/phantom chips title-tooltip only (no drill) |
| Orderbook | WS state only | L2 book, open orders, inventory (global + per-venue) | — (assumes WS live) | No symbol selector; if WS stale the whole page is blank. Candidate for a "WS stale" banner reuse. |
| History | `/decisions/recent` fan-out via `/agents/{a}/deployments/{d}/details/decisions_recent`, WS for fills/open-orders | open orders, fills(20), decisions ledger, audit stream | component-scoped; ledger silently slices to 200 | Ledger slice has no "… N more" indicator |

### Compliance group

| Page | Endpoints | Notes |
|------|-----------|-------|
| Compliance | via children: ViolationsPanel / AlertLog / ConnectivityPanel / ReportsPanel / ClientCircuitPanel / SentimentPanel / AuditStream | Pure layout — no page-level loading/error surface; operator sees nothing until every child hydrates |
| Surveillance | `/surveillance/fleet` (3s poll) | Production-grade: loading spinner, empty-state message explaining detector warm-up, kill-level badges, score bars. Thresholds 0.8 alert / 0.5 watch align with `risk::manipulation` enum |
| Incidents | `/incidents` (5s poll), `/incidents/{id}/ack`, `/incidents/{id}/resolve` | Filter tabs (open/acked/resolved/all), resolve modal requires root_cause; clean state machine |

### Configure group

| Page | Endpoints | Notes |
|------|-----------|-------|
| Strategy | `/strategy/catalog` (130 nodes), `/strategy/templates` (19 graph JSONs), `/strategy/custom_templates`, `/strategy/validate` (300ms debounce), POST `/admin/strategy/graph` with optional `rollback_from` / `restricted_ack` query params, `/strategy/graphs/{name}/history/{hash}`, `/strategy/preview`, `/fleet` (for deploy targets) | Draft auto-saved to `localStorage["mm.strategy.draft.v1"]`. Two-phase deploy modal (pick targets → dispatch parallel). Restricted-node (412) triggers ack modal. Preview decorates edges with live tick values. Rollback-by-hash wired. No stubs |
| Settings | `/fleet` (3s poll) | Hub/launcher — 4 tiles (Platform, Vault, Fleet, Profile) + fleet-wide rollup. Correctly disclaims per-strategy tuning lives in Fleet drilldown |
| Calibration | ws state only; children PendingCalibrationCard + SignalsPanel | Minimal layout; AdaptivePanel + ParamTuner moved to Settings/Fleet respectively post-refactor |

### Admin group

| Page | Endpoints | Notes |
|------|-----------|-------|
| Platform | `/tunables/schema`, GET/PUT `/tunables` | Schema-driven form, per-field dirty tracking, semver/bool/number inputs. Clean |
| Vault | GET/POST/PUT/DELETE `/vault` (5s poll) | Kind gallery (7 secret kinds); pick-kind → create flow; rotate UX. Expiry logic tagged Wave C6 |
| Users | delegates to `<UserManagementPanel>` | Page is a 1-component wrapper; depth lives in the panel (not audited here) |
| Auth audit | `/admin/auth/audit?from_ms&until_ms&limit&contains` (30s poll) | Window selector (1h/24h/7d/30d), substring filter, event counts rollup |
| Admin | `/fleet` (5s poll for kill-level escalations) + 10 subcomponents (VenuesHealth, SorDecisions, AtomicBundles, RebalanceRecommendations, FundingArbPairs, AdverseSelection, CalibrationStatus, ManipulationScores, OnchainScores, AdminConfigPanels, ClientOnboardingPanel) | Kill-switch panel lists every deployment with `kill_level > 0` and routes one-click to Fleet drilldown. Subcomponents not re-audited here — depth varies |

### Account group

| Page | Endpoints | Notes |
|------|-----------|-------|
| Profile | `/auth/me`, POST `/auth/password`, `/auth/totp/{enroll,verify,disable}` | Three TOTP states (disabled / enrollment-with-QR / enabled); password dirty/validation; copy-secret UX |
| ClientPortal | `/client/self/{pnl,sla,sla/certificate,fills,webhook-deliveries,webhooks}` (5s poll), test-fire | Webhook CRUD + test-fire with detailed result; signed SLA cert download |
| ClientSignup | POST `/auth/client-signup` with `invite_token` | Minimal form; replaces URL on success. Token validation backend-side |
| PasswordReset | POST `/auth/password-reset` with `reset_token` | Two-state (form / done); no auto-login by design |

## UI-PAGE-* findings (from the per-page walk)

| # | Severity | Finding | Suggested fix |
|---|----------|---------|---------------|
| UI-PAGE-1 | 🟡 P2 | `Compliance` is a pure layout; if ViolationsPanel or AlertLog is slow/broken the whole page looks dead. No page-level error boundary or "loading…" banner. | Tiny skeleton/banner at page level that clears once any child reports ready. |
| UI-PAGE-2 | 🟡 P2 | `Orderbook` page assumes WS is live. If WS is reconnecting, every card renders empty with no hint why. | Reuse the "stream stale" banner pattern (already used on HeroKpis) at the page level. |
| UI-PAGE-3 | 🟡 P2 | `History.DecisionsLedger` silently caps at 200 rows across fleet-wide fan-out. Large fleets lose older decisions with no "N more" affordance. | Either show a "…N older truncated" footer with a "Load more" button, or surface as a per-agent drilldown instead of flat fan-out. |
| UI-PAGE-4 | 🟡 P2 | `Reconciliation` collapses clean rows behind a button by default. Operators reviewing a specific deployment may assume it wasn't reported. | Invert default to "Show clean" expanded when total row count is under ~20. |
| UI-PAGE-5 | 🟢 P3 | `Clients` SLA tone uses hardcoded 99% / 95% thresholds with no legend. | Add tooltip on the SLA % showing the compliance band. |
| UI-PAGE-6 | 🟢 P3 | `Clients` webhook test-fire shows `attempted=0` in the warn tone, even when the tenant has no URLs configured (which is not a warning). | Detect "no URLs" separately and use the info tone. |

None of these block operator flow — all are polish / discoverability. The critical lifecycle holes were UI-DEPLOY-1 and UI-STOP-1, both already closed.