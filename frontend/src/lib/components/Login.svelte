<script>
  import BrandMark from './BrandMark.svelte'
  import Icon from './Icon.svelte'

  let { auth } = $props()

  // On mount: ask the server whether we should render the
  // first-run bootstrap form (no users yet) or the normal
  // login form. Falls back to login on failure so a reachable
  // server with a degraded status endpoint still shows
  // something interactive.
  let mode = $state('loading') // 'loading' | 'bootstrap' | 'login' | 'totp'
  let name = $state('')
  let password = $state('')
  let passwordConfirm = $state('')
  let totpCode = $state('')
  let error = $state('')
  let loading = $state(false)

  $effect(() => {
    auth.checkStatus()
      .then(s => { mode = s.needs_bootstrap ? 'bootstrap' : 'login' })
      .catch(() => { mode = 'login' })
  })

  async function handleLogin() {
    if (!name.trim() || !password) return
    error = ''
    loading = true
    try {
      await auth.login({ name: name.trim(), password })
    } catch (e) {
      if (e.needsTotp) {
        mode = 'totp'
        error = ''
      } else if (e.mustEnrollTotp) {
        error = e.message
      } else {
        error = e.message || 'Login failed'
      }
    } finally {
      loading = false
    }
  }

  async function handleTotp() {
    if (!/^\d{6}$/.test(totpCode)) {
      error = 'Enter the 6-digit code'
      return
    }
    error = ''
    loading = true
    try {
      await auth.login({ name: name.trim(), password, totpCode })
    } catch (e) {
      error = e.message || 'Code did not match'
    } finally {
      loading = false
    }
  }

  async function handleBootstrap() {
    if (!name.trim() || password.length < 8) {
      error = 'Password must be at least 8 characters'
      return
    }
    if (password !== passwordConfirm) {
      error = 'Passwords do not match'
      return
    }
    error = ''
    loading = true
    try {
      await auth.bootstrap({ name: name.trim(), password })
    } catch (e) {
      error = e.message || 'Bootstrap failed'
    } finally {
      loading = false
    }
  }

  function handleSubmit(e) {
    e.preventDefault()
    if (mode === 'bootstrap') handleBootstrap()
    else if (mode === 'totp') handleTotp()
    else handleLogin()
  }
</script>

<div class="login-root">
  <div class="bg-grid" aria-hidden="true"></div>
  <div class="bg-orb orb-a" aria-hidden="true"></div>
  <div class="bg-orb orb-b" aria-hidden="true"></div>

  <main class="login-card" role="main">
    <div class="brand">
      <BrandMark size={34} withText={true} />
      <div class="brand-sub">
        {#if mode === 'bootstrap'}
          First run · create the root admin account
        {:else if mode === 'totp'}
          Two-factor required · enter the code from your authenticator
        {:else if mode === 'login'}
          Operator console · sign in
        {:else}
          Checking server state…
        {/if}
      </div>
    </div>

    {#if mode === 'loading'}
      <div class="loading-stub"><span class="spinner big"></span></div>
    {:else}
      <form class="form" onsubmit={handleSubmit}>
        {#if mode === 'bootstrap'}
          <div class="info-banner">
            <Icon name="info" size={14} />
            <span>No users configured. The account you create now becomes the root admin — save the password somewhere safe.</span>
          </div>
        {/if}

        {#if mode !== 'totp'}
          <div class="field">
            <label class="label" for="name">Username</label>
            <input
              id="name"
              type="text"
              autocomplete="username"
              spellcheck="false"
              placeholder={mode === 'bootstrap' ? 'root' : 'your username'}
              bind:value={name}
              disabled={loading}
            />
          </div>

          <div class="field">
            <label class="label" for="password">Password</label>
            <input
              id="password"
              type="password"
              autocomplete={mode === 'bootstrap' ? 'new-password' : 'current-password'}
              placeholder={mode === 'bootstrap' ? 'at least 8 characters' : '••••••••'}
              bind:value={password}
              disabled={loading}
            />
          </div>
        {/if}

        {#if mode === 'bootstrap'}
          <div class="field">
            <label class="label" for="password-confirm">Confirm password</label>
            <input
              id="password-confirm"
              type="password"
              autocomplete="new-password"
              placeholder="repeat password"
              bind:value={passwordConfirm}
              disabled={loading}
            />
          </div>
        {/if}

        {#if mode === 'totp'}
          <div class="field">
            <label class="label" for="totp">6-digit authenticator code</label>
            <input
              id="totp"
              type="text"
              inputmode="numeric"
              pattern="\d{'{'}6{'}'}"
              maxlength="6"
              autocomplete="one-time-code"
              placeholder="123456"
              bind:value={totpCode}
              disabled={loading}
            />
          </div>
        {/if}

        {#if error}
          <div class="error" role="alert">
            <Icon name="alert" size={14} />
            <span>{error}</span>
          </div>
        {/if}

        <button
          type="submit"
          class="btn btn-primary btn-lg"
          disabled={loading
            || (mode === 'totp' ? totpCode.length !== 6 : (!name.trim() || !password))}
        >
          {#if loading}
            <span class="spinner"></span>
            <span>{mode === 'bootstrap' ? 'Creating admin…' : mode === 'totp' ? 'Verifying…' : 'Authenticating…'}</span>
          {:else}
            <span>{mode === 'bootstrap' ? 'Create admin account' : mode === 'totp' ? 'Verify & sign in' : 'Sign in'}</span>
            <Icon name="chevronR" size={16} />
          {/if}
        </button>
      </form>
    {/if}

    <p class="fineprint">
      {#if mode === 'bootstrap'}
        Password is hashed with argon2id before storage. 2FA can be enabled from Settings after first login.
      {:else}
        Session tokens are HMAC-SHA256, 24 h lifetime. Every attempt is audited.
      {/if}
    </p>
  </main>
</div>

<style>
  .login-root {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: #05110e;
    overflow: hidden;
  }
  .bg-grid {
    position: absolute;
    inset: 0;
    background-image:
      linear-gradient(rgba(0, 209, 178, 0.06) 1px, transparent 1px),
      linear-gradient(90deg, rgba(0, 209, 178, 0.06) 1px, transparent 1px);
    background-size: 42px 42px;
    mask-image: radial-gradient(ellipse at center, #000 0%, transparent 70%);
    -webkit-mask-image: radial-gradient(ellipse at center, #000 0%, transparent 70%);
  }
  .bg-orb {
    position: absolute;
    border-radius: 50%;
    filter: blur(120px);
    pointer-events: none;
  }
  .orb-a { width: 560px; height: 560px; background: #00d1b2; opacity: 0.30; top: -15%; left: -10%; }
  .orb-b { width: 420px; height: 420px; background: #3b82f6; opacity: 0.18; bottom: -12%; right: -8%; }

  .login-card {
    position: relative;
    width: 440px;
    max-width: calc(100vw - 32px);
    padding: var(--s-8);
    background: rgba(17, 19, 23, 0.72);
    backdrop-filter: blur(22px) saturate(160%);
    -webkit-backdrop-filter: blur(22px) saturate(160%);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-2xl);
    box-shadow: var(--shadow-lg), inset 0 1px 0 rgba(255, 255, 255, 0.04);
  }

  .brand {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--s-2);
    margin-bottom: var(--s-7);
  }
  .brand-sub {
    font-size: var(--fs-xs);
    color: var(--fg-muted);
    letter-spacing: 0.04em;
  }

  .form { display: flex; flex-direction: column; gap: var(--s-4); }
  .field { display: flex; flex-direction: column; gap: var(--s-2); }

  .info-banner {
    display: flex; align-items: flex-start; gap: var(--s-2);
    padding: var(--s-3);
    background: rgba(0, 209, 178, 0.08);
    border: 1px solid rgba(0, 209, 178, 0.25);
    border-radius: var(--r-md);
    color: var(--accent);
    font-size: var(--fs-xs);
    line-height: 1.5;
  }

  input {
    width: 100%;
    padding: 12px 14px;
    background: rgba(10, 14, 20, 0.8);
    color: var(--fg-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--r-lg);
    font-family: var(--font-sans);
    font-size: var(--fs-md);
    letter-spacing: 0.02em;
    outline: none;
    transition: border-color var(--dur-fast) var(--ease-out),
                box-shadow var(--dur-fast) var(--ease-out);
  }
  input:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-ring);
  }
  input::placeholder { color: var(--fg-faint); }
  input:disabled { opacity: 0.5; cursor: not-allowed; }

  .error {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--neg-bg);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: var(--r-md);
    color: var(--neg);
    font-size: var(--fs-xs);
  }

  .spinner {
    width: 14px; height: 14px;
    border: 2px solid rgba(0, 0, 0, 0.25);
    border-top-color: #001510;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  .spinner.big {
    width: 28px; height: 28px;
    border-width: 3px;
    border-color: rgba(255, 255, 255, 0.08);
    border-top-color: var(--accent);
  }
  .loading-stub {
    display: flex; justify-content: center;
    padding: var(--s-8) 0;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .fineprint {
    margin-top: var(--s-4);
    font-size: var(--fs-2xs);
    line-height: var(--lh-snug);
    color: var(--fg-muted);
    text-align: center;
  }
</style>
