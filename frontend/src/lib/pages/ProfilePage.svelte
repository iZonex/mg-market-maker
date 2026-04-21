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
  import QRCode from 'qrcode'
  import Card from '../components/Card.svelte'
  import Icon from '../components/Icon.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  let me = $state(null)
  let loadError = $state(null)

  // Password change ─────────────────────────────
  let pwOld = $state('')
  let pwNew = $state('')
  let pwConfirm = $state('')
  let pwBusy = $state(false)
  let pwMsg = $state(null)

  // TOTP enrollment ─────────────────────────────
  let enrollment = $state(null)     // { secret_base32, otpauth, qrSvg }
  let totpBusy = $state(false)
  let totpCode = $state('')
  let totpMsg = $state(null)
  let secretCopied = $state(false)

  // TOTP disable ────────────────────────────────
  let disablePw = $state('')
  let disableBusy = $state(false)
  let disableMsg = $state(null)
  let showDisable = $state(false)

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

  async function startEnroll() {
    totpMsg = null
    totpBusy = true
    try {
      const r = await api.authedFetch('/api/auth/totp/enroll', { method: 'POST' })
      if (!r.ok) throw new Error(await r.text() || 'enroll failed')
      const data = await r.json()
      // QRCode library renders a complete SVG string — fully RFC-4226
      // compliant, every authenticator app understands it.
      const qrSvg = await QRCode.toString(data.otpauth, {
        type: 'svg',
        margin: 1,
        width: 220,
        errorCorrectionLevel: 'M',
        color: { dark: '#0a0e14', light: '#ffffff' },
      })
      enrollment = { ...data, qrSvg }
    } catch (err) {
      totpMsg = { tone: 'err', text: err.message || 'Enrollment failed' }
    } finally {
      totpBusy = false
    }
  }

  async function verifyEnroll(e) {
    e.preventDefault()
    totpMsg = null
    if (!/^\d{6}$/.test(totpCode)) {
      totpMsg = { tone: 'err', text: 'Enter the 6-digit code from your app' }
      return
    }
    totpBusy = true
    try {
      const r = await api.authedFetch('/api/auth/totp/verify', {
        method: 'POST',
        body: JSON.stringify({ code: totpCode }),
      })
      if (!r.ok) throw new Error(await r.text() || 'verify failed')
      enrollment = null
      totpCode = ''
      totpMsg = { tone: 'ok', text: '2FA enabled. You will be asked for a code on next sign-in.' }
      await refreshMe()
    } catch (err) {
      totpMsg = { tone: 'err', text: err.message || 'Code did not match' }
    } finally {
      totpBusy = false
    }
  }

  async function disableTotp(e) {
    e.preventDefault()
    disableMsg = null
    if (!disablePw) {
      disableMsg = { tone: 'err', text: 'Enter your password to confirm' }
      return
    }
    disableBusy = true
    try {
      const r = await api.authedFetch('/api/auth/totp/disable', {
        method: 'POST',
        body: JSON.stringify({ password: disablePw }),
      })
      if (!r.ok) throw new Error(await r.text() || 'disable failed')
      disablePw = ''
      showDisable = false
      disableMsg = { tone: 'ok', text: '2FA disabled' }
      await refreshMe()
    } catch (err) {
      disableMsg = { tone: 'err', text: err.message || 'Disable failed' }
    } finally {
      disableBusy = false
    }
  }

  function cancelEnroll() {
    enrollment = null
    totpCode = ''
    totpMsg = null
    secretCopied = false
  }

  async function copySecret() {
    if (!enrollment) return
    try {
      await navigator.clipboard.writeText(enrollment.secret_base32)
      secretCopied = true
      setTimeout(() => (secretCopied = false), 2000)
    } catch (_) {}
  }

  function formatDate(ms) {
    if (!ms) return '—'
    return new Date(ms).toLocaleString(undefined, {
      year: 'numeric', month: 'short', day: 'numeric',
      hour: '2-digit', minute: '2-digit',
    })
  }
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
            <button type="submit" class="btn primary" disabled={pwBusy || !pwOld || !pwNew}>
              {#if pwBusy}<span class="spinner"></span>{/if}
              <span>{pwBusy ? 'Changing…' : 'Change password'}</span>
            </button>
          </div>
        </form>
      {/snippet}
    </Card>

    <!-- ── Two-factor auth ─────────────────────────────── -->
    <Card
      title="Two-factor authentication"
      subtitle="RFC 6238 TOTP · Google Authenticator, 1Password, Authy, …"
      span={1}
    >
      {#snippet children()}
        {#if !me}
          <div class="muted">Loading…</div>
        {:else if enrollment}
          <!-- Enrollment in progress -->
          <div class="enroll">
            <div class="enroll-steps">
              <div class="step">
                <span class="step-n">1</span>
                <span>Scan the QR code with your authenticator app</span>
              </div>
              <div class="step">
                <span class="step-n">2</span>
                <span>Enter the 6-digit code it shows to confirm</span>
              </div>
            </div>
            <div class="enroll-body">
              <div class="qr-frame">{@html enrollment.qrSvg}</div>
              <div class="enroll-right">
                <div class="secret-block">
                  <label>Can't scan? Enter this secret manually</label>
                  <div class="secret-row">
                    <code class="secret">{enrollment.secret_base32}</code>
                    <button type="button" class="btn ghost small" onclick={copySecret}>
                      <Icon name={secretCopied ? 'check' : 'link'} size={12} />
                      <span>{secretCopied ? 'Copied' : 'Copy'}</span>
                    </button>
                  </div>
                </div>
                <form class="stacked-form" onsubmit={verifyEnroll}>
                  <div class="field">
                    <label for="totp-code">6-digit code</label>
                    <input
                      id="totp-code"
                      type="text"
                      inputmode="numeric"
                      pattern="\d{'{'}6{'}'}"
                      maxlength="6"
                      autocomplete="one-time-code"
                      class="code-input"
                      bind:value={totpCode}
                      disabled={totpBusy}
                      placeholder="••••••"
                    />
                  </div>
                  {#if totpMsg}
                    <div class="inline-msg {totpMsg.tone === 'ok' ? 'ok' : 'err'}">
                      <Icon name={totpMsg.tone === 'ok' ? 'check' : 'alert'} size={12} />
                      <span>{totpMsg.text}</span>
                    </div>
                  {/if}
                  <div class="actions">
                    <button type="button" class="btn ghost" onclick={cancelEnroll} disabled={totpBusy}>Cancel</button>
                    <button type="submit" class="btn primary" disabled={totpBusy || totpCode.length !== 6}>
                      {#if totpBusy}<span class="spinner"></span>{/if}
                      <span>{totpBusy ? 'Verifying…' : 'Verify & enable'}</span>
                    </button>
                  </div>
                </form>
              </div>
            </div>
          </div>
        {:else if me.totp_enabled}
          <!-- Already enabled: show status + offer disable -->
          <div class="totp-enabled">
            <div class="enabled-panel">
              <span class="shield">
                <Icon name="shield" size={20} />
              </span>
              <div class="enabled-text">
                <div class="enabled-title">Two-factor is on</div>
                <div class="enabled-sub">You'll enter a 6-digit code on every sign-in.</div>
              </div>
              {#if !showDisable}
                <button type="button" class="btn ghost" onclick={() => (showDisable = true)}>Disable…</button>
              {/if}
            </div>

            {#if disableMsg}
              <div class="inline-msg {disableMsg.tone === 'ok' ? 'ok' : 'err'}">
                <Icon name={disableMsg.tone === 'ok' ? 'check' : 'alert'} size={12} />
                <span>{disableMsg.text}</span>
              </div>
            {/if}

            {#if showDisable}
              <form class="stacked-form disable-form" onsubmit={disableTotp}>
                <div class="warning-banner">
                  <Icon name="alert" size={14} />
                  <span>Turning 2FA off weakens this account to password-only. Confirm with your password.</span>
                </div>
                <div class="field">
                  <label for="dis-pw">Current password</label>
                  <input id="dis-pw" type="password" autocomplete="current-password" bind:value={disablePw} disabled={disableBusy} />
                </div>
                <div class="actions">
                  <button type="button" class="btn ghost" onclick={() => { showDisable = false; disablePw = '' }} disabled={disableBusy}>Cancel</button>
                  <button type="submit" class="btn danger" disabled={disableBusy || !disablePw}>
                    {#if disableBusy}<span class="spinner"></span>{/if}
                    <span>{disableBusy ? 'Disabling…' : 'Disable 2FA'}</span>
                  </button>
                </div>
              </form>
            {/if}
          </div>
        {:else}
          <!-- Not enabled: offer enable -->
          <div class="totp-pitch">
            <div class="pitch-icon"><Icon name="shield" size={22} /></div>
            <div class="pitch-text">
              <div class="pitch-title">Add a second factor</div>
              <div class="pitch-sub">
                Protects your account even if the password is leaked. Works with any RFC&nbsp;6238 authenticator.
              </div>
            </div>
            {#if totpMsg}
              <div class="inline-msg {totpMsg.tone === 'ok' ? 'ok' : 'err'}">
                <Icon name={totpMsg.tone === 'ok' ? 'check' : 'alert'} size={12} />
                <span>{totpMsg.text}</span>
              </div>
            {/if}
            <button type="button" class="btn primary large" disabled={totpBusy} onclick={startEnroll}>
              {#if totpBusy}<span class="spinner"></span>{/if}
              <span>{totpBusy ? 'Starting…' : 'Enable 2FA'}</span>
            </button>
          </div>
        {/if}
      {/snippet}
    </Card>
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
  .muted { color: var(--fg-muted); font-size: var(--fs-sm); }

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
  .code-input {
    font-family: var(--font-mono);
    font-size: var(--fs-lg);
    letter-spacing: 0.4em;
    text-align: center;
  }

  .actions { display: flex; gap: var(--s-2); justify-content: flex-end; margin-top: var(--s-1); }

  .btn {
    display: inline-flex; align-items: center; gap: var(--s-2);
    padding: 9px 18px;
    border: 1px solid;
    border-radius: var(--r-md);
    font-size: var(--fs-sm);
    font-weight: 600;
    background: transparent;
    color: inherit;
    cursor: pointer;
    font-family: var(--font-sans);
    transition: background var(--dur-fast) var(--ease-out),
                border-color var(--dur-fast) var(--ease-out),
                color var(--dur-fast) var(--ease-out);
  }
  .btn.small { padding: 5px 10px; font-size: var(--fs-xs); }
  .btn.large { padding: 11px 22px; font-size: var(--fs-md); }
  .btn.primary {
    background: var(--accent);
    color: #001510;
    border-color: var(--accent);
  }
  .btn.primary:hover:not(:disabled) { filter: brightness(1.1); }
  .btn.ghost {
    color: var(--fg-secondary);
    border-color: var(--border-default);
  }
  .btn.ghost:hover:not(:disabled) { background: var(--bg-chip); color: var(--fg-primary); }
  .btn.danger {
    color: var(--danger);
    border-color: var(--danger);
  }
  .btn.danger:hover:not(:disabled) { background: rgba(239, 68, 68, 0.1); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }

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

  .chip {
    display: inline-flex; align-items: center; gap: 4px;
    font-family: var(--font-sans);
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    padding: 3px 8px;
    border-radius: var(--r-sm);
    border: 1px solid currentColor;
  }
  .chip.tone-ok    { color: var(--accent); }
  .chip.tone-muted { color: var(--fg-muted); }
  .chip.role-admin    { color: var(--critical, #dc2626); }
  .chip.role-operator { color: var(--warn); }
  .chip.role-viewer   { color: var(--pos, #22c55e); }

  /* ── 2FA enabled panel ─────────────────────── */
  .totp-enabled { display: flex; flex-direction: column; gap: var(--s-3); }
  .enabled-panel {
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: var(--s-3);
    align-items: center;
    padding: var(--s-3) var(--s-4);
    background: rgba(0, 209, 178, 0.06);
    border: 1px solid rgba(0, 209, 178, 0.2);
    border-radius: var(--r-md);
  }
  .shield {
    width: 40px; height: 40px;
    display: flex; align-items: center; justify-content: center;
    border-radius: 50%;
    background: rgba(0, 209, 178, 0.12);
    color: var(--accent);
  }
  .enabled-text { min-width: 0; }
  .enabled-title { font-weight: 600; color: var(--fg-primary); font-size: var(--fs-sm); }
  .enabled-sub { color: var(--fg-muted); font-size: var(--fs-xs); margin-top: 2px; }

  .disable-form {
    padding: var(--s-3) var(--s-4);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .warning-banner {
    display: flex; gap: var(--s-2); align-items: flex-start;
    padding: var(--s-2) var(--s-3);
    background: rgba(245, 158, 11, 0.08);
    border: 1px solid rgba(245, 158, 11, 0.25);
    border-radius: var(--r-sm);
    color: var(--warn);
    font-size: var(--fs-xs);
    line-height: 1.45;
  }

  /* ── 2FA pitch (not enabled) ───────────────── */
  .totp-pitch {
    display: flex; flex-direction: column; gap: var(--s-3);
    align-items: flex-start;
    padding: var(--s-4);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .pitch-icon {
    width: 40px; height: 40px;
    display: flex; align-items: center; justify-content: center;
    border-radius: 50%;
    background: var(--bg-raised);
    color: var(--fg-secondary);
  }
  .pitch-text { max-width: 480px; }
  .pitch-title { font-weight: 600; color: var(--fg-primary); font-size: var(--fs-md); }
  .pitch-sub { color: var(--fg-muted); font-size: var(--fs-sm); margin-top: 4px; line-height: 1.5; }

  /* ── 2FA enrollment ─────────────────────────── */
  .enroll { display: flex; flex-direction: column; gap: var(--s-4); }
  .enroll-steps {
    display: flex; flex-direction: column; gap: var(--s-2);
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .step {
    display: flex; align-items: center; gap: var(--s-3);
    font-size: var(--fs-sm);
    color: var(--fg-secondary);
  }
  .step-n {
    flex-shrink: 0;
    width: 22px; height: 22px;
    display: flex; align-items: center; justify-content: center;
    border-radius: 50%;
    background: var(--accent-dim);
    color: var(--accent);
    font-size: var(--fs-xs);
    font-weight: 700;
  }

  .enroll-body {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: var(--s-4);
    align-items: start;
  }
  @media (max-width: 520px) {
    .enroll-body { grid-template-columns: 1fr; justify-items: center; }
  }
  .qr-frame {
    padding: var(--s-2);
    background: #ffffff;
    border-radius: var(--r-md);
    border: 1px solid var(--border-subtle);
    display: flex; align-items: center; justify-content: center;
  }
  .enroll-right { display: flex; flex-direction: column; gap: var(--s-3); min-width: 0; }

  .secret-block { display: flex; flex-direction: column; gap: 6px; }
  .secret-block label {
    font-size: 11px;
    color: var(--fg-muted);
    letter-spacing: 0.02em;
  }
  .secret-row { display: flex; gap: var(--s-2); align-items: stretch; }
  .secret {
    flex: 1; min-width: 0;
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    padding: 8px 10px;
    background: rgba(10, 14, 20, 0.5);
    border: 1px solid var(--border-default);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    word-break: break-all;
    line-height: 1.4;
  }
</style>
