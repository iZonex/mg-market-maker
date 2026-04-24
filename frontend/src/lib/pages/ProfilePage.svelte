<script>
  /*
   * Profile page — self-service account settings.
   *
   * Three sections stacked on a narrow column for readability:
   *   1. Identity summary — read-only overview
   *   2. Change password — old + new + confirm
   *   3. Two-factor (TOTP) — enroll with proper QR (via the
   *      `qrcode` library, not handrolled), verify, disable
   */
  import Card from '../components/Card.svelte'
  import Icon from '../components/Icon.svelte'
  import TotpCard from '../components/profile/TotpCard.svelte'
  import { createApiClient } from '../api.svelte.js'
  import { Button } from '../primitives/index.js'
  import { fmtDate } from '../format.js'
  const formatDate = (ms) => fmtDate(ms, 'full')

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let me = $state(null)
  let loadError = $state(null)

  // Password change ─────────────────────────────
  let pwOld = $state('')
  let pwNew = $state('')
  let pwConfirm = $state('')
  let pwBusy = $state(false)
  let pwMsg = $state(null)

  // TOTP state lives in <TotpCard>; parent only passes `me` +
  // `onChanged` so it can re-fetch the user row after enrol/disable.

  async function refreshMe() {
    try {
      me = await api.getJson('/api/auth/me')
      loadError = null
    } catch (e) {
      loadError = e?.message || String(e)
    }
  }

  $effect(() => { refreshMe() })

  async function submitPasswordChange(e) {
    e.preventDefault()
    pwMsg = null
    if (pwNew.length < 8) {
      pwMsg = { tone: 'err', text: 'New password must be at least 8 characters' }
      return
    }
    if (pwNew !== pwConfirm) {
      pwMsg = { tone: 'err', text: 'Confirmation does not match' }
      return
    }
    pwBusy = true
    try {
      const r = await api.authedFetch('/api/auth/password', {
        method: 'POST',
        body: JSON.stringify({ old_password: pwOld, new_password: pwNew }),
      })
      if (!r.ok) throw new Error(await r.text() || r.statusText)
      pwMsg = { tone: 'ok', text: 'Password changed successfully' }
      pwOld = ''; pwNew = ''; pwConfirm = ''
    } catch (err) {
      pwMsg = { tone: 'err', text: err.message || 'Change failed' }
    } finally {
      pwBusy = false
    }
  }


  // formatDate = fmtDate(ms, 'full') from format.js.
</script>

<div class="page scroll">
  <div class="container">
    <header class="page-header">
      <h1>Profile</h1>
      <p class="page-sub">Manage your account security and session</p>
    </header>

    {#if loadError}
      <div class="global-error">
        <Icon name="alert" size={14} />
        <span>{loadError}</span>
      </div>
    {/if}

    <!-- ── Identity ───────────────────────────────────── -->
    <Card title="Identity" subtitle="account overview" span={1}>
      {#snippet children()}
        {#if !me}
          <div class="muted">Loading…</div>
        {:else}
          <div class="identity">
            <div class="avatar" data-role={me.role}>
              {me.name.slice(0, 2).toUpperCase()}
            </div>
            <div class="identity-text">
              <div class="name">{me.name}</div>
              <div class="sub">
                <span class="chip role-{me.role}">{me.role}</span>
                {#if me.totp_enabled}
                  <span class="chip tone-ok">
                    <Icon name="shield" size={10} />
                    <span>2FA ON</span>
                  </span>
                {:else}
                  <span class="chip tone-muted">2FA OFF</span>
                {/if}
              </div>
            </div>
            <div class="created">
              <span class="k">Created</span>
              <span class="v">{formatDate(me.created_at_ms)}</span>
            </div>
          </div>
        {/if}
      {/snippet}
    </Card>

    <!-- ── Password ───────────────────────────────────── -->
    <Card title="Password" subtitle="argon2id · never stored in plaintext" span={1}>
      {#snippet children()}
        <form class="stacked-form" onsubmit={submitPasswordChange}>
          <div class="field">
            <label for="pw-old">Current password</label>
            <input id="pw-old" type="password" autocomplete="current-password" bind:value={pwOld} disabled={pwBusy} />
          </div>
          <div class="field-row">
            <div class="field">
              <label for="pw-new">New password</label>
              <input id="pw-new" type="password" autocomplete="new-password" bind:value={pwNew} disabled={pwBusy} placeholder="at least 8 characters" />
            </div>
            <div class="field">
              <label for="pw-confirm">Confirm</label>
              <input id="pw-confirm" type="password" autocomplete="new-password" bind:value={pwConfirm} disabled={pwBusy} />
            </div>
          </div>
          {#if pwMsg}
            <div class="inline-msg {pwMsg.tone === 'ok' ? 'ok' : 'err'}">
              <Icon name={pwMsg.tone === 'ok' ? 'check' : 'alert'} size={12} />
              <span>{pwMsg.text}</span>
            </div>
          {/if}
          <div class="actions">
            <Button variant="primary" type="submit" disabled={pwBusy || !pwOld || !pwNew}>
          {#snippet children()}{#if pwBusy}<span class="spinner"></span>{/if}
              <span>{pwBusy ? 'Changing…' : 'Change password'}</span>{/snippet}
        </Button>
          </div>
        </form>
      {/snippet}
    </Card>

    <!-- ── Two-factor auth ─────────────────────────────── -->
    <TotpCard {auth} {me} onChanged={refreshMe} />
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .container {
    max-width: 720px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--s-5);
  }
  .page-header {
    margin-bottom: var(--s-2);
  }
  .page-header h1 {
    margin: 0 0 var(--s-1);
    font-size: var(--fs-xl);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: var(--tracking-tight);
  }
  .page-sub {
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--fs-sm);
  }
  .global-error {
    display: flex; gap: var(--s-2); align-items: center;
    padding: var(--s-3);
    background: rgba(239, 68, 68, 0.08);
    border: 1px solid rgba(239, 68, 68, 0.25);
    border-radius: var(--r-md);
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  /* ── Identity ──────────────────────────────── */
  .identity {
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: var(--s-4);
    align-items: center;
  }
  .avatar {
    width: 56px; height: 56px;
    display: flex; align-items: center; justify-content: center;
    font-size: var(--fs-lg); font-weight: 700;
    border-radius: 50%;
    background: var(--bg-chip);
    color: var(--fg-primary);
  }
  .avatar[data-role='admin']    { background: var(--critical-bg); color: var(--critical); box-shadow: inset 0 0 0 1.5px rgba(220, 38, 38, 0.35); }
  .avatar[data-role='operator'] { background: var(--warn-bg);     color: var(--warn);     box-shadow: inset 0 0 0 1.5px rgba(245, 158, 11, 0.35); }
  .avatar[data-role='viewer']   { background: var(--pos-bg);      color: var(--pos);      box-shadow: inset 0 0 0 1.5px rgba(34, 197, 94, 0.35); }

  .identity-text { min-width: 0; display: flex; flex-direction: column; gap: 6px; }
  .name {
    font-size: var(--fs-lg);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: var(--tracking-tight);
  }
  .sub { display: flex; gap: var(--s-2); flex-wrap: wrap; align-items: center; }

  .created {
    display: flex; flex-direction: column; gap: 2px;
    text-align: right;
    font-size: var(--fs-xs);
  }
  .created .k {
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-size: 10px;
  }
  .created .v { color: var(--fg-primary); font-family: var(--font-mono); }

  /* ── Forms ─────────────────────────────────── */
  .stacked-form { display: flex; flex-direction: column; gap: var(--s-3); }
  .field { display: flex; flex-direction: column; gap: 6px; }
  .field-row { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-3); }
  @media (max-width: 520px) { .field-row { grid-template-columns: 1fr; } }
  .field label {
    font-size: 11px;
    font-weight: 500;
    color: var(--fg-muted);
    letter-spacing: 0.02em;
  }
  .field input {
    padding: 10px 12px;
    background: rgba(10, 14, 20, 0.5);
    color: var(--fg-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    outline: none;
    transition: border-color var(--dur-fast) var(--ease-out),
                box-shadow var(--dur-fast) var(--ease-out),
                background var(--dur-fast) var(--ease-out);
  }
  .field input:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-ring);
    background: rgba(10, 14, 20, 0.7);
  }
  .field input:disabled { opacity: 0.5; cursor: not-allowed; }

  .actions { display: flex; gap: var(--s-2); justify-content: flex-end; margin-top: var(--s-1); }

  .btn.primary:hover:not(:disabled) { filter: brightness(1.1); }
  .btn.ghost:hover:not(:disabled) { background: var(--bg-chip); color: var(--fg-primary); }
  .btn.danger:hover:not(:disabled) { background: rgba(239, 68, 68, 0.1); }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid rgba(255, 255, 255, 0.2);
    border-top-color: currentColor;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .inline-msg {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 8px 12px;
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
    line-height: 1.4;
  }
  .inline-msg.ok  { color: var(--accent); background: rgba(0, 209, 178, 0.10); border: 1px solid rgba(0, 209, 178, 0.25); }
  .inline-msg.err { color: var(--danger); background: rgba(239, 68, 68, 0.08);  border: 1px solid rgba(239, 68, 68, 0.25); }
  /* ── 2FA enabled panel ─────────────────────── */


  /* ── 2FA pitch (not enabled) ───────────────── */

  /* ── 2FA enrollment ─────────────────────────── */


  .secret-block .secret-label {
    font-size: 11px;
    color: var(--fg-muted);
    letter-spacing: 0.02em;
  }
</style>
