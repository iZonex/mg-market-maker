import { expect, test } from '@playwright/test'

/**
 * Navigation smoke — click every sidebar entry for each role tier,
 * verify the right page loads + the role gates reject what they
 * should reject. Catches the drift where a sidebar entry exists but
 * its page component was renamed/removed, or where an admin-only
 * route leaks into operator view.
 *
 * Runs against a live stand (same env contract as every other e2e
 * spec under this directory: STAND_HTTP_URL, STAND_ADMIN_TOKEN).
 * Authenticates via localStorage seed — no live login flow.
 */

const HTTP = process.env.STAND_HTTP_URL!
const TOKEN = process.env.STAND_ADMIN_TOKEN!

function seedAuth(context: import('@playwright/test').BrowserContext, role: string) {
  return context.addInitScript((payload) => {
    try {
      window.localStorage.setItem('mm_auth', JSON.stringify(payload))
    } catch {}
  }, {
    token: TOKEN,
    userId: `stand-${role}`,
    name: `stand-${role}`,
    role,
  })
}

type NavEntry = {
  /** Sidebar label visible to operators. */
  label: string
  /** An element selector that MUST be visible once the page loads. */
  proof: string
  /** Roles that can see the entry per Sidebar.svelte's `roles` arrays. */
  roles: Array<'admin' | 'operator' | 'viewer'>
}

const NAV: NavEntry[] = [
  // Live
  { label: 'Overview',       proof: '.overview, [data-testid="overview-page"], h1, h2', roles: ['admin', 'operator', 'viewer'] },
  { label: 'Orderbook',      proof: '[data-testid="orderbook-page"], main',              roles: ['admin', 'operator', 'viewer'] },
  { label: 'History',        proof: '[data-testid="history-page"], main',                roles: ['admin', 'operator', 'viewer'] },
  // Operations
  { label: 'Fleet',          proof: '[data-testid="fleet-page"], table, main',           roles: ['admin', 'operator', 'viewer'] },
  { label: 'Clients',        proof: '[data-testid="clients-page"], main',                roles: ['admin', 'operator', 'viewer'] },
  { label: 'Reconciliation', proof: '[data-testid="reconciliation-page"], main',         roles: ['admin', 'operator', 'viewer'] },
  { label: 'Incidents',      proof: '[data-testid="incidents-page"], main',              roles: ['admin', 'operator'] },
  // Venues & Execution
  { label: 'Venues',         proof: '[data-testid="venues-page"], main',                 roles: ['admin', 'operator'] },
  { label: 'Calibration',    proof: '[data-testid="calibration-page"], main',            roles: ['admin', 'operator'] },
  // Compliance
  { label: 'Compliance',     proof: '[data-testid="compliance-page"], main',             roles: ['admin', 'operator', 'viewer'] },
  // Configure
  { label: 'Strategy',       proof: '.svelte-flow, [data-testid="strategy-page"]',       roles: ['admin', 'operator'] },
  { label: 'Rules',          proof: '[data-testid="rules-page"], main',                  roles: ['admin', 'operator'] },
  // Admin
  { label: 'Kill switch',    proof: '[data-testid="kill-switch-page"], main',            roles: ['admin'] },
  { label: 'Platform',       proof: '[data-testid="platform-page"], main',               roles: ['admin'] },
  { label: 'Vault',          proof: '[data-testid="vault-page"], main',                  roles: ['admin'] },
  { label: 'Users',          proof: '[data-testid="users-page"], main',                  roles: ['admin'] },
  { label: 'Auth audit',     proof: '[data-testid="login-audit-page"], main',            roles: ['admin'] },
  { label: 'Surveillance',   proof: '[data-testid="surveillance-page"], main',           roles: ['admin'] },
]

test.describe('Navigation smoke — admin sees and reaches every entry', () => {
  test.beforeEach(async ({ context }) => {
    await seedAuth(context, 'admin')
  })

  for (const entry of NAV) {
    test(`admin: ${entry.label} loads`, async ({ page }) => {
      await page.goto(HTTP)
      // Sidebar renders the label as text in the button.
      const btn = page.getByRole('button', { name: entry.label, exact: true })
      await expect(btn).toBeVisible({ timeout: 10_000 })
      await btn.click()
      // Proof — either a data-testid selector (if the page sets one)
      // or a fallback generic tag. One of the two must be there.
      await expect(page.locator(entry.proof).first()).toBeVisible({
        timeout: 10_000,
      })
    })
  }
})

test.describe('Navigation smoke — role gates', () => {
  test('operator does NOT see admin-only entries', async ({ page, context }) => {
    await seedAuth(context, 'operator')
    await page.goto(HTTP)
    // Sidebar loaded.
    await expect(page.getByRole('button', { name: 'Overview', exact: true })).toBeVisible()
    // Admin-only labels must be absent.
    for (const adminOnly of ['Kill switch', 'Platform', 'Vault', 'Users', 'Auth audit', 'Surveillance']) {
      await expect(page.getByRole('button', { name: adminOnly, exact: true })).toHaveCount(0)
    }
  })

  test('viewer does NOT see operator-gated entries', async ({ page, context }) => {
    await seedAuth(context, 'viewer')
    await page.goto(HTTP)
    await expect(page.getByRole('button', { name: 'Overview', exact: true })).toBeVisible()
    for (const opGated of ['Strategy', 'Rules', 'Venues', 'Calibration', 'Incidents']) {
      await expect(page.getByRole('button', { name: opGated, exact: true })).toHaveCount(0)
    }
  })
})

test.describe('Branding smoke — product name is consistent', () => {
  test('browser tab title contains the brand name', async ({ page, context }) => {
    await seedAuth(context, 'admin')
    await page.goto(HTTP)
    // branding.js is authoritative; whatever it emits lands in
    // `document.title` from main.js at boot.
    const title = await page.title()
    expect(title.toLowerCase()).toContain('dashboard')
    // Negative: default Vite title must NOT leak.
    expect(title).not.toContain('Vite')
  })
})
