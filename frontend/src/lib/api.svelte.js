/**
 * Authed fetch helper — every non-public endpoint needs a Bearer
 * token after Epic 1. Centralise the header injection so
 * components don't hand-roll Authorization headers (and so a
 * 401 mass-forces a fresh login instead of silently failing).
 */

export function createApiClient(auth) {
  async function authedFetch(path, init = {}) {
    const token = auth?.state?.token || ''
    const headers = {
      'Content-Type': 'application/json',
      ...(init.headers || {}),
    }
    if (token) headers['Authorization'] = `Bearer ${token}`
    const resp = await fetch(path, { ...init, headers })
    if (resp.status === 401) {
      // Session expired — boot the operator back to the login
      // screen. Components already handle !loggedIn.
      auth?.logout?.()
    }
    return resp
  }

  async function getJson(path) {
    const r = await authedFetch(path)
    if (!r.ok) throw new Error(`${path} → ${r.status}`)
    return r.json()
  }

  async function postJson(path, body) {
    const r = await authedFetch(path, {
      method: 'POST',
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    if (!r.ok) {
      const text = await r.text().catch(() => '')
      throw new Error(`${path} → ${r.status} ${text}`)
    }
    return r.json().catch(() => ({}))
  }

  return { authedFetch, getJson, postJson }
}
