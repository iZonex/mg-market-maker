# Epic B — Stat-arb / cointegrated pairs

> Sprint plan for the third epic in the SOTA gap closure
> sequence (C → A → **B** → D → E → F). Epic C shipped the
> per-strategy PnL attribution that this epic depends on;
> Epic A's cross-venue SOR is available as an execution
> primitive but optional for v1.

## Why this epic

Cointegrated-pair stat-arb is the single largest **strategy
family** every production prop desk runs that we
structurally lack. Hummingbot ships a screening UI for it,
GSR publishes research on BTC/ETH and stablecoin pairs as
their bread-and-butter trade. Our `BasisStrategy` covers
*one* form (same-asset cash-vs-future basis) but not the
general cointegrated-pair case where the two legs are
**different assets**.

The gap is fundamentally architectural: every existing
`Strategy` trait impl sees exactly one symbol via
`StrategyContext`. Stat-arb wants two symbols, a spread
between them, and a Kalman-tracked hedge ratio that
adapts to regime. It does not fit into a single-symbol
quoter — it needs its own runner that subscribes to two
order books in parallel and dispatches across both legs
when the spread z-score crosses an entry threshold.

## Scope (4 sub-components)

| # | Component | New module | Why |
|---|---|---|---|
| 1 | Cointegration tester | `mm-strategy::stat_arb::cointegration` | Engle-Granger pair test on a rolling 30-day window — pure stats, no IO. v1 ships a 2-leg test; multivariate Johansen is stage-2 |
| 2 | Kalman filter for hedge ratio | `mm-strategy::stat_arb::kalman` | Adaptive β that updates on every mid-price tick, regime-resilient. Pure linear-Gaussian state-space, ~80 LoC |
| 3 | Z-score signal + entry/exit policy | `mm-strategy::stat_arb::signal` | Rolling residual mean + std, z-score, hysteresis-aware entry/exit thresholds |
| 4 | `StatArbDriver` + engine integration | `mm-strategy::stat_arb::driver` + engine hook | Composes #1+#2+#3, subscribes to two book feeds, dispatches via the existing `ExecAlgorithm` trait, threads PnL into the per-strategy bucket Epic C delivered |

## Pre-conditions

- ✅ Epic C per-strategy PnL labeling — stat-arb gets its
  own PnL bucket via `Strategy::name() == "stat_arb"`
- ✅ `ExecAlgorithm` trait (TWAP/VWAP/POV/Iceberg) for leg
  execution
- ✅ `ConnectorBundle` extension from Epic A — a stat-arb
  driver running across two venues fits the
  `primary + extra` shape
- ✅ `BasisStrategy::expected_cross_edge_bps` from v0.2.0
  Sprint H — same shape as the leg-cost computation we
  want for stat-arb v1 (reused as a building block)

## Total effort

**4 sprints × 1 week = 4 weeks** matching Epic C / A
cadence:

- **B-1** Planning + Study (no code) — design notes,
  audit existing FundingArbDriver pattern, pin
  Engle-Granger vs Johansen, pin Kalman filter math,
  resolve open questions
- **B-2** Cointegration tester + Kalman filter (sub-
  components #1 + #2)
- **B-3** Z-score signal + StatArbDriver (sub-components
  #3 + #4 partial)
- **B-4** Driver-engine wiring + audit + CHANGELOG +
  ROADMAP + memory + commit

---

## Sprint B-1 — Planning + Study (week 1)

**Goal.** Pin every implementation decision before code
lands. End the sprint with a per-sub-component design note
plus a resolved open-question list.

### Phase 1 — Planning

- [ ] Walk every relevant existing primitive (`Strategy`
  trait, `FundingArbDriver`, `BasisStrategy`,
  `ExecAlgorithm`, `ConnectorBundle`, `Portfolio`,
  per-strategy PnL labeling) and write a field-by-field
  delta of what stat-arb needs from each
- [ ] Pin the public API for the four sub-components
- [ ] Define DoD per sub-component
- [ ] Decide the call-site shape: standalone driver
  (FundingArbDriver pattern) vs new `Strategy` trait impl
  vs new top-level engine

### Phase 2 — Study

- [ ] Audit `mm-strategy::funding_arb_driver` — the
  driver pattern is the closest analog and most of the
  scaffolding (tick loop, audit hooks, sink trait)
  carries over
- [ ] Audit `mm-strategy::basis::expected_cross_edge_bps`
  — same shape we want for the leg cost computation
- [ ] Audit `mm-strategy::exec_algo` — which executors
  exist (TWAP/VWAP/POV/Iceberg), what their interface
  looks like, how the engine drives them today
- [ ] Re-read the Engle-Granger 1987 cointegration paper
  and the Cartea-Jaimungal-Penalva chapter 11 pairs
  trading section (the SOTA research doc's "Cartea ch.6
  hedge optimizer" was a misattribution; ch.11 is the
  actually-correct pairs reference per the verified TOC)
- [ ] Read Hummingbot `cross_exchange_market_making` +
  pairs-trading template for the operator-config shape
- [ ] Pin Kalman filter formulation (state-space, observation
  equation, transition matrix, observation matrix, state
  covariance, observation covariance)

### Open questions to resolve

1. **Pair selection scope.** v1 should be config-driven
   (operator picks BTC/ETH by hand) or auto-screened
   (background pair screener runs Engle-Granger on a basket
   of candidate pairs)? **Default: config-driven.**
   Auto-screen is operationally heavy and adds a background
   task that does not pay for itself until we have many
   pairs. v1 ships the screener as a CLI helper
   (`mm-pair-screen --pairs="BTC/USDT,ETH/USDT,SOL/USDT"
   --window=30d`) the operator runs offline.
2. **Cointegration test choice.** Engle-Granger vs Johansen?
   **Default: Engle-Granger.** Johansen is the multivariate
   generalisation that matters when you have 3+ assets in
   the cointegration vector. Every v1 pair we care about is
   strictly 2-leg, where Engle-Granger is the simpler
   correct choice. Cartea ch.11 derives both; v1 uses the
   2-leg simplification.
3. **Hedge ratio estimation method.** Static OLS vs Kalman
   filter? **Default: Kalman filter.** OLS gives a single β
   over the whole window; Kalman gives an adaptive β that
   drifts with the regime. For crypto pairs (where regime
   shifts are frequent) the adaptive estimate is meaningful.
   Pure linear-Gaussian Kalman, ~80 LoC of `Decimal` math.
4. **Driver call-site shape.** Standalone tokio task
   spawned by the server (FundingArbDriver pattern) vs
   embedded inside one `MarketMakerEngine` (Strategy trait
   impl pattern)? **Default: standalone driver.** The
   `Strategy` trait sees one symbol per call site, but
   stat-arb intrinsically needs two book references at
   once. Embedding it would require extending the trait.
   The driver pattern keeps the `Strategy` trait stable
   and inherits the audit / kill-switch / shutdown story
   from `FundingArbDriver`.
5. **Leg execution.** Direct connector calls vs `ExecAlgorithm`
   trait (TWAP/VWAP/POV) vs new `StatArbExecutor`?
   **Default: TWAP via the existing `ExecAlgorithm`
   trait** for the entry leg, market dispatch for the exit
   leg. The entry side wants to minimise market impact
   (TWAP fits), the exit side wants speed (market take).
6. **PnL attribution scope.** Does stat-arb book its PnL
   into a single `"stat_arb"` strategy class or per-pair
   (`"stat_arb_BTC_ETH"`)? **Default: per-pair.** The
   per-strategy PnL bucket from Epic C accepts arbitrary
   string keys, so emitting `stat_arb_BTC_ETH` as a class
   gives operators independent visibility on each pair
   without changing the Portfolio API.

### Deliverables

- Audit findings inline at the bottom of this sprint doc
  (same shape as Epic C / Epic A Sprint 1 audit sections)
- Per-sub-component public API sketch
- All 6 open questions resolved with defaults or explicit
  "decide in Sprint B-2"

### DoD

- Every sub-component has a public API sketch, a tests
  list, and a "files touched" estimate
- The next 3 sprints can execute without further open
  rounds

### Audit findings — existing primitives we reuse

Phase-2 audit done against the live tree on 2026-04-15.
Line counts are from `wc -l` at the time of the audit.

#### A — `FundingArbDriver` (810 LoC, closest analog)

File: `crates/strategy/src/funding_arb_driver.rs`

The driver pattern for stat-arb should be a structural
copy of this file with three swaps:

1. `FundingArbExecutor` → direct per-leg dispatch via
   `OrderManager::execute_unwind_slice` (funding arb uses
   an atomic pair executor because it cares about taker
   latency symmetry; stat-arb's entry leg is TWAP-paced
   so the atomicity guarantee is neither needed nor free).
2. `FundingArbEngine` → `StatArbDecisionCore` composing
   `EngleGrangerTest`, `KalmanHedgeRatio`, `ZScoreSignal`.
3. `DriverEvent` → `StatArbEvent` (same shape: variants
   for Entered / Exited / Hold / NotCointegrated / Warmup /
   InputUnavailable).

Transferred wholesale (no changes):

- `DriverEventSink` trait — same shape, engine-side sink
  adapter routes into audit + portfolio PnL
- `NullSink` for tests
- `run(shutdown_rx: watch::Receiver<bool>)` tick loop —
  literally copy-paste with `FundingArb` → `StatArb`
- Tick-interval configurable via `FundingArbDriverConfig.tick_interval`
  (default 60 s) — stat-arb wants this too
- `on_primary_fill` / `on_hedge_fill` accounting hooks —
  stat-arb wants `on_y_fill` / `on_x_fill` of the same shape
- Shutdown semantics: tick loop exits on `shutdown_rx` changed

Divergences stat-arb introduces:

- **Two symbols, not one pair**. Funding arb has a single
  `InstrumentPair { primary_symbol, hedge_symbol, multiplier }`
  but both legs are the same underlying asset. Stat-arb
  needs a `StatArbPair { y_symbol, x_symbol, y_venue,
  x_venue }` where the two legs are different assets,
  possibly on different venues.
- **No `get_funding_rate` dependency**. Stat-arb's inputs
  are just two mid prices — the driver fetches them from
  the respective connectors via `get_best_quote` (same
  call funding arb uses in `sample_mids`).
- **Cointegration gate before signal eval**. Funding arb
  has no analog — every tick evaluates the signal. Stat-
  arb re-runs Engle-Granger on a slow cadence (default
  every 60 min, configurable) and caches the result; the
  tick-loop signal eval is gated on the cached bool.

**Verdict**: the driver-pattern call-site shape is a
structural match. The `Strategy` trait impl shape does
NOT fit — stat-arb needs two book references per call,
and the trait is locked at single-symbol.

#### B — `ExecAlgorithm` trait (673 LoC)

File: `crates/strategy/src/exec_algo.rs`

Four impls exist: `TwapAlgo`, `VwapAlgo`, `PovAlgo`,
`IcebergAlgo`. The trait shape:

```rust
pub trait ExecAlgorithm: Send {
    fn on_fill(&mut self, client_order_id: OrderId, price: Decimal, qty: Decimal);
    fn tick(&mut self, ctx: ExecContext) -> Vec<ExecAction>;
    fn filled(&self) -> Decimal;
    fn remaining(&self) -> Decimal;
    fn is_finished(&self) -> bool;
}
```

`ExecAction` is `Place / Cancel / Hold / Done`.
`ExecContext` is `{ now, best_bid, best_ask, recent_volume,
lot_size }` — all we need for stat-arb leg execution.

**Entry leg (TWAP)**: `TwapAlgo::new(TwapConfig { side,
total_qty, duration, num_slices }, lot_size)` is a direct
fit. Stat-arb creates two TwapAlgos (one per leg) on
entry, drives them on each tick until both return `Done`,
and the driver state machine advances to `Open`.

**Exit leg (market take)**: no `MarketTakeAlgo` exists —
the cleanest fit is a direct `OrderManager::execute_unwind_slice`
call with taker flag, same as `PairedUnwindExecutor` uses
for kill-switch L4. Adding a new `MarketTakeAlgo` impl is
gold-plating — a single-shot call is <5 LoC at the driver
call-site.

**Verdict**: TWAP entry + direct market take on exit is
the right split. No new `ExecAlgorithm` impl needed for
v1.

#### C — `BasisStrategy::expected_cross_edge_bps` (755 LoC)

File: `crates/strategy/src/basis.rs`, line 145.

Takes `(maker_side, maker_price, size, hedge_book: &LocalOrderBook)`
and returns `Option<Decimal>` — the expected edge in bps
after walking the hedge book to close out `size`. Uses
`market_impact()` from `strategy::features::market_impact`
and returns `None` if the hedge book can't absorb the
full size (`impact.partial == true`).

This is the exact shape stat-arb wants for leg-cost
estimation in `size_legs(beta)`. The driver wants to
answer: "If I enter long Y / short β·X, what is the
slippage-adjusted round-trip cost in bps?" The existing
function computes one direction; stat-arb would call it
twice (once per leg) and sum.

**Verdict**: reuse `market_impact()` directly. Do not
wrap `expected_cross_edge_bps` — it is too basis-specific
(single underlying, same asset both sides). Stat-arb's
`StatArbCostEstimator` is a clean thin new function next
to `ZScoreSignal::decide` that calls `market_impact` on
both books.

#### D — Per-strategy PnL bucket (Epic C #2)

Files: `crates/portfolio/src/lib.rs`, `crates/risk/src/audit.rs`

`Portfolio::on_fill(symbol, qty, price, strategy_class: &str)`
accepts arbitrary string keys. Epic C landed this with
funding arb as the first non-trivial consumer. Stat-arb
reuses it directly with `strategy_class = "stat_arb_{pair}"`
(e.g. `"stat_arb_BTCUSDT_ETHUSDT"`).

**Verdict**: no Portfolio API changes needed. The per-
pair granularity is a pure call-site choice at the
driver's fill-routing sink.

### Open questions — resolved

All six open questions resolved against the defaults from
the sprint plan:

1. **Pair selection scope** → ✅ config-driven. Operator
   configures the pair(s) in `stat_arb_pairs` TOML
   section. Screener shipped as an offline CLI only
   (`mm-pair-screen`) — no background screener task in v1.
2. **Cointegration test choice** → ✅ Engle-Granger
   (2-leg). Johansen is stage-2 when / if 3+ asset
   cointegration vectors appear in the research pipeline.
3. **Hedge ratio estimation** → ✅ Kalman filter. OLS
   gives the *initial* β fed into `KalmanHedgeRatio::new`
   via the Engle-Granger result, then the Kalman adapts
   per tick. Q and R are operator-tuned per pair; defaults
   `Q=1e-6, R=1e-3`.
4. **Driver call-site shape** → ✅ standalone driver,
   FundingArbDriver pattern. The `Strategy` trait stays
   single-symbol.
5. **Leg execution** → ✅ TWAP entry via existing
   `ExecAlgorithm::TwapAlgo`, market-take exit via direct
   `OrderManager::execute_unwind_slice`. No new
   `ExecAlgorithm` impl.
6. **PnL attribution scope** → ✅ per-pair. Class key
   format `"stat_arb_{Y_SYMBOL}_{X_SYMBOL}"`, e.g.
   `"stat_arb_BTCUSDT_ETHUSDT"`.

### Per-sub-component API surface — pinned

Full formulas live in
`docs/research/stat-arb-pairs-formulas.md`. API types
pinned below.

#### #1 Cointegration — `mm_strategy::stat_arb::cointegration`

```rust
pub struct EngleGrangerTest;

pub struct CointegrationResult {
    pub is_cointegrated: bool,
    pub beta: Decimal,
    pub alpha: Decimal,
    pub adf_statistic: Decimal,
    pub critical_value_5pct: Decimal,
    pub sample_size: usize,
}

impl EngleGrangerTest {
    pub fn run(y: &[Decimal], x: &[Decimal]) -> Option<CointegrationResult>;
}
```

Files touched: `crates/strategy/src/stat_arb/mod.rs`
(new), `crates/strategy/src/stat_arb/cointegration.rs`
(new, ~250 LoC), `crates/strategy/src/lib.rs` (pub mod
export).

Tests list (≥10):
- empty / mismatched-length inputs return `None`
- n < `MIN_SAMPLES_FOR_TEST` returns `None`
- perfectly cointegrated synthetic pair → `is_cointegrated = true`
- independent random walks → `is_cointegrated = false`
- known-β synthetic recovers β within ε
- intercept-α recovery on non-zero-mean series
- ADF statistic sign correct on mean-reverting residuals
- MacKinnon critical-value table interpolation at `n=75`
- clamp at table extremes (`n=10` and `n=1000`)
- deterministic output across repeated calls

#### #2 Kalman — `mm_strategy::stat_arb::kalman`

```rust
pub struct KalmanHedgeRatio { /* private */ }

impl KalmanHedgeRatio {
    pub fn new(transition_var: Decimal, observation_var: Decimal) -> Self;
    pub fn with_initial_beta(beta: Decimal, transition_var: Decimal, observation_var: Decimal) -> Self;
    pub fn update(&mut self, y: Decimal, x: Decimal) -> Decimal;
    pub fn current_beta(&self) -> Decimal;
    pub fn current_variance(&self) -> Decimal;
}
```

Files touched: `crates/strategy/src/stat_arb/kalman.rs`
(new, ~150 LoC).

Tests list (≥10):
- stationary pair converges to the true β within 50 steps
- regime shift (β jumps mid-stream) is tracked
- tiny Q (1e-12) → β barely adapts
- large Q (1e-2) → β chases noise
- `x = 0` degenerate guard returns prior β
- variance decreases monotonically on a stationary pair
- `with_initial_beta` seeds correctly
- `current_*` accessors return the latest post-update state
- `update` is idempotent if called with the same `(y,x)` ≥ 2 times (variance shrinks, β stable)
- serde round-trip (if we derive Serialize for snapshot persistence)

#### #3 Z-score signal — `mm_strategy::stat_arb::signal`

```rust
pub struct ZScoreSignal { /* private */ }
pub struct ZScoreConfig {
    pub window: usize,
    pub entry_threshold: Decimal,  // default 2.0
    pub exit_threshold: Decimal,   // default 0.5
}

pub enum SignalAction {
    Open { z: Decimal, direction: SpreadDirection },
    Close { z: Decimal },
    Hold { z: Decimal },
}

pub enum SpreadDirection { SellY, BuyY }

impl ZScoreSignal {
    pub fn new(config: ZScoreConfig) -> Self;
    pub fn update(&mut self, spread: Decimal) -> Option<Decimal>;
    pub fn decide(&self, z: Decimal, in_position: bool) -> SignalAction;
    pub fn sample_count(&self) -> usize;
    pub fn window(&self) -> usize;
}
```

Files touched:
`crates/strategy/src/stat_arb/signal.rs` (new, ~220 LoC).

Tests list (≥10):
- warmup returns `None` until `window` samples seen
- z-score matches a reference scalar computation on a
  fixed fixture
- rolling eviction: after `window+1` samples, the front
  sample's contribution has decayed
- `z > entry` with `!in_position` → `Open { SellY }`
- `z < -entry` with `!in_position` → `Open { BuyY }`
- `|z| < entry && !in_position` → `Hold`
- `|z| < exit && in_position` → `Close`
- `exit < |z| < entry && in_position` → `Hold` (hysteresis)
- zero-variance window returns `None` (degenerate guard)
- Welford's numerical stability: ≥10k updates, variance
  stays finite and within ε of the naive recomputation

#### #4 `StatArbDriver` — `mm_strategy::stat_arb::driver`

```rust
pub struct StatArbPair {
    pub y_symbol: String,
    pub x_symbol: String,
    pub y_venue: String,
    pub x_venue: String,
    pub strategy_class: String, // e.g. "stat_arb_BTCUSDT_ETHUSDT"
}

pub struct StatArbDriverConfig {
    pub tick_interval: Duration,              // default 60s
    pub cointegration_recheck_interval: Duration, // default 60min
    pub zscore: ZScoreConfig,
    pub kalman_transition_var: Decimal,       // default 1e-6
    pub kalman_observation_var: Decimal,      // default 1e-3
    pub leg_notional_usd: Decimal,            // per-leg sizing
    pub entry_twap_duration: Duration,        // default 5min
    pub entry_twap_slices: usize,             // default 10
}

pub enum StatArbEvent {
    Entered { direction: SpreadDirection, y_qty: Decimal, x_qty: Decimal, z: Decimal },
    Exited { z: Decimal, realised_pnl_estimate: Decimal },
    Hold { z: Decimal },
    NotCointegrated { adf_stat: Decimal },
    Warmup { samples: usize, required: usize },
    InputUnavailable { reason: String },
}

pub struct StatArbDriver { /* private */ }

impl StatArbDriver {
    pub fn new(
        y_connector: Arc<dyn ExchangeConnector>,
        x_connector: Arc<dyn ExchangeConnector>,
        pair: StatArbPair,
        config: StatArbDriverConfig,
        sink: Arc<dyn DriverEventSink>,  // reuses funding_arb_driver trait
    ) -> Self;

    pub async fn tick_once(&mut self) -> StatArbEvent;
    pub async fn run(self, shutdown_rx: watch::Receiver<bool>);
    pub fn state(&self) -> &StatArbState;
}
```

Files touched: `crates/strategy/src/stat_arb/driver.rs`
(new, ~500 LoC), `crates/strategy/src/funding_arb_driver.rs`
(promote `DriverEventSink` + `NullSink` to a shared
`driver_common` module, re-export from both — one-line
each side), `crates/strategy/src/lib.rs`.

Tests list (≥8):
- synthetic cointegrated pair round trip: warmup → Hold
  → Entered → Hold → Exited
- `NotCointegrated` fires when Engle-Granger rejects
- cointegration gate recheck cadence — the ADF is NOT
  re-run on every tick, only every `cointegration_recheck_interval`
- `InputUnavailable` fires when either book is empty
- `Warmup` fires until the z-score window fills
- kill-switch shutdown: `run` exits cleanly on
  `shutdown_rx = true`
- audit-sink routing: `Entered` and `Exited` events both
  fire `StatArbEntered` / `StatArbExited` audit records
- per-pair PnL bucket: fills route through Portfolio with
  the correct `"stat_arb_{pair}"` class

### Per-sub-component DoD — pinned

| # | Component | Files | LoC (est) | Tests | DoD |
|---|---|---|---|---|---|
| 1 | `cointegration` | `stat_arb/cointegration.rs` | ~250 | ≥10 | Pure function, no IO, ≥10 unit tests, clippy/fmt clean |
| 2 | `kalman` | `stat_arb/kalman.rs` | ~150 | ≥10 | Pure struct, `update`/`current_beta`/`current_variance` public, ≥10 tests incl. regime-shift, degenerate `x=0` guard |
| 3 | `signal` | `stat_arb/signal.rs` | ~220 | ≥10 | Welford rolling window, hysteresis bands, `SignalAction` enum, warmup → `None` |
| 4 | `driver` | `stat_arb/driver.rs` (+ `driver_common` extraction) | ~500 | ≥8 | Standalone tokio task, `DriverEventSink` shared with funding arb, per-pair PnL routing, end-to-end synthetic round-trip test |

**Epic total**: ~1120 LoC of new code across four files,
≥38 unit tests, plus 1 end-to-end integration test in
Sprint B-4.

---

## Sprint B-2 — Cointegration tester + Kalman filter (week 2)

**Goal.** Land sub-components **#1** and **#2** as the pure
stats layer the driver will consume.

### Phase 3 — Collection

- [ ] Build a synthetic cointegrated pair generator for
  unit tests (two random walks with a known mean-reverting
  spread, parameterised by half-life and noise level)
- [ ] Pull a small fixture of historical BTC/ETH mid-price
  pairs from the existing JSONL recorder fixtures (or
  generate a deterministic fake one if the recorder
  doesn't have BTC + ETH simultaneously)

### Phase 4a — Dev

- [ ] **Sub-component #1** — `mm-strategy::stat_arb::cointegration`:
  - `EngleGrangerTest::run(&[Decimal], &[Decimal]) -> CointegrationResult`
  - `CointegrationResult { is_cointegrated, beta, residual_adf_stat, p_value }`
  - Pure function — no IO, no async, no allocations
    beyond the result
- [ ] **Sub-component #2** — `mm-strategy::stat_arb::kalman`:
  - `KalmanHedgeRatio` struct holding `(beta, beta_var, obs_var, transition_var)`
  - `update(y: Decimal, x: Decimal) -> Decimal` — returns
    the new β estimate after one observation
  - `current_beta()` accessor
  - Pure linear-Gaussian state-space, no external deps
- [ ] 10+ unit tests on each

### Deliverables

- `crates/strategy/src/stat_arb/mod.rs` (new module root)
- `crates/strategy/src/stat_arb/cointegration.rs`
- `crates/strategy/src/stat_arb/kalman.rs`
- ≥10 tests on each
- Workspace test + fmt + clippy green

---

## Sprint B-3 — Z-score signal + StatArbDriver (week 3)

**Goal.** Land sub-components **#3 (signal)** and the
**#4 driver** scaffolding.

### Phase 4b — Dev

- [ ] **Sub-component #3** — `mm-strategy::stat_arb::signal`:
  - `ZScoreSignal { window, mean, m2, count }` — Welford's
    rolling mean / variance over a fixed-size window of
    spread observations
  - `update(spread: Decimal) -> Option<Decimal>` returns
    the latest z-score after warmup
  - `entry_threshold` + `exit_threshold` config knobs
  - `decide(z: Decimal, position: Position) -> SignalAction`
    enum (Open / Close / Hold)
- [ ] **Sub-component #4 partial** — `StatArbDriver`:
  - Owns one `KalmanHedgeRatio`, one `ZScoreSignal`, one
    `EngleGrangerTest` reference
  - `tick_once(&self, y_mid, x_mid) -> StatArbEvent`
    pure-ish function that updates Kalman, computes
    spread, updates z-score, and emits an event
  - `StatArbEvent { Entered, Exited, Hold, Hysteresis }`
  - 10+ unit tests covering each branch

### Deliverables

- `crates/strategy/src/stat_arb/signal.rs`
- `crates/strategy/src/stat_arb/driver.rs`
- ≥18 unit tests across both
- Workspace test + fmt + clippy green

---

## Sprint B-4 — Engine wiring + audit + docs + commit (week 4)

**Goal.** Close the epic: engine-side hook for the driver,
audit events, per-pair PnL attribution, CHANGELOG /
CLAUDE / ROADMAP / memory updates, single commit.

### Phase 4c — Dev

- [ ] `MarketMakerEngine::with_stat_arb_driver(driver,
  tick_interval)` builder + select-loop arm that runs
  the driver on its tick cadence
- [ ] Driver's `StatArbEvent` routes to:
  - Per-pair PnL bucket via Portfolio per-strategy
    labeling (`"stat_arb_<pair>"` class)
  - New audit events `StatArbEntered` / `StatArbExited`
  - Direct-dispatch of leg execution via
    `OrderManager::execute_unwind_slice` (the same
    primitive `PairedUnwindExecutor` uses)

### Phase 5 — Testing

- [ ] One end-to-end test that drives a synthetic
  cointegrated pair through the full pipeline
  (kalman → signal → driver → engine event) and asserts
  the entered/exited transitions

### Phase 6 — Documentation

- [ ] CHANGELOG entry following the Epic A / Epic C shape
- [ ] CLAUDE.md: add `stat_arb` to the strategy crate
  module list, bump stats line
- [ ] ROADMAP.md: mark Epic B as DONE, list stage-2
  follow-ups (auto pair screener, multivariate Johansen,
  multi-pair driver, dynamic hedge-ratio uncertainty
  bounds)
- [ ] Memory: extend `reference_sota_research.md` with
  Epic B closure

### Deliverables

- `MarketMakerEngine::with_stat_arb_driver` builder + tick
  arm
- 1+ end-to-end test
- CHANGELOG, CLAUDE, ROADMAP, memory all updated
- Single epic commit without CC-Anthropic line

---

## Definition of done — whole epic

- All 4 sub-components shipped or explicitly deferred
- All tests green, clippy `-D warnings` clean, fmt clean
- Single commit lands the epic per commit discipline
- `MarketMakerEngine::with_stat_arb_driver` is callable
  and the driver runs its tick loop on the engine's
  select arm
- Per-pair PnL flows into the Portfolio strategy bucket
  with `"stat_arb_<pair>"` keys
- CHANGELOG, CLAUDE, ROADMAP, memory all updated

## Risks and open questions

- **Cointegration test stability.** Engle-Granger ADF
  critical values are sample-size dependent. v1 uses a
  fixed sample-size lookup table; stage-2 may need to
  refine if the operator runs with non-standard windows.
- **Kalman filter convergence.** The transition variance
  is the primary tuning knob — too high and β chases
  noise; too low and β cannot adapt to regime shifts.
  Default `1e-6` is a reasonable starting point but pair-
  specific tuning will surface in the first live run.
- **Two-book subscription.** The driver needs simultaneous
  mid-price feeds from both legs. v1 expects the engine
  caller to pass two `BookKeeper` references (or just two
  mid prices on each tick) — no new market-data
  infrastructure inside the driver itself.

## Sprint cadence rules

- **One week per sprint.** Friday EOD = sprint review,
  Monday morning = next sprint kickoff.
- **No code in Sprint B-1.** Planning + study only.
- **Working tree stays uncommitted across all 4 sprints**
  per `feedback_commit_discipline.md`. One commit at the
  end of B-4.
