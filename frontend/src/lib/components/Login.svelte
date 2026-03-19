<script>
  let { auth } = $props()
  let apiKey = $state('')
  let error = $state('')
  let loading = $state(false)

  async function handleLogin() {
    if (!apiKey.trim()) return
    error = ''
    loading = true
    try {
      await auth.login(apiKey)
    } catch (e) {
      error = e.message || 'Login failed'
    }
    loading = false
  }

  function handleKeydown(e) {
    if (e.key === 'Enter') handleLogin()
  }
</script>

<div class="login-overlay">
  <div class="login-card">
    <div class="logo">
      <svg viewBox="0 0 32 32" width="48" height="48">
        <rect width="32" height="32" rx="4" fill="#161b22"/>
        <text x="16" y="22" text-anchor="middle" font-family="monospace" font-size="18" font-weight="bold" fill="#58a6ff">M</text>
      </svg>
    </div>

    <h2>Market Maker</h2>
    <p class="subtitle">Enter your API key to access the dashboard</p>

    <div class="form">
      <input
        type="password"
        placeholder="API Key"
        bind:value={apiKey}
        onkeydown={handleKeydown}
        disabled={loading}
      />

      {#if error}
        <div class="error">{error}</div>
      {/if}

      <button onclick={handleLogin} disabled={loading || !apiKey.trim()}>
        {loading ? 'Authenticating...' : 'Login'}
      </button>
    </div>

    <div class="help">
      <p>Contact your MM operator for access credentials.</p>
      <p class="roles">
        <span class="role admin">Admin</span> Full control
        <span class="role operator">Operator</span> View + controls
        <span class="role viewer">Viewer</span> Read-only
      </p>
    </div>
  </div>
</div>

<style>
  .login-overlay {
    position: fixed; inset: 0;
    display: flex; align-items: center; justify-content: center;
    background: #0a0e17;
    z-index: 1000;
  }
  .login-card {
    background: #161b22;
    border: 1px solid #21262d;
    border-radius: 12px;
    padding: 40px;
    width: 400px;
    text-align: center;
  }
  .logo { margin-bottom: 16px; }
  h2 {
    font-size: 20px; font-weight: 700; color: #58a6ff;
    margin-bottom: 4px;
  }
  .subtitle { color: #8b949e; font-size: 13px; margin-bottom: 24px; }
  .form { display: flex; flex-direction: column; gap: 12px; }
  input {
    padding: 10px 14px; border: 1px solid #30363d; border-radius: 6px;
    background: #0d1117; color: #e1e4e8;
    font-family: inherit; font-size: 14px; outline: none;
  }
  input:focus { border-color: #58a6ff; }
  button {
    padding: 10px; border: none; border-radius: 6px;
    background: #238636; color: #fff;
    font-family: inherit; font-size: 14px; font-weight: 600;
    cursor: pointer;
  }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  button:hover:not(:disabled) { background: #2ea043; }
  .error {
    color: #f85149; font-size: 12px; padding: 8px;
    background: rgba(248, 81, 73, 0.1); border-radius: 4px;
  }
  .help { margin-top: 24px; color: #484f58; font-size: 11px; }
  .roles { margin-top: 8px; display: flex; gap: 8px; justify-content: center; flex-wrap: wrap; align-items: center; }
  .role {
    padding: 2px 6px; border-radius: 3px; font-size: 10px; font-weight: 700;
  }
  .admin { background: #da3633; color: #fff; }
  .operator { background: #d29922; color: #000; }
  .viewer { background: #238636; color: #fff; }
</style>
