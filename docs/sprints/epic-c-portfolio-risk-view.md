# Epic C — Portfolio-level risk view

> Sprint plan for the first epic from the SOTA gap pass
> (`docs/research/production-mm-state-of-the-art.md`).
> Sequenced first because it unlocks Epic B (per-strategy
> PnL is a prerequisite for stat-arb attribution) and is
> the smallest of the P1 epics in pure dev cost.

## Why this epic

Risk guards in v0.4.0 are per-strategy, per-symbol, and
per-asset-class (P2.1). What is missing is the **portfolio**
layer — the aggregation that turns "eight strategies quoting
on three venues in four assets" into one coherent risk view.
Without it: every strategy lives in a PnL silo, the operator
has no per-asset delta number to look at, there is no way to
compare expected hedge cost against current funding rates,
and there is no credible institutional risk story.

## Scope (5 sub-components)

| # | Component | New module / extension | Why |
|---|---|---|---|
| 1 | Per-factor delta aggregation | `mm-portfolio::Portfolio` extension | Show BTC-delta, ETH-delta, SOL-delta, stablecoin-delta as one view |
| 2 | Per-strategy PnL labeling | `mm-portfolio::Portfolio` + dashboard tag | Distinguishes Avellaneda PnL from Basis PnL from FundingArb PnL — prereq for Epic B |
| 3 | Cross-asset hedge optimizer | new `mm-risk::hedge_optimizer` module | Cartea-Jaimungal ch.6 closed-form: takes the exposure vector, emits the optimal hedge basket subject to funding cost |
| 4 | Per-strategy VaR limit | new `mm-risk::var_guard` module | Rolling 24h PnL variance per strategy → 95 %-VaR ceiling → soft-throttle on breach |
| 5 | Stress replay library + CLI | `mm-backtester` + new `mm-stress-test` bin | Five canonical crypto crashes (2020 covid, 2021 China ban, 2022 LUNA, 2022 FTX, 2023 USDC depeg) as standardised replay scenarios |

## Pre-conditions

- Per-strategy PnL labeling — partial today (each engine has
  one strategy, so per-engine PnL ≈ per-strategy PnL); just
  needs a strategy-class tag on the dashboard push so the
  daily report can group by strategy class
- All Cartea ch.6 inputs are already in the codebase (mid
  prices, funding rates, balance cache)
- Backtester replay primitive is in place — we only need the
  curated event data and the CLI wrapper

## Total effort

**4 sprints × 1 week = 4 weeks** (the research doc said 3
weeks of dev; the 4th sprint covers planning, study, and the
test+doc tail honestly).

---

## Sprint C-1 — Planning + Study (week 1)

**Goal.** Pin every implementation decision before any code
lands. End the sprint with a per-sub-component design note
that the dev sprints can execute against without further
research.

### Phase 1 — Planning

- [ ] Walk through every module that touches `Portfolio` /
  `PnlTracker` / `InventoryManager` / `BalanceCache` and write
  the **exact** field-by-field delta this epic introduces
- [ ] Build a dependency graph: which sub-component blocks
  which (#1 → #3 because the optimizer reads the delta;
  #2 is independent; #4 needs #2; #5 is independent)
- [ ] Pin the public APIs for each new module before writing
  code (function signatures, error types, config fields)
- [ ] Define DoD per sub-component (see "Definition of done"
  section below)

### Phase 2 — Study

- [ ] Re-read **Cartea-Jaimungal-Penalva chapter 6** end to
  end, transcribe the closed-form hedge optimizer into
  pseudo-code with the variable names we will use in
  `mm-risk::hedge_optimizer`
- [ ] Re-read **Cartea-Jaimungal chapter 7** for the VaR /
  drawdown formalism, decide whether we want simple
  rolling-window VaR (cheaper) or the parametric Gaussian
  VaR (more honest but needs a covariance estimate)
- [ ] Audit the existing `mm-portfolio::Portfolio` struct
  field by field: what it tracks today, what's missing for
  per-factor delta, what's missing for per-strategy
  attribution
- [ ] Audit `mm-backtester::data` and `mm-backtester::simulator`
  to figure out the JSONL event schema that the stress
  replay library needs to consume
- [ ] Survey what historical event data is available locally
  vs what would need a Tardis subscription. Map out exactly
  which symbols / dates / venues we can replay today

### Deliverables

- `docs/sprints/epic-c-portfolio-risk-view.md` (this file)
  updated with the design notes inline at the bottom of
  each sub-component section
- `docs/research/cartea-ch6-hedge-optimizer-notes.md` — short
  transcription of the formulas with our variable names
- A clear go/no-go answer on each sub-component: does it
  fit the 4-week budget, or does it slip to its own epic

### Files touched in this sprint

- `docs/sprints/epic-c-portfolio-risk-view.md` (this file)
- `docs/research/cartea-ch6-hedge-optimizer-notes.md` (new)

### DoD

- Every sub-component has a public API sketch, a tests-list,
  and a "files touched" estimate
- The next 3 sprints can be executed without further
  open-question rounds

---

## Sprint C-2 — Collection + Dev start (week 2)

**Goal.** Land sub-component **#1 (per-factor delta)** and
sub-component **#2 (per-strategy labeling)** as the smallest
useful slice. Curate the stress event JSONL data set in
parallel.

### Phase 3 — Collection

- [ ] Download or generate event JSONL snapshots for the five
  canonical crypto crashes:
  - 2020-03-12 BTC/USDT covid crash (-50 % in 24h)
  - 2021-05-19 China ban day
  - 2022-05-09 → 2022-05-12 LUNA collapse
  - 2022-11-08 → 2022-11-11 FTX collapse
  - 2023-03-10 → 2023-03-13 USDC depeg
- [ ] Each scenario gets a directory under
  `data/stress/{slug}/` containing the JSONL plus a
  `metadata.json` describing the time window, symbols, and
  source
- [ ] Anonymise the data so it can ship in the repo (no
  per-account context) — these are public market data only
- [ ] Add a one-paragraph README per scenario explaining what
  happened and what the stress test is exercising

### Phase 4a — Dev (sub-components #1 + #2)

- [ ] **Sub-component #1**: extend `mm-portfolio::Portfolio`
  with `per_factor_delta(asset) -> Decimal` and `factors() ->
  Vec<(String, Decimal)>` accessors. Backed by an internal
  `HashMap<String, Decimal>` updated on every `on_fill`.
  Aggregation is signed (long contributes +qty, short -qty)
  and zero-clean (factors with abs delta below `dust_threshold`
  are pruned from the iterator).
- [ ] New Prometheus gauge
  `mm_portfolio_delta{asset="BTC"}` per asset, pushed from
  `DashboardState::update_portfolio`.
- [ ] New `SymbolState`-side / `Portfolio`-side dashboard
  field exposing the factor list to the daily report
- [ ] **Sub-component #2**: add `strategy_class: &'static str`
  to `MarketMakerEngine` (read from the strategy's
  `name()` method, normalised) and thread it through the
  dashboard push so per-strategy aggregation is possible.
  This is mostly plumbing — no new logic.

### Deliverables

- 5 directories under `data/stress/` with JSONL + metadata
- `mm-portfolio::Portfolio` with per-factor delta + factor
  iterator
- New Prometheus gauge `mm_portfolio_delta`
- New dashboard daily-report field `factors: Vec<{asset, delta}>`
- `strategy_class` tag visible on `SymbolState`

### Files touched

- `crates/portfolio/src/lib.rs`
- `crates/dashboard/src/state.rs`
- `crates/dashboard/src/metrics.rs`
- `crates/dashboard/src/client_api.rs`
- `crates/engine/src/market_maker.rs` (one push line)
- `data/stress/*/` (new files)

### DoD

- 7+ unit tests on the per-factor aggregation (single asset,
  multi-asset, signed cancellation, dust pruning, fill-by-fill
  invariant)
- `cargo test --workspace` green
- `cargo clippy --workspace --all-targets -- -D warnings` clean

---

## Sprint C-3 — Dev main (week 3)

**Goal.** Land the two heavy components: **#3 hedge
optimizer** and **#4 VaR guard**. Both are pure-logic Rust
modules with no IO.

### Phase 4b — Dev (sub-components #3 + #4)

- [ ] **Sub-component #3** — `mm-risk::hedge_optimizer`:
  - `HedgeOptimizer::new(funding_cost_bps, max_basket_qty)`
  - `optimize(exposure: &[(Asset, Decimal)], hedge_universe: &[HedgeInstrument]) -> HedgeBasket`
  - Closed-form Cartea-Jaimungal ch.6 formula transcribed
    in Sprint C-1
  - Returns the basket as a list of (instrument, qty)
    pairs the operator can hand off to an `ExecAlgorithm`
  - Pure function — no IO, no async
  - Exposed via a new `MarketMakerEngine::recommend_hedge_basket()`
    accessor that the operator dashboard can poll
- [ ] **Sub-component #4** — `mm-risk::var_guard`:
  - `VarGuard::new(window_secs, var_pct, breach_action)`
  - `record_pnl_sample(strategy_class, pnl)`
  - `effective_throttle(strategy_class) -> Decimal` — returns
    a `[0, 1]` multiplier the engine threads into
    `effective_size_multiplier()` next to the kill switch one
  - Rolling 24h ring buffer per strategy class
  - Parametric Gaussian VaR (mean + variance) — cheap and
    enough for a v1
  - On 95 %-VaR breach: returns `0.5` (half size) for the
    breached strategy; on 99 %-VaR breach: returns `0.0`
    (full halt for that strategy class)
- [ ] Wire `VarGuard` into the engine's autotune path next to
  `MarketResilienceCalculator` and `BorrowManager` — same
  `effective_*_multiplier()` shape

### Deliverables

- New `mm-risk::hedge_optimizer` module (~300-400 LoC + tests)
- New `mm-risk::var_guard` module (~250 LoC + tests)
- Both wired into the engine
- New audit events `HedgeBasketRecommended` and
  `VarGuardThrottleApplied`

### Files touched

- `crates/risk/src/lib.rs`
- `crates/risk/src/hedge_optimizer.rs` (new)
- `crates/risk/src/var_guard.rs` (new)
- `crates/risk/src/audit.rs` (2 audit variants)
- `crates/engine/src/market_maker.rs` (wiring)

### DoD

- ≥10 unit tests on `hedge_optimizer` (degenerate cases:
  flat exposure, single-asset, perfectly-hedged-already,
  funding-cost-too-high → empty basket, asymmetric universe)
- ≥8 unit tests on `var_guard` (warm-up, 95 % vs 99 % bands,
  multi-strategy isolation, rolling window correctness)
- Engine integration test that drives a synthetic PnL series
  through `var_guard` and asserts the throttle multiplier
- `cargo test + clippy` clean

---

## Sprint C-4 — Stress CLI + Test wrap + Documentation (week 4)

**Goal.** Land sub-component **#5 (stress replay)**, the
end-to-end integration test, and all the documentation that
makes the epic shippable.

### Phase 4c — Dev (sub-component #5)

- [ ] New binary `crates/backtester/src/bin/mm_stress_test.rs`
- [ ] CLI shape:
  `cargo run -p mm-backtester --bin mm-stress-test --
  --scenario=ftx --config=config/default.toml`
- [ ] Loads the JSONL from `data/stress/{scenario}/`,
  drives the existing simulator + queue model + strategy
  config through the event stream, captures:
  - max drawdown (engine-side from `pnl_tracker.attribution`)
  - time to recovery (first time PnL returns to peak)
  - inventory peak abs
  - kill-switch escalation events
  - SLA presence breakdown for the scenario window
  - VaR guard throttle activations
  - Hedge basket recommendations along the path
- [ ] Output: a single JSON report on stdout +
  `--output report.md` for a human-readable markdown version
- [ ] `--all` flag runs every scenario sequentially and
  emits one summary table

### Phase 5 — Testing

- [ ] One end-to-end integration test in
  `crates/engine/tests/integration.rs` that drives the
  engine through one of the smaller stress scenarios
  (USDC depeg is the cleanest because it's narrow in time
  and only touches one asset)
- [ ] Property-based test on `hedge_optimizer` — random
  exposure vectors should always return a basket whose
  notional is bounded by `max_basket_qty`
- [ ] Re-run full workspace test + clippy + fmt

### Phase 6 — Documentation

- [ ] Update `CHANGELOG.md` `[Unreleased]` section with the
  Epic C entry following the same shape as the v0.4.0
  entries (one bullet per sub-component, source citations,
  effort retrospective)
- [ ] Inline doc on every new public type + module-level
  doc on the two new `mm-risk::*` modules
- [ ] Update `CLAUDE.md` Architecture section: add
  `hedge_optimizer` and `var_guard` to the risk crate
  module list, add per-factor delta + stress-test CLI to
  the dashboard / backtester sections
- [ ] Update `ROADMAP.md` Epic C section: mark as DONE,
  list any items that slipped to a stage-2 follow-up
- [ ] Update memory: add a closing entry for Epic C in
  `epic_production_spot_mm_gap_closure.md` style or extend
  `reference_sota_research.md` with the closed status
- [ ] Update CLAUDE.md stats line to reflect the new
  workspace state

### Deliverables

- `mm-stress-test` binary running against all five scenarios
- One integration test exercising the full pipeline through
  one scenario
- CHANGELOG, CLAUDE.md, ROADMAP.md, memory all updated
- Workspace `cargo test + clippy + fmt` green

### Files touched

- `crates/backtester/src/bin/mm_stress_test.rs` (new)
- `crates/backtester/Cargo.toml` (binary entry)
- `crates/engine/tests/integration.rs`
- `CHANGELOG.md`
- `CLAUDE.md`
- `ROADMAP.md`
- `~/.claude/.../memory/*` (epic closure pointer)

### DoD

- `mm-stress-test --scenario=usdc-depeg` runs cleanly and
  emits a non-trivial report
- Workspace test + clippy + fmt all green
- CHANGELOG entry passes the same "honest about deferrals"
  review the v0.4.0 entries did
- One commit lands the whole epic per
  `feedback_commit_discipline.md`

---

## Definition of done — whole epic

- All 5 sub-components shipped or explicitly deferred to a
  stage-2 follow-up tracked in `ROADMAP.md`
- All tests green, clippy `-D warnings` clean, fmt clean
- Single commit lands the epic per the user's commit
  discipline
- CHANGELOG, CLAUDE.md, ROADMAP, memory all updated
- `mm-stress-test` runnable on every scenario without manual
  setup beyond the JSONL data being in `data/stress/`
- Operator-facing dashboard exposes the new
  `mm_portfolio_delta` gauges and the daily report shows
  per-strategy + per-factor breakdown

## Risks and open questions

- **Per-strategy attribution scope.** Each engine today owns
  one strategy, so per-engine ≈ per-strategy. But the
  funding-arb driver runs *inside* an engine that quotes
  basis on the same leg — its PnL is currently commingled.
  If we want a clean stat-arb PnL view in Epic B, we may
  need to split that. Decide in Sprint C-1.
- **Hedge optimizer LP solver.** Cartea ch.6 is a closed
  form *for the unconstrained case*. With per-asset position
  caps and funding-cost constraints, we either project the
  closed-form solution onto the feasible set (cheap, slightly
  suboptimal) or pull in a small LP crate (
  `good_lp` / `clarabel` / `microlp`). Recommend projection
  for v1; LP for stage-2 only if the projection version
  gives bad results.
- **Stress data sourcing.** If the local data set does not
  cover all five scenarios, we either fetch from Tardis (paid
  but cheap), generate synthetic event streams that mimic the
  shock profile, or ship the epic with three of five scenarios
  and tracker the missing two as a stage-2 follow-up. Decide
  in Sprint C-2.
- **VaR guard interaction with kill switch.** Both can
  throttle size. Need to define the precedence: VaR throttle
  composes multiplicatively with the kill switch multiplier
  (so a 0.5 VaR throttle on top of a 0.5 kill-switch widen
  → 0.25 effective size), or the engine takes the max
  (most-restrictive wins). Recommend max-restrictive for
  safety; pin in Sprint C-1.

## Sprint C-1 audit findings (Portfolio + PnlTracker)

### `mm-portfolio::Portfolio` — current state

**Architecture.** Plain struct, single `Arc<Mutex<>>` shared
across every per-symbol engine in the deployment. No async,
no clock, drives off the engine's main tick.

**Fields:**
- `reporting_currency: String` (e.g. `"USDT"`)
- `positions: HashMap<String, Position>` — keyed by **symbol**
  (`"BTCUSDT"`, `"ETHBTC"`), not by base asset
- `marks_native: HashMap<String, Decimal>` — per-symbol mark
  in the symbol's own native quote currency
- `fx_to_reporting: HashMap<String, Decimal>` — per-symbol FX
  factor (1 unit native quote → N units reporting), defaults
  to `1` for already-reporting-currency-quoted pairs

**Position struct:** `qty` (signed), `avg_entry`,
`realised_pnl_native`. Weighted-average cost basis with the
canonical long-flip-cover semantics. 9 unit tests pinning
the position arithmetic — solid foundation, no rewrites
needed.

**Snapshot path:** iterates `positions`, computes per-symbol
unrealised native, applies FX, sums `total_realised` and
`total_unrealised`, returns `PortfolioSnapshot { reporting_currency,
total_equity, total_realised_pnl, total_unrealised_pnl,
per_asset: HashMap<String, AssetSnapshot> }`. Per-asset map
is also keyed by symbol, not by base asset.

### `mm-risk::PnlTracker` — current state

**Architecture.** One per-engine PnL attribution tracker (NOT
per-portfolio). Owns its own `inventory: Decimal` (single
base asset) and `last_mid` for incremental MTM. Exposes the
attribution breakdown {`spread_pnl`, `inventory_pnl`,
`rebate_income`, `fees_paid`, `round_trips`, `total_volume`}.
Hot-swap fee rates via `set_fee_rates` (P1.2). No knowledge
of the Portfolio aggregation layer.

**Important:** PnlTracker's `inventory` is a **scalar** that
double-tracks the engine's `InventoryManager.inventory()` —
two different paths are running in parallel, both get
updated by `on_fill`. Not a bug today, but worth knowing
that the Portfolio aggregation will need a third path
(per-asset rather than per-engine).

### Gap analysis vs Epic C sub-components

#### Sub-component #1 — Per-factor delta aggregation

**The hard part nobody's thinking about yet:** the current
keying is *per-symbol*, not *per-base-asset*. For an MM that
quotes only USDT-quoted pairs (BTCUSDT, ETHUSDT, SOLUSDT)
this is a one-line fix — pull the base asset off each symbol
and aggregate. **For non-USDT-quoted pairs like ETHBTC the
math gets non-trivial:**

A long position of `1 ETH` on `ETHBTC` at `0.05` contributes:
- ETH-delta = `+1` (the base leg)
- BTC-delta = `-0.05` (the quote leg, an implicit short)

For the BTC-delta of an `ETHBTC` long to net correctly against
the BTC-delta of a `BTCUSDT` long, the portfolio aggregator
needs to know the base AND quote leg of every symbol. This
information is in `ProductSpec.base_asset` /
`ProductSpec.quote_asset`, but **`Portfolio` does not see
`ProductSpec` today** — `on_fill` only takes `(symbol, qty,
price)`.

**Decision pinned in Sprint C-1**: extend `Portfolio::on_fill`
to take `(symbol, base_asset, quote_asset, qty, price)` OR
add a separate `Portfolio::register_symbol(symbol, base, quote)`
seed call the engine makes once at startup (cleaner — fewer
arg changes per fill, no breaking the existing test surface
on the position arithmetic). **Recommend the seed call.**

Then per-factor delta is:
```text
factor_delta(asset) = Σ_{positions where base == asset} qty
                    + Σ_{positions where quote == asset} -qty * mark_native
```

**Effort estimate confirmed:** 1 week (was the research-doc
estimate). Mostly plumbing once the symbol → (base, quote)
seed is in place.

#### Sub-component #2 — Per-strategy PnL labeling

**Current state:** Each `MarketMakerEngine` owns one strategy
and pushes into the *same* `Portfolio` via the shared `Arc<Mutex<>>`.
The Portfolio has no concept of which engine / which strategy
class the fill came from — `on_fill(symbol, qty, price)`
loses that information immediately.

**The funding-arb commingling risk surfaced in Sprint C-1
open question #1:** when a basis engine ALSO drives a funding
arb leg via `FundingArbDriver`, both flows hit the same
Portfolio bucket and cannot be told apart in the snapshot.
This blocks Epic B (stat-arb needs its own clean PnL view).

**Decision pinned in Sprint C-1**: extend `on_fill` to
`on_fill(symbol, qty, price, strategy_class: &str)` and add
a second aggregation dimension:
`per_strategy: HashMap<String, AggregatedPnl>` alongside
`per_asset`. Strategy class is read from
`Strategy::name()` (already exists on the trait) at engine
startup — engines pass the static `&'static str` through to
every fill push.

For multi-strategy engines (basis + funding-arb concurrent)
the funding-arb driver pushes its own fills with
`"funding_arb"` while the basis engine pushes with
`"basis"`. Same Portfolio key (symbol stays the same) but
the per-strategy bucket keeps them separate.

**Effort estimate:** ~2 days for the labeling, +1 day for
the dashboard surface. Smaller than I assumed.

#### Sub-component #3 — Cross-asset hedge optimizer

**Current state:** zero. No `mm-risk::hedge_optimizer` module,
no Cartea ch.6 implementation, no LP solver dependency.

**Inputs the optimizer needs (every one already in the codebase):**
- Portfolio per-factor delta vector (sub-component #1)
- Live mid prices per hedge instrument (book_keeper.book.mid_price)
- Funding cost per perp instrument (`get_funding_rate` already
  in the connector trait)
- Per-asset position caps (config.risk.max_inventory)
- Per-instrument tick/lot rounding (`ProductSpec`)

**No new external dependency for v1**: the unconstrained
Cartea ch.6 closed form is a single matrix multiply
(quadratic minimisation of variance subject to a linear
hedge equation), and we can use `nalgebra` (which mm-strategy
already pulls indirectly via `rust_decimal`'s integer ops…
actually we don't, let me re-check). If `nalgebra` isn't
already in the workspace, the 2x2 / 3x3 cases this v1 needs
can be hand-rolled in pure `Decimal` math — Epic C does NOT
need a general LP solver.

**Decision pinned in Sprint C-1**: hand-rolled 2x2/3x3 closed
form for v1, defer LP solver (`good_lp` / `clarabel`) to
stage-2 if the projection version is too suboptimal.

#### Sub-component #4 — Per-strategy VaR guard

**Current state:** zero. We have `mm-risk::exposure::ExposureManager`
which does global drawdown tracking, but no per-strategy
rolling variance and no VaR ceiling.

**Inputs:** per-strategy PnL time series. Requires
sub-component #2 (per-strategy labeling) to be live first.

**Approach pinned**: parametric Gaussian VaR over a rolling
24h ring buffer per strategy class. Mean + variance from the
buffer, 95 %-VaR = `mean - 1.645·σ`, 99 %-VaR =
`mean - 2.326·σ`. On a breach: return a `[0, 1]` size
multiplier (`0.5` for 95 % breach, `0.0` for 99 %). The
existing autotune path already accepts multiplier inputs
from MarketResilience and InventoryGammaPolicy — VaR slots
in identically. **No new precedence rule needed for the
kill switch interaction**: take the **max-restrictive**
(`min(var_mult, kill_switch_mult, mr_mult, ...)`) per the
recommendation in the open-questions section — already the
shape the engine uses for the other multipliers.

**Effort:** ~1 week confirmed (~250 LoC + 8 tests).

#### Sub-component #5 — Stress replay library + CLI

**Current state of the backtester:**
- `crates/backtester/src/data` — JSONL event recorder/loader
  (writes/reads `MarketEvent` streams to `data/replays/*.jsonl`)
- `crates/backtester/src/simulator` — replays events through
  a strategy with the queue-aware fill model
- `crates/backtester/src/lookahead` — generic O(N²) prefix
  check for lookahead bias
- `crates/backtester/src/bin/mm_probe.rs` — existing CLI we
  ship (dev-time inspector)

**The replay primitive is in place.** What's missing is:
- The curated event JSONL for the five canonical crashes
- A wrapper binary `mm_stress_test.rs` that drives
  `Simulator` against a scenario directory and emits a
  standardised report
- The report shape itself (max DD, time-to-recovery,
  inventory peak, kill-switch trips, SLA breaches,
  VaR throttle activations, hedge basket recommendations
  along the path)

**Stress data sourcing decision** (open question #3):
- We do **not** currently have any of the five crashes in
  `data/replays/` — survey confirmed
- The Tardis API exposes `book_change` + `trade` channels for
  Binance Spot back to 2019; the LUNA / FTX / USDC depeg
  windows are all available via their `csv_download` endpoint
  - not free but reasonably cheap (~$50/mo for the
  resolution we need)
- The 2020 covid window pre-dates Tardis BTC/USDT spot
  coverage on some smaller venues; Binance Spot is fine
- 2021 China ban — Binance Spot covered

**Decision pinned**: target Binance Spot BTC/USDT for all 5
windows; download a 24h JSONL slice from Tardis for each;
ship 4 of 5 in the repo (compress with gzip, ~50MB total),
the 2020 covid one is bigger so it stays as a download
script + `.gitignore` entry. **If Tardis subscription is not
in place by Sprint C-2 start, we synthesize the stress
profile** (volatility shock + spread blowout + book
thinning) deterministically from a seed — strictly worse than
real data, but unblocks the stress-test CLI and the VaR
guard's behaviour can still be validated.

### Resolved open questions

| # | Question | Resolution |
|---|---|---|
| 1 | Per-strategy attribution scope incl. funding-arb commingling | Add `strategy_class` arg to `Portfolio::on_fill`. Funding-arb driver pushes with `"funding_arb"`, basis engine pushes with `"basis"`. Same symbol bucket, separate per-strategy bucket. |
| 2 | Hedge optimizer LP vs closed-form-projection | Hand-rolled 2x2/3x3 closed form in pure Decimal for v1. No new dependency. LP solver = stage-2 only if projection results are bad. |
| 3 | Stress data sourcing fallback | Tardis fetch for 4 of 5 scenarios (download script + checked-in JSONL); 2020 covid → ignore-listed download script. If Tardis blocked → deterministic synthesised stress profile as fallback. |
| 4 | VaR guard precedence vs kill switch | **Max-restrictive** = `min` of all multipliers. Same shape as MR / IGP / kill switch composition. No new rule. |

### Updated effort estimates after audit

| Sub-component | Original estimate | Audited estimate | Delta |
|---|---|---|---|
| #1 Per-factor delta | 1 week | **3-4 days** | smaller — the seed-call approach is cleaner |
| #2 Per-strategy labeling | 2 days | **2-3 days** | as expected |
| #3 Hedge optimizer | 2-3 weeks | **1 week** | smaller — closed-form, no LP dep |
| #4 VaR guard | 1 week | **1 week** | confirmed |
| #5 Stress replay + CLI | 1 week | **1 week** | confirmed (assuming Tardis access) |
| **Total dev** | ~6 weeks | **~4 weeks** | **fits the 4-sprint plan** |

### Updated public API sketches

```rust
// mm-portfolio
impl Portfolio {
    pub fn register_symbol(&mut self, symbol: &str, base: &str, quote: &str);
    pub fn on_fill(&mut self, symbol: &str, qty: Decimal, price: Decimal,
                   strategy_class: &'static str);
    pub fn factor_delta(&self, asset: &str) -> Decimal;
    pub fn factors(&self) -> Vec<(String, Decimal)>;
    pub fn per_strategy_pnl(&self) -> HashMap<&'static str, Decimal>;
}

// mm-risk::hedge_optimizer
pub struct HedgeOptimizer { funding_cost_bps: Decimal, max_basket_qty: Decimal }
impl HedgeOptimizer {
    pub fn new(funding_cost_bps: Decimal, max_basket_qty: Decimal) -> Self;
    pub fn optimize(&self,
        exposure: &[(String, Decimal)],
        hedge_universe: &[HedgeInstrument],
    ) -> HedgeBasket;
}

// mm-risk::var_guard
pub struct VarGuard { window_secs: u64, ring: HashMap<&'static str, Vec<(Instant, Decimal)>> }
impl VarGuard {
    pub fn new(window_secs: u64) -> Self;
    pub fn record_pnl_sample(&mut self, strategy_class: &'static str, pnl: Decimal);
    pub fn effective_throttle(&self, strategy_class: &'static str) -> Decimal;
}
```

### Cartea attribution correction — moved to its own doc

Full transcription of the hedge-optimizer and VaR formulas
with our variable names now lives at
`docs/research/hedge-optimizer-and-var-formulas.md`. Key
decisions pinned there:

- **The SOTA research doc's Cartea attribution was wrong.**
  Web-verified TOC (Cambridge Uni Press frontmatter [1]):
  Ch 6-8 are execution, Ch 10 is market making, Ch 11 is
  pairs trading. Our hedge optimizer is classic Markowitz
  1952 / Merton 1972 — NOT in Cartea. Our VaR is
  RiskMetrics standard parametric Gaussian — also NOT in
  Cartea.
- **Hedge optimizer v1 = diagonal β + diagonal Σ + L1
  funding shrinkage + hard cap.** Pure `Decimal` loop over
  K factors, no matrix ops, ~80 LoC. No `nalgebra`, no LP
  solver, no new dependencies.
- **VaR guard v1 = parametric Gaussian with frozen z-scores**
  (`1.645` for 95 %, `2.326` for 99 %). Ring buffer of
  1440 samples per strategy class (24 h × 60 s cadence).
  Throttle tiers 1.0 / 0.5 / 0.0 on 95 % / 99 % breaches.
  Composed with other multipliers via `min()` (max-restrictive).
- **10 test cases per component** pinned in the formula doc.

### Stress-event data survey — findings

**Existing state of the repo (verified by filesystem scan):**

- No `data/` directory at all. Neither `data/replays/` nor
  `data/stress/` exist. The backtester JSONL loader takes
  paths from config, no hardcoded locations.
- **No Tardis integration anywhere in the workspace.** No
  Tardis API client, no auth handling, no CSV ingester.
  Building one is a 2-3 day project in its own right.
- `crates/backtester/src/data.rs` ships a JSONL
  recorder/loader keyed to the `MarketEvent` enum shape.
  Any stress scenario data must conform to that schema.
- `crates/backtester/src/bin/mm_probe.rs` already exists
  as a CLI binary template — `mm-stress-test` can copy its
  Cargo.toml plumbing.

**Decision pinned (revises the Sprint C-1 draft):**

> **Sprint C-2 ships synthetic stress scenarios, not Tardis
> data.** Real historical replay of the five canonical
> crashes is deferred to a stage-2 follow-up that builds a
> proper Tardis ingestor crate. Stage-1 uses deterministic
> synthetic event streams that reproduce the *shock profile*
> (volatility spike + spread blowout + book thinning +
> one-sided flow) for each scenario seeded from a fixed
> seed.

**Why synthetic is the right call for v1:**

1. **Epic C's value proposition is the aggregation /
   throttling / reporting plumbing**, not the authenticity
   of the replay data. A synthetic -40 % move in 2 hours
   exercises the VaR guard → kill switch → hedge optimizer
   path exactly as a real LUNA replay would.
2. **Tardis integration is a sub-week undertaking**
   (auth, CSV schema conversion, rate limit handling, 50 MB+
   data in the repo or a fetch script) that blocks the Epic
   C dev sprints on pure data engineering.
3. **Deterministic synthetic streams are replay-stable by
   construction** — every test run produces identical
   results, no flakes on historical data drift.
4. **Real Tardis replay becomes a stage-2 follow-up epic**
   tracked in `ROADMAP.md` under Epic C. When it lands it
   drops into the same `mm-stress-test` CLI as a new
   `--scenario=luna-real` mode alongside the synthetic
   `--scenario=luna-synthetic`.

**Scenario shapes for the synthetic library (Sprint C-2):**

| Scenario | Shock profile | Duration | Primary asset |
|---|---|---|---|
| `covid-2020` | -50 % price in 24 h, 5× volume spike, spread blowout 30× | 24 h | BTC/USDT |
| `china-2021` | -30 % price in 6 h, 3× volume, one-sided sell flow | 6 h | BTC/USDT |
| `luna-2022` | -95 % price over 72 h, book thinning to 10 % of baseline | 72 h | LUNA-like synthetic pair |
| `ftx-2022` | -25 % price in 2 h, liquidity withdrawal 80 %, funding flip | 2 h | BTC/USDT |
| `usdc-depeg-2023` | 8 % stablecoin depeg in 3 h, recovery in 48 h | 48 h | USDC/USDT |

Each scenario is generated by a deterministic function with
a seed — no external data dependency, no gigabyte JSONL in
the repo. The mm-stress-test CLI accepts
`--scenario={slug}` and calls the generator directly.

### Pin per-sub-component DoD (task #59)

| Sub | DoD (v1 shipping bar) |
|---|---|
| **#1 Per-factor delta** | (a) `Portfolio::register_symbol(symbol, base, quote)` seed call exists and is called by every engine at startup; (b) `factor_delta(asset) -> Decimal` and `factors() -> Vec<(String, Decimal)>` accessors return correct aggregated values for both USDT-quoted and cross-quoted pairs; (c) new Prometheus gauge `mm_portfolio_delta{asset}`; (d) new dashboard daily-report field `factors`; (e) ≥7 unit tests: single asset, multi-asset, signed cancellation, dust pruning, cross-quote (ETHBTC contributing to BTC-delta via quote leg), unknown symbol does not panic, register_symbol overwrites idempotently. |
| **#2 Per-strategy PnL labeling** | (a) `strategy_class: &'static str` threaded through `Portfolio::on_fill`; (b) `per_strategy_pnl() -> HashMap<&'static str, Decimal>` accessor; (c) funding-arb driver pushes with `"funding_arb"` while the basis engine pushes with `"basis"`, not commingled; (d) new dashboard field `per_strategy: Vec<{class, pnl}>`; (e) ≥4 tests: single class, multi-class isolation, funding+basis non-commingling, unknown class treated as its own bucket. |
| **#3 Hedge optimizer** | (a) New `mm-risk::hedge_optimizer` module with the v1 diagonal-β closed form from the reference doc; (b) pure Decimal math, no new dependencies; (c) new audit `HedgeBasketRecommended` event; (d) ≥10 unit tests matching the test matrix in the reference doc including a property-based hedge-never-exceeds-cap test; (e) engine accessor `recommend_hedge_basket()` that the dashboard can poll. |
| **#4 Per-strategy VaR guard** | (a) New `mm-risk::var_guard` module with the parametric Gaussian formula from the reference doc; (b) 60-second sample cadence driven from the `sla_interval` arm; (c) 1440-entry ring buffer per strategy class; (d) frozen z-score constants 1.645 / 2.326; (e) `effective_throttle()` composed into the engine via `min()` next to MR / IGP / kill-switch; (f) new audit `VarGuardThrottleApplied` event; (g) ≥8 tests matching the test matrix. |
| **#5 Stress replay library + CLI** | (a) New `mm-backtester::stress` module with 5 deterministic synthetic-scenario generators; (b) `mm_stress_test.rs` binary with `--scenario=<slug>` and `--all` flags; (c) standardised markdown report: max DD, time-to-recovery, inventory peak, kill-switch trips, VaR throttle activations, hedge basket path, SLA breaches; (d) `--output report.md` option; (e) one engine-integration test that runs usdc-depeg end to end. |

### Dependency graph (pinned)

```
#2 (strategy labeling)  ──┐
                           ├──► #4 (VaR guard)  ──┐
#1 (per-factor delta)    ──┤                       │
                           ├──► #3 (hedge opt)   ──┤
                           │                       │
                           │                       ▼
                           └──────────────────►  #5 (stress CLI) → integration test
```

- #1 and #2 are independent and ship first in Sprint C-2
- #3 depends on #1 (reads per-factor delta)
- #4 depends on #2 (reads per-strategy PnL)
- #5 depends on #3 + #4 (drives both through the scenarios)

### Sprint C-1 status — FINAL

| Task | Status | Deliverable |
|---|---|---|
| Audit Portfolio + PnlTracker | ✅ done | Audit section above (~270 lines) |
| Cartea ch.6 transcription | ✅ done (with correction) | `docs/research/hedge-optimizer-and-var-formulas.md` |
| Cartea ch.7 VaR review | ✅ done (with correction) | Same doc, VaR section |
| Stress data survey | ✅ done | Synthetic-scenarios decision above |
| Pin per-sub-component design + DoD | ✅ done | Table above |
| Resolve open questions | ✅ done | All 4 answered in audit section |

**Sprint C-1 DoD met.** Zero code written — design-only pass
as the sprint cadence rules require. Every dev sprint (C-2,
C-3, C-4) can now execute without further open-question
rounds.

---

---

## Sprint cadence rules

- **One week per sprint.** Friday end-of-day = sprint review,
  Monday morning = next sprint kickoff.
- **No code in Sprint C-1.** Planning + study only. The
  no-code rule keeps the design pass honest — we don't get
  to "I'll figure it out while coding".
- **Working tree stays uncommitted across all 4 sprints**
  per `feedback_commit_discipline.md`. One commit at the
  end of Sprint C-4.
- **Sprint review checklist** at the end of every sprint:
  did we close every checkbox in the phase, are the DoDs
  met, are there new risks for the next sprint?
