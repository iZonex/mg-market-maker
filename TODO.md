# MM — Open Work Tracker

Last updated: 2026-04-19 (post-INV-4)

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
- [ ] **PERP-2** Use venue-reported `total_maintenance_margin`
  for `projected_ratio` instead of the flat `IM ≈ notional /
  leverage` approximation. A more accurate projection prevents
  the engine from over-sizing on a tight book.
- [ ] **PERP-3** Differentiate cross-margin vs isolated-margin
  modes. Extend `MarginConfig.per_symbol` with `MarginMode`
  enum; gate quote generation + reduce logic accordingly.
- [ ] **PERP-4** Insurance fund / ADL awareness. Read venue
  insurance balance + our ADL rank where exposed; widen on
  elevated ADL rank because a venue-forced deleverage can
  close our position at the mark.

### Inventory truth
- [ ] **INV-2** Per-trade drawdown + holding-time on
  `InventoryManager`. Track peak-to-trough inventory within a
  single trade; track seconds-since-open.
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
- [ ] **KILL-L5** Real Disconnect teardown. Level 5 is
  currently a state label; wire actual socket close, engine
  pause, and manual-only reset.

---

## P1 — operator-visible gaps

### Paper validation
- [ ] **PAPER-2** 30-minute two-venue paper smoke test.
  Run binance paper + bybit paper side-by-side; deploy a
  graph using cross-venue reads; verify DecisionLedger
  resolves, tiered OTR publishes per venue, cross-venue
  inventory aggregates. Operator-driven — write a smoke-test
  runbook in `docs/guides/`.
- [ ] **PAPER-3** Paper-fill sanity tests for the new graph
  sources (Plan.Accumulate, Cost.Sweep, Risk.UnrealizedIfFlatten)
  — unit tests that feed synthetic book state and assert the
  source node emits the expected value.

### UI / UX
- [ ] **UI-5** Graph deploy diff viewer. History panel shows
  hash + operator + timestamp; add a side-by-side JSON diff
  against the previous deployed version when the operator
  clicks a history row.
- [ ] **UI-6** Pentest template review flow. Operator must
  explicitly ack the restricted-node list before the graph
  deploys when `MM_RESTRICTED_ALLOW=1` is on.
- [ ] **UI-7** Admin config panels that are backend-ready but
  frontend-absent: webhooks, alerts, loans, sentiment
  overrides (see `prod_readiness_audit_apr17` memory for the
  full list).
- [ ] **UI-8** Per-venue book-update latency p95 on
  VenuesHealth. Read `mm_book_update_latency_ms` histogram
  via either a new aggregator endpoint or a direct Prometheus
  query from the server.

### Observability
- [ ] **OBS-1** OTel traces with request/tick spans. Sentry
  error reporting already present on `server.rs` init but
  untested against a real DSN.
- [ ] **OBS-2** Per-venue latency Prometheus view: aggregator
  endpoint for the frontend.

---

## P2 — full-featured

### Multi-venue
- [ ] **MV-1** `MultiVenueOrderRouter` — replaces the
  degenerate dispatcher that ignores remote legs with a real
  cross-engine dispatch via `ExternalVenueQuotes` channel.
  Comment marker at `engine/src/market_maker.rs:3195`.
- [ ] **MV-2** `Out.AtomicBundle` cross-venue ack watch (3.E.2).
  Today cross-venue legs stay `acked=false` forever. Add a
  distributed ack loop so the watchdog rollback actually
  flips on a failed leg.
- [ ] **MV-3** Fee-aware SOR routing.
- [ ] **MV-4** `StatArbDriver` auto-dispatch (currently
  advisory-only per `engine/src/market_maker.rs:453`).

### Strategies + sources
- [ ] **STRAT-1** Stateful feature extractors behind Strategy
  trait. Reuse the MM-2 `on_fill` / `on_tick` hooks for
  regret memory / Q-table / bandit strategies.
- [ ] **STRAT-2** Composite `Strategy.Queue-aware` — takes a
  `Book.FillProbability` input, skews size + level by the
  probability estimate.

### Graph polish
- [ ] **GR-1** Per-node kind schema validation extensions —
  enum coverage for fields like `cmp` on Cast.ToBool, range
  validators on windows (> 0).
- [ ] **GR-2** Per-venue kill-switch link (detector score
  ≥ 0.8 on venue X kills pool entry on venue X only).

---

## P3 — hardening / polish

- [ ] **HARD-1** Hot-path unwrap audit in `market_maker.rs`
  (38 `unwrap`/`expect` calls; `prod_readiness_audit_apr17`
  flagged).
- [ ] **HARD-2** Audit log hash-chain verify on startup. The
  `verify_chain` helper exists (Sprint 5d); call it on boot,
  refuse to start on a broken chain unless
  `MM_AUDIT_RESUME_ON_BROKEN=yes` is set.
- [ ] **HARD-3** Reconciliation loop real-exchange testing.
  The reconcile code exists; no integration test fires against
  a real venue account.
- [ ] **HARD-4** Remove 9 `unsafe` blocks across workspace —
  every one needs a safety comment or refactor.
- [ ] **HARD-5** CI/CD pipeline: github actions for build +
  clippy + test + frontend build on every PR.
- [ ] **HARD-6** Fix flaky `auth::tests::token_round_trips_and_expires`.
  The test tampers a token by `pop()` + `push('a')`; if the
  original token already ends in `'a'` the tamper is a no-op
  and the assertion fails. Replace with a deterministic tamper
  (e.g. flip the final char to a known-different byte).

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
