# MM — Open Work Tracker

Last updated: 2026-04-19 (post-Sprint 10)

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
