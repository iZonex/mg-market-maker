<script>
  /*
   * Wave E4 — client signup via signed invite URL.
   *
   * URL shape: `/client-signup/{invite_token}` — admin hands
   * this link to the tenant. The token is an HMAC-signed claim
   * carrying `client_id` + `invite_id` + 24h expiry. On submit
   * the server verifies the signature, creates a ClientReader
   * user, and returns an auth token so the user lands on the
   * portal immediately.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth, inviteToken } = $props()
  const api = $derived(createApiClient(auth))

  let name = $state('')
  let password = $state('')
  let confirm = $state('')
  let busy = $state(false)
  let error = $state(null)

  async function submit(e) {
    e.preventDefault()
    error = null
    if (!name.trim()) { error = 'Name required'; return }
    if (password.length < 8) { error = 'Password must be at least 8 characters'; return }
    if (password !== confirm) { error = 'Passwords do not match'; return }

    busy = true
    try {
      const resp = await fetch('/api/auth/client-signup', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          invite_token: inviteToken,
          name: name.trim(),
          password,
        }),
      })
      if (!resp.ok) {
        const text = await resp.text().catch(() => '')
        throw new Error(text || `${resp.status}`)
      }
      const body = await resp.json()
      // The backend returns a full login response; plug it into
      // the auth store just like the /login path does.
      auth.saveSession(body)
      // Drop the ?invite param from the URL so a back-button
      // + refresh doesn't re-POST.
      window.history.replaceState({}, '', '/')
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      busy = false
    }
  }
</script>

<div class="signup-shell">
  <form class="signup-card" onsubmit={submit}>
    <h1>Welcome</h1>
    <p class="lead">
      You've been invited to the client portal. Set a name and
      password — you'll use these to log in at any time from the
      normal login page.
    </p>
    <label class="field">
      <span>Display name</span>
      <input
        type="text"
        bind:value={name}
        disabled={busy}
        autocomplete="username"
        placeholder="how you'd like to be addressed"
      />
    </label>
    <label class="field">
      <span>Password</span>
      <input
        type="password"
        bind:value={password}
        disabled={busy}
        autocomplete="new-password"
        placeholder="at least 8 characters"
      />
    </label>
    <label class="field">
      <span>Confirm password</span>
      <input
        type="password"
        bind:value={confirm}
        disabled={busy}
        autocomplete="new-password"
      />
    </label>

    {#if error}
      <div class="error">{error}</div>
    {/if}

    <button type="submit" class="btn primary" disabled={busy}>
      {busy ? 'Creating account…' : 'Create account'}
    </button>

    <p class="hint">
      This invite is valid for 24 hours from when your admin
      generated it. If it's expired, ask them for a fresh link.
    </p>
  </form>
</div>

<style>
  .signup-shell {
    min-height: 100vh;
    background: var(--bg-base);
    display: flex; align-items: center; justify-content: center;
    padding: var(--s-4);
  }
  .signup-card {
    width: 420px; max-width: 100%;
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-lg);
    padding: var(--s-5);
    display: flex; flex-direction: column; gap: var(--s-3);
  }
  h1 { margin: 0; font-size: var(--fs-lg); color: var(--fg-primary); }
  .lead { margin: 0; font-size: var(--fs-sm); color: var(--fg-secondary); }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .field span {
    font-size: 10px; color: var(--fg-muted);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .field input {
    padding: var(--s-2); background: var(--bg-chip);
    border: 1px solid var(--border-subtle); border-radius: var(--r-sm);
    color: var(--fg-primary); font-family: var(--font-mono);
    font-size: var(--fs-sm);
  }
  .error {
    padding: var(--s-2); border-radius: var(--r-sm);
    background: color-mix(in srgb, var(--danger) 15%, transparent);
    color: var(--danger); font-size: var(--fs-xs);
  }
  .btn.primary {
    padding: var(--s-2) var(--s-4);
    background: var(--accent); color: var(--bg-base);
    border: 0; border-radius: var(--r-sm);
    font-size: var(--fs-sm); font-weight: 600;
    cursor: pointer;
  }
  .btn.primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .hint { margin: 0; font-size: 10px; color: var(--fg-muted); text-align: center; }
</style>
