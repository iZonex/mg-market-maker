# Epic F — Defensive layer

> Sprint plan for the fifth epic in the SOTA gap closure
> sequence. The user reordered as **F → E** so the
> defensive surface lands before the execution polish.
> Epics C, A, B, D all closed stage-1 in Apr 2026. Epic F
> ships the three defensive controls every production prop
> desk runs that we structurally lack.

## Why this epic

A market maker survives by reading the room: when the room
gets dangerous, you widen, you skew, or you walk away. Our
existing defensive controls (kill switch L1-L5, VPIN
spread widening, market-resilience score, autotuner regime
shifts) handle the **observable** danger — toxicity in
trades, regime shifts in volatility, hard kill triggers
from the operator. What's missing is the **predictive**
defensive layer: signals that say "danger is *about to*
arrive" so the MM can retreat in the 100-500 ms window
before adverse fills land.

Three production-MM defensive controls cover that gap:

1. **Lead-lag guard** (Makarov-Schoar 2020). Subscribe to
   a "leader" venue mid feed — typically Binance Futures
   for crypto, since the perpetual leads spot by 200-800 ms
   on average. When the leader makes a sharp move, push a
   soft-widen signal into the autotune path before the
   primary venue's spot quotes get hit.
2. **News / sentiment retreat state machine.** Background
   task subscribes to a news feed and trips a
   `news_retreat` flag on a high-priority headline. The
   quoter consults the flag and widens or pulls. Wintermute,
   GSR, Flow Traders all publicly discuss this as a
   first-class control.
3. **Listing sniper / probation onboard** ← deferred to
   stage-2. The venue-level `list_symbols` API doesn't
   exist on our `ExchangeConnector` trait yet; adding it
   across 4 venues is a multi-venue sub-epic on its own.
   Stage-1 ships the two predictive defensive signals
   above and tracks the listing sniper as a follow-up.

Closing these closes the "informed flow eats the MM"
failure mode that toxicity-based widening only mitigates
*after the fact*.

## Scope (2 sub-components stage-1, 1 deferred)

| # | Component | New module | Why |
|---|---|---|---|
| 1 | Lead-lag guard | `mm-risk::lead_lag_guard` (new) | Cross-venue leader-mid → soft-widen signal. ~200 LoC, pure stats + state machine, no IO |
| 2 | News retreat state machine | `mm-risk::news_retreat` (new) | Background-task-friendly state machine with regex priority list + cooldown. ~250 LoC, no built-in feed source — operators wire any tx-side via a simple `on_headline(text)` API |
| 3 | ~~Listing sniper~~ | ~~`mm-engine::listing_sniper`~~ | **Stage-2 follow-up.** Needs a new `ExchangeConnector::list_symbols` trait method shipped across all 4 venues first |

## Pre-conditions

- ✅ `AutoTuner::set_toxicity` / `set_market_resilience` —
  the autotuner already accepts external soft-widen
  signals; lead-lag plugs in via the same shape
- ✅ `KillSwitch` 5-level — news retreat state machine
  escalates through L1 (WidenSpreads) and L2 (StopNewOrders)
  on its highest-priority class
- ✅ `AlertManager` (`mm-dashboard::alerts`) — both
  defensive controls fire alerts on transitions for
  operator visibility
- ✅ `AuditLog::risk_event` — every defensive transition
  writes an audit record so post-mortem replay is clean
- ✅ Cross-venue `ConnectorBundle` (since v0.2.0 Sprint G)
  — the lead-lag guard subscribes to a second venue's
  mid feed via the same `ConnectorBundle.extra` slot Epic A
  introduced

## Total effort

**4 sprints × 1 week = 4 weeks** matching prior epic
cadence:

- **F-1** Planning + Study (no code) — audit existing
  defensive primitives, transcribe lead-lag formula
  family, pin news-retreat state-machine design,
  resolve open questions
- **F-2** Lead-lag guard (sub-component #1)
- **F-3** News retreat state machine (sub-component #2)
- **F-4** Engine wiring + audit + CHANGELOG + ROADMAP +
  memory + single epic commit. Defer listing sniper to
  a follow-up tracked in ROADMAP.

---

## Sprint F-1 — Planning + Study (week 1)

**Goal.** Pin every implementation decision before code
lands. End the sprint with a per-sub-component design
note plus a resolved open-question list.

### Phase 1 — Planning

- [ ] Walk every relevant existing primitive
  (`AutoTuner`, `KillSwitch`, `AlertManager`,
  `MarketMakerEngine::refresh_quotes`, `ConnectorBundle`,
  `pair_lifecycle`, `circuit_breaker`) and write a
  field-by-field delta of what defensive layer needs
  from each
- [ ] Pin the public API for the two sub-components
- [ ] Define DoD per sub-component
- [ ] Decide call-site shape: lead-lag plugs into
  `AutoTuner` like the existing toxicity / MR
  multipliers; news retreat plugs into `KillSwitch`
  via a new soft-trigger source

### Phase 2 — Study

- [ ] Read **Makarov, I., Schoar, A. — "Trading and
  Arbitrage in Cryptocurrency Markets"** (*J. Financial
  Economics*, 135(2), 293–319, 2020) §4 on lead-lag
  effects across venues. The 200-800 ms binance-futures
  → spot lead is the empirical anchor for the default
  window choice
- [ ] Read **Cartea, Á., Jaimungal, S., Penalva, J. —
  "Algorithmic and High-Frequency Trading"** ch.10
  §10.4 "Trading on News" for the academic framing of
  news-driven retreat. The CJP formulation is a Poisson
  jump-process risk model; v1 ships the simpler
  state-machine approximation that covers ~80% of the
  value
- [ ] Audit `AutoTuner::set_toxicity` and
  `set_market_resilience` — the lead-lag guard's output
  shape (a single `[1, N]` spread multiplier) is
  isomorphic to the toxicity multiplier. Reuse the same
  call-site convention
- [ ] Audit `KillSwitch::manual_trigger` — news retreat
  uses `manual_trigger(WidenSpreads, "news: <headline>")`
  for medium-priority headlines and
  `manual_trigger(StopNewOrders, ...)` for high-priority
- [ ] Audit `AlertManager::send` — every defensive
  transition fires an alert with severity tag

### Open questions to resolve

1. **Lead-lag guard — where does the leader feed come
   from?** The cleanest path is the existing
   `ConnectorBundle.extra` slot from Epic A: operators
   register a secondary connector (e.g. Binance Futures
   when the primary is Bybit spot) and the guard subscribes
   to its mid stream. **Default: ConnectorBundle.extra,
   first slot.** No new connector trait method needed.

2. **Lead-lag guard — sigma source.** The N-sigma threshold
   needs an estimate of the leader's short-horizon return
   volatility. Two choices:
   - (a) Reuse the engine's `VolatilityEstimator` against
     the leader feed
   - (b) Compute a local rolling-window EWMA inside the
     guard
   - **Default: (b), local EWMA.** Decouples the guard
     from the engine's vol estimator (which is sized for
     the 60 s spread-quote loop, not the 100-500 ms
     lead-lag window). ~30 LoC.

3. **Lead-lag guard — window length.** Cont 2014 uses
   100 ms equity windows; Makarov-Schoar 2020 measures
   200-800 ms crypto lead-lag. **Default: 250 ms**, with
   `min`/`max` bounds at 100 ms / 1 s for operator tuning
   per pair.

4. **News retreat — feed source.** Production prop desks
   pay for Kaiko / Laevitas / Tiingo. Free alternatives:
   Telegram/X scrapers. v1 must NOT depend on any
   external HTTP service. **Default: caller-supplied
   `on_headline(text: &str)` API.** The state machine is
   pure-function; operators wire the feed source themselves
   (their own scraper, a paid feed adapter, a Telegram
   bot relay, etc.) and call `on_headline` for each item.
   Stage-2 can ship a Tiingo / Telegram adapter as a
   parallel sub-component.

5. **News retreat — priority classification.** A regex
   priority list (e.g. `"SEC|fraud|hack|exploit"` →
   critical, `"FOMC|CPI|jobs"` → high) is the operator-
   tuned classifier. **Default: 3 classes** — `Critical`
   (full retreat / kill switch L2), `High` (widen via
   autotune mult), `Low` (alert only, no quote impact).
   Class definitions are config-driven; v1 ships sensible
   defaults the operator can override.

6. **News retreat — cooldown.** A single headline should
   not trigger forever. Default cooldown: `Critical` =
   30 min, `High` = 5 min, `Low` = 0 (no cooldown). After
   the cooldown the state machine reverts to `Normal` if
   no fresh headline at the same level lands in the
   window. **Default: 30 / 5 / 0 minutes**, all
   config-overridable.

7. **Listing sniper deferral.** The sniper needs a new
   `ExchangeConnector::list_symbols(&self) -> Vec<ProductSpec>`
   trait method shipped across Binance, Bybit, HyperLiquid,
   and the custom `mm-exchange-client`. That's 4 venue
   adapters + the trait method + a per-venue REST endpoint
   per call (`/api/v3/exchangeInfo` on Binance,
   `/v5/market/instruments-info` on Bybit, etc.). **Defer
   to stage-2.** Epic F stage-1 ships the two predictive
   defensive signals; the listing sniper is tracked in
   ROADMAP as a follow-up.

### Deliverables

- Audit findings inline at the bottom of this sprint
  doc (same shape as Epic C / A / B / D Sprint 1 audit
  sections)
- Per-sub-component public API sketch
- All 7 open questions resolved with defaults or
  explicit "decide in Sprint F-2"
- Companion formulas doc at
  `docs/research/defensive-layer-formulas.md`

### DoD

- Every sub-component has a public API sketch, a tests
  list, and a "files touched" estimate
- The next 3 sprints can execute without further open
  rounds
- Listing sniper deferral is documented in the sprint
  doc + ROADMAP so it doesn't get dropped

### Audit findings — existing primitives we reuse

Phase-2 audit done against the live tree on 2026-04-15.

#### A — `AutoTuner` (`crates/strategy/src/autotune.rs`)

Already accepts external soft-widen multipliers via
`set_toxicity(vpin)` and `set_market_resilience(score)`
hooks. Both flow into `effective_spread_mult()` /
`effective_gamma_mult()`. The lead-lag guard plugs in via a
new parallel `set_lead_lag_mult(mult)` field/setter that
multiplies the same effective output. News retreat folds
in identically — the difference is just the *source* of
the multiplier.

**Verdict:** no new soft-widen plumbing needed. Both
defensive controls land as new fields on `AutoTuner` with
the same call-site shape as `toxicity_spread_mult` and
`market_resilience`.

#### B — `KillSwitch` (`crates/risk/src/kill_switch.rs`)

`KillSwitch::manual_trigger(level, reason)` is the
operator-facing escalation primitive. News retreat fires
`manual_trigger(WidenSpreads, "news: <headline>")` for
`High` class headlines and `manual_trigger(StopNewOrders,
"news: <headline>")` for `Critical`. The 5-level kill
switch already supports L1 (WidenSpreads) and L2
(StopNewOrders) — both trip from the existing
`manual_trigger` path.

**Verdict:** no new escalation primitive needed. News
retreat is a new *consumer* of the existing kill switch.

#### C — `AlertManager` (`crates/dashboard/src/alerts.rs`)

`AlertManager::send(severity, msg)` already supports the
3-level severity scheme (`Info / Warning / Critical`) and
de-duplicates on the message key. Both defensive controls
fire alerts on every state-change transition for operator
visibility.

**Verdict:** no new alert-routing changes needed.

#### D — `AuditLog::risk_event` (`crates/risk/src/audit.rs`)

The append-only JSONL audit trail already has a
`risk_event(symbol, type, detail)` entry point that all
prior epics use. Epic F adds three new
`AuditEventType` variants (`LeadLagTriggered`,
`NewsRetreatActivated`, `NewsRetreatExpired`) and routes
through the same writer.

**Verdict:** straightforward enum extension, same pattern
as Epic D's `OfiFeatureSnapshot` / `AsSpreadWidened`.

#### E — `ConnectorBundle.extra` (Epic A prerequisite)

Epic A's `extra: Vec<Arc<dyn ExchangeConnector>>` slot is
the natural home for the lead-lag leader feed. Operators
register a secondary venue (e.g. Binance Futures when the
primary is Bybit spot) via `with_extra(connector)`, and
the engine's `subscribe` call iterates over
`ConnectorBundle.all_connectors()` to spin up the leader
mid stream. Lead-lag guard receives the leader mids via
the engine's existing `handle_extra_event` hook (or a
parallel one if the existing hook only handles SOR-related
events).

**Verdict:** no new connector trait method needed. The
guard's L1 mid stream rides the existing extra-connector
infrastructure.

### Open questions — resolved

All seven open questions resolved against the defaults
from the sprint plan:

1. **Lead-lag leader feed** → ✅ `ConnectorBundle.extra`
   slot, first registered extra connector. No new trait
   method.
2. **Lead-lag sigma source** → ✅ local EWMA inside the
   guard. ~30 LoC, decoupled from the engine's
   `VolatilityEstimator`.
3. **Lead-lag window length** → ✅ default 250 ms,
   half-life 20 events, configurable per pair.
4. **News retreat feed source** → ✅ caller-supplied
   `on_headline(text)` API. v1 ships no built-in feed
   adapter; operators wire their own (Telegram bot, file
   tail, paid Tiingo adapter) in the engine integration
   layer.
5. **News retreat priority classification** → ✅ 3-class
   regex priority lists (`Critical / High / Low`). v1
   ships sensible defaults the operator overrides per
   config.
6. **News retreat cooldown** → ✅ default 30 / 5 / 0
   minutes for `Critical / High / Low`, all
   config-overridable.
7. **Listing sniper deferral** → ✅ explicitly deferred
   to Epic F stage-2 follow-up. Tracked in ROADMAP closure
   note. Stage-2 needs a new
   `ExchangeConnector::list_symbols` trait method shipped
   across all 4 venue adapters first.

### Per-sub-component API surface — pinned

Full formulas live in
`docs/research/defensive-layer-formulas.md`. API types
pinned below.

#### #1 Lead-lag guard — `mm_risk::lead_lag_guard`

```rust
pub struct LeadLagGuardConfig {
    pub half_life_events: usize,    // default 20
    pub z_min: Decimal,              // default 2.0
    pub z_max: Decimal,              // default 4.0
    pub max_mult: Decimal,           // default 3.0
}

pub struct LeadLagGuard { /* private */ }

impl LeadLagGuard {
    pub fn new(config: LeadLagGuardConfig) -> Self;
    pub fn on_leader_mid(&mut self, mid: Decimal);
    pub fn current_multiplier(&self) -> Decimal;
    pub fn current_z_abs(&self) -> Decimal;
    pub fn is_active(&self) -> bool;
    pub fn reset(&mut self);
}
```

Files touched: `crates/risk/src/lead_lag_guard.rs` (new,
~250 LoC), `crates/risk/src/lib.rs` (export).

Tests list (≥10):
- first update returns `multiplier = 1.0` (no prior state)
- second update with stable returns produces
  `multiplier ≈ 1.0`
- sharp leader move at `|z| = 2.0` → `multiplier = 1.0`
  (at the floor of the ramp)
- sharp leader move at `|z| = 4.0` → `multiplier = 3.0`
  (saturated)
- sharp leader move at `|z| = 3.0` → `multiplier ≈ 2.0`
  (mid-ramp)
- positive vs negative shocks symmetric (both fire)
- decay back to `1.0` after a quiet stream
- zero-variance window guard (no division by zero)
- `is_active()` flips on / off correctly
- `reset()` clears all state
- hand-verified fixture: known input sequence → known
  output multiplier sequence

#### #2 News retreat — `mm_risk::news_retreat`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsRetreatState { Normal, Low, High, Critical }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsClass { Low, High, Critical }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsRetreatTransition {
    NoMatch,
    Promoted { from: NewsRetreatState, to: NewsRetreatState },
    Refreshed(NewsRetreatState),
}

pub struct NewsRetreatConfig {
    pub critical_patterns: Vec<String>,
    pub high_patterns: Vec<String>,
    pub low_patterns: Vec<String>,
    pub critical_cooldown_ms: i64,   // default 30 * 60_000
    pub high_cooldown_ms: i64,       // default 5 * 60_000
    pub low_cooldown_ms: i64,        // default 0
}

pub struct NewsRetreatStateMachine { /* private */ }

impl NewsRetreatStateMachine {
    pub fn new(config: NewsRetreatConfig) -> anyhow::Result<Self>;
    pub fn on_headline(&mut self, text: &str, now_ms: i64) -> NewsRetreatTransition;
    pub fn current_state(&mut self, now_ms: i64) -> NewsRetreatState;
    pub fn current_multiplier(&mut self, now_ms: i64) -> Decimal;
    pub fn should_stop_new_orders(&mut self, now_ms: i64) -> bool;
}
```

Files touched: `crates/risk/src/news_retreat.rs` (new,
~350 LoC), `crates/risk/src/lib.rs` (export),
`crates/risk/Cargo.toml` (`regex` dep — already
workspace-resolved via `mm-server`).

Tests list (≥10):
- empty patterns: every headline is `NoMatch`
- critical regex match transitions `Normal → Critical`
- high regex match transitions `Normal → High`
- low regex match transitions `Normal → Low`
- promotion ladder: `Low → High → Critical` on
  successive escalating headlines
- demotion is impossible: low headline in `Critical` is
  a no-op
- cooldown expiry: `Critical → Normal` after 30 min of
  no fresh headline
- multiplier per state matches the table
- `should_stop_new_orders` true only in `Critical`
- regex compile error returns `Err` from `new`
- malformed regex propagates the error

### Per-sub-component DoD — pinned

| # | Component | Files | LoC (est) | Tests | DoD |
|---|---|---|---|---|---|
| 1 | `lead_lag_guard` | `lead_lag_guard.rs` | ~250 | ≥10 | Pure Decimal, EWMA + ramp, no IO, hand-verified fixture, decay-to-neutral test |
| 2 | `news_retreat` | `news_retreat.rs` | ~350 | ≥10 | Regex priority lists, 3-class state machine, per-class cooldown, multiplier table tests |

**Epic total**: ~600 LoC of new code across 2 new files,
≥20 unit tests, plus 2 end-to-end pipeline tests in
Sprint F-4 (lead-lag guard → autotuner mult, news Critical
→ kill switch L2).

---

## Sprint F-2 — Lead-lag guard (week 2)

**Goal.** Land sub-component **#1** as the first
predictive defensive signal.

### Phase 3 — Collection

- [ ] Build a synthetic leader-vs-follower fixture: two
  correlated price streams with a deterministic
  cross-venue lag (the leader moves first, the follower
  catches up after `N` ticks). Used to verify the guard
  fires on the leader move and not on the follower
- [ ] Build a "no-signal" fixture (uncorrelated white
  noise on both streams) to verify the guard does NOT
  fire spuriously

### Phase 4a — Dev

- [ ] **Sub-component #1** — `mm-risk::lead_lag_guard`:
  - `LeadLagGuard::new(window_ms, sigma_threshold)`
  - `on_leader_mid(timestamp_ms, mid: Decimal)` — fold
    a leader-side mid update; computes EWMA std and
    z-score
  - `current_multiplier() -> Decimal` — returns the
    soft-widen multiplier in `[1, max_mult]` based on
    the latest |z-score|. Default `max_mult = 3.0`.
  - `is_active() -> bool` — convenience for the engine
    to log "lead-lag triggered"
  - Pure `Decimal`, no IO, no async
- [ ] 10+ unit tests

### Deliverables

- `crates/risk/src/lead_lag_guard.rs` (new module)
- ≥10 tests
- Workspace test + fmt + clippy green

---

## Sprint F-3 — News retreat state machine (week 3)

**Goal.** Land sub-component **#2** as the second
predictive defensive signal.

### Phase 4b — Dev

- [ ] **Sub-component #2** — `mm-risk::news_retreat`:
  - `NewsRetreatState { Normal, Low, High, Critical }`
  - `NewsRetreatConfig` with regex priority lists +
    per-class cooldown durations
  - `NewsRetreatStateMachine::new(config)`
  - `on_headline(text: &str, now_ms: i64) -> NewsRetreatTransition`
    — classifies the text against the regex lists and
    returns the new state + a transition flag
  - `current_state(now_ms: i64) -> NewsRetreatState` —
    accounts for cooldown; returns `Normal` if the active
    state has expired
  - `current_multiplier() -> Decimal` — soft-widen
    multiplier per state (Critical = 3.0, High = 2.0,
    Low = 1.0)
  - `should_stop_new_orders() -> bool` — Critical → true
- [ ] 10+ unit tests covering classification, cooldown,
  transitions, and multiplier output

### Deliverables

- `crates/risk/src/news_retreat.rs` (new module)
- ≥10 tests
- Workspace test + fmt + clippy green

---

## Sprint F-4 — Engine wiring + audit + docs + commit (week 4)

**Goal.** Wire the two defensive signals into
`MarketMakerEngine`, close the epic.

### Phase 4c — Dev

- [ ] `MarketMakerEngine` grows two new optional fields:
  `lead_lag_guard: Option<LeadLagGuard>` and
  `news_retreat: Option<NewsRetreatStateMachine>`
- [ ] New builder methods `with_lead_lag_guard(guard)`
  and `with_news_retreat(state_machine)`
- [ ] On every leader-side `MarketEvent::BookSnapshot` /
  `BookDelta`, the engine calls
  `guard.on_leader_mid(...)` (when attached). The
  resulting multiplier is folded into the autotuner via
  `auto_tuner.set_lead_lag_mult(guard.current_multiplier())`.
- [ ] New `MarketMakerEngine::on_news_headline(text)`
  public method that operators call from any wiring
  layer (Telegram bot, file tail, future feed adapter).
  Routes to the state machine, fires audit + alert,
  escalates kill switch on Critical class.
- [ ] New audit event types `LeadLagTriggered`,
  `NewsRetreatActivated`, `NewsRetreatExpired`
- [ ] `AutoTuner::set_lead_lag_mult` extension
  (parallel to `set_toxicity`)

### Phase 5 — Testing

- [ ] One end-to-end test that drives a synthetic
  leader feed through the engine and asserts the
  autotuner spread multiplier responds
- [ ] One end-to-end test that fires a "Critical"
  headline through `on_news_headline` and asserts the
  kill switch escalates to L2 `StopNewOrders`

### Phase 6 — Documentation

- [ ] CHANGELOG entry following the Epic A / B / C / D shape
- [ ] CLAUDE.md: add `lead_lag_guard` + `news_retreat`
  to the risk crate module list, bump stats
- [ ] ROADMAP.md: mark Epic F as DONE stage-1, list
  stage-2 follow-ups (listing sniper, paid news feed
  adapters, multi-leader lead-lag, Cartea Poisson
  formulation)
- [ ] Memory: extend `reference_sota_research.md` with
  Epic F closure notes

### Deliverables

- `MarketMakerEngine::with_lead_lag_guard` +
  `with_news_retreat` builders
- `MarketMakerEngine::on_news_headline` public method
- 2 end-to-end tests
- CHANGELOG, CLAUDE, ROADMAP, memory all updated
- Single epic commit without CC-Anthropic line

---

## Definition of done — whole epic

- Both stage-1 sub-components (lead-lag guard + news
  retreat) shipped
- Listing sniper explicitly deferred and tracked
- All tests green, clippy `-D warnings` clean, fmt clean
- Single commit lands the epic per commit discipline
- `MarketMakerEngine::with_lead_lag_guard` and
  `with_news_retreat` builders are callable
- Per-sub-component audit events flow into the audit log
- CHANGELOG, CLAUDE, ROADMAP, memory all updated

## Risks and open questions

- **Lead-lag false positives.** A genuine leader move
  triggers the guard, but so does a stuck or stale
  leader feed. v1 mitigation: the EWMA std fuses to a
  high value during stale periods, so the |z-score|
  drops naturally. Stage-2 can add an explicit
  staleness gate.
- **News headline mis-classification.** Regex priority
  lists are fragile — a critical-priority regex that's
  too broad will spuriously trip the kill switch. v1
  mitigation: every classification fires an audit
  event so operators can replay and tune. The cooldown
  prevents single-headline runaway.
- **Listing sniper deferral.** Tracked in ROADMAP
  stage-2 follow-ups. The trait-method scope means
  Epic F stage-1 has 2 sub-components instead of 3 —
  ROADMAP will reflect the closure correctly.

## Sprint cadence rules

- **One week per sprint.** Friday EOD = sprint review,
  Monday morning = next sprint kickoff.
- **No code in Sprint F-1.** Planning + study only.
- **Working tree stays uncommitted across all 4 sprints**
  per `feedback_commit_discipline.md`. One commit at the
  end of F-4.
