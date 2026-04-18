<script>
  import BrandMark from './BrandMark.svelte'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  let apiKey = $state('')
  let error = $state('')
  let loading = $state(false)

  async function handleLogin() {
    if (!apiKey.trim()) return
    error = ''
    loading = true
    try {
      await auth.login(apiKey.trim())
    } catch (e) {
      error = e.message || 'Login failed'
    } finally {
      loading = false
    }
  }
  function handleKeydown(e) { if (e.key === 'Enter') handleLogin() }
</script>

<div class="login-root">
  <div class="bg-grid" aria-hidden="true"></div>
  <div class="bg-orb orb-a" aria-hidden="true"></div>
  <div class="bg-orb orb-b" aria-hidden="true"></div>

  <main class="login-card" role="main">
    <div class="brand">
      <BrandMark size={34} withText={true} />
      <div class="brand-sub">Operator console · HMAC session</div>
    </div>

    <form class="form" onsubmit={(e) => { e.preventDefault(); handleLogin() }}>
      <div class="field">
        <label class="label" for="api-key">API key</label>
        <input
          id="api-key"
          type="password"
          autocomplete="off"
          spellcheck="false"
          placeholder="paste your operator key"
          bind:value={apiKey}
          onkeydown={handleKeydown}
          disabled={loading}
        />
      </div>

      {#if error}
        <div class="error" role="alert">
          <Icon name="alert" size={14} />
          <span>{error}</span>
        </div>
      {/if}

      <button type="submit" class="btn btn-primary btn-lg" disabled={loading || !apiKey.trim()}>
        {#if loading}
          <span class="spinner"></span>
          <span>Authenticating…</span>
        {:else}
          <span>Sign in</span>
          <Icon name="chevronR" size={16} />
        {/if}
      </button>
    </form>

    <div class="roles">
      <span class="chip" data-role="admin">Admin</span>
      <span class="chip" data-role="operator">Operator</span>
      <span class="chip" data-role="viewer">Viewer</span>
    </div>

    <p class="fineprint">
      Tokens are HMAC-SHA256, 24 h lifetime. Every attempt is audited.
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

  input {
    width: 100%;
    padding: 12px 14px;
    background: rgba(10, 14, 20, 0.8);
    color: var(--fg-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--r-lg);
    font-family: var(--font-mono);
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
  @keyframes spin { to { transform: rotate(360deg); } }

  .roles {
    display: flex; gap: var(--s-2); justify-content: center;
    margin-top: var(--s-6);
  }
  .chip[data-role='admin']    { background: var(--critical-bg); border-color: rgba(220, 38, 38, 0.35); color: var(--critical); }
  .chip[data-role='operator'] { background: var(--warn-bg); border-color: rgba(245, 158, 11, 0.35); color: var(--warn); }
  .chip[data-role='viewer']   { background: var(--pos-bg); border-color: rgba(34, 197, 94, 0.35); color: var(--pos); }

  .fineprint {
    margin-top: var(--s-4);
    font-size: var(--fs-2xs);
    line-height: var(--lh-snug);
    color: var(--fg-muted);
    text-align: center;
  }
</style>
