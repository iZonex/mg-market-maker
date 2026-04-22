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