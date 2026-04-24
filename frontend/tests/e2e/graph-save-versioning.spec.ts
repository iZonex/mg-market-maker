import { expect, test } from '@playwright/test'

/**
 * Save-diff + versioning E2E for custom strategy templates.
 *
 * Flow:
 *   1. POST a fresh custom template (v1). 201/200 + hash.
 *   2. POST a mutated graph with the same name → v2 appends to
 *      history.jsonl; response echoes version=2.
 *   3. GET /custom_templates/:name — history has 2 entries
 *      newest-first; graph returned matches the v2 hash.
 *   4. GET /custom_templates/:name/versions/{v1_hash} — returns
 *      the original graph.
 *   5. POST an IDENTICAL graph (same content) — history entry
 *      is appended but no new hash file on disk (dedup); the
 *      returned `version` count increments.
 *   6. Cleanup: DELETE the template.
 */

const HTTP = process.env.STAND_HTTP_URL!
const TOKEN = process.env.STAND_ADMIN_TOKEN!

const NAME = `e2e-save-${Date.now()}`

async function api(url: string, init: RequestInit = {}): Promise<any> {
  const r = await fetch(`${HTTP}${url}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      'Content-Type': 'application/json',
      ...(init.headers ?? {}),
    },
  })
  if (r.status >= 400) {
    throw new Error(
      `${init.method ?? 'GET'} ${url} → ${r.status} ${await r.text()}`,
    )
  }
  const ct = r.headers.get('content-type') ?? ''
  if (ct.includes('application/json')) return r.json()
  return r.text()
}

/**
 * Minimal valid graph: Math.Const → Out.SpreadMult sink.
 * Graph::content_hash is deterministic from nodes+edges+scope
 * so we reuse the same IDs to ensure the second POST flags as
 * a modification rather than fresh.
 */
function baseGraph(mult: string) {
  return {
    version: 1,
    name: NAME,
    scope: { kind: 'global' },
    stale_hold_ms: 0,
    nodes: [
      {
        id: '00000000-0000-4000-8000-000000000001',
        kind: 'Math.Const',
        config: { value: mult },
        pos: [0, 0],
      },
      {
        id: '00000000-0000-4000-8000-000000000002',
        kind: 'Out.SpreadMult',
        config: {},
        pos: [200, 0],
      },
    ],
    edges: [
      {
        from: {
          node: '00000000-0000-4000-8000-000000000001',
          port: 'value',
        },
        to: {
          node: '00000000-0000-4000-8000-000000000002',
          port: 'mult',
        },
      },
    ],
  }
}

test.describe('Custom template versioning', () => {
  test.afterAll(async () => {
    await fetch(`${HTTP}/api/v1/strategy/custom_templates/${NAME}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${TOKEN}` },
    }).catch(() => {})
  })

  test('save v1 → save v2 → history + per-version read + dedup', async () => {
    // v1
    const r1 = await api('/api/v1/strategy/custom_templates', {
      method: 'POST',
      body: JSON.stringify({
        name: NAME,
        description: 'first',
        graph: baseGraph('1'),
      }),
    })
    expect(r1.status).toBe('saved')
    expect(r1.hash).toMatch(/^[0-9a-f]+$/)
    const v1Hash = r1.hash
    expect(r1.version).toBe(1)

    // v2 with mutated config (mult "1" → "2") → different hash.
    const r2 = await api('/api/v1/strategy/custom_templates', {
      method: 'POST',
      body: JSON.stringify({
        name: NAME,
        description: 'bump mult',
        graph: baseGraph('2'),
      }),
    })
    expect(r2.hash).not.toBe(v1Hash)
    expect(r2.version).toBe(2)
    const v2Hash = r2.hash

    // GET — latest graph is v2, history has both.
    const full = await api(
      `/api/v1/strategy/custom_templates/${NAME}`,
    )
    expect(full.history).toHaveLength(2)
    expect(full.history[0].hash).toBe(v2Hash) // newest-first
    expect(full.history[1].hash).toBe(v1Hash)
    expect(full.graph.nodes[0].config.value).toBe('2')

    // Per-version read returns the older graph.
    const v1Graph = await api(
      `/api/v1/strategy/custom_templates/${NAME}/versions/${v1Hash}`,
    )
    expect(v1Graph.nodes[0].config.value).toBe('1')

    // Dedup: re-POSTing v2's exact graph appends a history line
    // but doesn't rewrite the hash file. The response's
    // `version` count reflects the new history length.
    const r2Dup = await api('/api/v1/strategy/custom_templates', {
      method: 'POST',
      body: JSON.stringify({
        name: NAME,
        description: 'same bytes',
        graph: baseGraph('2'),
      }),
    })
    expect(r2Dup.hash).toBe(v2Hash)
    expect(r2Dup.version).toBe(3)
  })
})

test.describe('Save-diff preview UI', () => {
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

  test('save with existing name shows a diff preview before commit', async ({ page }) => {
    // Seed a template via the API so the UI's save dialog has
    // something to diff against. Unique name per run to avoid
    // cross-test pollution.
    const uiName = `ui-save-${Date.now()}`
    await api('/api/v1/strategy/custom_templates', {
      method: 'POST',
      body: JSON.stringify({
        name: uiName,
        description: 'seed',
        graph: baseGraph('1'),
      }),
    })

    await page.goto(`${HTTP}/`)
    // Nav to Strategy — Sidebar role=button name=Strategy.
    await page.getByRole('button', { name: /^Strategy$/ }).click()
    await page
      .getByRole('combobox', { name: /Template/i })
      .selectOption(`custom:${uiName}`)
    await expect(page.locator('.v-pill.ok')).toBeVisible({ timeout: 15_000 })

    // Open save dialog.
    const saveBtn = page.getByRole('button', { name: 'Save as reusable template' })
    await saveBtn.click()
    await expect(page.locator('.save-modal')).toBeVisible()

    // Re-enter the same name — first Save click should flip
    // into the diff-preview phase (no actual POST yet).
    const nameInput = page.locator('.save-modal input').first()
    await nameInput.fill(uiName)
    const primary = page.locator('.save-modal').getByRole('button', { name: /^Save$/ })
    await primary.click()

    // Expect the save-diff block. Since we didn't mutate the
    // canvas between load + save, it should report 0 changes
    // (clean state).
    await expect(page.locator('.save-diff')).toBeVisible({ timeout: 10_000 })
    await expect(page.locator('.save-diff.clean')).toBeVisible()

    // Commit — phase 2 saves a new history entry.
    const commit = page.locator('.save-modal').getByRole('button', {
      name: /Save new version/,
    })
    await commit.click()
    await expect(page.locator('.save-modal')).toBeHidden({ timeout: 10_000 })

    // Cleanup.
    await fetch(`${HTTP}/api/v1/strategy/custom_templates/${uiName}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${TOKEN}` },
    })
  })
})
