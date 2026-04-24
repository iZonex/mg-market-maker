<script>
  /*
   * TotpCard — self-service 2FA (RFC 6238 TOTP) enrol / verify /
   * disable. Extracted from ProfilePage as part of the design-
   * system wave-5 decomposition. Owns all TOTP state; parent
   * provides `auth` + the current user (`me`) + `onChanged` to
   * re-fetch `me` after any mutation that flips `totp_enabled`.
   */
  import QRCode from 'qrcode'
  import Card from '../Card.svelte'
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'
  import { createApiClient } from '../../api.svelte.js'

  let { auth, me, onChanged = () => {} } = $props()
  const api = $derived(createApiClient(auth))

  let enrollment = $state(null)     // { secret_base32, otpauth, qrSvg }
  let totpBusy = $state(false)
  let totpCode = $state('')
  let totpMsg = $state(null)
  let secretCopied = $state(false)

  let disablePw = $state('')
  let disableBusy = $state(false)
  let disableMsg = $state(null)
  let showDisable = $state(false)

  async function startEnroll() {
    totpMsg = null
    totpBusy = true
    try {
      const r = await api.authedFetch('/api/auth/totp/enroll', { method: 'POST' })
      if (!r.ok) throw new Error(await r.text() || 'enroll failed')
      const data = await r.json()
      const qrSvg = await QRCode.toString(data.otpauth, {
        type: 'svg',
        margin: 1,
        width: 220,
        errorCorrectionLevel: 'M',
        // High-contrast dark-on-white — required for reliable phone-
        // camera scanning. Not themed via `tokens.css` because a
        // low-contrast brand would break the QR for users.
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
      onChanged()
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
      onChanged()
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
    } catch {}
  }
</script>

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
              <span class="secret-label">Can't scan? Enter this secret manually</span>
              <div class="secret-row">
                <code class="secret">{enrollment.secret_base32}</code>
                <Button variant="ghost" size="sm" onclick={copySecret}>
                  {#snippet children()}<Icon name={secretCopied ? 'check' : 'link'} size={12} />
                    <span>{secretCopied ? 'Copied' : 'Copy'}</span>{/snippet}
                </Button>
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
                <Button variant="ghost" onclick={cancelEnroll} disabled={totpBusy}>
                  {#snippet children()}Cancel{/snippet}
                </Button>
                <Button variant="primary" type="submit" loading={totpBusy} disabled={totpCode.length !== 6}>
                  {#snippet children()}Verify & enable{/snippet}
                </Button>
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
            <Button variant="ghost" onclick={() => (showDisable = true)}>
              {#snippet children()}Disable…{/snippet}
            </Button>
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
              <Button variant="ghost" onclick={() => { showDisable = false; disablePw = '' }} disabled={disableBusy}>
                {#snippet children()}Cancel{/snippet}
              </Button>
              <Button variant="danger" type="submit" loading={disableBusy} disabled={!disablePw}>
                {#snippet children()}Disable 2FA{/snippet}
              </Button>
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
        <Button variant="primary" loading={totpBusy} onclick={startEnroll}>
          {#snippet children()}Enable 2FA{/snippet}
        </Button>
      </div>
    {/if}
  {/snippet}
</Card>

<style>
  .stacked-form { display: flex; flex-direction: column; gap: var(--s-3); }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .field label { font-size: 10px; letter-spacing: var(--tracking-label); text-transform: uppercase; color: var(--fg-muted); }
  .field input {
    background: var(--bg-base); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); padding: 6px 10px;
    color: var(--fg-primary); font: inherit; min-height: 32px;
  }
  .actions { display: flex; gap: var(--s-2); justify-content: flex-end; }
  .inline-msg {
    display: inline-flex; align-items: center; gap: 6px;
    font-size: var(--fs-xs);
  }
  .inline-msg.ok  { color: var(--pos); }
  .inline-msg.err { color: var(--danger); }

  .enroll { display: flex; flex-direction: column; gap: var(--s-4); }
  .enroll-steps {
    display: flex; flex-direction: column; gap: var(--s-2);
    padding: var(--s-3); background: var(--bg-chip);
    border-radius: var(--r-md); font-size: var(--fs-sm);
  }
  .step { display: flex; align-items: center; gap: var(--s-2); }
  .step-n {
    width: 22px; height: 22px; border-radius: 50%;
    background: var(--accent-dim); color: var(--accent);
    display: inline-flex; align-items: center; justify-content: center;
    font-weight: 700; font-size: var(--fs-xs);
  }
  .enroll-body {
    display: grid; grid-template-columns: auto 1fr; gap: var(--s-4);
    align-items: start;
  }
  @media (max-width: 520px) {
    .enroll-body { grid-template-columns: 1fr; justify-items: center; }
  }
  .qr-frame {
    padding: var(--s-2);
    /* White background is REQUIRED for QR contrast (the SVG inside
       renders dark-on-white). Not themed — low-contrast brand would
       break phone-camera scanning. */
    background: #ffffff;
    border-radius: var(--r-sm);
  }
  .enroll-right { display: flex; flex-direction: column; gap: var(--s-4); }
  .secret-block { display: flex; flex-direction: column; gap: 4px; }
  .secret-label { font-size: 10px; letter-spacing: var(--tracking-label); text-transform: uppercase; color: var(--fg-muted); }
  .secret-row { display: flex; gap: var(--s-2); align-items: center; }
  .secret {
    flex: 1; padding: 6px 10px; font-family: var(--font-mono);
    font-size: var(--fs-xs); background: var(--bg-base);
    border: 1px solid var(--border-subtle); border-radius: var(--r-sm);
    overflow: hidden; text-overflow: ellipsis;
  }
  .code-input {
    font-family: var(--font-mono) !important;
    font-size: var(--fs-xl) !important;
    letter-spacing: 8px; text-align: center;
  }

  .totp-enabled { display: flex; flex-direction: column; gap: var(--s-3); }
  .enabled-panel {
    display: flex; align-items: center; gap: var(--s-3);
    padding: var(--s-3); background: var(--pos-bg);
    border-radius: var(--r-md);
  }
  .shield { color: var(--pos); }
  .enabled-text { flex: 1; display: flex; flex-direction: column; gap: 2px; }
  .enabled-title { font-weight: 600; color: var(--fg-primary); }
  .enabled-sub { font-size: var(--fs-xs); color: var(--fg-muted); }
  .disable-form { padding: var(--s-3); background: var(--bg-chip); border-radius: var(--r-md); }
  .warning-banner {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2); background: var(--warn-bg);
    color: var(--warn); border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }

  .totp-pitch {
    display: flex; flex-direction: column; align-items: center;
    gap: var(--s-3); padding: var(--s-4);
    background: var(--bg-chip); border-radius: var(--r-md);
    text-align: center;
  }
  .pitch-icon {
    width: 48px; height: 48px; border-radius: 50%;
    background: var(--accent-dim); color: var(--accent);
    display: inline-flex; align-items: center; justify-content: center;
  }
  .pitch-title { font-weight: 600; color: var(--fg-primary); }
  .pitch-sub { font-size: var(--fs-xs); color: var(--fg-muted); max-width: 320px; }
</style>
