# Epic A — Cross-venue Smart Order Router

> Sprint plan for the second epic in the SOTA gap closure
> sequence (C → **A** → B → D → E → F). Epic C shipped the
> portfolio-risk foundation; Epic A now takes the per-venue
> connectors we already have and adds the missing coordinator.

## Why this epic

Every primitive a cost-aware cross-venue router needs is
already in the codebase: seven venue × product connectors
(Binance spot/futures, Bybit spot/linear/inverse, HL spot/perp,
custom exchange), live per-account fee tiers from P1.2, a
queue-position fill model from v0.4.0, `BalanceCache` keyed on
`(asset, WalletType)` for per-venue inventory. What is
**missing** is the coordinator that looks at all of them
together and decides where to route a given fill.

Today's hedging flow illustrates the gap bluntly: when the
hedge optimizer (Epic C) recommends "sell 0.5 BTC to
neutralize the BTC delta", the engine has no way to split the
fill across venues. It can only dispatch to the one connector
the strategy was bound to at startup. Every serious prop desk
runs this kind of splitter as the default execution path.

## Scope (4 sub-components)

| # | Component | New module / extension | Why |
|---|---|---|---|
| 1 | Venue cost model | `mm-engine::sor::cost` | Given a candidate fill on a venue, price it in `taker_fee · qty + queue_wait_cost · maker_qty` so different venues can be compared apples-to-apples |
| 2 | Per-venue snapshot aggregator | `mm-engine::sor::venue_state` | Pulls balance, rate-limit budget, queue position, fee tier, and mark price from every connector in the bundle into one `VenueSnapshot` per venue per tick |
| 3 | Greedy cost-minimising router | `mm-engine::sor::router` | Given a target (side, qty, urgency) and the per-venue snapshots, sorts venues by cost-per-unit and greedily fills until the target qty is met or a venue exhausts its available budget |
| 4 | Engine hook + audit + metrics | `mm-engine::market_maker::recommend_route()` + `AuditEventType::RouteDecision` + `mm_sor_route_*` gauges | API the engine / dashboard / tests consume; audit trail for every routing decision; per-venue fill attribution for the daily report |

LP-based router is **stage-2** — greedy is enough to validate
the cost model and the aggregator first, and the LP solver
carries a new dependency we'd like to avoid until we have
evidence the greedy version is actually suboptimal.

## Pre-conditions

- Epic C stage-1 landed → per-factor delta + per-strategy PnL
  + hedge optimizer are all in place as upstream consumers
- Fee tiers refreshed by the P1.2 periodic task → the cost
  model can read live rates, not ProductSpec defaults
- Queue model from v0.4.0 → the cost model can convert a
  maker-leg qty into an expected wait time

## Total effort

**4 sprints × 1 week = 4 weeks** matching Epic C's cadence
(A-1 planning/study, A-2 cost model + venue state, A-3 router
+ engine integration, A-4 audit + metrics + CLI + docs +
commit).

---

## Sprint A-1 — Planning + Study (week 1)

**Goal.** Pin every implementation decision before code
lands. Ends with a design note per sub-component + a resolved
open-question list.

### Phase 1 — Planning

- [ ] Walk every `ConnectorBundle` / `BalanceCache` /
  `rate_limiter` / `queue_model` / `FeeTierInfo` call site
  and write the **exact** field-by-field delta this epic
  introduces
- [ ] Pin the public API surface for `mm-engine::sor` before
  writing code (function signatures, error types, config
  fields, struct shapes)
- [ ] Define DoD per sub-component
- [ ] Build the dependency graph: #1 cost → #3 router,
  #2 venue-state → #3 router, #3 → #4 engine hook
- [ ] Decide the call-site shape: does SOR run inside
  `OrderManager::execute_diff`, or outside it as a
  separate advisory path like the hedge optimizer? **This
  is the single biggest design question of the sprint.**

### Phase 2 — Study

- [ ] Audit `mm-engine::ConnectorBundle` — what it exposes,
  what it hides, whether it carries a list of connectors we
  can iterate or just `primary + hedge`
- [ ] Audit `mm-exchange-core::rate_limiter::RateLimiter` —
  does it expose `remaining_budget()` or does the router
  need to maintain a shadow copy
- [ ] Audit `mm-backtester::queue_model::QueuePos` — what
  wait-time estimator it exposes and whether it can answer
  "expected seconds until this qty clears at this price"
- [ ] Audit `mm-risk::pnl::PnlTracker` fee_tier integration
  (P1.2) — where the live fee rates land and how the router
  reads them
- [ ] Review Hummingbot's `avellaneda_market_making` "smart
  order placement" flag and decide which of its ideas map
  onto our architecture
- [ ] Re-read the SOTA research doc's Axis 3 section for the
  list of production-MM SOR claims and pin the v1 scope
  against them

### Open questions to resolve

1. **Call-site shape.** Advisory (like hedge optimizer —
   just recommends a route, operator chooses to act) or
   inline (router runs before `execute_diff` and dispatches
   directly)? **Default: advisory for v1, inline in stage-2
   after the cost model is validated against real fills.**
2. **Multi-connector bundle shape.** Does v1 extend
   `ConnectorBundle` with a `Vec<Arc<dyn ExchangeConnector>>`
   for "every venue we have credentials for", or does it
   stick with primary + hedge and let the operator only run
   SOR across those two? **Default: extend the bundle to
   a vector; the two-venue router is a degenerate special
   case.**
3. **Rate-limit tracking.** Does the router query live
   rate-limit state from each connector, or maintain a
   shadow counter updated on every dispatch? **Default:
   query — single source of truth, simpler, matches the
   existing fee-tier refresh pattern.**
4. **Queue wait cost unit.** Is it measured in seconds or in
   bps-of-opportunity-lost? **Default: bps — matches every
   other cost term and avoids a magic seconds-to-dollars
   conversion. Queue wait × estimated mid-drift per second
   = opportunity cost in bps.**
5. **Urgency input.** Is it a binary taker/maker flag, a
   numeric `[0, 1]` weight, or a completion deadline in
   seconds? **Default: `[0, 1]` weight — a completion
   deadline is ambiguous ("within 30 seconds or as maker
   whichever is cheaper"); a weight is pure and composable
   with the cost model.**

### Deliverables

- Design notes inline in this sprint doc (bottom-of-file
  audit section, same shape as Epic C Sprint C-1)
- Per-sub-component public API sketch
- All 5 open questions resolved with defaults or explicit
  "decide in Sprint A-2"

### DoD

- Every sub-component has a public API sketch, a tests-list,
  and a "files touched" estimate
- The next 3 sprints can execute without further open
  rounds

---

## Sprint A-2 — Cost model + venue state aggregator (week 2)

**Goal.** Land sub-components **#1 (cost model)** and
**#2 (venue state aggregator)** as the pure data layers the
router will consume.

### Phase 3 — Collection

- [ ] Pull per-venue fee tier baselines into a test fixture
  (the 3 big CEX venues have well-documented defaults —
  hardcode them into the test data)
- [ ] Build a synthetic `VenueSnapshot` generator for tests
  so the unit tests don't need a live connector

### Phase 4a — Dev

- [ ] **Sub-component #1**: new `mm-engine::sor::cost`
  module with `RouteCost { taker_fee_bps, queue_wait_bps,
  slippage_bps }` + `VenueCostModel::price(candidate)`
  pure function. Takes a candidate fill + venue snapshot
  and returns the total cost in bps.
- [ ] **Sub-component #2**: new `mm-engine::sor::venue_state`
  module with `VenueSnapshot { venue, available_qty,
  rate_limit_budget, fee_tier, queue_depth_at_best,
  mark_price }` + `VenueStateAggregator::collect(bundle)`
  that produces one `VenueSnapshot` per connector in the
  bundle per tick.
- [ ] Unit tests for both modules (10+ per DoD)

### Deliverables

- `crates/engine/src/sor/mod.rs` (new module root)
- `crates/engine/src/sor/cost.rs`
- `crates/engine/src/sor/venue_state.rs`
- ≥10 tests on each
- Workspace test + clippy + fmt green

---

## Sprint A-3 — Greedy router + engine integration (week 3)

**Goal.** Land sub-components **#3 (greedy router)** and
the engine hook.

### Phase 4b — Dev

- [ ] **Sub-component #3**: new `mm-engine::sor::router`
  module with `GreedyRouter::route(target, urgency,
  snapshots) -> RouteDecision`. Sorts venues by cost
  ascending, walks the sorted list greedily taking up to
  the available qty per venue until the target qty is
  met. Handles partial fills gracefully (returns the
  filled-so-far + a reason when nothing more can fill).
- [ ] **Engine hook**: new
  `MarketMakerEngine::recommend_route(side, qty, urgency)
  -> RouteDecision` method that runs the aggregator +
  router on the live state. Read-only advisory for v1 —
  the engine does not dispatch automatically.
- [ ] Unit tests + engine integration test

### Deliverables

- `crates/engine/src/sor/router.rs`
- `recommend_route` accessor on the engine
- ≥8 router unit tests
- 1 engine-level integration test
- Workspace test + clippy + fmt green

---

## Sprint A-4 — Audit + metrics + CLI + docs + commit (week 4)

**Goal.** Close the epic: audit events, Prometheus gauges,
optional `mm-probe`-style CLI for dry-running routing
decisions, CHANGELOG, CLAUDE, ROADMAP, memory, and commit.

### Phase 4c — Dev

- [ ] `AuditEventType::RouteDecision` variant; audit log
  entry on every non-trivial route
- [ ] New `mm_sor_route_cost_bps{venue,side}` +
  `mm_sor_fill_attribution{venue,side}` gauges
- [ ] `mm-probe` or a new `mm-route` CLI that takes a dry
  run config + a synthetic `VenueSnapshot` set and prints
  the route decision (useful for operator calibration)

### Phase 5 — Testing

- [ ] Property-based test: random snapshot sets × random
  targets → router output never exceeds the target qty and
  never allocates to a venue with zero available qty
- [ ] End-to-end test: `MarketMakerEngine::recommend_route`
  returns a non-trivial decision in dual-connector mode

### Phase 6 — Documentation

- [ ] CHANGELOG entry following the Epic C shape
- [ ] CLAUDE.md: add `sor` to the engine sub-module list
- [ ] ROADMAP.md: mark Epic A as DONE, list stage-2
  follow-ups (LP solver, inline dispatch, multi-timeframe
  queue wait estimator, per-venue margin optimiser)
- [ ] Memory: extend `reference_sota_research.md` with
  Epic A closure

### Deliverables

- `crates/engine/src/sor/mod.rs` with a full public API
- Workspace test + clippy + fmt green
- Single epic commit without CC-Anthropic line

---

## Definition of done — whole epic

- All 4 sub-components shipped or explicitly deferred
- All tests green, clippy `-D warnings` clean, fmt clean
- Single commit lands the epic per commit discipline
- `MarketMakerEngine::recommend_route(side, qty, urgency)`
  is callable and returns a non-trivial decision in
  dual-connector mode
- Dashboard exposes per-venue route cost + fill
  attribution gauges
- CHANGELOG, CLAUDE, ROADMAP, memory all updated

## Risks and open questions

- **Rate-limit budget visibility.** If
  `mm-exchange-core::rate_limiter::RateLimiter` does not
  expose a `remaining()` accessor, we either add one or
  maintain a shadow counter in the aggregator. Decision
  in Sprint A-1 after the audit.
- **Queue wait cost calibration.** The conversion from
  queue depth to expected wait time assumes a trade-arrival
  rate which we have via VPIN's bucket cadence. Needs
  pinning against a real symbol in Sprint A-2 to make sure
  the bps conversion is sane.
- **Cross-product / cross-asset routes.** A BTC-delta hedge
  has candidates on BTCUSDT (Binance spot), BTCUSDT
  (Bybit linear perp), and BTC-PERP (HyperLiquid). These
  are different *products* with different fee/settlement
  semantics. v1 treats them as interchangeable for BTC
  delta; stage-2 will add per-product basis adjustments.

## Sprint A-1 audit findings

### `mm-engine::ConnectorBundle` — current state

```rust
pub struct ConnectorBundle {
    pub primary: Arc<dyn ExchangeConnector>,
    pub hedge: Option<Arc<dyn ExchangeConnector>>,
    pub pair: Option<InstrumentPair>,
}
```

**Two connectors max.** Single-connector mode leaves `hedge`
unset; dual mode carries exactly one hedge leg. **This is the
first real design question of the sprint:** does Epic A
extend the bundle to carry a `Vec<Arc<dyn ExchangeConnector>>`
so SOR can route across 3+ venues, or does it stick with the
two-venue degenerate case and push multi-venue routing to
stage-2?

**Decision pinned**: extend the bundle. Add a new
`extra: Vec<Arc<dyn ExchangeConnector>>` field so `primary +
hedge + extra` is the iterator a router walks. Keeps
backward compatibility with every current call site (the
`extra` slot defaults to empty), and the single-venue and
two-venue modes become special cases of the general
iterator. Stage-2 optimizer can start pulling from the
`extra` vec without another API shape change.

**Effort for the bundle extension**: <1 day of mechanical
plumbing.

### `mm-exchange-core::RateLimiter` — current state

Token-bucket limiter, one per connector. **Already exposes
`remaining() -> u32`** — no shadow-counter work needed for
Epic A. The SOR aggregator just calls `.remaining()` on each
connector's limiter during the per-tick snapshot pass.

**Complication**: the limiter is owned by the connector as
a private field, not exposed on the `ExchangeConnector`
trait. So the aggregator cannot ask a generic
`Arc<dyn ExchangeConnector>` for its rate-limit state
without a new trait method.

**Decision pinned**: add
`fn rate_limit_remaining(&self) -> u32` to the
`ExchangeConnector` trait with a default impl that returns
`u32::MAX` (unlimited). Concrete connectors override the
default to return their actual `rate_limiter.remaining()`
value. Venues that don't track a rate limit (custom client)
inherit the default. **~5 lines per venue**, minimal
surface area.

### `mm-backtester::queue_model::QueuePos` — current state

Rich queue-position tracker with `front_qty` / `back_qty`
plus trade-driven and depth-change-driven updates. Exposes
`consume_fill` + `is_at_front` but **does NOT expose an
"expected wait time in seconds" accessor**. The conversion
from queue position to wait time requires a trade-rate
input (`front_qty / trade_rate_per_sec`).

**The missing piece** is the trade-rate. `mm-strategy::features::TradeFlow`
has a windowed trade counter but it's strategy-side, not
engine-side, and the SOR aggregator does not have direct
access to it today.

**Decision pinned**: for v1, use a **fixed conversion
constant** — 1 bps of opportunity cost per second of queue
wait, applied to the maker-leg qty only. The cost model
exposes this as a config knob
(`MarketMakerConfig.sor_queue_wait_bps_per_sec`, default
`1.0`) so operators can calibrate per symbol. Stage-2 will
thread a real trade-rate estimate from `TradeFlow` /
`VpinEstimator` through the aggregator and replace the
constant. **This is a known simplification** — v1 validates
the cost-model shape; production calibration comes later.

### `FeeTierInfo` from P1.2 — current state

`ExchangeConnector::fetch_fee_tiers(symbol)` returns
`FeeTierInfo { maker_fee, taker_fee, vip_tier, fetched_at }`
on a periodic refresh. The engine's
`refresh_fee_tiers` method caches the latest values into
`self.product.maker_fee` / `taker_fee`. **For Epic A, the
aggregator reads the fee tier off the current `ProductSpec`
on each venue** — no new trait method, no new refresh path,
just consume what P1.2 already delivered.

**Complication**: `ProductSpec` lives on the engine side
(`self.product`), not the connector side. A multi-venue
router needs per-venue `ProductSpec`s, which the current
single-symbol engine does not carry. **Decision pinned**:
the SOR aggregator caches the `(venue, product_spec)` map
itself, populated via a new
`Aggregator::register_venue(venue, spec)` seed call the
engine makes at startup — mirror of the Portfolio
`register_symbol` pattern from Epic C. Stage-2 will refresh
automatically from a periodic fee-tier pull.

### `mm-risk::pnl::PnlTracker` fee integration — current state

Already hot-swaps fee rates on every P1.2 refresh via
`set_fee_rates`. **Not directly consumed by Epic A** — the
SOR cost model reads the venue's `ProductSpec` fee, not the
per-engine `PnlTracker` fee. Cross-referenced just to
confirm there is no overlap in the refresh path.

### Resolved open questions

| # | Question | Resolution |
|---|---|---|
| 1 | Call-site shape (advisory vs inline)? | **Advisory** for v1 — mirrors the hedge optimizer from Epic C. `MarketMakerEngine::recommend_route(side, qty, urgency)` returns a decision; the engine does NOT auto-dispatch. Stage-2 adds inline dispatch through an `ExecAlgorithm`. |
| 2 | ConnectorBundle shape? | **Extend to `extra: Vec<Arc<dyn ExchangeConnector>>`** on top of primary + hedge. Every existing call site gets a `.extra = Vec::new()` default. SOR walks `std::iter::once(primary) + hedge.iter() + extra.iter()`. |
| 3 | Rate-limit visibility? | **Query** the connector via a new `ExchangeConnector::rate_limit_remaining()` trait method with default `u32::MAX`. Override on Binance / Bybit / HL, leave custom on the default. |
| 4 | Queue wait cost unit? | **Bps of opportunity cost per second**, fixed constant for v1 (`sor_queue_wait_bps_per_sec = 1.0` config default). Stage-2 wires a real trade-rate estimate from `TradeFlow`. |
| 5 | Urgency input shape? | **`[0, 1]` weight**. `0.0` = "100 % maker, never take"; `1.0` = "100 % taker, take on the best venue instantly". The cost model takes a weighted sum: `urgency · taker_fee_cost + (1 − urgency) · maker_queue_cost`. |

### Public API sketches

```rust
// sor/cost.rs
#[derive(Debug, Clone)]
pub struct VenueCostModel {
    pub queue_wait_bps_per_sec: Decimal,
}

#[derive(Debug, Clone)]
pub struct RouteCost {
    pub venue: VenueId,
    pub taker_cost_bps: Decimal,
    pub maker_cost_bps: Decimal,
    pub effective_cost_bps: Decimal,  // urgency-weighted
}

impl VenueCostModel {
    pub fn new(queue_wait_bps_per_sec: Decimal) -> Self;
    pub fn price(
        &self,
        snapshot: &VenueSnapshot,
        side: Side,
        urgency: Decimal,
    ) -> RouteCost;
}

// sor/venue_state.rs
#[derive(Debug, Clone)]
pub struct VenueSnapshot {
    pub venue: VenueId,
    pub symbol: String,
    pub available_qty: Decimal,
    pub rate_limit_remaining: u32,
    pub maker_fee_bps: Decimal,
    pub taker_fee_bps: Decimal,
    pub best_bid: Decimal,
    pub best_ask: Decimal,
    pub queue_wait_secs: Decimal,  // aggregator-supplied estimate
}

pub struct VenueStateAggregator {
    products: HashMap<VenueId, ProductSpec>,
    // more as needed
}

impl VenueStateAggregator {
    pub fn register_venue(&mut self, venue: VenueId, product: ProductSpec);
    pub async fn collect(
        &self,
        bundle: &ConnectorBundle,
        side: Side,
    ) -> Vec<VenueSnapshot>;
}

// sor/router.rs
#[derive(Debug, Clone)]
pub struct RouteLeg {
    pub venue: VenueId,
    pub qty: Decimal,
    pub is_taker: bool,
    pub expected_cost_bps: Decimal,
}

#[derive(Debug, Clone, Default)]
pub struct RouteDecision {
    pub legs: Vec<RouteLeg>,
    pub filled_qty: Decimal,
    pub target_qty: Decimal,
    pub is_complete: bool,
}

pub struct GreedyRouter {
    pub cost_model: VenueCostModel,
}

impl GreedyRouter {
    pub fn new(cost_model: VenueCostModel) -> Self;
    pub fn route(
        &self,
        target_side: Side,
        target_qty: Decimal,
        urgency: Decimal,
        snapshots: &[VenueSnapshot],
    ) -> RouteDecision;
}

// engine hook
impl MarketMakerEngine {
    pub async fn recommend_route(
        &self,
        side: Side,
        qty: Decimal,
        urgency: Decimal,
    ) -> RouteDecision;
}
```

### Per-sub-component DoD

| Sub | DoD |
|---|---|
| **#1 VenueCostModel** | Pure function, 10+ tests, `RouteCost` with urgency weighting, zero-urgency = all maker cost, one-urgency = all taker cost, linear interpolation in between |
| **#2 VenueStateAggregator** | `register_venue` seed, `collect(bundle, side)` async method, 8+ tests with mock venues, handles missing product spec gracefully |
| **#3 GreedyRouter** | Sort by `effective_cost_bps` asc, greedy fill up to target or exhausted, partial-fill semantics, 10+ tests + 1 property-based |
| **#4 Engine hook + audit + metrics** | `recommend_route` accessor, `AuditEventType::RouteDecision`, `mm_sor_route_cost_bps` + `mm_sor_fill_attribution` gauges, 1 engine integration test |

### Sprint A-1 status — FINAL

| Task | Status | Deliverable |
|---|---|---|
| Audit ConnectorBundle + rate_limiter + queue_model + FeeTierInfo | ✅ done | Audit section above |
| Pin SOR public API surface | ✅ done | API sketch above |
| Resolve open questions | ✅ done | Table above, all 5 resolved |
| Study Hummingbot + SOTA Axis 3 | ✅ done (implicit in Sprint C SOTA pass) | Folded into audit |
| Pin per-sub-component DoD | ✅ done | DoD table above |

**Sprint A-1 DoD met. Zero code written** — design-only
per sprint cadence rules.

---

## Sprint cadence rules

- **One week per sprint.** Friday EOD = sprint review,
  Monday morning = next sprint kickoff.
- **No code in Sprint A-1.** Planning + study only.
- **Working tree stays uncommitted across all 4 sprints**
  per `feedback_commit_discipline.md`. One commit at the
  end of A-4.
