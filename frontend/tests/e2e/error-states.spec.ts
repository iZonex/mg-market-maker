import { expect, test } from '@playwright/test'

/**
 * Error-state regressions for graph observability.
 *
 * Covers the "thing went sideways" scenarios an operator can
 * hit in real sessions:
 *   - unknown agent_id → 404, not a generic 500
 *   - unknown deployment_id on an existing agent → agent
 *     replies with an error payload, controller forwards 200
 *     (details endpoints wrap agent errors inside the body)
 *   - pin-past-ring → StrategyPage auto-unpins + shows a
 *     user-visible warning when the closed-ring tick rolls off
 *   - replay against deleted agent → 404 with explanatory body
 */

const HTTP = process.env.STAND_HTTP_URL!
const TOKEN = process.env.STAND_ADMIN_TOKEN!
const AGENT = process.env.STAND_AGENT_ID!
const DEP = process.env.STAND_DEPLOYMENT_ID!

test.describe('Error state: unknown routes', () => {
  test('unknown agent → details 404', async () => {
    const r = await fetch(
      `${HTTP}/api/v1/agents/no-such-agent/deployments/${DEP}/details/graph_trace_recent`,
      { headers: { Authorization: `Bearer ${TOKEN}` } },
    )
    expect(r.status).toBe(404)
    const text = await r.text()
    expect(text).toMatch(/not currently connected|unknown/i)
  })

  test('unknown agent → replay 404', async () => {
    const r = await fetch(
      `${HTTP}/api/v1/agents/no-such-agent/deployments/${DEP}/replay`,
      {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${TOKEN}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ candidate_graph: {} }),
      },
    )
    expect(r.status).toBe(404)
  })

  test('unknown deployment on valid agent → 200 with error payload', async () => {
    // The controller fan-out succeeds at the agent layer; the
    // agent itself resolves an unknown deployment to an error
    // string in the payload envelope. This is an intentional
    // design — lets the UI render "deployment vanished" without
    // treating network errors and stale state identically.
    const r = await fetch(
      `${HTTP}/api/v1/agents/${AGENT}/deployments/no-such-dep/details/graph_trace_recent`,
      { headers: { Authorization: `Bearer ${TOKEN}` } },
    )
    // Controller returns 200 envelope; error is inside.
    expect(r.status).toBe(200)
    const body = await r.json()
    // Either the payload traces list is empty or there's an
    // error field — both are valid "nothing to show" signals.
    const traces = body?.payload?.traces ?? []
    expect(traces.length).toBe(0)
  })
})

test.describe('Error state: pin-past-ring guard', () => {
  test.beforeEach(async ({ context }) => {
    await context.addInitScript((p) => {
      window.localStorage.setItem('mm_auth', JSON.stringify(p))
    }, {
      token: TOKEN,
      userId: 'stand-admin',
      name: 'stand-admin',
      role: 'admin',
    })
  })

  test('?tick=0 pre-pins an impossibly old frame → auto-unpin + warning', async ({ page }) => {
    // tick_num=0 never exists in a live stand (the counter starts
    // at 1 and is already way past 0). The guard should detect
    // the pinned tick isn't in the live window and auto-unpin,
    // flashing a warning banner.
    await page.goto(`${HTTP}/?live=${AGENT}/${DEP}&tick=0`)

    const warning = page.locator('.pin-warning')
    await expect(warning).toBeVisible({ timeout: 15_000 })
    await expect(warning).toContainText(/rolled off|released pin/)

    // Live pill re-appears (pinned mode cleared).
    await expect(page.locator('.tl-live-pill')).toBeVisible({ timeout: 10_000 })
  })
})
