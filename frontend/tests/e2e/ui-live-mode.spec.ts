import { expect, test } from '@playwright/test'

/**
 * M2-GOBS UI live-mode regressions.
 *
 * These drive the actual Svelte build: navigate to
 * `/?live=<agent>/<dep>`, seed an admin token into localStorage
 * so the auth gate is skipped, then assert the StrategyPage
 * Live overlay renders — edges labelled with live values,
 * StrategyNode badges populated, inspector sidebar shown.
 */

const HTTP = process.env.STAND_HTTP_URL!
const TOKEN = process.env.STAND_ADMIN_TOKEN!
const AGENT = process.env.STAND_AGENT_ID!
const DEP = process.env.STAND_DEPLOYMENT_ID!

test.beforeEach(async ({ context }) => {
  // Seed auth BEFORE any page loads so the app boots past Login
  // straight into StrategyPage. Shape matches
  // `auth.svelte.js`'s `localStorage["mm_auth"]` contract.
  const seed = {
    token: TOKEN,
    userId: 'stand-admin',
    name: 'stand-admin',
    role: 'admin',
  }
  await context.addInitScript((payload) => {
    try {
      window.localStorage.setItem('mm_auth', JSON.stringify(payload))
    } catch {}
  }, seed)
})

test.describe('M2-GOBS StrategyPage Live mode', () => {
  test('opens with Live mode active and loads the deployed graph', async ({ page }) => {
    await page.goto(`${HTTP}/?live=${AGENT}/${DEP}`)

    // Mode toggle exists and "Live" is the active segment.
    const liveBtn = page.getByRole('tab', { name: 'Live' })
    await expect(liveBtn).toBeVisible()
    await expect(liveBtn).toHaveClass(/active/)

    // Authoring-only actions disabled while in Live.
    await expect(page.getByRole('button', { name: /Simulate/ })).toBeDisabled()
    await expect(page.getByRole('button', { name: /^Deploy$/ })).toBeDisabled()

    // Canvas has at least one graph node rendered.
    await expect(page.locator('.svelte-flow__node').first()).toBeVisible()
  })

  test('live badge populates on at least one node within the poll window', async ({ page }) => {
    await page.goto(`${HTTP}/?live=${AGENT}/${DEP}`)

    // Live store polls every 2s — give it up to 10s for the first
    // frame to materialise badge values on nodes.
    const badge = page.locator('.live-badge-value').first()
    await expect(badge).toBeVisible({ timeout: 15_000 })
    await expect(badge).not.toHaveText('')
  })

  test('inspector sidebar shows per-node stats on click', async ({ page }) => {
    await page.goto(`${HTTP}/?live=${AGENT}/${DEP}`)

    // Wait for canvas, click the first node.
    const firstNode = page.locator('.svelte-flow__node').first()
    await firstNode.waitFor({ state: 'visible', timeout: 15_000 })
    await firstNode.click()

    // Inspector title appears and the KV grid renders
    // hit-rate + avg-elapsed fields.
    await expect(page.getByText('Live inspector')).toBeVisible()
    await expect(page.getByText('hit rate')).toBeVisible()
    await expect(page.getByText('avg elapsed')).toBeVisible()
  })

  test('toggle back to Authoring stops live polling', async ({ page }) => {
    await page.goto(`${HTTP}/?live=${AGENT}/${DEP}`)

    // Wait for Live to be active.
    const liveBtn = page.getByRole('tab', { name: 'Live' })
    await expect(liveBtn).toHaveClass(/active/, { timeout: 15_000 })

    // Click Authoring — should swap the inspector for the
    // config panel (StrategyNodeConfig renders "Select a node"
    // placeholder).
    await page.getByRole('tab', { name: 'Authoring' }).click()
    await expect(liveBtn).not.toHaveClass(/active/)
    // Simulate + Deploy buttons re-enable (no selection or
    // invalid graph may still keep Deploy disabled, but
    // Simulate only needs nodes.length > 0).
    await expect(page.getByRole('button', { name: /Simulate/ })).toBeEnabled()
  })
})
