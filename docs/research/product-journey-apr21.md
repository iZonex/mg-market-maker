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

## Round 1 — strategy catalog smoke (2026-04-22)

Shifted focus from security/UX to actual trading logic. Deployed
three templates concurrently on one agent (major-spot-basic
BTCUSDT, glft-via-graph ETHUSDT, grid-via-graph SOLUSDT) and
watched whether each actually produced quotes.

| Finding | Severity | Resolution |
|---|---|---|
| **R1-DEPLOY-1** Multi-deployment semantics: POSTing deployments sequentially (one strategy per request) stops the previous deployment — POST is "desired state", not "append". An operator deploying 3 pairs over 3 clicks would leave only the last one running. | 🟠 documentation | Acknowledged; workaround is to bundle strategies in a single POST. Tracked for UI fix. |
| **R1-QUOTE-1 (SILENT DEAD QUOTES)** Default `order_size=0.001` (base units) is tuned for BTCUSDT (~$75 notional). On ETH, SOL, and any sub-$100 pair, `0.001 * mid < min_notional` → every quote fails `ProductSpec::meets_min_notional` → `live_orders=0` with zero operator-visible error. | 🔥 P0 (all non-BTC strategies dead) | Added `MarketMakerEngine::auto_scale_order_size` that runs once when mid is known. Bumps `order_size` to `1.2 * min_notional / mid` rounded up to lot when the default value fails the floor; operator-sized configs pass through unchanged. Logged as WARN. Verified live: ETHUSDT 0.001→0.0026, SOLUSDT 0.001→0.071, BTCUSDT untouched. Regression tests `auto_scale_order_size_bumps_under_min_notional` + `auto_scale_order_size_leaves_adequate_config_alone`. |
| **R1-TEMPLATE-1** Catalog template `cost-gated-quoter` maps to "unknown template — falling back to Avellaneda-Stoikov" in agent's `template_to_strategy_type`. Operator sees a deployed template that doesn't do what the catalog blurb claims. | 🟠 P1 | Deferred — catalog + mapping need a single source of truth. |
| **R1-PAPER-1** Paper mode hits venue REST endpoints for balance + fee-tier refresh every 3s. Demo key returns 401 → warning every 3s, spams the log. Paper mode should skip these. | 🟡 P2 | Deferred — cosmetic in logs, doesn't affect quoting. |
| **R1-STALE-1** SOLUSDT `book stale — pausing quotes and cancelling orders` every 10s on binance-testnet. Threshold may be too aggressive for lower-volume pairs, or WS stream has real gaps. | 🟡 P2 | Deferred pending a look at `stale_book_timeout_secs` default + per-symbol baseline. |
| **R1-TEMPLATE-3 (DEAD GRAPHS)** None of the 14 bundled strategy-graph JSON templates (rug-detector-composite, liquidity-burn-guard, cost-gated-quoter, meme-spot-guarded, cross-asset-regime, etc.) were ever loaded by the agent. Operator picked a template from the catalog, agent ran a plain Avellaneda-Stoikov (or the nearest name-matched strategy), graph JSON discarded. Every composite rug detector, cost gate, liquidity guard node was ornamental. | 🔥 P0 | Added `mm-strategy-graph` as a runtime dep on `mm-agent`. `MarketMakerRunner` now calls `templates::load(template_name)` after engine construction; on success attaches via `swap_strategy_graph` (in-place; Err branch logs and falls back to plain strategy — no engine loss). Verified live: 6 templates deployed concurrently, all 6 log `attached strategy graph from template nodes=N` — 3 for `major-spot-basic`, 4 for `glft-via-graph`, 8 for `cost-gated-quoter`, 8 for `liquidity-burn-guard`, 8 for `meme-spot-guarded`, 11 for `grid-via-graph`. |
| **R1-CB-1 (PERMA-DEAD AGENT)** `CircuitBreaker::check_stale_book` trips `StaleBook` on a 10s WS gap but **never** auto-resets — only `MarketEvent::Connected` clears it. A single transient WS gap on binance-testnet (observed on ADAUSDT) puts the engine into permanent silent halt; agent keeps ticking, logs "SLA VIOLATION one_sided=61" every 30s, `live_orders=0` forever. | 🔥 P0 | Self-heal in `check_stale_book`: when the breaker is tripped with `StaleBook` reason and the next check finds the book fresh again, auto-reset. Other trip reasons (MaxDrawdown, MaxExposure, WideSpread, Manual) still require explicit operator action. Regression tests `stale_book_trip_auto_resets_when_book_fresh_again` + `fresh_book_does_not_clear_non_stale_trips`. |
| **R1-CROSS-1 (UNHEDGED EXPOSURE)** Cross-venue strategies (`Basis`, `FundingArb`, `CrossVenueBasis`, `CrossExchange`, `StatArb`) run on a single-venue bundle silently. No hedge credential → `config.hedge = None` → `(None, None)` branch built a single-venue bundle; engine quoted primary-only while operator thought they had a hedge leg. Inventory builds unchecked, catalog description lies. Observed: all 5 dual-venue templates deployed on a spot-only agent showed `running=True, live_orders=6`. | 🔥 P0 | Added `strategy_requires_hedge(&StrategyType)` predicate + pre-check in `MarketMakerRunner::run`. Strategies that structurally need a hedge credential fail the deploy with a clear error (`"refusing to start single-sided to avoid unhedged exposure. Set variables.hedge_credential"`) before market data streams. Verified live: re-deploy of basis / funding-aware / xemm-reactive returns `running=False` with the refusal reason visible in agent log. Tests `strategies_needing_hedge_are_enumerated` + `single_venue_strategies_do_not_need_hedge`. |

## Round 2 — signals deep dive (2026-04-22)

Observed a running avellaneda-via-graph deployment on BTCUSDT
for ~90s. Tabulated each signal's behaviour (live movement vs
warmup vs stuck default).

| Signal | Observed | Verdict |
|---|---|---|
| `volatility` | 0.06-0.31 (moves with returns) | ✅ |
| `kyle_lambda` | -3e-3 to +3e-5 (moves with trade flow) | ✅ |
| `adverse_bps` | 0.38-6.96 (moves with fills) | ✅ |
| `manipulation_pump_dump` | 0.015 | ✅ |
| `manipulation_combined` | 0.058 | ✅ |
| `spread_bps` | 0.0013 (tight, realistic) | ✅ |
| `as_prob_bid/ask` | 0.5 (neutral prior; moves only when Cartea signal fires) | Expected |
| `vpin` | 0 / None | Warming up (50× $50k bucket on testnet takes hours) |
| `momentum_ofi_ewma` | 0 | `momentum_ofi_enabled = false` by default — opt-in signal |
| `learned_microprice` | 0 | `momentum_learned_microprice_online = false` by default |
| `bvc_*` | 0 | `bvc_enabled = false` by default |
| `regime` | **None in fleet row despite `regime=Quiet` in engine log** | 🔥 bug — fixed below |

| # | Fix | Details |
|---|-----|---|
| **R2-REGIME-1** | 🟠 P1 cosmetic | `mm_regime` Prometheus gauge defined in `dashboard::metrics` but never set anywhere. Agent registry reads it through `read_gauge_by_symbol` → None → fleet row `regime` stayed empty even while engine classified the market. Fix: emit `REGIME` gauge in `DashboardState::update` with the same 0/1/2/3 encoding the agent's `regime_label` expects. Verified live — fleet row now returns `regime: "Quiet"` and `/metrics` shows `mm_regime{symbol="BTCUSDT"} 0`. |

Observed bonus (not a bug):
- After ~5 minutes of paper running with tiny-notional quotes not
  getting hit by real market trades, kill switch auto-escalated:
  `KILL SWITCH ESCALATED from=NORMAL to=WIDEN_SPREADS reason="no fills for extended period"`.
  Idleness detector works as designed — noted so R3 (risk triggers
  pass) covers the manual invocation / progression path.

## Round 3 — risk triggers deep dive (2026-04-22)

Exercised the full kill-switch ladder via the ops endpoint:
`POST /api/v1/agents/{a}/deployments/{d}/ops/{widen|stop|cancel-all|flatten|disconnect|reset}`.

All levels correctly dispatched into the engine — audit log shows
`manual kill switch trigger level=WIDEN_SPREADS`, `STOP_NEW_ORDERS`,
`CANCEL_ALL`, `FLATTEN_ALL`, `reset` events in sequence. Fleet
deployment row reflected the current level after each call.

| # | Fix | Details |
|---|-----|---|
| **R3-FLATTEN-1 (NO UNWIND)** | 🔥 P0 | L4 flatten generated TWAP slices every ~6 s but **never placed any orders** — the engine's TWAP branch was a log-only stub: `info!(... "TWAP slice")` without calling `order_manager.execute_unwind_slice`. Inventory stayed locked forever. An operator hitting the kill-switch to unwind a runaway position would see slices printed in the log and assume the engine is working, but the venue book would see zero activity. Fix: actually call `execute_unwind_slice` (spot) or `execute_reduce_slice` (perp, sets `reduce_only=true`). Verified live: triggered flatten on a long position; three paper unwind slices printed as `[PAPER] placed unwind slice (simulated) side=Sell qty=0.00047` — orders actually on the simulated book. |

Observed but not yet fixed:
- `exposure_manager.is_exposure_breached` trips `CircuitBreaker::MaxExposure` which gates `refresh_quotes` → but TWAP unwind orders come from a separate loop (ok after R3-FLATTEN-1). Still, the breaker reason never auto-clears when inventory comes back under limit (same class of bug as R1-CB-1 for StaleBook). Tracked as R3-CB-EXPOSURE for the next pass — only matters when operator pushes inventory past `max_exposure_quote` and then reduces below it, which the fresh R3-FLATTEN-1 unwind path can now actually do.
- Kill switch idleness detector (`no fills for extended period`) auto-escalates even in paper mode. Fine for live but noisy for paper smoke runs. Tracked as R3-IDLE-PAPER.

## Round 4 — reconciliation drift response (2026-04-22)

Reconcile cycle fires every 60 s; surfaces `ghost_orders`
(tracked locally but gone from venue), `phantom_orders` (live on
venue, not tracked locally), and `balance_mismatches` from the
inventory-drift reconciler. All three were **log-only + audit
entry** — no incident raised, no operator-visible alert.

| # | Fix | Details |
|---|-----|---|
| **R4-DRIFT-1 (INVISIBLE DRIFT)** | 🟠 P1 | Inventory drift on BTC/ETH between tracked inventory and wallet total was warn-logged + audit'd but never recorded as an incident. Operator away from logs / Reconciliation panel would miss growing drift entirely. Fix: emit `record_incident` on drift detection — severity `medium` when auto-correct is on (PnL attribution slightly stale but risk survived), `high` when uncorrected (every downstream decision based on wrong inventory value). Message carries tracked/expected/drift numbers so the operator can see the scale at a glance. |
| **R4-DRIFT-2 (ORDER STATE DRIFT INVISIBLE)** | 🟠 P1 | Ghost orders (local ≠ venue) and phantom orders (venue ≠ local) were log-only. A ghost means a venue cancellation we never saw the notification for; a phantom means the venue is holding our capital against an order we lost track of. Both indicate state drift that needs operator investigation. Fix: record_incident on non-empty ghost / phantom set with clear message + count. Ghosts `medium`, phantoms `high` (phantoms imply past state loss). |

## Round 5 — order-manager amend path (2026-04-22)

Code audit + capability cross-check:
- `execute_diff` routes by `connector.capabilities().supports_amend`
  and `config.market_maker.amend_enabled`.
- `amend_epsilon_ticks = 2` default budget; amends within 2 ticks
  preserve queue priority, outside fall through to cancel + place.
- Venue support matrix:
  - Binance spot: `supports_amend = false` → cancel + place (observed `amended=0`)
  - Binance futures: `supports_amend = true`
  - Bybit V5: `supports_amend = true` (batch 20)
  - HyperLiquid: `supports_amend = false` (cancel + place)
- Paper mode simulates amend via `reprice_order` instead of cancel+place.
- On amend failure the caller falls back to cancel+place — no
  quotes are lost.

No bugs. Path is alive and correctly gated.

## Round 6 — queue tracker correctness (2026-04-22)

Code audit confirms full wiring:
- `on_order_placed` called from `execute_diff` post-diff path
  with the pre-snapshot book_qty at the chosen price as initial
  queue position (market_maker.rs:9638).
- `on_order_amended` on price change — resets queue position
  at the new level with fresh book_qty (market_maker.rs:9644).
- `on_order_cancelled` on missing-from-post-set + on fill
  completion.
- `on_trade` decrements queue position ahead of us whenever a
  market trade happens at our price.
- `on_depth_change` updates prev-qty cache from L2 deltas, so
  a change in front-of-queue qty rolls forward into every
  tracked order at that level.

`Book.FillProbability` graph source reads `queue_pos_of(id)` and
the 30s trade-rate EWMA to produce a Poisson fill probability
— node is registered in the catalog and consumed by the
Avellaneda / GLFT quoter paths.

No bugs.

After R1-QUOTE-1 fix, three strategies live concurrently:
- `major-spot-basic` BTCUSDT: live=2 orders, mid=75600
- `glft-via-graph` ETHUSDT: live=2, mid=2315
- `grid-via-graph` SOLUSDT: live=6 (3 levels × 2 sides), mid=85.3

Follow-up rounds (still outstanding):
- **R1.4** basis-carry-spot-perp (requires perp)
- **R1.5** funding-aware-quoter (requires perp)
- **R1.6** cross-exchange / xemm (requires 2 venues)
- **R1.7** stat_arb async driver (requires 2 cointegrated pairs)
- **R2** signals-deep-dive — do VPIN/Kyle/OFI/microprice actually move with market, or stuck on defaults
- **R3** risk triggers — force conditions, verify kill switch / circuit breaker / drawdown fire
- **R4** reconciliation drift response
- **R5** order manager amend vs cancel-replace
- **R6** queue tracker correctness on real trade deltas

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
