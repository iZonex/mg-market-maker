/**
 * M2-GOBS — live-mode polling store for the strategy graph canvas.
 *
 * Wraps the `graph_trace_recent` details topic exposed by the agent.
 * Polls every 2 s while the owner page is visible; caller invokes
 * `stop()` on unmount. Returns a reactive `state` object with:
 *   - `traces`: last N TickTraces, newest-first (array).
 *   - `latestTrace`: convenience — first element of `traces`, or null.
 *   - `edgeValues`: derived `sourceHandle → value` lookup for the
 *     latest tick, shape compatible with StrategyPage's
 *     `decorateEdges()`.
 *   - `nodeStats`: derived per-node rollup over the trace window
 *     (hit rate, avg elapsed_ns, last value, sparkline values).
 *   - `graphAnalysis`: static topology snapshot (dead nodes,
 *     required sources, unconsumed outputs) — fetched alongside
 *     traces so the UI has it in one poll.
 *   - `error`, `loading`, `lastFetch`: standard status fields.
 *
 * Tolerant of flaky polls — a failed fetch keeps the previous
 * state visible and surfaces `error` for a banner. The engine
 * emits a `TickTrace` per `refresh_quotes`, so the 2 s poll
 * captures ~4 ticks at 2 Hz.
 */

import { createApiClient } from './api.svelte.js'

const POLL_MS = 2_000
const TRACE_LIMIT = 20

export function createGraphLiveStore(auth, agentId, deploymentId) {
  const api = createApiClient(auth)

  const state = $state({
    traces: [],
    graphAnalysis: null,
    error: null,
    loading: true,
    lastFetch: null,
  })

  async function poll() {
    if (!agentId || !deploymentId) {
      state.loading = false
      state.error = 'missing agent/deployment'
      return
    }
    try {
      const url =
        `/api/v1/agents/${encodeURIComponent(agentId)}` +
        `/deployments/${encodeURIComponent(deploymentId)}` +
        `/details/graph_trace_recent?limit=${TRACE_LIMIT}`
      const resp = await api.getJson(url)
      const payload = resp?.payload ?? {}
      state.traces = Array.isArray(payload.traces) ? payload.traces : []
      state.graphAnalysis = payload.graph_analysis ?? null
      state.error = null
      state.lastFetch = new Date()
    } catch (e) {
      state.error = e?.message || String(e)
    } finally {
      state.loading = false
    }
  }

  let iv = null
  poll()
  iv = setInterval(poll, POLL_MS)

  function stop() {
    if (iv) {
      clearInterval(iv)
      iv = null
    }
  }

  return { state, stop, refresh: poll }
}

/**
 * Derive a `{sourceHandle → displayValue}` lookup from the latest
 * trace, shape-compatible with the existing `decorateEdges()`
 * function so we reuse the same rendering path as the preview-tick
 * flow. Key format: `"${nodeId}:${portName}"`.
 */
export function edgeValuesFromTrace(trace) {
  if (!trace || !Array.isArray(trace.nodes)) return {}
  const out = {}
  for (const n of trace.nodes) {
    for (const [port, value] of n.outputs || []) {
      out[`${n.id}:${port}`] = formatValue(value)
    }
  }
  return out
}

/**
 * Per-node stats rollup across the trace window. Returns a map
 * keyed by node_id:
 *   - `hitRate`: fraction of ticks where the node fired (status=ok|source).
 *   - `avgElapsedNs`: mean of elapsed_ns over non-zero samples.
 *   - `history`: ordered array of (up to 20) last output values, for
 *     sparklines. Only pulled from the node's PRIMARY output port
 *     (first declared); multi-port nodes render the first port.
 *   - `lastStatus`: the most recent status tag.
 *   - `lastError`: populated when lastStatus is error.
 */
export function nodeStatsFromTraces(traces) {
  const agg = new Map()
  if (!Array.isArray(traces) || traces.length === 0) return agg

  // Walk oldest-first so history ordering matches time order.
  const ordered = [...traces].reverse()
  for (const t of ordered) {
    if (!Array.isArray(t.nodes)) continue
    for (const n of t.nodes) {
      let row = agg.get(n.id)
      if (!row) {
        row = {
          id: n.id,
          kind: n.kind,
          fired: 0,
          elapsedSum: 0,
          elapsedCount: 0,
          history: [],
          lastStatus: null,
          lastError: null,
        }
        agg.set(n.id, row)
      }
      const status = n.status?.kind ?? 'ok'
      row.lastStatus = status
      if (status === 'error') {
        row.lastError = n.status?.detail ?? 'error'
      } else {
        row.lastError = null
        row.fired += 1
      }
      if (typeof n.elapsed_ns === 'number' && n.elapsed_ns > 0) {
        row.elapsedSum += n.elapsed_ns
        row.elapsedCount += 1
      }
      const firstOutput = n.outputs?.[0]
      if (firstOutput) {
        row.history.push(firstOutput[1])
      }
    }
  }

  // Finalise.
  const totalTicks = traces.length
  for (const row of agg.values()) {
    row.hitRate = totalTicks > 0 ? row.fired / totalTicks : 0
    row.avgElapsedNs = row.elapsedCount > 0 ? row.elapsedSum / row.elapsedCount : 0
  }
  return agg
}

/**
 * Short stringification for on-edge labels and node badges. Matches
 * the backend preview path's shape: scalars render cleanly, `Missing`
 * renders as `·`, `Quotes`/`VenueQuotes` collapse to a count.
 */
export function formatValue(v) {
  if (v === null || v === undefined) return '·'
  if (typeof v !== 'object') return String(v)
  // Value enum — `{ "Number": "3.5" }` or `{ "Missing": null }`.
  if ('Number' in v) return String(v.Number)
  if ('Bool' in v) return v.Bool ? 'true' : 'false'
  if ('Unit' in v) return '◦'
  if ('String' in v) return `"${v.String}"`
  if ('KillLevel' in v) return `L${v.KillLevel}`
  if ('StrategyKind' in v) return v.StrategyKind
  if ('PairClass' in v) return v.PairClass
  if ('Quotes' in v) {
    const n = Array.isArray(v.Quotes) ? v.Quotes.length : 0
    return `${n} quote${n === 1 ? '' : 's'}`
  }
  if ('VenueQuotes' in v) {
    const n = Array.isArray(v.VenueQuotes) ? v.VenueQuotes.length : 0
    return `${n} vquote${n === 1 ? '' : 's'}`
  }
  if ('Missing' in v) return '·'
  return '?'
}
