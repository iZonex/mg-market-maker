import { expect, test } from '@playwright/test'

/**
 * M1-GOBS API-layer regressions.
 *
 * These hit the details endpoint directly — no browser needed
 * for the trace pipeline, but we keep them in the Playwright
 * suite so CI runs one regression entry point (`npm run test:e2e`).
 * If the engine→store→agent→controller pipeline breaks these
 * fail loud before the UI tests even load a canvas.
 */

const HTTP = process.env.STAND_HTTP_URL!
const TOKEN = process.env.STAND_ADMIN_TOKEN!
const AGENT = process.env.STAND_AGENT_ID!
const DEP = process.env.STAND_DEPLOYMENT_ID!

async function fetchJson(url: string) {
  const r = await fetch(url, { headers: { Authorization: `Bearer ${TOKEN}` } })
  expect(r.status, `GET ${url}`).toBe(200)
  return r.json()
}

test.describe('M1-GOBS API pipeline', () => {
  test('graph_trace_recent returns live ticks with node output values', async () => {
    const body = await fetchJson(
      `${HTTP}/api/v1/agents/${AGENT}/deployments/${DEP}/details/graph_trace_recent?limit=5`,
    )
    const payload = body.payload
    expect(payload).toBeTruthy()
    expect(Array.isArray(payload.traces)).toBe(true)
    expect(payload.traces.length).toBeGreaterThan(0)

    const t = payload.traces[0]
    expect(typeof t.tick_ms).toBe('number')
    expect(typeof t.tick_num).toBe('number')
    expect(typeof t.graph_hash).toBe('string')
    expect(Array.isArray(t.nodes)).toBe(true)
    expect(t.nodes.length).toBeGreaterThan(0)

    // At least one node carries a non-empty outputs list.
    const nodeWithOutput = t.nodes.find(
      (n: any) => Array.isArray(n.outputs) && n.outputs.length > 0,
    )
    expect(nodeWithOutput, 'expected at least one node with an output').toBeTruthy()

    // Sinks fired is shape-valid (may be empty on the first tick).
    expect(Array.isArray(t.sinks_fired)).toBe(true)
  })

  test('graph_analysis returns topology snapshot with depth map', async () => {
    const body = await fetchJson(
      `${HTTP}/api/v1/agents/${AGENT}/deployments/${DEP}/details/graph_analysis`,
    )
    const payload = body.payload
    expect(payload).toBeTruthy()
    expect(typeof payload.graph_hash).toBe('string')
    expect(payload.graph_hash.length).toBeGreaterThan(10)
    expect(Array.isArray(payload.depth_map)).toBe(true)
    expect(payload.depth_map.length).toBeGreaterThan(0)
    expect(Array.isArray(payload.required_sources)).toBe(true)
    expect(Array.isArray(payload.dead_nodes)).toBe(true)
    expect(Array.isArray(payload.unconsumed_outputs)).toBe(true)
  })

  test('tick counter is monotonic within the window', async () => {
    const body = await fetchJson(
      `${HTTP}/api/v1/agents/${AGENT}/deployments/${DEP}/details/graph_trace_recent?limit=5`,
    )
    const traces = body.payload.traces
    expect(traces.length).toBeGreaterThanOrEqual(2)
    // Newest-first: tick_num must strictly decrease down the array.
    for (let i = 1; i < traces.length; i++) {
      expect(traces[i - 1].tick_num).toBeGreaterThan(traces[i].tick_num)
    }
  })
})
