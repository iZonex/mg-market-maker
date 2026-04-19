# MM — Open Work Tracker

Last updated: 2026-04-19 (post-Sprint 18)

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
- [ ] **R11.4 DEFERRED** Engine REST-poll integration (mock
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
- [ ] **R10.2c DEFERRED** Engine tick integration (spin
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

## Renumbered — old Sprint 17 "deferred research" → now Sprint 19
## Renumbered — old Sprint 18 "honest MM side" → now Sprint 20

## Sprint 19 — deferred research from cascade doc (planned)

Closing the remaining "future research" items called out in
`docs/research/liquidation-cascades.md`. All restricted /
pentest-only.

- [ ] **R9.1** Cross-venue cascade: trigger a perp cascade via a
  spot push on a different venue. Needs sub-graph composition —
  `Out.VenueQuotes` exists, orchestration across graphs TBD.
- [ ] **R9.2** Funding-rate weaponization: use extreme funding
  rates to force leverage unwinds without a price push. Open
  question whether this works without large counterparty
  inventory; investigation + proof-of-concept.
- [ ] **R9.3** Index-composition gaming: move a weighted index by
  pushing its thinnest constituent. Needs per-index metadata
  source the venue has to expose.
- [ ] **R9.4** `Strategy.BasketPush`: coordinate pushes across
  correlated symbols (RAVE + SIREN + MYX shape).

## Sprint 20 — honest MM side closeout (planned)

Long-deferred MM-side quality work that's been sitting behind the
Epic R run. All non-restricted.

- [ ] **R10.1** Client onboarding UX polish — dashboard flow for
  new client registration needs end-to-end smoke.
- [ ] **PAPER-2** Two-venue paper smoke runbook exercise (operator
  task, runbook at `docs/guides/paper-smoke-two-venue.md`).
- [ ] **OBS-1** OTel DSN + Sentry sanity — both exist, neither
  tested against a live endpoint (operator task).
- [ ] **HARD-3** Reconciliation loop real-exchange test — exists
  in code, needs live venue keys (operator task).
- [ ] **R10.2** Integration test coverage audit: we have ~1600
  unit tests, but how many integration / E2E? Sprint 14 showed
  this gap is what lets gate drift hide. Enumerate, fill gaps.

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
