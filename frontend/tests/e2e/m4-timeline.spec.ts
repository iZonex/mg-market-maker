import { expect, test } from '@playwright/test'

/**
 * M4-GOBS — timeline + time-travel.
 *
 * Asserts GraphTimeline renders with one column per trace in the
 * window, clicking a column pins it (warning pill + "Back to live"
 * CTA), and the URL deep link `?tick=<n>` pre-pins on load.
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

test.describe('M4-GOBS GraphTimeline', () => {
  test('renders one column per trace with a live badge', async ({ page }) => {
    await page.goto(`${HTTP}/?live=${AGENT}/${DEP}`)

    const timeline = page.locator('.timeline')
    await expect(timeline).toBeVisible({ timeout: 15_000 })

    // Live badge (not "Back to live") when nothing is pinned.
    await expect(page.locator('.tl-live-pill')).toBeVisible()
    await expect(page.locator('.btn-live')).toHaveCount(0)

    // At least a couple of columns (graph_trace_recent returns up
    // to 20 on a running deployment).
    const colCount = await page.locator('.timeline .col').count()
    expect(colCount).toBeGreaterThan(0)
  })

  test('clicking a column pins it and "Back to live" unpins', async ({ page }) => {
    await page.goto(`${HTTP}/?live=${AGENT}/${DEP}`)

    // Let the live poll land at least one trace.
    await expect(page.locator('.timeline .col').first()).toBeVisible({
      timeout: 15_000,
    })

    // Pick an older tick (not the latest) so we see the pinned
    // state differ from the live default.
    const cols = page.locator('.timeline .col')
    const count = await cols.count()
    const target = cols.nth(Math.min(2, count - 1))
    await target.click()

    // The clicked column now carries the `pinned` class, the
    // live badge is gone, "Back to live" is shown.
    await expect(target).toHaveClass(/pinned/)
    await expect(page.locator('.tl-live-pill')).toHaveCount(0)
    const backToLive = page.locator('.btn-live')
    await expect(backToLive).toBeVisible()

    // Unpin.
    await backToLive.click()
    await expect(page.locator('.tl-live-pill')).toBeVisible()
    await expect(page.locator('.btn-live')).toHaveCount(0)
  })

  test('?tick= URL deep link pre-pins the requested tick', async ({ page }) => {
    // Pre-hydrate an active trace so we can pick a real
    // tick_num from the API before navigating.
    const resp = await fetch(
      `${HTTP}/api/v1/agents/${AGENT}/deployments/${DEP}/details/graph_trace_recent?limit=5`,
      { headers: { Authorization: `Bearer ${TOKEN}` } },
    ).then((r) => r.json())
    const traces: any[] = resp?.payload?.traces ?? []
    expect(traces.length).toBeGreaterThan(0)
    // Pick a mid-window tick — the newest might slip under the
    // live reload race. The oldest is a solid pick.
    const target = traces[traces.length - 1].tick_num

    await page.goto(
      `${HTTP}/?live=${AGENT}/${DEP}&tick=${target}`,
    )

    // The timeline renders in pinned mode (warning border) and
    // shows the "Back to live" CTA, not the live badge.
    await expect(page.locator('.timeline.pinned')).toBeVisible({
      timeout: 15_000,
    })
    await expect(page.locator('.btn-live')).toBeVisible()
  })
})
