import { expect, test } from '@playwright/test'

/**
 * Authz smoke for graph observability topics.
 *
 * Creates fresh viewer + operator users via admin, logs each
 * in to get a token, then hits graph_trace_recent /
 * graph_analysis / replay with each token:
 *   - viewer: 200 on all (internal_view tier includes replay)
 *   - operator: 200 on all
 *   - bad token: 401
 *   - no auth: 401
 *
 * ClientReader end-to-end requires the client onboarding +
 * invite flow which is already covered by tenant-scope tests
 * elsewhere; we rely on `tenant_scope_middleware` behaviour
 * proven there.
 */

const HTTP = process.env.STAND_HTTP_URL!
const ADMIN = process.env.STAND_ADMIN_TOKEN!
const AGENT = process.env.STAND_AGENT_ID!
const DEP = process.env.STAND_DEPLOYMENT_ID!

async function createUser(role: 'viewer' | 'operator'): Promise<string> {
  // Create the user, receive the freshly-generated API key
  // (`api_key` is shown once on creation), then login to mint a
  // session token the graph endpoints will accept.
  const unique = `authz-${role}-${Date.now()}`
  const createResp = await fetch(`${HTTP}/api/admin/users`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${ADMIN}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ name: unique, role, allowed_symbols: [] }),
  })
  expect(createResp.status, `create ${role}`).toBe(200)
  const { api_key } = await createResp.json()
  expect(api_key).toBeTruthy()

  const loginResp = await fetch(`${HTTP}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ api_key }),
  })
  expect(loginResp.status, `login ${role}`).toBe(200)
  const { token } = await loginResp.json()
  expect(token).toBeTruthy()
  return token
}

async function probe(url: string, method: 'GET' | 'POST', token?: string, body?: unknown) {
  const init: RequestInit = {
    method,
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      'Content-Type': 'application/json',
    },
  }
  if (body !== undefined) init.body = JSON.stringify(body)
  const r = await fetch(`${HTTP}${url}`, init)
  return r.status
}

const TRACE = `/api/v1/agents/${AGENT}/deployments/${DEP}/details/graph_trace_recent?limit=1`
const ANALYSIS = `/api/v1/agents/${AGENT}/deployments/${DEP}/details/graph_analysis`
const REPLAY = `/api/v1/agents/${AGENT}/deployments/${DEP}/replay`

test.describe('Authz smoke — graph observability topics', () => {
  test('no auth is 401 on every graph endpoint', async () => {
    expect(await probe(TRACE, 'GET')).toBe(401)
    expect(await probe(ANALYSIS, 'GET')).toBe(401)
    expect(
      await probe(REPLAY, 'POST', undefined, { candidate_graph: {} }),
    ).toBe(401)
  })

  test('garbage token is 401', async () => {
    expect(await probe(TRACE, 'GET', 'bad.garbage.token')).toBe(401)
    expect(
      await probe(REPLAY, 'POST', 'bad.garbage.token', { candidate_graph: {} }),
    ).toBe(401)
  })

  test('viewer + operator can read traces + analysis + run replay', async () => {
    const viewer = await createUser('viewer')
    const operator = await createUser('operator')

    for (const [role, token] of [
      ['viewer', viewer],
      ['operator', operator],
    ] as const) {
      expect(await probe(TRACE, 'GET', token), `${role} GET trace`).toBe(200)
      expect(await probe(ANALYSIS, 'GET', token), `${role} GET analysis`).toBe(200)

      // Fetch the real template so replay actually runs — an empty
      // body would return 200 with candidate_issues but we want
      // to prove the authz path, which is same either way.
      const tpl = await fetch(
        `${HTTP}/api/v1/strategy/templates/rug-detector-composite`,
        { headers: { Authorization: `Bearer ${token}` } },
      ).then((r) => r.json())
      expect(
        await probe(REPLAY, 'POST', token, {
          candidate_graph: tpl,
          ticks: 2,
        }),
        `${role} POST replay`,
      ).toBe(200)
    }
  })
})
