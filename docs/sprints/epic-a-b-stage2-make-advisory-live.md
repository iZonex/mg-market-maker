# Stage-2 Track 1 — Make advisory live (Epic A SOR + Epic B stat-arb dispatch)

Follow-up sprint for the two "advisory-only" closures from Stage-1:

- **Epic A** shipped the cross-venue Smart Order Router but only as an
  advisory loop (`MarketMakerEngine::recommend_route` writes an audit
  record and returns a `RouteDecision`; operators decide whether to
  dispatch).
- **Epic B** shipped the stat-arb cointegration driver but
  `handle_stat_arb_event` only logs the intent — no leg orders get
  dispatched on `Entered` / `Exited`.

Stage-2 Track 1 wires real leg execution on both paths while keeping
the advisory entry points available so operators can opt in.

## Scope

### Sub-component 1A — SOR inline dispatch

- New `crates/engine/src/sor/dispatch.rs` module with `DispatchOutcome`
  and per-leg `LegOutcome` structs plus a free-function
  `dispatch_route(...)` helper that executes every leg of a decision.
- New public method `MarketMakerEngine::dispatch_route(side, qty, urgency)`
  that re-uses `recommend_route` to produce a decision and then fires
  `dispatch::dispatch_route` against the bundle's connectors.
- Urgency drives maker vs taker: `urgency >= 0.5` (the router's
  `TAKER_THRESHOLD`) uses taker-style IOC via
  `OrderManager::execute_unwind_slice`; `urgency < 0.5` uses maker-style
  PostOnly via `ExchangeConnector::place_order` directly.
- `recommend_route` keeps its existing shape — `dispatch_route` is a
  new parallel public API.
- Reuses the existing `AuditEventType::RouteDecisionEmitted` audit
  event (Track 4 owns `audit.rs`). Dispatch outcomes are logged via
  `tracing::info!` so ops can grep the log stream.

### Sub-component 1B — Stat-arb real leg dispatch

- New methods on `StatArbDriver`:
  - `try_dispatch_legs_for_entry(event, om, product, &mut y_om, &mut x_om)`
    — launches a TWAP-style IOC leg on both y and x via
    `OrderManager::execute_unwind_slice`. v1 dispatches the first
    slice synchronously; the engine driver tick advances subsequent
    slices (default: `stat_arb_tick` cadence). v2 can promote this
    to a proper `TwapAlgo` state machine.
  - `try_dispatch_legs_for_exit(event, om, product, &mut y_om, &mut x_om)`
    — single IOC slice per leg to market-take out of the position.
- `handle_stat_arb_event` in `market_maker.rs` calls these helpers on
  `Entered` / `Exited`. Drops the advisory-only log-only path but keeps
  the audit event so the pre-existing stat_arb audit tests stay green.
- Per-pair PnL routing: on every primary-leg fill, if
  `stat_arb_driver.is_some() && funding_arb_driver.is_none()`, the
  portfolio gets labelled with `pair.strategy_class` instead of the
  generic primary-strategy name. Same on the hedge path.

## Audit findings from Stage-1

- **Epic A advisory caveat**: `recommend_route` had no dispatch hook —
  every call returned a decision the engine logged but never acted on.
  Pinned in closure note as a stage-2 deferral.
- **Epic B advisory caveat**: `handle_stat_arb_event` writes
  `StatArbEntered` / `StatArbExited` audit records with direction and
  qty but never calls `OrderManager::execute_unwind_slice`. Config
  `StatArbDriverConfig::leg_notional_usd` sizes the legs correctly but
  the sizing outcome lives only in the event payload.
- **No existing per-pair PnL route for stat-arb**: the funding-arb path
  uses `Strategy::name()` as the label; stat-arb needs the
  `pair.strategy_class` string instead.

## Open design questions (resolved)

1. **DispatchOutcome shape** — `{ target_side, legs, total_target_qty,
   total_dispatched_qty, errors }` with per-leg `{ venue, target_qty,
   dispatched, is_taker, error: Option<String> }`. Stays close to
   `RouteDecision` so dashboard code can zip the two.
2. **Which OrderManager helper for SOR dispatch?** Urgency-driven:
   taker (urgency ≥ 0.5) → `execute_unwind_slice` (IOC limit).
   maker (urgency < 0.5) → `connector.place_order` directly with
   `TimeInForce::PostOnly`. The dispatch helper handles both branches
   inline so there's one code path per leg.
3. **Stat-arb TWAP scheduler** — v1 does a single-shot dispatch per
   event rather than scheduling slices across ticks. `entry_twap_*`
   config exists conceptually but the current `StatArbDriverConfig`
   only carries `leg_notional_usd`. Slicing is left to v2. The single
   IOC shot is enough to validate the dispatch pipeline.
4. **Per-pair PnL discriminator** — `stat_arb_driver.is_some() &&
   funding_arb_driver.is_none()` → use `pair.strategy_class`. If both
   drivers are attached, stat-arb wins (operators don't typically mix).

## File ownership

### Writeable (this track)

- `crates/engine/src/sor/dispatch.rs` — **NEW**
- `crates/engine/src/sor/mod.rs` — add module export
- `crates/engine/src/market_maker.rs` — dispatch_route + handle_stat_arb_event + tests
- `crates/strategy/src/stat_arb/driver.rs` — try_dispatch_legs_* methods
- `crates/strategy/src/stat_arb/mod.rs` — re-export if needed
- `docs/sprints/epic-a-b-stage2-make-advisory-live.md` — this file

### Forbidden

- `crates/risk/src/audit.rs` (Track 4). Reuse existing audit variants.
- `crates/engine/src/lib.rs` (Track 3 adds a listing_sniper export).
- Everything in `crates/exchange/` (Track 3).
- `crates/strategy/src/{learned_microprice,cartea_spread,glft,trait}.rs` (Track 2).
- `Cargo.toml` / `Cargo.lock` / `CHANGELOG.md` / `CLAUDE.md` /
  `ROADMAP.md` / memory files — orchestrator handles.

## Definition of Done

| Sub-component | Artifact | Tests |
|---|---|---|
| 1A SOR dispatch | `sor/dispatch.rs` + `MarketMakerEngine::dispatch_route` | ≥8 unit tests |
| 1B stat-arb dispatch | `StatArbDriver::try_dispatch_legs_for_entry/exit` + engine call sites | ≥6 unit tests |
| Integration | `mod stage2_track1_integration` in market_maker.rs | ≥2 e2e tests |
| Quality | `cargo test -p mm-engine`, `cargo test -p mm-strategy`, `cargo clippy -p mm-engine --all-targets -- -D warnings`, `cargo clippy -p mm-strategy --all-targets -- -D warnings`, `cargo fmt -p mm-engine`, `cargo fmt -p mm-strategy` | all green |

DO NOT run `cargo test --workspace` — parallel tracks are running.
