<script>
  /*
   * Wave H1 — password reset via signed admin-issued URL.
   *
   * URL shape: `/password-reset/{reset_token}` — admin mints
   * this link from the Users admin page and hands it to the
   * user out-of-band. Token is HMAC-signed, carries
   * `user_id` + `reset_id` + 1h expiry. On submit the server
   * verifies + updates the password + burns the token.
   *
   * Success lands on `/` so the user types their new password
   * at the normal login form — we do NOT auto-login them
   * because the admin may want to watch the login row hit the
   * audit trail from the user's own browser.
   */
  let { resetToken } = $props()

  let password = $state('')
  let confirm = $state('')
  let busy = $state(false)
  let error = $state(null)
  let done = $state(false)

  async function submit(e) {
    e.preventDefault()
    error = null
    if (password.length < 8) { error = 'Password must be at least 8 characters'; return }
    if (password !== confirm) { error = 'Passwords do not match'; return }

    busy = true
    try {
      const resp = await fetch('/api/auth/password-reset', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          reset_token: resetToken,
          new_password: password,
        }),
      })
      if (!resp.ok) {
        const text = await resp.text().catch(() => '')
        throw new Error(text || `${resp.status}`)
      }
      done = true
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      busy = false
    }
  }

  function goToLogin() {
    window.history.replaceState({}, '', '/')
    window.location.reload()
  }
</script>

<div class="reset-shell">
  <form class="reset-card" onsubmit={submit}>
    <h1>Set new password</h1>
    {#if done}
      <p class="lead">
        Your password has been updated. The reset link is no
        longer valid. Log in with your new password.
      </p>
      <button type="button" class="btn primary" onclick={goToLogin}>
        Go to login
      </button>
    {:else}
      <p class="lead">
        An administrator generated this reset link for you. Pick
        a new password below. The link is valid for one hour and
        only works once.
      </p>
      <label class="field">
        <span>New password</span>
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
        {busy ? 'Updating…' : 'Set new password'}
      </button>

      <p class="hint">
        If this link is expired or already used, ask your admin
        for a fresh one.
      </p>
    {/if}
  </form>
</div>

<style>
  .reset-shell {
    min-height: 100vh;
    background: var(--bg-base);
    display: flex; align-items: center; justify-content: center;
    padding: var(--s-4);
  }
  .reset-card {
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
