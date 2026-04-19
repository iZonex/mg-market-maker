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
