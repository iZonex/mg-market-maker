# Visual strategy builder — architecture

> Node-editor for composing market-maker strategies — analogous to Blender's
> shader graph, After Effects' node compositor, N8N's workflow editor. An
> operator (or quant) drags typed data sources + transforms + risk outputs
> onto a canvas, wires them up, validates, deploys. The engine evaluates
> the resulting DAG on every tick; the outputs feed the existing autotuner
> multiplier / skew / kill pipeline — no replacement of the engine, just a
> programmable front-end to the same knobs.

Status: **architecture draft**, zero code yet. MVP phasing at the end.

---

## 0. Why

Today each signal path (momentum, toxicity, social, news-retreat, lead-lag,
market-resilience) is hand-wired in `crates/engine/src/market_maker.rs`. Adding
a new fusion rule means touching 3–5 Rust files and shipping a new binary.
The logic a strategy needs — `if sentiment_high AND ofi_confirms THEN
widen_1.5x + skew_+5bps, ELSE IF vol_quiet AND imbalance_small THEN
tighten_0.8x` — is simple to draw but awkward to encode.

Visual composition solves three real problems:

1. **Rate of iteration** — change a threshold or rewire a branch without a
   binary rebuild. Hot-reload via the existing `ConfigOverride` channel.
2. **Communicate strategy** — quants + operators + compliance look at the
   same picture; no translation between a rendered explanation and the
   actual code path. Diff is visual.
3. **Safe experimentation** — per-strategy-class graphs, A/B split at the
   graph level, per-client graphs, per-venue graphs. Versioned, audited,
   reverted with one click.

---

## 1. Conceptual model

### 1.1 Graph

A **strategy graph** is a directed acyclic graph (DAG) of typed nodes. Each
graph is associated with a scope:

```text
Scope = Symbol | AssetClass | Client | Global
```

Evaluation order is a topological sort of reachable nodes from the graph's
**required outputs** (at least one `SpreadMult` sink — see §3.6).

### 1.2 Node

```rust
struct Node {
    id: NodeId,            // stable UUID
    kind: NodeKind,        // from a closed catalog
    config: Value,         // JSON — node-specific params
    pos: (f32, f32),       // canvas position (UI-only)
    inputs: Vec<Edge>,     // incoming connections
}
```

Each `NodeKind` declares:

- `input_ports()` — list of `(name, PortType)`
- `output_ports()` — same
- `evaluate(ctx, inputs) -> outputs` — pure function of inputs + config +
  node-local state (persisted across ticks)

### 1.3 Port types

```text
Number      // Decimal
Bool
Series      // bounded window of (ts, Decimal) pairs
Enum<E>     // closed set — e.g. KillLevel, Regime, SentimentSignal
Book        // L2 snapshot reference (read-only, zero-copy)
TradeTick   // last trade event
String      // labels / tags
Unit        // explicit "trigger" signal with no payload
```

Edges must connect same-typed ports. Casts are explicit nodes
(`SeriesToNumber`, `NumberToBool` via threshold, etc.).

### 1.4 Edges

```rust
struct Edge {
    from: (NodeId, OutputName),
    to:   (NodeId, InputName),
}
```

No edge may create a cycle (validated). No input port may have two incoming
edges (fan-in uses explicit `Merge` nodes — `Min`, `Max`, `First`, etc.).
Fan-out is unrestricted.

---

## 2. Node catalog (MVP)

Grouped the way the UI palette will display them.

### 2.1 Sources (read engine state)

| Node | Outputs | Notes |
|---|---|---|
| `Book.L1` | `bid_px, bid_qty, ask_px, ask_qty, mid, spread_bps` | From `BookKeeper` |
| `Book.Imbalance` | `Number` | Top-of-book qty imbalance |
| `Inventory.Level` | `Number` | Base-asset inventory |
| `Inventory.Value` | `Number` | Quote-asset value |
| `Volatility.Realised` | `Number` | EWMA annualised |
| `Toxicity.VPIN` | `Number` | From `VpinEstimator` |
| `Toxicity.KyleLambda` | `Number` | |
| `Momentum.OFIZ` | `Number` | Epic G — the score we just added |
| `Momentum.HMA` | `Series` | Hull MA stream |
| `Sentiment.Rate` | `Number` | `mentions_rate` per asset |
| `Sentiment.Score` | `Number` | EWMA score |
| `Sentiment.Delta` | `Number` | |
| `Risk.KillLevel` | `Enum<KillLevel>` | Current kill switch state |
| `Risk.MarginRatio` | `Number` | Perp only |
| `Funding.Rate` | `Number` | Perp only |
| `Clock.UtcHour` | `Number` | For session-based gating |
| `Hedge.Mid` | `Number` | Cross-venue leader |

### 2.2 Transforms (stateless or simple state)

| Node | Inputs → Outputs | |
|---|---|---|
| `Math.Add` / `Sub` / `Mul` / `Div` | `Number, Number → Number` | |
| `Math.Abs` / `Sign` / `Neg` | `Number → Number` | |
| `Math.Clamp` | `Number, min:Number, max:Number → Number` | |
| `Math.LinRamp` | `Number, x0, x1, y0, y1 → Number` | Piecewise-linear |
| `Stats.EWMA` | `Number, α → Number` | State: previous |
| `Stats.ZScore` | `Number, window → Number` | State: deque |
| `Stats.Diff` | `Number → Number` | State: previous |
| `Series.Window` | `Number, size → Series` | State: deque |
| `Series.Last` | `Series → Number` | Stateless read |
| `Cast.ToBool` | `Number, threshold, comparator → Bool` | `>`, `≥`, `<`, `≤`, `=` |

### 2.3 Logic + control flow

| Node | Purpose |
|---|---|
| `Logic.And` / `Or` / `Not` / `Xor` | Bool combinators |
| `Logic.Mux` | `cond:Bool, then:T, else:T → T` — ternary select |
| `Logic.Gate` | `Number, Bool → Number` (pass-through when Bool, else 0) |
| `State.Hysteresis` | `Number, on:Number, off:Number → Bool` — two-threshold latch |
| `State.Cooldown` | `Bool, duration:ms → Bool` — edge-triggered hold |
| `State.Transition` | `Enum → Unit` on any state change — fires exactly once per tick that transitions |

### 2.4 Aggregators

| Node | |
|---|---|
| `Agg.Mean` | `Series → Number` |
| `Agg.Std` | `Series → Number` |
| `Agg.Min` / `Max` | `Series → Number` |
| `Agg.Correlate` | `Series, Series → Number` — rolling ρ |
| `Agg.Count.Above` | `Series, threshold → Number` |

### 2.5 Sinks (write into engine multiplier pipeline)

| Node | Semantics |
|---|---|
| `Out.SpreadMult` | Floor at 1.0 by the autotuner. Multiple sinks compose by product. |
| `Out.SizeMult` | Clamped to `(0, 1]` by the autotuner. |
| `Out.SkewBps` | Additive skew on reservation price. Multiple sinks sum. |
| `Out.KillEscalate` | `level, reason:String → Unit` — calls `kill_switch.manual_trigger` |
| `Out.Audit` | `String → Unit` — writes an `AuditEventType::Strategy` event |
| `Out.Metric` | `name, labels, value:Number → Unit` — Prometheus gauge write |

---

## 3. Execution semantics

### 3.1 Pull-based per engine tick

Every engine tick (`market_maker::run_symbol`'s hot loop) does one `graph.tick()`
call:

```text
for node in topological_order:
    inputs = [edge.source.output(port) for each input port]
    outputs = node.evaluate(ctx, inputs)
    cache outputs on the node
apply each Out.* sink to the engine (spread_mult, size_mult, skew_bps,
    kill_escalate, audit, metric)
```

Source nodes read from engine state directly (via a borrowed `&EngineState`
proxy struct — no allocation). Sinks write into `auto_tuner` / `kill_switch` /
`audit` just like the hand-wired paths do today.

Why pull-based, not push-based:
- Deterministic ordering (topological sort is unambiguous).
- One evaluation per tick — easy to reason about latency.
- Matches the engine's existing per-tick cadence.
- Push-based buys lower latency at a cost of ordering complexity;
  market-making latency is already bounded by the engine tick, so there's
  no headroom to gain.

### 3.2 Node state

Each node carries its own `NodeState` (EWMAs, deques, cooldown timers).
Persisted in memory only for MVP; serialised to a sidecar state file next
to the graph JSON in Phase 3 for graceful restarts mid-session.

### 3.3 Missing / stale inputs

Source nodes return `Option<Value>`. A `None` propagates through transforms
unchanged (other languages: "the null row"). A sink receiving `None` holds
its last-good output for the graph's configured `stale_hold_ms`; after
that it fails closed (multiplier → 1.0, skew → 0, no kill).

### 3.4 Validation (compile step)

Every graph save triggers `validate()`:

1. Port-type compatibility on every edge.
2. No cycles (DFS with color marks).
3. Reachability: at least one `Out.SpreadMult` reachable from sources.
4. Depth bound (≤ 128) — guards against pathological import.
5. Restricted-node gate (see §6).

A graph that fails validation is never evaluated. The previous valid graph
stays active.

### 3.5 Deterministic + reproducible

No random sources, no wall-clock reads inside nodes (except the one
explicit `Clock.UtcHour` source). Given the same input sequence on the
same graph, the output sequence is byte-identical. This is what makes the
graph **replay-able on a backtest**.

### 3.6 Fail-closed default

An operator who deletes the last `Out.SpreadMult` doesn't get a graph that
silently disables the MM's spread widening. Validation rejects that state.
At deploy time, if validation has any errors, the engine falls back to the
hand-wired default pipeline and surfaces a big red banner on the dashboard.

---

## 4. Storage format

```json
{
  "version": 1,
  "name": "btc-spot-conservative",
  "scope": { "kind": "Symbol", "value": "BTCUSDT" },
  "nodes": [
    { "id": "n1", "kind": "Sentiment.Rate", "pos": [120, 80], "config": {} },
    { "id": "n2", "kind": "Cast.ToBool",    "pos": [320, 80],
      "config": { "threshold": "5.0", "cmp": "ge" } },
    { "id": "n3", "kind": "Out.SpreadMult", "pos": [520, 80],
      "config": { "high": "1.5", "low": "1.0" } }
  ],
  "edges": [
    { "from": ["n1", "value"], "to": ["n2", "x"] },
    { "from": ["n2", "out"],   "to": ["n3", "cond"] }
  ]
}
```

Stored under `config/graphs/{name}.json`. Config references them:

```toml
[strategy_graph]
default = "btc-spot-conservative"
[strategy_graph.per_symbol]
ETHUSDT = "eth-spot-aggressive"
```

Version bump is required on any breaking schema change; the server refuses
to load a graph with an unknown version. Graphs hashes into SHA-256 and
the hash goes into the audit trail on every deploy.

---

## 5. Backend design

### 5.1 New crate `mm-strategy-graph`

```
crates/strategy-graph/
├── src/
│   ├── lib.rs          // public API + facade
│   ├── types.rs        // NodeId, Port, Edge, Value
│   ├── graph.rs        // Graph type + validation + topo sort
│   ├── nodes/          // one file per node kind
│   │   ├── sources.rs
│   │   ├── transforms.rs
│   │   ├── logic.rs
│   │   ├── aggregators.rs
│   │   └── sinks.rs
│   ├── evaluator.rs    // per-tick eval, state cache, sink dispatch
│   ├── storage.rs      // load/save JSON, version guard, hash
│   └── catalog.rs      // runtime registry — maps string kind → Box<dyn NodeKind>
└── Cargo.toml
```

`NodeKind` is a trait object on the evaluation path — closed set of
implementations, but dispatch happens through `Box<dyn NodeKind>` to keep
`evaluate_node(graph, id)` simple. Enum-dispatch optimisation is a
perf follow-up if profiling shows it matters (the whole graph should run
in < 50 μs/tick for a 50-node graph).

### 5.2 Engine integration

```rust
// crates/engine/src/market_maker.rs
pub fn with_strategy_graph(mut self, g: StrategyGraph) -> Self {
    self.strategy_graph = Some(g);
    self
}

// in the hot loop, AFTER the existing hand-wired signal updates:
if let Some(ref mut g) = self.strategy_graph {
    let source_ctx = SourceContext::from_engine(self);
    let sink_actions = g.tick(&source_ctx);
    for action in sink_actions {
        match action {
            SinkAction::SpreadMult(m)  => self.auto_tuner.set_graph_spread_mult(m),
            SinkAction::SizeMult(m)    => self.auto_tuner.set_graph_size_mult(m),
            SinkAction::SkewBps(b)     => self.social_skew_bps += b,
            SinkAction::KillEscalate { level, reason } => {
                self.kill_switch.manual_trigger(level, &reason);
            }
            SinkAction::Audit(tag)     => self.audit.risk_event(&self.symbol, AuditEventType::Strategy, &tag),
            SinkAction::Metric { .. }  => { /* emit */ }
        }
    }
}
```

A graph that's **not** configured is fully transparent — the engine runs
its existing hand-wired pipeline. A graph that *is* configured **layers on
top**, i.e. the graph's spread multiplier composes with the regime /
toxicity / news-retreat multipliers instead of replacing them.

### 5.3 Hot-reload

`POST /api/admin/strategy/graph` with `{ scope, json }` body:

1. Parse + validate.
2. On success, compute SHA-256 hash, write `config/graphs/<name>.json`,
   push `ConfigOverride::StrategyGraphSwap(bytes)` through the existing
   per-symbol override channel.
3. Engine builds new `StrategyGraph`, swaps the `Option<StrategyGraph>`
   atomically (drop old state machine, start fresh).
4. Audit-log the swap with `event_type = StrategyGraphDeployed`, detail =
   `{ name, hash }`. Hash + name end up on the monthly MiCA export too.

---

## 6. Restricted nodes (pentest path)

Per the user's explicit constraint (see `memory/project_restricted_strategies.md`):
some future strategies are for penetration testing of our own exchange. They
MUST not run on client accounts.

Mechanism:

- Every `NodeKind` declares `fn restricted() -> bool`. Catalog defaults to
  `false`. Predatory nodes (`Spoof.Layer`, `QuoteStuff.Burst`, etc.) return
  `true`.
- Validation rejects any graph containing a restricted node UNLESS
  `MM_ALLOW_RESTRICTED=yes-pentest-mode` env var is set AND
  `ClientConfig.jurisdiction != "client-facing"`.
- Every restricted-node evaluation writes an audit event with
  `AuditEventType::RestrictedNode`. No plausible deniability.
- CI test: the default `config.toml` MUST NOT reference any restricted
  graph. Catches "accidentally shipped" scenarios.

---

## 7. Frontend design

### 7.1 Library choice

Recommend **svelte-flow** (`@xyflow/svelte`, MIT, active). Gives us pan/zoom,
node selection, keyboard shortcuts, minimap, custom node renderers, typed
handles, and — critically — accessibility + touch support that hand-rolled
canvases always end up lacking. Svelvet is the alternative but has slower
release cadence.

### 7.2 Layout

```
┌──────────────────────────────────────────────────────────────┐
│ Top bar: graph name • scope picker • Save • Validate • Deploy│
├─────────┬────────────────────────────────────────┬───────────┤
│         │                                        │           │
│  Node   │            Canvas (svelte-flow)        │ Selected  │
│ palette │   (nodes + edges, live value chips     │ node      │
│ search  │    when preview mode is on)            │ config    │
│         │                                        │ form      │
│         ├────────────────────────────────────────┤           │
│         │  Validation + live preview log panel   │           │
├─────────┴────────────────────────────────────────┴───────────┤
│ Bottom: deploy history (graph hash, deployed_at, operator)    │
└──────────────────────────────────────────────────────────────┘
```

### 7.3 Live preview mode

Toggle in the top bar. When on, the engine enters a **shadow** mode for
the selected scope: it evaluates **both** the currently-deployed graph
*and* the draft on the canvas. Sinks from the draft are **discarded**
(nothing touches real orders) but every intermediate value streams back
over WS so each edge chip on the canvas shows the live number. Lets a
quant feel out thresholds on real market data without ever placing an
order.

### 7.4 Diff + history

Every deploy writes `{ graph_name, hash, operator, deployed_at, diff_from }`
to an append-only `data/strategy_deploys.jsonl`. The UI's history tab renders:

- Rollback button per historical deploy.
- Visual diff between two graphs (nodes added / removed / reconfigured /
  rewired) — rendered as two side-by-side canvases with colour coding.

---

## 8. Observability

- `mm_graph_eval_latency_us{graph}` — histogram per graph name.
- `mm_graph_nodes_total{graph, kind}` — gauge snapshot.
- `mm_graph_sink_fires_total{graph, sink}` — counter per sink type.
- `mm_graph_validation_errors_total{graph}` — counter.
- `mm_graph_active_version{graph}` — gauge of hash (first 8 hex chars as
  stable integer).

---

## 9. Compliance hooks

Cross-references to `docs/research/complince.md`:

1. **Audit trail** — graph deploy + every restricted-node firing lands on
   `data/audit.jsonl` with a new `AuditEventType` variant. Hash-chain
   carries through unchanged.
2. **Monthly export** — graph hashes + deploy timestamps ride with the
   existing monthly bundle. Regulator answering "what strategy was live
   on 2026-04-17 at 14:32 UTC?" goes via the deploy log.
3. **S3 archive** — `config/graphs/*.json` + `data/strategy_deploys.jsonl`
   both shipped on the shipper's cadence. Bundle ZIP can include the
   active graph JSON for that period.
4. **Bundle** — `summary.json` in the compliance bundle gains a
   `strategy_graph: { name, hash, deployed_at }` field.

---

## 10. Phasing

### Phase 1 — **MVP** (2–3 sprints)

Goal: someone can author a trivial graph (single `Sentiment.Rate >
threshold → SpreadMult = 1.5x`) via JSON file, deploy via `POST`, see it
widen the engine's quotes in paper mode.

- [ ] `mm-strategy-graph` crate scaffold + types + validation
- [ ] 15 nodes: `Book.L1`, `Sentiment.Rate`, `Sentiment.Score`,
      `Volatility.Realised`, `Toxicity.VPIN`, `Momentum.OFIZ`, `Math.Mul`,
      `Math.Add`, `Stats.EWMA`, `Cast.ToBool`, `Logic.And`, `Logic.Mux`,
      `Out.SpreadMult`, `Out.SizeMult`, `Out.KillEscalate`
- [ ] Pull-based evaluator
- [ ] JSON storage + hash
- [ ] Engine integration — `with_strategy_graph`, sink dispatch, layered
      multiplier pipeline
- [ ] `POST /api/admin/strategy/graph` endpoint
- [ ] Audit event type + metric set
- [ ] 50+ unit tests on individual nodes; 10+ integration tests on eval

### Phase 2 — **frontend canvas** (2 sprints)

- [ ] svelte-flow integration on a new `StrategyPage.svelte` route
- [ ] Node palette driven by backend `GET /api/v1/strategy/catalog`
- [ ] Config form auto-generated from node JSON schema
- [ ] Save / load / validate / deploy buttons wired to backend
- [ ] Live preview mode + shadow evaluation

### Phase 3 — **operational polish** (1–2 sprints)

- [ ] Deploy history + diff viewer + rollback
- [ ] Per-pair-class template graphs (match `PairClass` classifier)
- [ ] Backtest integration: replay a historical event log through a draft
      graph, compute resulting quotes, produce a PnL delta report
- [ ] Node state serialisation (graceful restart preserves EWMAs)

### Phase 4 — **restricted nodes + pentest loop**

- [ ] `restricted = true` mechanism in catalog
- [ ] `MM_ALLOW_RESTRICTED` env gate
- [ ] Predatory node set (spec-first; implementations live out of tree
      until user hands them over)
- [ ] CI test forbidding restricted graphs in default config

### Phase 5 — **community + sharing** (speculative)

- [ ] Graph export to shareable `.mmg` bundle (graph JSON + screenshot +
      meta)
- [ ] Central operator-scoped catalog of template graphs
- [ ] Sandbox accounts for trying other operators' graphs on paper fills
      only — never touches a live account

---

## 11. Open questions

1. **Multi-symbol aggregator nodes** — a `Portfolio.InventoryValue` node
   reads across symbols. Does that force the graph scope to `Global`, or
   can a symbol-scoped graph legally read portfolio-level state? Leaning
   toward: yes, reading is legal, writing is not; the node declares
   `READ_ONLY_PORTFOLIO`.

2. **Graph-to-graph composition** — at what complexity do we need a
   `Subgraph` node (include another graph as a reusable unit)? Probably
   Phase 3, and the container-of-graphs pattern is worth a design pass
   before shipping it — the N8N "workflow that calls another workflow"
   path has real footguns (recursion, cycle detection across graphs).

3. **Execution model for event-driven strategies** — some strategies
   genuinely want to react to *each* trade event (spoof detection,
   latency arb). MVP pull-based won't serve those. Push-based extensions
   are a Phase 4+ item and will likely involve a separate node category
   (`Trigger.*`) with different eval semantics.

4. **Hyperopt integration** — the existing `mm-hyperopt` crate optimises
   numeric parameters. Can it optimise graph-embedded thresholds? The
   obvious path is to expose `NodeConfig` values as a search space and
   let hyperopt treat a graph as a parameterised function. Solvable but
   non-trivial — the search space is combinatorial, not continuous.

5. **Who can deploy?** — roles today are admin / operator / viewer.
   Strategy deploy should probably be admin-only in Phase 1 and gain a
   new `strategist` role in Phase 3 that can deploy to non-live scopes
   only (paper + pentest).

---

## 12. What this is NOT

- Not a replacement for `mm-strategy` strategies (Avellaneda, GLFT, Grid,
  XEMM, ...). Those are the **base strategy**. The graph sits on top and
  shapes the base strategy's output via multipliers / skew / kill.
- Not a general-purpose scripting language. Graphs have a closed node
  catalog; adding new node kinds is a Rust PR, not a user action.
- Not a free-form dashboarding tool. The canvas is for *strategy* logic
  only, not for plotting.
- Not a trading bot IDE in the Pine-Script sense. It's lower-level than
  Pine (node-granular) and higher-level than Rust (typed DAG instead of
  free code).

---

## Appendix A — worked example

Sketch of the "Sentiment crowd + OFI confirm → widen & skew" strategy
from §3 of the user's architecture notes, expressed as a graph:

```text
[Sentiment.Rate] ──► [Cast.ToBool: ≥ 3] ─┐
                                          ├─► [Logic.And] ──► [Logic.Mux]
[Momentum.OFIZ] ─► [Math.Abs] ────────────┘                      │  then: 1.5
                                                                 │  else: 1.0
                                                                 ▼
                                                          [Out.SpreadMult]

[Sentiment.Score] ─► [Math.Sign] ─► [Math.Mul : × 10] ─► [Out.SkewBps]
```

Validation: one `Out.SpreadMult`, one `Out.SkewBps`, no cycles, all edges
typed. Deploys. The engine's `auto_tuner.effective_spread_mult()` now
picks up the graph's 1.5× factor whenever the Sentiment crowd + OFI cross-
validation trips.

Written in the strictly-Rust form this is ~80 LOC in `market_maker.rs`. As
a graph it's 6 nodes + 5 edges, auditable at a glance, iterable without
recompile.

---

## Appendix B — node-state example

`Stats.EWMA` in Rust form:

```rust
#[derive(Default)]
struct EwmaState { prev: Option<Decimal> }

impl NodeKind for EwmaNode {
    fn evaluate(&self, ctx: &EvalCtx, inputs: &[Value], state: &mut NodeState) -> Vec<Value> {
        let x: Decimal = inputs[0].as_number();
        let alpha: Decimal = self.config.alpha;
        let st = state.get_or_insert_default::<EwmaState>();
        let out = match st.prev {
            None => x,
            Some(p) => alpha * x + (dec!(1) - alpha) * p,
        };
        st.prev = Some(out);
        vec![Value::Number(out)]
    }
}
```

Everything else in the catalog follows the same shape: pure function of
`(inputs, config, mutable local state)`.
