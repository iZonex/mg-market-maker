# Strategy graph Live mode — operator guide

Live mode turns the strategy-graph canvas into a window on the
running deployment. Values stream from the engine onto the
same DAG the operator authored — every edge gets its live
value, every node shows its latest output, sinks pulse when
they fire.

This guide walks the everyday loop and the three escape hatches
when something looks wrong.

## Opening Live mode

Two entry points:

1. **Fleet → Deployment drilldown → "Open graph (live)"**
   - The drilldown header only shows the button when the
     deployment has a strategy graph attached
     (`active_graph.name` populated in fleet telemetry).
   - One click closes the drilldown and opens the Strategy
     page with Live mode pre-selected.

2. **URL `?live=<agent_id>/<deployment_id>`**
   - Used by deep links — Incidents post-mortem (see below),
     alerts, shared diagnostic URLs.
   - Can be combined with `?tick=<N>` to land on a specific
     frame (post-mortem flow).

When Live mode loads:
- StrategyPage fetches the deployment's graph JSON from the
  `/api/v1/strategy/templates/<name>` endpoint and seeds the
  svelte-flow canvas.
- `createGraphLiveStore` starts polling
  `/api/v1/agents/<a>/deployments/<d>/details/graph_trace_recent`
  every 2 s for the newest up to 20 ticks.
- The inspector sidebar replaces the authoring config panel.
- A timeline strip appears under the canvas.

## What the canvas shows

| Visual cue                    | Meaning                                                   |
|-------------------------------|-----------------------------------------------------------|
| Node `out:` badge             | latest output value from the selected/latest tick         |
| Node header pulse             | node fired on the current frame                           |
| Red dashed node border        | dead branch — no path to any sink (authoring error)       |
| Diagonal-stripe node          | dormant source — graph has this node but nothing reads it |
| Edge label                    | value flowing from the upstream node on the current tick  |
| Edge red dashed stroke        | one endpoint is a dead node                               |

Click any node → inspector sidebar updates:
- Sparkline of the last 20 numeric outputs
- Hit rate (% of ticks the node fired)
- Average `elapsed_ns` per tick
- Current status chip (`ok` / `source` / `error`)

## Timeline + time travel

The strip below the canvas shows one column per TickTrace in the
window. Column height = relative `total_elapsed_ns`, colour tone
= number of sinks that fired.

- Hover a column → tooltip with tick number, timestamp,
  elapsed, and sinks_fired count.
- Click a column → "pin" that tick. Canvas freezes to the
  pinned frame's values, timeline header shows "Back to live",
  border glows warning-yellow.
- Click "Back to live" → unpin; canvas resumes tracking the
  newest frame.

### Stale pin guard

If you leave a pin in place longer than the ring window
(default 256 ticks ≈ 2 min at 2 Hz), the tick rolls off the
oldest end. The page detects this, automatically unpins, and
flashes a "tick #N rolled off the ring — released pin" banner
for a few seconds. You never end up silently viewing "current"
while the chrome claims "pinned".

## Replay against deployed

`Replay vs deployed` (toolbar, authoring mode, requires a
`?live=` target) is the "what if I had deployed THIS graph
instead" check.

1. Flip to Authoring mode (toggle next to Simulate / Deploy).
2. Edit the canvas — add, remove, reconfigure nodes.
3. Click **Replay vs deployed**.

The button POSTs the current canvas to
`/api/v1/agents/<a>/deployments/<d>/replay`; the controller
fans out to the agent, where the candidate graph is re-evaluated
against the last 20 TickTraces using the same source values
(matched by source kind, since NodeIds differ between the two
graphs).

The modal reports:
- **Summary line**: `20 tick(s) replayed · candidate matches deployed behaviour`
  OR `… diverges on K tick(s)`.
- **Per-tick divergence list**: side-by-side JSON of deployed
  sinks and candidate sinks, for every tick where they differ.

Replay is read-only — it touches the agent's CPU briefly but
doesn't mutate any running state. Safe to invoke during live
trading.

## Filing an incident at the current frame

From the Deployment drilldown:

1. Click **File incident** (next to Open graph live).
2. The drilldown snapshots the deployment's latest tick number
   via `graph_trace_recent?limit=1` and POSTs an incident with
   `graph_agent_id`, `graph_deployment_id`, `graph_tick_num`.
3. You're navigated to the Incidents page; the new row is
   `open`.
4. On the row, `Open graph at incident · tick #N` deep-links
   back to this canvas at that exact tick (pinned via
   `?tick=N`), so the post-mortem reviewer can see what the
   engine saw when you filed.

## When things look wrong — checklist

| Symptom                              | Likely cause + fix                                                        |
|--------------------------------------|---------------------------------------------------------------------------|
| Empty canvas in Live mode            | deployment has no strategy graph attached. Check `active_graph` in Fleet. |
| No badges on nodes                   | agent hasn't ticked yet (wait ~2 s) or graph poll failed (check banner)   |
| `"0 traces — deployment may not…"`   | engine never ticked; deployment stuck in admission or restart loop        |
| Dead-branch banner on a node         | authoring error; the node has no path to any `Out.*` sink                 |
| Dormant-source banner                | node is in the graph but no downstream consumer; wire it or remove it     |
| Replay modal: `candidate rejected`   | new graph fails compile; check error and fix in authoring                 |
| Replay summary: `no traces for …`    | deployment hasn't ticked since its last swap; wait and retry              |
| Pin banner: `rolled off the ring`    | pinned tick aged out of the 256-tick window; normal after long sessions   |

## Data retention + gating

- Trace ring: **256 ticks per deployment per symbol** (memory-bounded).
- Graph analysis: **one snapshot per graph swap**.
- On swap, the previous graph's traces are cleared so a fresh
  subscriber doesn't see stale frames.
- Detectors whose source kinds are not referenced by the
  currently-deployed graph **don't update** — their aggregators
  skip the `on_trade` / `on_book` hot paths (M6 lazy gating).
  Prometheus + DashboardState publish `null` honestly when a
  gate is closed, rather than stale zeros.

## Roles

`graph_trace_recent`, `graph_analysis`, and the replay endpoint
all sit under the controller's `internal_view` tier. Admin,
operator, and viewer roles can read them; ClientReader is
blocked by the tenant-scope middleware.
