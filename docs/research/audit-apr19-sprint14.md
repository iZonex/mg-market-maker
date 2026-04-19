# Sprint 14 Audit Findings — Apr 19

> The operator said "I feel it doesn't work but can't prove it" — this
> doc is the record of what we actually traced end-to-end. Keep it for
> future audit passes so we don't re-verify the same paths twice.

## Method

Five critical data paths from Epics R2 / R3 / R4 / R7 were traced end
to end with an adversarial stance (find real holes, not "looks ok").
Each path was checked at: producer writes, transport delivers, consumer
reads, output hits graph / dashboard / behaviour.

## Results — 2 REAL bugs, 3 FALSE POSITIVES

### REAL BUG #1 — env-var gate drift between dashboard + graph

**Found by:** Sprint 14 R8.2 integration test could not compile
`pentest-liquidation-cascade` with what the docs claimed was the right
env var.

**Root cause:**
- `crates/strategy-graph/src/graph.rs:480` — graph evaluator checks
  `MM_ALLOW_RESTRICTED == "yes-pentest-mode"`.
- `crates/dashboard/src/server.rs:1814` (pre-fix) — dashboard deploy
  handler checked `MM_RESTRICTED_ALLOW` (different name!)
  `== "1"` (different value!).
- `docs/guides/pentest.md` (pre-fix) — told operators to
  `export MM_RESTRICTED_ALLOW=1`.

**Impact:** operator following the docs would set the env var
the DASHBOARD gate accepts but the EVALUATOR rejects. Deploy would
either:
  - Pass the dashboard's 403 gate but fail at `Evaluator::build`
    with `RestrictedNotAllowed`, OR
  - Fail at the dashboard gate before ever reaching the evaluator.
Either way, the pentest suite was effectively unreachable without
reading the source. This is exactly the "I feel it doesn't work"
symptom.

**Fix:** consolidated on `MM_ALLOW_RESTRICTED=yes-pentest-mode` (the
more explicit value, already honoured by the evaluator). Dashboard
server fixed; every docstring + template description + pentest.md
fixed via `sed`. The new E2E test
(`crates/strategy-graph/tests/pentest_templates_e2e.rs`) exercises
the real gate end-to-end so this cannot regress silently.

### REAL BUG #2 — `Strategy.CascadeHunter` couldn't propagate pool output

**Found by:** same E2E test expected `Out.Quotes` to fire when
`trigger` was true and found `SinkAction::Quotes` absent.

**Root cause:** the engine's strategy-pool overlay at
`crates/engine/src/market_maker.rs:4414` injects
`(node_id, "quotes")` values into `source_inputs` for every
`Strategy.*` node. The graph evaluator only consults
`source_inputs` for SOURCE nodes (zero input ports). Input-having
nodes go through `evaluate()` which for CascadeHunter returned
`Value::Missing`. Net effect: CascadeHunter in the template
always emitted `Missing`, Quote.Mux input `a` never populated,
`Out.Quotes` never fired.

**Impact:** the flagship Sprint 13 pentest template
(`pentest-liquidation-cascade`) was a no-op — it compiled,
deployed, and did nothing. Same shape would have broken any
future input-having pool-backed Strategy node.

**Fix:** `Strategy.CascadeHunter` refactored to a pure source
node (zero inputs). Triggering semantics moved downstream via
`Quote.Mux` — the template now wires
`LongShortRatio.ratio → Cast.ToBool → Quote.Mux.cond`,
`CascadeHunter.quotes → Quote.Mux.a`, `Quote.Mux.quotes →
Out.Quotes`. The E2E test confirms both sides of the mux fire
correctly (high ratio → quotes emit, low ratio → quotes suppress,
RugScore trips kill independently).

## FALSE POSITIVES — paths that actually work

### ✓ Path 1 — Liquidation heatmap populates from real WS

Binance / Bybit / HyperLiquid parsers all emit
`MarketEvent::Liquidation`; engine handler at
`market_maker.rs:7091` routes to `liquidation_heatmap.on_liquidation()`;
overlay at `market_maker.rs:4054` exposes the heatmap state as graph
source values. All three venues' subscribe codepaths add the
liquidation topic to the WS subscription list. Clean E2E wiring.

### ✓ Path 2 — `spawn_leverage_setup` logs on success

Initial auditor concern: "tokio::spawn fire-and-forget might not log
anything". Not true — `market_maker.rs:2052` has an
`info!("⚠ Strategy.LeverageBuilder set account leverage …")`
on the success branch and a matching `warn!` on the error branch
(line 2060). Forensic trail exists.

### ✓ Path 5 — `CampaignOrchestratorStrategy` wired (not advisory stub)

Verified `build_strategy_pool` at `market_maker.rs:2518` constructs
the real `CampaignOrchestratorStrategy::with_config(oc)` (not the
Sprint 10 `IgniteStrategy::default()` placeholder). The strategy's
`compute_quotes` branches by phase correctly; three unit tests pin
the FSM timeline.

### ✓ Path 3 — `Signal.OpenInterest` reads real OI (partial)

`refresh_funding_rate` calls `get_open_interest` + stores on
`last_open_interest`. Overlay prefers real value over proxy.
Auditor flagged "gated inside `has_funding()`" as a potential
hole — but OI + long/short ratio are perp-only concepts, so the
gate is by design. Honest: a spot template that reads
`Signal.LongShortRatio` will always see `Missing`, which is
correct fail-open semantics.

## Takeaways for future sprints

- **E2E integration tests are the only way to catch gate drift.**
  Unit tests on the detector + unit tests on the sink didn't
  overlap; the drift hid between them. Keep the
  `pentest_templates_e2e` harness honest — add one test per new
  bundled template.
- **Input-having `Strategy.*` nodes break the pool-overlay
  contract.** Don't add more of them. `Strategy.QueueAware` is the
  documented exception that runs its transform in `evaluate()` and
  doesn't rely on the pool.
- **Env-var gates must be consolidated at one site.** A future
  refactor could put the gate logic in `mm-common::config` so no
  code path can drift again.
