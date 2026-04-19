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
