# Dashboard UI Guide

Operator walkthrough of the MG Market Maker dashboard. Covers every page in the navigation sidebar, what each panel shows, when to use it, and the common flows (deploy a strategy, investigate an incident, rotate a vault entry, acknowledge a MiCA-flagged surveillance score).

**Tech stack:** Svelte 5 frontend on `:3000` (dev) or served from the Rust backend on `:9090` (prod), WebSocket + REST to the same Axum server.

**Auth:** JWT bearer, four roles — `viewer` (read-only fleet), `operator` (deploy, pause, ack incidents), `admin` (users, vault, kill switch reset, platform config), `client-reader` (tenant-scoped, sees only their own client's data). Route visibility is role-gated; the sidebar hides what your token can't reach.

---

## Navigation overview

Role gates come from `Sidebar.svelte`'s declared `roles: [...]` per entry:

### Live
| Route | Roles | Primary use |
|-------|-------|-------------|
| Overview | admin / operator / viewer | Single-symbol snapshot: mid, spread, PnL, inventory, kill level, SLA chip, regime chip, per-venue market strip |
| Orderbook | admin / operator / viewer | Top-20 L2 view of the primary symbol |
| History | admin / operator / viewer | Historical charts + daily report browser |

### Operations
| Route | Roles | Primary use |
|-------|-------|-------------|
| Fleet | admin / operator / viewer | All deployments across agents — search, filter, drilldown |
| Clients | admin / operator / viewer | Client list + scope browser |
| Reconciliation | admin / operator / viewer | Order + balance reconciliation vs venue |
| Incidents | admin / operator | Open / triage / resolve deployment incidents |

### Venues & Execution
| Route | Roles | Primary use |
|-------|-------|-------------|
| Venues | admin / operator | Per-venue connector status, capability flags |
| Calibration | admin / operator | Hyperopt trial history + pending calibration recommendations |

### Compliance
| Route | Roles | Primary use |
|-------|-------|-------------|
| Compliance | admin / operator / viewer | MiCA report export, SLA certificates, audit-chain verify |

### Configure
| Route | Roles | Primary use |
|-------|-------|-------------|
| Strategy | admin / operator | Graph authoring canvas + live-mode observability |
| Rules | admin / operator | Alert rules for webhook / Telegram routing |

### Admin
| Route | Roles | Primary use |
|-------|-------|-------------|
| Kill switch | admin | 5-level kill-switch state + manual reset |
| Platform | admin | Cluster-wide config — feature flags, loans, sentiment headlines |
| Vault | admin | Credential entries with rotation UI |
| Users | admin | User CRUD, role assignment |
| Auth audit | admin | Login attempt log for compliance audit |
| Surveillance | admin | Raw manipulation-score roster (drilldown preferred path) |

### Client-scoped (separate auth flow)
A user logging in with a `ClientReader` token sees a tenant-scoped Client Portal view (per-client PnL, positions, fills, reports) instead of the operator sidebar. This is a distinct UX that shares the HTTP API surface but not the navigation.

### Account (all roles)
| Route | Primary use |
|-------|-------------|
| My account | Password rotation, 2FA setup |

---

## Overview

Landing page when you're an operator of a single symbol. Header strip shows mid price, spread, PnL, inventory, and the kill-switch level pill. If the pill is `L0 Normal` green, the engine is quoting; anything else means one or more guards fired — click through to Incidents for the reason.

**Per-venue market strip** (UX-VENUE-1) — one row per `(venue, symbol, product)` on the data bus with bid/ask/mid, spread in bps, feed age, and a regime chip (QUIET / TREND / VOL / MR, per-venue since UX-VENUE-2). A row turning red means the venue's L1 feed went stale — check the corresponding connector status under Venues.

**Market-quality card** — volatility, VPIN, Kyle's lambda, market resilience, order-to-trade ratio. Use it to sanity-check toxicity before pushing a tighter-spread config.

**PnL ladder** — attribution breakdown: spread, inventory, rebates, fees, funding. A negative `spread` pnl with positive `rebates` is the "paying to be there" failure mode; a negative `inventory` pnl with positive `spread` is adverse selection.

**SLA ring** — 24h presence %, two-sided % and the spread-compliance bar. Below the configured MM agreement threshold and the ring turns red.

**Open orders + live fills** — the current live book and the last 20 fills. The fills table is the fastest way to verify "orders are actually executing" during a paper-smoke or incident triage.

---

## Fleet

When you run more than one deployment (common in multi-venue, multi-client, or pentest setups), Fleet is the home page.

The table lists every known `(agent_id, deployment_id)` pair with symbol, venue, mode, strategy, live PnL, live kill-level, live SLA. Sort/filter by any column. Click a row → `DeploymentDrilldown` modal with the full per-deployment state.

**Drilldown modal** — 22 live scalars + 6 structured sections (funding-arb events, SOR decisions, graph analysis, calibration sub-panel, manipulation sub-panel, funding-arb sub-panel). The data is fetched via the `FetchDeploymentDetails` control-plane command against the agent — no engine-side polling.

**Open graph (live)** button on the drilldown — jumps to Strategy with `?live=agentId/deploymentId`, loading the deployed graph onto the canvas in read-only "live mode". Edge values decorate in real time as ticks flow.

---

## Strategy

The graph-authoring canvas + live observability. Two modes:

### Authoring mode (default)
- **Palette** (left) — node catalog split by category: Sources (Book, Trade, Risk, Surveillance, Onchain), Math, Logic, Stats, Indicators, Strategies, Exec, Plan, Sinks. Drag into the canvas.
- **Canvas** (center) — svelte-flow with pan / zoom / mini-map. Drag from output port to input port to connect. Click a node to open the right-hand Config panel.
- **Config panel** (right) — per-node config fields. Schema-driven: the node's `config_schema()` in Rust drives the inputs.
- **Top toolbar** — Name, Scope (symbol / asset-class / client / global), Template picker (built-in + saved), Import / Export / Save / Versions / Simulate / Replay vs deployed / Deploy.
- **Validation strip** — below the toolbar. Shows validation issues live; the Deploy button enables only when green.
- **Deploy modal** — pick targets (agents × deployments) from the fleet roster, confirm. Restricted (pentest) nodes trigger a 412 ack modal with the `yes-pentest-mode` token.
- **Replay vs deployed** modal (M5) — replays the current canvas against the last 20 captured ticks of the selected live deployment, diffs sink actions and per-node-kind outputs. Side-by-side SVG mini-canvas with glow-highlight on diverging nodes (M5.2).

### Live mode
Entered via `?live=agentId/deploymentId` or the "Live" tab. Canvas becomes read-only; the engine's actual deployed graph renders with:
- Edge labels showing the most recent `(port, value)` tuple per edge
- Node badges with per-node output for the currently-pinned tick
- **Graph Timeline** footer — scroll through the last 256 trace ticks, pin one (URL updates `?tick=N`)
- **Graph Inspector** right-panel instead of Config — shows static `GraphAnalysis` (dead nodes, unconsumed outputs, required sources) + per-node stats from the live trace ring

---

## Rules

Alert rule editor. Every rule ties a condition (e.g. `kill_level >= 3`, `vpin > 0.8`, `spread_bps > 50 for 60s`) to a set of recipients (Telegram chat IDs, webhook URLs) and a severity (info / warn / critical).

Rules live in config TOML + admin-editable storage. See [Alerts & Webhooks](alerts-and-webhooks.md) for the full flow.

---

## Venues

Per-venue connector card. Shows:
- Connection state (connected / reconnecting / disconnected)
- Subscribed streams (book, trades, own fills, funding) with last-message timestamp
- Capability flags (`supports_ws_trading`, `supports_fix`, `supports_amend`, `supports_batch`)
- Rate-limit headroom (429 backoff state)

Use this page when "orders not placing" — verify capabilities, check reconnect timestamps, look for an active 429 backoff.

---

## Orderbook

Top-20 L2 visualization of the primary symbol. Primarily a sanity-check tool: is our book looking normal, do we have quotes at the tight end, is the opposite side being filled faster than our side.

---

## History

Charts of historical PnL / spread / inventory time-series + a daily-report browser. The browser lists every `daily-YYYY-MM-DD.json` snapshot the engine wrote at midnight UTC.

---

## Calibration

Two sub-views:
- **Trial history** — every hyperopt run: loss function, gamma sweep, sigma floor sweep, best parameter vector
- **Pending calibration** — the `PendingCalibrationCard` shows a recommendation the adaptive-calibration loop proposes but hasn't applied yet. Operator clicks Approve → config override dispatches.

---

## Reconciliation

Per-symbol + per-venue reconciliation state. For each reconcile tick (every 60s) shows:
- Orders diff — live-tracked vs venue-reported (mismatches flagged)
- Balance diff — internal vs venue wallet (per-asset)
- Last reconcile timestamp + duration

Mismatches here are the canary for "engine and venue disagree about reality". Root-cause in the audit log.

---

## Incidents

Open / triage / resolve deployment incidents. Each incident is an auto-generated record from a kill-switch escalation, a circuit-breaker trip, a reconciliation mismatch, or a manual operator post.

**Triage flow:**
1. Click open incident
2. Read the trigger event + attached audit rows
3. Click "Open graph (@tick)" to jump to Strategy live mode pre-pinned at the tick that tripped the guard
4. Acknowledge or resolve; both write to the audit chain

---

## Compliance

MiCA Article 17 report export + SLA certificates + audit-chain verify. Operator can:
- Generate a monthly MiCA report (JSON / CSV / XLSX / PDF with HMAC-signed manifest)
- Export date-range filtered audit log (signed JSONL)
- Run audit-chain SHA-256 verify against the agent's local file — detects tampering

---

## Surveillance

**Admin-only.** Raw manipulation-score roster: every deployment × every score (Spoofing, Layering, QuoteStuffing, Wash, MarkingClose, FakeLiquidity, MomentumIgnition) in a grid.

For contextual scores, the Fleet → Deployment Drilldown's surveillance sub-panel is the better UX — this page is kept for quick fleet-wide scanning.

---

## Kill Switch

**Admin-only.** Shows the current kill-switch level across the fleet and lets admin trigger a manual reset when automatic escalation fires.

**5 levels** (each triggered automatically by its guard):
- **L0 Normal** — quoting
- **L1 WidenSpreads** — toxicity > threshold OR Market Resilience < 0.3 for 3s+
- **L2 StopNewOrders** — drawdown / VaR / news-retreat Critical
- **L3 CancelAll** — hard limit breach
- **L4 FlattenAll** — disaster; TWAP-out the inventory
- **L5 Disconnect** — manual-reset-required paused state

The reset button is the only manual kill-switch control — you can escalate via risk breach, never de-escalate automatically past L3.

---

## Vault

**Admin-only.** Per-entry credentials: `(client_id, venue, kind)` → encrypted blob. Rotation UI rewrites the entry + emits a `VaultRotated` audit event.

Never stores plaintext secrets in config files — `MM_API_KEY` / `MM_API_SECRET` env vars on startup populate the vault, which is then the source of truth for every engine.

---

## Platform

**Admin-only.** Cluster-wide config panel — feature flags, loan agreements, sentiment headline feeds, webhook delivery endpoints.

Edits here route through the admin config endpoints and broadcast to every agent via the control-plane `ConfigOverride` message.

---

## Users

**Admin-only.** User CRUD + role assignment + 2FA enforcement flag.

---

## Login Audit

**Admin-only.** Every login attempt (successful + failed) for compliance audit. Filters by user, IP, outcome, date range.

---

## Client Portal

Read-only views for a client logged in with a per-client token. Scoped to the client's symbols only — no cross-client leakage.

- **Positions** — per-symbol current position + value
- **PnL** — attribution breakdown (spread / inventory / rebates / fees / funding)
- **Fills** — recent fills
- **Reports** — daily reports signed with HMAC for auditability

---

## Profile

Any role. Password change, 2FA setup, API-key management.

---

## Common flows

**Deploy a new strategy:**
1. Strategy → drag palette nodes onto canvas → connect → configure
2. Click Simulate to preview without deploy
3. Click Save → pick name + description → saves as custom template with version history
4. Click Deploy → pick targets from fleet modal → confirm
5. Switch to Fleet → verify the deployment appears in the table with the new graph hash

**Triage an L2 kill-switch trip:**
1. Incidents → click the red row
2. Read trigger (e.g. `drawdown_exceeded: −$520 > −$500`)
3. "Open graph (@tick)" → Strategy live mode pre-pinned at the tick
4. Inspect node outputs; find which signal spiked
5. Either acknowledge (accept the trip) or resolve with a note (after correcting the config)

**Rotate a venue credential:**
1. Vault → find the entry (filter by venue)
2. Click Rotate → paste new key/secret → confirm
3. Venue card on Venues should show a reconnect within 30s
4. Verify normal order placement on Orderbook page

**Export a MiCA monthly report:**
1. Compliance → MiCA Report
2. Pick month + client scope → Generate
3. Pick format (XLSX for regulators, JSON for machine ingest)
4. Download manifest (contains the HMAC signature for the bundle)
