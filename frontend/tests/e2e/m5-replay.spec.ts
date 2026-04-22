import { expect, test } from '@playwright/test'

/**
 * M5-GOBS — replay v1 vs v2.
 *
 * Backend: POST /api/v1/strategy/replay takes a candidate graph
 * and replays it against the last N ticks of the given
 * deployment. Returns divergence count + per-tick side-by-side.
 *
 * UI: "Replay vs deployed" button in StrategyPage toolbar opens
 * a result modal. Visible only in Authoring mode when a
 * liveTarget is set (operator arrived here from Live view).
 */

const HTTP = process.env.STAND_HTTP_URL!
const TOKEN = process.env.STAND_ADMIN_TOKEN!
const AGENT = process.env.STAND_AGENT_ID!
const DEP = process.env.STAND_DEPLOYMENT_ID!

test.describe('M5-GOBS replay API', () => {
  test('identical graph → zero divergences', async () => {
    const tpl = await fetch(
      `${HTTP}/api/v1/strategy/templates/rug-detector-composite`,
      { headers: { Authorization: `Bearer ${TOKEN}` } },
    ).then((r) => r.json())

    const r = await fetch(`${HTTP}/api/v1/agents/${AGENT}/deployments/${DEP}/replay`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        candidate_graph: tpl,
        ticks: 10,
      }),
    }).then((r) => r.json())

    const p = r?.payload ?? r
    expect(p.ticks_replayed).toBeGreaterThan(0)
    expect(p.divergence_count).toBe(0)
    expect(p.summary).toMatch(/matches deployed behaviour/)
  })

  test('rejected candidate surfaces issues instead of divergences', async () => {
    // Empty graph fails Evaluator::build (no SpreadMult sink).
    const invalid = {
      version: 1,
      name: 'invalid-replay',
      scope: { kind: 'global' },
      nodes: [],
      edges: [],
    }
    const r = await fetch(`${HTTP}/api/v1/agents/${AGENT}/deployments/${DEP}/replay`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        candidate_graph: invalid,
      }),
    }).then((r) => r.json())

    const p = r?.payload ?? r
    expect(p.candidate_issues.length).toBeGreaterThan(0)
    expect(p.ticks_replayed).toBe(0)
    expect(p.summary).toMatch(/rejected|parse failed/)
  })
})

test.describe('M5-GOBS Replay button in StrategyPage', () => {
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

  test('click opens result modal with ticks_replayed + match summary', async ({ page }) => {
    await page.goto(`${HTTP}/?live=${AGENT}/${DEP}`)

    // Wait for graph to load in Live mode, then flip to Authoring
    // so the Replay button becomes available.
    const authoringTab = page.getByRole('tab', { name: 'Authoring' })
    await authoringTab.waitFor({ state: 'visible', timeout: 15_000 })
    await authoringTab.click()

    const replayBtn = page.getByRole('button', { name: /Replay vs deployed/ })
    await expect(replayBtn).toBeVisible()
    await replayBtn.click()

    await expect(page.locator('.replay-card')).toBeVisible({ timeout: 15_000 })
    // Same graph deployed → 0 divergences. The summary text
    // contains "matches deployed behaviour" when green.
    await expect(page.locator('.replay-summary-line')).toContainText(
      /matches deployed behaviour/,
      { timeout: 10_000 },
    )
  })
})
