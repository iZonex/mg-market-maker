import { expect, test } from '@playwright/test'

/**
 * M4-4 GOBS — incident → graph deep-link.
 *
 * POSTs an incident with graph_agent_id / graph_deployment_id /
 * graph_tick_num populated, then opens the Incidents page and
 * asserts the "Open graph at incident" button appears and
 * navigates to `/?live=<a>/<d>&tick=<n>`.
 */

const HTTP = process.env.STAND_HTTP_URL!
const TOKEN = process.env.STAND_ADMIN_TOKEN!
const AGENT = process.env.STAND_AGENT_ID!
const DEP = process.env.STAND_DEPLOYMENT_ID!

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

test.describe('M4-4 GOBS incident deep-link', () => {
  test('graph fields persist + surface Open-graph button that pre-pins tick', async ({
    page,
    context,
  }) => {
    // Pick a real tick from the ring so the deep link can be
    // verified end-to-end.
    const traces = await fetch(
      `${HTTP}/api/v1/agents/${AGENT}/deployments/${DEP}/details/graph_trace_recent?limit=1`,
      { headers: { Authorization: `Bearer ${TOKEN}` } },
    ).then((r) => r.json())
    const tickNum = traces?.payload?.traces?.[0]?.tick_num
    expect(typeof tickNum).toBe('number')

    // File the incident via the HTTP API directly — same path
    // the DeploymentDrilldown `File incident` button uses.
    const filed = await fetch(`${HTTP}/api/v1/incidents`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        // Dedup by violation_key is a feature of `open_incident`
        // — same key merges into the existing open row. Bake a
        // per-run suffix so a second Playwright run doesn't hit
        // the cached incident from the first.
        violation_key: `graph#${AGENT}/${DEP}#e2e-${Date.now()}`,
        severity: 'warning',
        category: 'strategy-graph',
        target: `${AGENT}/${DEP}`,
        metric: 'rug-detector-composite',
        detail: 'playwright m4-4 regression',
        graph_agent_id: AGENT,
        graph_deployment_id: DEP,
        graph_tick_num: tickNum,
      }),
    }).then((r) => r.json())
    expect(filed.graph_agent_id).toBe(AGENT)
    expect(filed.graph_tick_num).toBe(tickNum)

    // Navigate to Incidents and expand the freshly-filed row.
    await page.goto(`${HTTP}/`)
    await page.getByRole('button', { name: /^Incidents$/ }).click()
    // `target` (agent/deployment) is rendered in inc-head so
    // the row is matchable before the card is expanded. The
    // detail lives in inc-body which only renders on click.
    const targetRow = page
      .locator('.inc-card')
      .filter({ hasText: `${AGENT}/${DEP}` })
      .first()
    await targetRow.locator('.inc-head').click()

    const btn = targetRow.getByRole('button', {
      name: /Open graph at incident/,
    })
    await expect(btn).toBeVisible({ timeout: 10_000 })
    await expect(btn).toContainText(`tick #${tickNum}`)

    // Click → URL gets ?live=... &tick=...
    await btn.click()
    await expect(page).toHaveURL(new RegExp(`live=${AGENT}%2F${DEP}|live=${AGENT}/${DEP}`))
    await expect(page).toHaveURL(new RegExp(`tick=${tickNum}`))
  })
})
