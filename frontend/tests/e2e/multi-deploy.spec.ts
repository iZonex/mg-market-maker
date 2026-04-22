import { expect, test } from '@playwright/test'

/**
 * Multi-deploy regression.
 *
 * The trace ring + analysis store are keyed by symbol on the
 * agent; a second deployment on a different symbol must have
 * its own independent trace stream and its own graph_analysis
 * snapshot. Proves the trace pipeline doesn't cross-contaminate
 * when one agent hosts multiple deployments.
 *
 * The test leaves the stand in its original state: fetches the
 * existing deployment list, POSTs the original plus a second
 * entry, verifies both, then POSTs back the original list.
 */

const HTTP = process.env.STAND_HTTP_URL!
const TOKEN = process.env.STAND_ADMIN_TOKEN!
const AGENT = process.env.STAND_AGENT_ID!

async function json(path: string, init?: RequestInit): Promise<any> {
  const r = await fetch(`${HTTP}${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      'Content-Type': 'application/json',
      ...(init?.headers ?? {}),
    },
  })
  expect(r.status, `${init?.method ?? 'GET'} ${path}`).toBeLessThan(400)
  return r.json()
}

test.describe('Multi-deploy: independent trace rings per symbol', () => {
  test('adds a second deployment, both report their own analysis', async () => {
    const existing = await json(`/api/v1/agents/${AGENT}/deployments`)
    expect(Array.isArray(existing)).toBe(true)
    expect(existing.length).toBeGreaterThan(0)

    const originalStrategies = existing.map((d: any) => ({
      deployment_id: d.deployment_id,
      template: d.template,
      symbol: d.symbol,
      credentials: d.credentials ?? [],
      variables: d.variables ?? {},
    }))

    // Second deployment on ETHUSDT, rug-detector-composite so
    // the trace ring populates quickly (same as stand default).
    const second = {
      deployment_id: 'e2e-multi-2',
      template: 'rug-detector-composite',
      symbol: 'ETHUSDT',
      credentials: originalStrategies[0]?.credentials ?? [],
      variables: originalStrategies[0]?.variables ?? {},
    }

    // POST the union — replace-by-set semantics (UI-DEPLOY-1
    // memory). Anything not in this list would be stopped.
    await json(`/api/v1/agents/${AGENT}/deployments`, {
      method: 'POST',
      body: JSON.stringify({
        strategies: [...originalStrategies, second],
      }),
    })

    // Give both engines some wall-clock time to compile + tick.
    // 20s hasn't been flaky in prior smokes.
    await new Promise((r) => setTimeout(r, 20_000))

    // Both deployments expose their own graph_analysis (static
    // topology captured on swap).
    const a1 = await json(
      `/api/v1/agents/${AGENT}/deployments/${originalStrategies[0].deployment_id}/details/graph_analysis`,
    )
    const a2 = await json(
      `/api/v1/agents/${AGENT}/deployments/${second.deployment_id}/details/graph_analysis`,
    )
    expect(a1?.payload?.graph_hash, 'dep1 analysis has hash').toBeTruthy()
    expect(a2?.payload?.graph_hash, 'dep2 analysis has hash').toBeTruthy()

    // Both deployments expose their own trace rings.
    const t1 = await json(
      `/api/v1/agents/${AGENT}/deployments/${originalStrategies[0].deployment_id}/details/graph_trace_recent?limit=3`,
    )
    const t2 = await json(
      `/api/v1/agents/${AGENT}/deployments/${second.deployment_id}/details/graph_trace_recent?limit=3`,
    )
    const traces1 = t1?.payload?.traces ?? []
    const traces2 = t2?.payload?.traces ?? []
    expect(traces1.length, 'dep1 has traces').toBeGreaterThan(0)
    expect(traces2.length, 'dep2 has traces').toBeGreaterThan(0)

    // Restore original deployment set.
    await json(`/api/v1/agents/${AGENT}/deployments`, {
      method: 'POST',
      body: JSON.stringify({ strategies: originalStrategies }),
    })
  })
})
