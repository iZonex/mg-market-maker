# Strategy Graph Authoring Guide

How to compose a market-making strategy in the graph editor — no Rust required for most strategies.

A **strategy graph** is a DAG of typed nodes connected by edges. Every engine tick:
1. Source nodes produce values (mid price, inventory, trade tape, risk scores...)
2. Values flow along edges into downstream nodes (math, stats, logic, strategies)
3. Sink nodes produce **actions** (autotuner multipliers, kill-switch escalations, quote bundles)

You author graphs in the **Strategy** page of the dashboard, save them as versioned templates, deploy them to one or many `(agent, deployment)` targets, then watch them tick in Live mode.

---

## When to use a graph vs a Rust strategy

| Pick graph | Pick Rust `Strategy` trait |
|-----------|---------------------------|
| Combining existing signals into a custom policy | Inventing a new quoting algorithm |
| Per-client customization without recompiling | Hot-path code that needs zero allocation |
| Operator wants to author / tweak live | Stateful execution (multi-tick FSMs beyond what graph primitives offer) |
| A/B testing a spread-multiplier rule | The full signal extraction (e.g. a new toxicity metric) |

Graphs compose existing sources + computation + sinks. If the behaviour you want needs a new source kind, add it in `crates/strategy-graph/src/nodes/sources.rs` first — then graphs can use it.

---

## The canvas

**Palette** (left) — drag nodes onto the canvas. Palette groups:

| Group | Examples |
|-------|----------|
| **Sources** | `Book.L1`, `Book.L2`, `Trade.Tape`, `Trade.OwnFill`, `Funding`, `Balance`, `Portfolio.NetDelta`, `Volatility.Realised`, `Toxicity.VPIN`, `Toxicity.KyleLambda`, `Momentum.OFIZ`, `Risk.MarginRatio`, `Risk.OTR`, `Sentiment.Score`, ~40 more |
| **Math** | `Math.Add`, `Math.Mul`, `Math.Const` |
| **Stats** | `Stats.EWMA` |
| **Cast** | `Cast.ToBool`, `Cast.StrategyEq`, `Cast.PairClassEq` |
| **Logic** | `Logic.And`, `Logic.Mux` |
| **Indicators** | `Indicator.SMA`, `Indicator.EMA`, `Indicator.HMA`, `Indicator.RSI`, `Indicator.ATR`, `Indicator.Bollinger` |
| **Signals** | `Signal.ImbalanceDepth`, `Signal.TradeFlow`, `Signal.Microprice`, `Signal.FillDepth` |
| **Strategies** (node-pool) | `Strategy.Avellaneda`, `Strategy.GLFT`, `Strategy.Grid`, `Strategy.Basis`, `Strategy.CrossExchange`, plus pentest variants |
| **Exec / Plan** | exec-algorithm composites (TWAP, VWAP, POV, Iceberg) and plan nodes |
| **Risk sources** | `Risk.CircuitBreaker`, `Risk.ToxicityWiden`, `Risk.InventoryUrgency`, `Risk.NewsRetreatState`, `Risk.LeadLagMultiplier` |
| **Surveillance** ⚠️ | `Surveillance.ManipulationScore`, `Surveillance.SpoofingScore`, `Surveillance.LayeringScore`, etc. — paired with detector-only graphs on the defence side |
| **Onchain** | `Onchain.HolderConcentration`, `Onchain.SuspectInflowRate` |
| **Sinks** | `Out.SpreadMult`, `Out.SizeMult`, `Out.KillEscalate`, `Out.Quotes`, `Out.AtomicBundle` |

Drag a node → it drops onto the canvas → click it → Config panel opens on the right.

**Edges** — hover an output port (right side of a node) until it highlights, drag to an input port on another node. Port colors indicate type:
- **Number** — green. Most common (Decimal-valued).
- **Bool** — orange. For gating (`Logic.And`, `Logic.Mux` selector).
- **StrategyKind** — blue. Emitted by `Strategy.Active`, consumed by `Cast.StrategyEq`.
- **PairClass** — purple. Emitted by `PairClass.Current`, consumed by `Cast.PairClassEq`.
- **Unit** — grey. Marker ports; every sink output is `Unit` ("the action fired this tick").
- **QuoteBundle** — for `Out.Quotes` and exec-algo sinks.

The canvas enforces type compatibility on connect — mismatched port types reject the edge with a tooltip.

---

## Validation strip

Below the toolbar, live server-side validation tells you whether the graph is deployable:

- **empty** — no nodes yet, drag from the palette
- **ready** (green) — `Evaluator::build` succeeded, no dangling edges, every sink is reachable, no cycles
- **invalid** (red) — lists every blocker. Common issues:
  - `missing required source kind: Book.L1` — a strategy node needs live book data
  - `dangling edge: Math.Mul.a → Out.SpreadMult.mult expects Number, got Missing` — an upstream input is unset
  - `cycle detected: A → B → A` — DAG rule broken
  - `unconsumed sink output` — you added a sink but nothing references its `action` output (usually harmless, just warning)

The Deploy button is enabled only on **ready**.

---

## Scope

Every graph declares its scope:
- **symbol** — deploys to engines running that symbol (most common, e.g. `BTCUSDT`)
- **asset_class** — deploys to every engine whose symbol classifies into the given class (e.g. `StableQuoteMajor`)
- **client** — deploys to every engine under the given `client_id`
- **global** — deploys everywhere (rarely desired; usually just sentiment / news-retreat hooks)

Scope is the primary filter in the Deploy Targets modal.

---

## Save + versions

Click **Save** → dialog with name + description. Behaviour:

- **First save of a name** — creates `user_templates/<name>/<hash>.json` plus an append-only `history.jsonl` entry.
- **Re-save with changes** — computes a client-side diff (added / removed / modified nodes, added / removed edges), shows a preview. Confirm to append as version #N.
- **Re-save with no graph changes** — appends a history entry with just an updated description / timestamp. No new graph file written (content-addressed dedup).
- **Legacy flat `<name>.json`** — auto-migrates to the versioned layout on the first save.

Load a saved template from the Template dropdown (top toolbar) or from the Versions modal.

---

## Deploy

Click **Deploy** when validation is green. A modal opens with the fleet roster, multi-select agents × deployments as targets. Confirm:
1. Server validates the graph once more (authoritative — the frontend validator is advisory)
2. Save the graph body via POST `/api/admin/strategy/graph` (stores + computes content hash)
3. Fan out a `SwapStrategyGraph` control-plane command to every selected `(agent, deployment)` with the hash
4. Each agent loads the graph, builds an `Evaluator`, hot-swaps it into the engine's `strategy_graph` slot
5. First tick after swap — engine re-derives the detector gate (`gate_manipulation`, `gate_onchain`) from the new graph's `required_sources`, clears the previous trace ring

Results land in the deploy-history footer: one row per `(target, outcome)` with the accepted hash or the error.

### Rollback
The deploy-history footer has per-row **Rollback** buttons. Clicking:
1. Fetches the graph body at the historical hash via `/api/v1/strategy/graphs/{name}/history/{hash}`
2. Opens the Deploy modal scoped to targets NOT currently on that hash
3. Marks the deploy with `rollback_from=<previous_hash>` so the audit row records intent (rollback vs accidental hash match)

---

## Live mode

Open from Fleet → Deployment → "Open graph (live)" — Strategy loads with `?live=agentId/deploymentId`. Differences from authoring:

- **Canvas is read-only** — no dragging, no config edits
- **Edge labels** show the last tick's value per edge: `0.123` for Numbers, `true`/`false` for Bools, `Quiet`/`Trending`/etc. for regime kinds
- **Node badges** show the currently-pinned tick's output (first port by default)
- **Right panel** — `GraphInspector` instead of Config: shows the `GraphAnalysis` (dead nodes, unconsumed outputs, required sources) + per-node execution stats aggregated over the trace ring
- **Graph Timeline** (footer) — horizontal strip of the last 256 captured ticks, newest-first. Click any tick to **pin** the canvas to that frame (URL updates `?tick=N`); operator can scroll through the scrubber without losing the pin
- **Long-session guard** — if the pinned tick rolls off the 256-frame ring (~2 min at 2 Hz), the pin auto-releases and shows a banner

---

## Replay vs deployed

Authoring mode only, when arriving from Live. Click **Replay vs deployed** in the toolbar:

1. Backend fetches the last 20 captured ticks from the live deployment's trace ring
2. For each tick, pulls the source-kind values the deployed graph saw
3. Re-runs the CURRENT canvas (candidate) with the same source values
4. Diffs sink actions AND per-node-kind outputs

Modal shows:
- **Summary** — "N ticks replayed, K divergences"
- **Divergence scrubber** — `‹ ›` + range slider over divergent ticks only (identical ticks are skipped)
- **Per-tick diverging kinds** — chips listing which node kinds produced different outputs
- **Side-by-side mini-canvas** — deployed graph on the left, candidate on the right. Nodes of a diverging kind glow with a warning-colour ring on BOTH panes
- **Sink JSON diff** (collapsed) — raw deployed vs replay sink action JSON

Identical-candidate check → "matches deployed behaviour"; candidate rejected by `Evaluator::build` → `candidate_issues` listed.

---

## Restricted (pentest) deploy

Nodes flagged `restricted: true` are pentest exploits (Spoof, Layer, Ignite, Wash, etc.). Deploying a graph that uses one requires BOTH:

1. **Server env** — `MM_ALLOW_RESTRICTED=yes-pentest-mode` on the controller process. No env = 403 refuse.
2. **Explicit operator ack** — first POST returns 412 Precondition Required with the restricted node list. The frontend opens an ack modal showing each restricted node; operator ticks "I understand" + clicks "Acknowledge & deploy". The retry POST carries `restricted_ack=yes-pentest-mode` in the query string.

This two-factor gate (env + per-deploy ack) means a dotfile-set `=1` won't unlock and a command-line `=yes-pentest-mode` still needs an operator to click through. See [pentest.md](pentest.md) for the end-to-end pentest flow.

---

## Node config reference

Each node's config schema is declared in Rust via `NodeKind::config_schema()` and the dashboard renders form fields automatically — Number / Integer / Text / Bool / Enum. Schema changes ship with code changes; the canvas's Config panel always matches what the current backend expects.

For the full catalog (~50 source kinds + 15 strategies + 8 exec composites + others), use the palette's in-app hover tooltips or read the Rust source at `crates/strategy-graph/src/nodes/`.

---

## Import / export

The toolbar's **Download** button serializes the current canvas to JSON (same shape as the deploy body). **Upload** loads a JSON file back onto the canvas.

Use this for:
- Version control in Git (commit the JSON alongside your repo)
- Sharing a strategy with another operator without exposing the full template store
- Reproducing an incident — export the canvas at the moment of a kill-switch trip, attach to the incident for triage

---

## Gotchas

- **Missing propagation** — most math/logic nodes short-circuit to `Missing` when any input is `Missing`. A graph that seems to "do nothing" is often missing a single source (check the validation strip's `required_sources` diagnostic).
- **1-tick lag in Strategy.* → graph feedback** — composite `Strategy.Avellaneda` nodes feed their `QuoteBundle` into the graph on the NEXT tick (current tick's strategy output is applied when `Out.Quotes` doesn't override).
- **Content hash stability** — two graphs with the same nodes + edges but different NodeId UUIDs will have different hashes. Re-saving a template preserves its NodeIds; import/export does not guarantee ID stability.
- **Graph swap clears the trace ring** — the previous graph's last 256 ticks are dropped on swap so a fresh subscriber's first frame isn't polluted. If you're debugging via replay, do the replay before swapping.
- **Kill escalation sinks are one-way** — `Out.KillEscalate` with a level higher than the current engine level escalates; lower does NOT de-escalate. Only an operator can manually reset from L5.
