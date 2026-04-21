/**
 * Auth store — manages login state, token, role.
 */

const AUTH_KEY = 'mm_auth'

export function createAuthStore() {
  // Load from localStorage.
  const saved = localStorage.getItem(AUTH_KEY)
  const initial = saved ? JSON.parse(saved) : null

  const state = $state({
    loggedIn: !!initial,
    token: initial?.token || '',
    userId: initial?.userId || '',
    name: initial?.name || '',
    role: initial?.role || '',  // 'admin', 'operator', 'viewer'
  })

  function saveSession(data) {
    state.loggedIn = true
    state.token = data.token
    state.userId = data.user_id
    state.name = data.name
    state.role = data.role
    localStorage.setItem(AUTH_KEY, JSON.stringify({
      token: data.token,
      userId: data.user_id,
      name: data.name,
      role: data.role,
    }))
  }

  async function checkStatus() {
    const resp = await fetch('/api/auth/status')
    if (!resp.ok) throw new Error('cannot reach auth status endpoint')
    return resp.json()
  }

  async function login({ name, password, apiKey, totpCode } = {}) {
    const body = apiKey
      ? { api_key: apiKey, ...(totpCode ? { totp_code: totpCode } : {}) }
      : { name, password, ...(totpCode ? { totp_code: totpCode } : {}) }
    const resp = await fetch('/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })
    // 202 = first factor ok, server wants a TOTP code before
    // issuing a token. Caller renders the code prompt and
    // resubmits with `totpCode`.
    if (resp.status === 202) {
      const data = await resp.json().catch(() => ({}))
      if (data.needs_totp) {
        const err = new Error('TOTP code required')
        err.needsTotp = true
        throw err
      }
    }
    // 403 with `must_enroll_totp` — Wave H3 hard-gate: admin
    // login blocked because TOTP is required but not enrolled.
    // Surface a typed error the UI uses to render a guided
    // message rather than a generic "forbidden" string.
    if (resp.status === 403) {
      const data = await resp.json().catch(() => ({}))
      if (data && data.must_enroll_totp) {
        const err = new Error(
          data.message ||
            'Admin login requires TOTP. Ask another admin to disable the requirement temporarily so you can enroll, or contact your operator.',
        )
        err.mustEnrollTotp = true
        throw err
      }
    }
    if (!resp.ok) {
      const text = await resp.text().catch(() => '')
      throw new Error(text || 'Invalid credentials')
    }
    const data = await resp.json()
    saveSession(data)
    return data
  }

  async function bootstrap({ name, password }) {
    const resp = await fetch('/api/auth/bootstrap', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, password }),
    })
    if (!resp.ok) {
      const text = await resp.text().catch(() => '')
      throw new Error(text || 'Bootstrap failed')
    }
    const data = await resp.json()
    saveSession(data)
    return data
  }

  function logout() {
    state.loggedIn = false
    state.token = ''
    state.userId = ''
    state.name = ''
    state.role = ''
    localStorage.removeItem(AUTH_KEY)
  }

  function canControl() {
    return state.role === 'admin' || state.role === 'operator'
  }

  function canViewInternals() {
    return state.role === 'admin' || state.role === 'operator'
  }

  return {
    get state() { return state },
    login,
    bootstrap,
    checkStatus,
    logout,
    canControl,
    canViewInternals,
    // Wave E4 — client-signup flow sets the session directly
    // from the response it got on /api/auth/client-signup.
    saveSession,
  }
}
