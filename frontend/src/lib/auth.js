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

  async function login(apiKey) {
    const resp = await fetch('/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ api_key: apiKey }),
    })

    if (!resp.ok) {
      throw new Error('Invalid API key')
    }

    const data = await resp.json()
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
    logout,
    canControl,
    canViewInternals,
  }
}
