# Strategy-graph observability — design

Operator problem: we have a visual graph builder and a runtime
that executes graphs on live data, but **we can't see what flows
through each node and edge while a deployment is running**.
Authoring works, deploy works, but the feedback loop between
"I drew this" and "is it actually firing the way I think" is
missing. Operators are blind-debugging from kill_level + output
scalars.

This doc pins down the architecture, wire contracts, UI surface,
and milestone breakdown for closing the loop.

## Hard boundary: Prometheus vs. our UI

Per-node / per-edge / per-tick trace data lives **only in our UI
via the `details_store` → HTTP pipeline**. Prometheus carries
**system metrics only**:

| Prometheus (stays)               | Our UI / details_store (new)      |
|----------------------------------|-----------------------------------|
| `mm_strategy_graph_deploys_total`| per-node inputs / outputs         |
| `mm_strategy_graph_nodes`        | per-edge live values              |
| `mm_regime` / `mm_inventory`     | per-tick fired sinks              |
| tick latency, venue latency p95  | skipped nodes, hit rate           |
| deploy accepted/rejected         | sparklines, tick timeline         |
| (low cardinality, SRE view)      | (high cardinality, product view)  |

Rationale: node-level trace would explode Prometheus cardinality
(`node_id` × `port` × `symbol` × `deployment`), and the consumer
is a strategy author inside the app — not an SRE dashboard.
Different retention, different sampling, different UX.

## Operator workflows (what we're solving)

```
1. Author     — "what does this node actually produce on real ticks?"
2. Tuner      — "I changed γ; what moved downstream?"
3. Debugger   — "why did tick T=17:34:21 escalate to L4?"
4. Reviewer   — "is this graph valid? unused outputs? dead branches?"
5. Architect  — "v1 → v2: same input, where do outputs diverge?"
6. Risk       — "which source-detectors does this graph actually use?"
```

Plan below closes **4 of 6** in the first three milestones; 5 and
the deepest part of 3 land later.

## Architecture

```
                       ┌─ ENGINE TICK (market_maker.rs:~8963) ────────────┐
                       │                                                   │
                       │   refresh_quotes()                                │
                       │     └─ tick_strategy_graph()                      │
                       │          └─ Evaluator::tick_with_trace()          │
                       │                    ↑                              │
                       │       (already computes per-tick                  │
                       │        outputs HashMap<(NodeId,Port), Value>      │
                       │        — currently discarded after tick)          │
                       │                    │                              │
                       │                    ▼                              │
                       │   Evaluator::take_trace() → TickTrace             │
                       │                    │                              │
                       └────────────────────┼──────────────────────────────┘
                                            ▼
                       ┌─ DETAILS STORE (dashboard/details_store.rs) ─────┐
                       │   graph_traces: HashMap<Symbol, RingBuffer<…>>   │
                       │   replace-on-tick, capped at N (config)          │
                       └────────────────────┼──────────────────────────────┘
                                            │
                                     (agent reads locally)
                                            ▼
                       ┌─ AGENT (agent/src/lib.rs) ───────────────────────┐
                       │   FetchDeploymentDetails("graph_trace_recent")   │
                       │   → details_store.graph_traces(symbol)           │
                       │   → DetailsReply { payload: {traces: [..]} }     │
                       └────────────────────┼──────────────────────────────┘
                                            │
                                     (WS, request_id correlated)
                                            ▼
                       ┌─ CONTROLLER (controller/src/http.rs) ────────────┐
                       │   GET /api/v1/agents/:a/deployments/:d/          │
                       │       details/graph_trace_recent                 │
                       │   → fan-out via registry.pending_details_*       │
                       │   → 5s timeout, envelope returned unchanged      │
                       └────────────────────┼──────────────────────────────┘
                                            │
                                     (operator's browser)
                                            ▼
                       ┌─ UI (StrategyPage.svelte Live mode) ─────────────┐
                       │   poll 2s while canvas open, map payload.traces  │
                       │   onto decorateEdges() + node badges             │
                       │   sidebar inspector reads history per node       │
                       └──────────────────────────────────────────────────┘
```

Key property: **no new protocol, no Prometheus touch**. This is
additive at engine (take existing intermediate map instead of
throwing it away), ring-buffer (one new HashMap field), agent
(one match arm), controller (zero changes — generic handler),
UI (new mode in existing page).

## Wire contract

### New details topic: `graph_trace_recent`

Request: existing `FetchDeploymentDetails { topic: "graph_trace_recent", args: { limit?: u32 } }`.

Response payload:
```json
{
  "graph_hash": "a1b2c3…",
  "graph_name": "my-strategy",
  "window_size": 256,
  "traces": [
    {
      "tick_ms": 1713891234567,
      "tick_num": 182931,
      "elapsed_ns": 142000,
      "nodes": [
        {
          "id": "n7",
          "kind": "Surveillance.RugScore",
          "inputs": [["vpin", 0.23], ["book_depth", 184.5]],
          "output": 0.18,
          "status": "ok",
          "elapsed_ns": 2100
        },
        { "id": "n8", "status": "skipped_no_input" },
        { "id": "n9", "status": "error", "error": "divide by zero" }
      ],
      "edges": [
        { "from": "n7", "to": "n12", "port": "score", "value": 0.18 }
      ],
      "sinks_fired": [
        { "node_id": "n14", "kind": "Out.KillEscalate", "value": "WidenSpreads" }
      ],
      "skipped": ["n20", "n21"]
    }
  ]
}
```

### New details topic: `graph_analysis`

Static, computed once on graph load. Cheap.

```json
{
  "graph_hash": "a1b2c3…",
  "depth_map": { "n1": 0, "n2": 1, … },
  "required_sources": ["Book.L1", "Surveillance.RugScore", …],
  "dead_nodes": [],
  "unconsumed_outputs": [
    { "node_id": "n5", "port": "secondary_score" }
  ],
  "cycles": []
}
```

Returned without roundtrip to agent — could cache at controller
per `graph_hash`, or (simplest) served alongside the first
`graph_trace_recent` response as a second key.

## Engine instrumentation plan

1. **Add `TickTrace` capture flag to `Evaluator`.**
   - Off by default. Turns on when any subscriber requests.
   - Zero cost when off (HashMap already exists for `outputs`).

2. **Add `Evaluator::take_trace(&mut self) -> Option<TickTrace>`.**
   - Called from `tick_strategy_graph()` post-tick.
   - Moves the already-computed outputs map + metadata into a
     `TickTrace`, returns it, replaces internal with empty.

3. **Add `DeploymentDetailsStore::push_graph_trace(symbol, TickTrace)`.**
   - Per-symbol ring buffer, default depth 256 ticks.
   - Bounded: when full, drop oldest.

4. **Add static analysis at swap.**
   - In `swap_strategy_graph()` at `market_maker.rs:2971`, after
     `Evaluator::build()` succeeds, run:
     - `compute_depth_map(&graph)`
     - `compute_required_sources(&graph)`
     - `compute_dead_nodes(&graph)`
     - `compute_unconsumed_outputs(&graph)`
   - Store result in `DeploymentDetailsStore::graph_analysis[symbol]`.
   - Runs once per swap, not per tick.

5. **Don't touch Prometheus.** Only exception: existing
   `mm_strategy_graph_deploys_total` + `mm_strategy_graph_nodes`
   stay. No new Prom series.

## Transport plan

- **One new topic** `graph_trace_recent` in `agent/src/lib.rs` `FetchDeploymentDetails` match.
- **One new topic** `graph_analysis` (same file).
- `control/messages.rs` — **zero changes**. Topic is opaque string.
- `controller/http.rs` — **zero changes**. Generic `/details/{topic}` handler passes through.
- Authorization — **zero changes**. Inherits `internal_view` gate
  (admin/operator/viewer), ClientReader blocked at middleware.

## UI plan

### Modes in StrategyPage

Two modes, toggle in top bar:

```
┌───────────────────────────────────────────────────────────┐
│  Strategy · my-fund-arb            [Authoring] [●Live]    │
│  ────────────────────────                                 │
│  Validate: 23 nodes · 31 edges · 4 sinks · ok             │
├───────────────────────────────────────────────────────────┤
│ Palette ┊            Canvas              ┊  Inspector     │
│         ┊  ┌─n1─┐                       ┊  Node n7        │
│         ┊  │BookL1│                     ┊  Surv.RugScore  │
│         ┊  └──┬─┘ 0.512 (14 ticks)      ┊                 │
│         ┊     ▼                          ┊  last output   │
│         ┊  ┌─n7─┐●  ← pulse on fire      ┊     0.18 ▲     │
│         ┊  │RugScore│                   ┊  history:       │
│         ┊  └──┬─┘ 0.18                  ┊  [sparkline]    │
│         ┊     ▼                          ┊                 │
│         ┊  ┌─n12─┐  ← grey = dormant    ┊  hit rate: 94%  │
│         ┊  │ThresBool│                  ┊  avg elapsed:   │
│         ┊  └────┘                        ┊     2.1 µs     │
├───────────────────────────────────────────────────────────┤
│ Timeline: [─────●─────────────] 17:34:21 kill L4 pinned   │
│           tick -60  -40  -20    now                       │
└───────────────────────────────────────────────────────────┘
```

**Authoring mode** (current behaviour, no change): palette, drag,
validate, preview-tick, save, deploy, rollback.

**Live mode** (new):
- Requires selecting a deployed graph (modal or auto-bound if
  opened from DeploymentDrilldown).
- Polls `graph_trace_recent` every 2 s while canvas visible; stops on hide.
- Loads `graph_analysis` once on enter.

### Canvas overlay layers

1. **Node badge** (new on StrategyNode.svelte):
   - Latest `output` value under node name.
   - 200ms pulse animation on tick-fire.
   - Status chip: `error` red, `skipped` grey diagonal, `ok` nothing.
   - Dormant (in graph but never fires within window): muted diagonal stripe.

2. **Edge decoration** (extends existing `decorateEdges()`):
   - Label: current value (monospace).
   - Thickness: proportional to fire-rate in last 20 ticks.
   - Color: heatmap by value range per-port-type.

3. **Dead-node indicator** (from `graph_analysis.dead_nodes`):
   - Red dashed border, tooltip "no path to any sink".

### Inspector sidebar (new — replaces ConfigPanel when Live mode on)

Per-selected node:
- Last 20 outputs as sparkline.
- Min / max / mean / stddev over window.
- Hit rate % (how many ticks this node fired).
- Average `elapsed_ns`.
- "Open config" (switch back to Authoring on this node).

### Timeline (new — bottom panel)

- X-axis: last 256 ticks.
- Overlay traces: kill_level, combined manipulation score, key scalars.
- Click tick → snapshot that tick across all nodes (time-travel).
- Pin marker: arrives as URL param from Incidents page (`?tick=…`).

### Validation panel (extends existing)

Current: syntax errors + issue list.
Adds:
- `Surveillance.RugScore connected but unused — remove or wire?` (orange).
- `node "n14" has no path to any sink — dead branch` (red).
- `node "n22" never fired in the last 60 ticks since deploy` (blue informational, live-mode only).

### Entry points into Live mode

- **Sidebar route**: `/strategy` stays Authoring by default.
- **DeploymentDrilldown** → new button "Open graph (live)" →
  opens StrategyPage with `?live=<deployment_id>`.
- **Incidents** → for graph-escalated kill events, new button
  "Open graph at incident" → StrategyPage with
  `?live=<deployment_id>&tick=<tick_ms>`.

## Validation beyond syntax

Today `POST /api/v1/strategy/validate` checks schema + cycles.
After this work, it also returns (all cheap, run on graph build):

- `required_sources: [SourceKind]` — feed UI palette "active" badge.
- `dead_nodes: [NodeId]` — UI red border.
- `unconsumed_outputs: [(NodeId, Port)]` — UI orange info.

No new endpoint. Extends existing validate response.

## Lazy-detector gating (user concern)

After static analysis lands, engine gets:
```rust
if !self.required_sources.contains(&SourceKind::Surveillance) {
    // Skip ManipulationScoreAggregator::tick()
}
```

Each detector (manipulation, on-chain scores, wash detector, etc.)
checks whether its source kind is in the `required_sources` of
the current graph. If not — doesn't run. CPU saved, honest
architecture (what's in the graph is what runs).

**Note**: this lives in M6, after visibility is built. Turning
off detectors before operators can see their graphs fire is too
risky.

## Milestones

```
M1  — Engine telemetry + ring buffer + two new topics          2 days
      Evaluator::take_trace, DeploymentDetailsStore ext,
      agent match arms, static analysis at swap.
      Goal: `curl /details/graph_trace_recent` returns sane JSON.

M2  — UI Live mode: overlay on canvas + inspector sidebar      3 days
      Node badges, edge decoration (extend decorateEdges),
      dormant indicator, inspector sparklines.
      Goal: open deployed graph, see values flow in real time.

M3  — Extended validation + dead-node detection                1 day
      Extends /validate response, UI shows dead/unconsumed/
      dormant warnings. No new endpoint.
      Goal: operator sees orange/red issues pre-deploy.

M4  — Timeline + time-travel snapshot                          2 days
      Scroll back N ticks, click to pin, snapshot overlay.
      Incidents → "open at tick" deep link.
      Goal: post-incident debug from graph state at T.

M5  — Diff / replay v1 vs v2                                   3+ days
      Deterministic replay on captured source inputs, side-by-side.
      Deferred: needs fixed feature-snapshot persistence.

M6  — Lazy-detector gating                                     1 day
      required_sources filter in engine tick. CPU savings
      + honest "if not in graph, doesn't run".
      Depends on M3 static analysis landing.
```

**M1–M3 is the MVP** — 6 days of work, closes operator stories
1, 2, 4, 6 fully and story 3 partially.

## Open questions

1. **Ring buffer depth** — 256 ticks (~2 min at 2Hz) default.
   Configurable via tunable. Memory per deployment ~500 KB max.
2. **Trace enablement** — on-demand (agent turns on when
   controller requests first `graph_trace_recent`) or always-on?
   Leaning on-demand for zero cost when no UI is watching, but
   costs 1 tick of latency at subscribe time.
3. **Persistence** — on incident, should we snapshot last 60
   ticks into audit? Probably yes, phase into M4 with Timeline.
4. **Editing in Live mode** — view-only, with "Fork to author"
   that copies current graph into a draft in Authoring mode.
   Do not edit-in-place a running deployment's graph.
5. **Diff-only trace** — if a node's output equals prev tick,
   omit from payload. Reduces wire size ~60% typical.
   Implement in M2 if payload size becomes an issue.

## What this explicitly does NOT do

- **No Prometheus metrics for trace.** Hard boundary.
- **No auto-trip based on manipulation / other detectors.** Graph
  is the contract. If operator wants kill on score, they wire
  `Score → Threshold → Out.KillEscalate` explicitly.
- **No graph editor in Live mode.** View-only to prevent
  accidental modification of a running strategy.
- **No historical trace storage outside the ring buffer** (until
  M4 adds incident-scoped snapshot).
