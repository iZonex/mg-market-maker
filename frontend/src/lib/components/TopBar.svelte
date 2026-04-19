<script>
  import Icon from './Icon.svelte'

  // Top context bar — symbol switcher + rx-freshness + user menu.
  // Sits above the main content area so the operator's context
  // (which symbol am I looking at, how fresh is the data, what
  // mode are we in) never scrolls off-screen.
  import { createApiClient } from '../api.svelte.js'

  let {
    symbols = [],
    activeSymbol = '',
    onSymbolChange,
    rxMs = null,
    connected = false,
    auth,
    route = 'overview',
    symData = {},
    // 23-UX-10 — max kill_level across every symbol the WS has
    // published. Surfaces a global kill badge in the TopBar so
    // operators see L2+ from any page, not just Admin.
    maxKillLevel = 0,
    onKillClick = () => {},
  } = $props()

  // 23-UX-12 — client scope selector. Operators running multiple
  // clients on one engine need to filter which client's legs
  // they're watching. Empty string = "all clients".
  const api = createApiClient(auth)
  let clients = $state([])
  let activeClient = $state('') // '' = all
  let clientMenuOpen = $state(false)

  async function refreshClients() {
    try {
      clients = await api.getJson('/api/v1/clients')
    } catch (_) { clients = [] }
  }
  $effect(() => {
    refreshClients()
    const id = setInterval(refreshClients, 30_000)
    return () => clearInterval(id)
  })

  function pickClient(id) {
    activeClient = id
    clientMenuOpen = false
    // When a client is picked, jump to the first symbol owned
    // by that client (if any) so the Overview refreshes to a
    // matching context. "" (all) leaves the symbol alone.
    if (id) {
      const c = clients.find(c => c.id === id)
      if (c && c.symbols.length > 0 && !c.symbols.includes(activeSymbol)) {
        onSymbolChange?.(c.symbols[0])
      }
    }
  }
  const clientLabel = $derived(activeClient || 'all clients')

  const strategy = $derived(symData?.strategy || '—')
  const venue = $derived(symData?.venue || '—')
  const product = $derived(symData?.product || '—')
  const mode = $derived((symData?.mode || 'paper').toLowerCase())
  const modeSev = $derived(mode === 'live' ? 'neg' : mode === 'smoke' ? 'warn' : 'pos')

  let now = $state(Date.now())
  $effect(() => {
    const id = setInterval(() => { now = Date.now() }, 500)
    return () => clearInterval(id)
  })

  const ageSecs = $derived(rxMs ? Math.max(0, Math.floor((now - rxMs) / 1000)) : null)
  const fresh = $derived(ageSecs !== null && ageSecs <= 2)
  const stale = $derived(ageSecs !== null && ageSecs > 2 && ageSecs <= 5)
  const frozen = $derived(ageSecs !== null && ageSecs > 5)

  const routeLabel = $derived({
    overview: 'Overview',
    orderbook: 'Orderbook',
    calibration: 'Calibration',
    compliance: 'Compliance',
    admin: 'Admin',
  }[route] || 'Overview')

  // 23-UX-10 — badge severity + label per kill level.
  const killSev = $derived(
    maxKillLevel === 0 ? 'ok'
      : maxKillLevel === 1 ? 'warn'
        : 'neg'
  )
  const killLabel = $derived({
    0: 'NOMINAL',
    1: 'WIDEN',
    2: 'STOP',
    3: 'CANCEL',
    4: 'FLATTEN',
    5: 'DISC',
  }[maxKillLevel] || 'NOMINAL')

  let symbolMenuOpen = $state(false)
  let userMenuOpen = $state(false)
  function pickSymbol(s) {
    onSymbolChange?.(s)
    symbolMenuOpen = false
  }
  function closeMenus() { symbolMenuOpen = false; userMenuOpen = false; clientMenuOpen = false }
  function onGlobalClick(e) {
    // Close dropdowns when user clicks outside them.
    if (!(e.target.closest('.symbol-picker') || e.target.closest('.user-menu-wrap') || e.target.closest('.client-picker'))) {
      closeMenus()
    }
  }
  $effect(() => {
    window.addEventListener('mousedown', onGlobalClick)
    return () => window.removeEventListener('mousedown', onGlobalClick)
  })

  function handleLogout() {
    userMenuOpen = false
    auth?.logout?.()
  }

  const initials = $derived(
    (auth?.state?.name || 'Operator')
      .split(/\s+/)
      .map(w => w[0])
      .join('')
      .slice(0, 2)
      .toUpperCase()
  )
</script>

<header class="topbar">
  <div class="context">
    <span class="crumb-route">{routeLabel}</span>
    <span class="crumb-sep">/</span>
    <!-- 23-UX-12 client selector, visible when any clients are
         registered so single-client deployments don't see a
         useless "all clients" dropdown. -->
    {#if clients.length > 0}
      <div class="client-picker">
        <button class="btn btn-ghost btn-sm symbol-btn" onclick={() => (clientMenuOpen = !clientMenuOpen)} aria-haspopup="listbox" aria-expanded={clientMenuOpen} title="Client scope">
          <span class="client-tag">client:</span>
          <span class="client-name num">{clientLabel}</span>
          <Icon name="chevronDown" size={14} />
        </button>
        {#if clientMenuOpen}
          <div class="symbol-menu card-glass scroll">
            <button class="sym-opt" class:active={activeClient === ''} onclick={() => pickClient('')}>
              <span>all clients</span>
              {#if activeClient === ''}<Icon name="check" size={12} />{/if}
            </button>
            {#each clients as c (c.id)}
              <button class="sym-opt" class:active={c.id === activeClient} onclick={() => pickClient(c.id)}>
                <span class="num">{c.id}</span>
                <span class="muted small">{c.symbols.length}</span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
      <span class="crumb-sep">/</span>
    {/if}

    <div class="symbol-picker">
      <button class="btn btn-ghost btn-sm symbol-btn" onclick={() => (symbolMenuOpen = !symbolMenuOpen)} aria-haspopup="listbox" aria-expanded={symbolMenuOpen}>
        <span class="sym-ticker num">{activeSymbol || '—'}</span>
        <Icon name="chevronDown" size={14} />
      </button>
      {#if symbolMenuOpen && symbols.length > 0}
        <div class="symbol-menu card-glass scroll">
          {#each symbols as s}
            <button class="sym-opt" class:active={s === activeSymbol} onclick={() => pickSymbol(s)}>
              <span class="num">{s}</span>
              {#if s === activeSymbol}
                <Icon name="check" size={12} />
              {/if}
            </button>
          {/each}
        </div>
      {/if}
    </div>

    <div class="freshness" class:fresh class:stale class:frozen class:offline={!connected}>
      <span class="freshness-dot"></span>
      <span class="freshness-text">
        {#if !connected}
          DISCONNECTED
        {:else if ageSecs === null}
          WAITING
        {:else if fresh}
          LIVE · {ageSecs}s
        {:else if stale}
          STALE · {ageSecs}s
        {:else}
          FROZEN · {ageSecs}s
        {/if}
      </span>
    </div>

    <!-- 23-UX-10 global kill-switch indicator. Visible on every
         page, clicking jumps to Admin where Controls is mounted. -->
    <button
      type="button"
      class="kill-badge kill-{killSev}"
      class:kill-alarm={maxKillLevel >= 2}
      onclick={onKillClick}
      title={`Kill switch (max across symbols): L${maxKillLevel} — click to open Admin`}
    >
      <span class="kill-dot"></span>
      <span class="kill-level">L{maxKillLevel}</span>
      <span class="kill-label">{killLabel}</span>
    </button>

    <div class="ctx-chips">
      <span class="chip chip-{modeSev}" title="Engine mode">{mode.toUpperCase()}</span>
      <span class="chip" title="Venue · product">
        <span class="chip-key">{venue}</span>
        <span class="chip-sep">/</span>
        <span class="chip-val">{product}</span>
      </span>
      <span class="chip chip-accent" title="Active strategy">{strategy}</span>
    </div>
  </div>

  <div class="user-menu-wrap">
    <button
      type="button"
      class="user-btn"
      class:open={userMenuOpen}
      onclick={() => (userMenuOpen = !userMenuOpen)}
      aria-haspopup="menu"
      aria-expanded={userMenuOpen}
    >
      <span class="avatar" data-role={auth?.state?.role || 'viewer'}>{initials}</span>
      <span class="user-meta">
        <span class="user-name">{auth?.state?.name || 'Operator'}</span>
        <span class="user-role">{auth?.state?.role || 'viewer'}</span>
      </span>
      <Icon name="chevronDown" size={14} />
    </button>

    {#if userMenuOpen}
      <div class="user-menu card-glass" role="menu">
        <div class="menu-header">
          <span class="avatar avatar-lg" data-role={auth?.state?.role || 'viewer'}>{initials}</span>
          <div class="menu-header-text">
            <div class="menu-name">{auth?.state?.name || 'Operator'}</div>
            <div class="menu-sub">
              <span class="chip chip-role" data-role={auth?.state?.role || 'viewer'}>
                {auth?.state?.role || 'viewer'}
              </span>
            </div>
          </div>
        </div>
        <div class="menu-items">
          <a class="menu-item" href="/api/v1/system/preflight" target="_blank" rel="noopener">
            <Icon name="shield" size={14} />
            <span>System preflight</span>
            <Icon name="external" size={11} />
          </a>
          <a class="menu-item" href="/metrics" target="_blank" rel="noopener">
            <Icon name="pulse" size={14} />
            <span>Prometheus metrics</span>
            <Icon name="external" size={11} />
          </a>
          <button type="button" class="menu-item menu-item-danger" onclick={handleLogout}>
            <Icon name="logout" size={14} />
            <span>Log out</span>
          </button>
        </div>
      </div>
    {/if}
  </div>
</header>

<style>
  .topbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--s-4) var(--s-6);
    background: var(--bg-base);
    border-bottom: 1px solid var(--border-subtle);
    position: sticky;
    top: 0;
    z-index: var(--z-sticky);
  }

  .context {
    display: flex;
    align-items: center;
    gap: var(--s-3);
  }
  .crumb-route {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: var(--tracking-tight);
  }
  .crumb-sep {
    color: var(--fg-faint);
    font-size: var(--fs-md);
  }

  .symbol-picker { position: relative; }
  .client-picker { position: relative; }
  .client-tag {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-weight: 600;
    margin-right: 4px;
  }
  .client-name {
    font-weight: 600;
    font-size: var(--fs-sm);
    color: var(--fg-primary);
  }
  .muted { color: var(--fg-muted); }
  .small { font-size: var(--fs-2xs); }
  .symbol-btn {
    padding: 0 var(--s-3);
  }
  .sym-ticker {
    font-weight: 600;
    font-size: var(--fs-md);
    color: var(--fg-primary);
  }
  .symbol-menu {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    min-width: 180px;
    max-height: 320px;
    overflow-y: auto;
    padding: var(--s-1);
    z-index: var(--z-dropdown);
    box-shadow: var(--shadow-lg);
  }
  .sym-opt {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--s-2) var(--s-3);
    background: transparent;
    color: var(--fg-secondary);
    border: none;
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    cursor: pointer;
    text-align: left;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .sym-opt:hover { background: var(--bg-chip); color: var(--fg-primary); }
  .sym-opt.active { background: var(--accent-dim); color: var(--accent); }

  .freshness {
    display: inline-flex;
    align-items: center;
    gap: var(--s-2);
    height: 24px;
    padding: 0 var(--s-3);
    border-radius: var(--r-pill);
    font-size: var(--fs-2xs);
    font-weight: 600;
    letter-spacing: var(--tracking-label);
    border: 1px solid var(--border-subtle);
    background: var(--bg-chip);
    color: var(--fg-muted);
    font-family: var(--font-mono);
  }
  .freshness.fresh  { color: var(--pos); background: var(--pos-bg); border-color: rgba(34, 197, 94, 0.3); }
  .freshness.stale  { color: var(--warn); background: var(--warn-bg); border-color: rgba(245, 158, 11, 0.3); }
  .freshness.frozen, .freshness.offline { color: var(--neg); background: var(--neg-bg); border-color: rgba(239, 68, 68, 0.3); }
  .freshness-dot {
    width: 6px; height: 6px;
    border-radius: 50%;
    background: currentColor;
  }

  .ctx-chips {
    display: inline-flex;
    align-items: center;
    gap: var(--s-2);
    margin-left: var(--s-2);
  }
  .ctx-chips .chip {
    font-family: var(--font-sans);
    text-transform: none;
    letter-spacing: normal;
    font-size: var(--fs-xs);
    font-weight: 500;
  }
  .chip-key, .chip-val {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--fs-2xs);
    letter-spacing: 0.02em;
  }
  .chip-key  { color: var(--fg-secondary); text-transform: uppercase; font-weight: 600; }
  .chip-val  { color: var(--fg-muted); text-transform: uppercase; }
  .chip-sep  { color: var(--fg-faint); }

  /* 23-UX-10 kill-switch badge. */
  .kill-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    padding: 0 var(--s-3);
    border-radius: var(--r-pill);
    border: 1px solid var(--border-subtle);
    background: var(--bg-chip);
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    font-weight: 700;
    letter-spacing: var(--tracking-label);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out),
                border-color var(--dur-fast) var(--ease-out);
  }
  .kill-badge:hover { border-color: var(--border-default); }
  .kill-badge.kill-ok   { color: var(--pos);  background: var(--pos-bg);  border-color: rgba(34, 197, 94, 0.3); }
  .kill-badge.kill-warn { color: var(--warn); background: var(--warn-bg); border-color: rgba(245, 158, 11, 0.3); }
  .kill-badge.kill-neg  { color: var(--neg);  background: var(--neg-bg);  border-color: rgba(239, 68, 68, 0.35); }
  .kill-dot {
    width: 6px; height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
  .kill-badge.kill-alarm .kill-dot {
    animation: killPulse 0.7s ease-in-out infinite;
  }
  @keyframes killPulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.3; transform: scale(0.6); }
  }
  .kill-level { opacity: 0.85; }
  .kill-label { font-weight: 600; }
  .freshness.fresh .freshness-dot {
    animation: pulseDot 1.8s ease-in-out infinite;
  }
  .freshness.frozen .freshness-dot, .freshness.offline .freshness-dot {
    animation: pulseDot 0.7s ease-in-out infinite;
  }
  @keyframes pulseDot {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.4; transform: scale(0.7); }
  }

  .user-menu-wrap {
    position: relative;
    display: flex;
    align-items: center;
  }
  .user-btn {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: 4px var(--s-3) 4px 4px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-pill);
    color: var(--fg-primary);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out),
                border-color var(--dur-fast) var(--ease-out);
  }
  .user-btn:hover,
  .user-btn.open { background: var(--bg-chip-hover); border-color: var(--border-default); }
  .avatar {
    width: 28px; height: 28px;
    display: flex; align-items: center; justify-content: center;
    font-size: var(--fs-xs);
    font-weight: 700;
    letter-spacing: 0.02em;
    border-radius: 50%;
    background: var(--bg-chip-hover);
    color: var(--fg-primary);
  }
  .avatar-lg { width: 40px; height: 40px; font-size: var(--fs-sm); }
  .avatar[data-role='admin']    { background: var(--critical-bg); color: var(--critical); box-shadow: 0 0 0 1px rgba(220, 38, 38, 0.35) inset; }
  .avatar[data-role='operator'] { background: var(--warn-bg);     color: var(--warn);     box-shadow: 0 0 0 1px rgba(245, 158, 11, 0.35) inset; }
  .avatar[data-role='viewer']   { background: var(--pos-bg);      color: var(--pos);      box-shadow: 0 0 0 1px rgba(34, 197, 94, 0.35) inset; }
  .user-meta {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    line-height: 1;
  }
  .user-name {
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--fg-primary);
  }
  .user-role {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }

  .user-menu {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    min-width: 240px;
    padding: var(--s-3);
    z-index: var(--z-dropdown);
    box-shadow: var(--shadow-lg);
  }
  .menu-header {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    margin-bottom: var(--s-2);
  }
  .menu-header-text { display: flex; flex-direction: column; gap: var(--s-1); }
  .menu-name {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .menu-sub { display: flex; }
  .menu-items { display: flex; flex-direction: column; gap: 2px; }
  .menu-item {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: transparent;
    border: none;
    border-radius: var(--r-md);
    color: var(--fg-secondary);
    font-family: var(--font-sans);
    font-size: var(--fs-sm);
    text-align: left;
    text-decoration: none;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out),
                color var(--dur-fast) var(--ease-out);
  }
  .menu-item > span { flex: 1; }
  .menu-item:hover { background: var(--bg-chip); color: var(--fg-primary); }
  .menu-item-danger:hover { background: var(--neg-bg); color: var(--neg); }

  .chip-role[data-role='admin']    { color: var(--critical); background: var(--critical-bg); border-color: rgba(220, 38, 38, 0.35); }
  .chip-role[data-role='operator'] { color: var(--warn);     background: var(--warn-bg);     border-color: rgba(245, 158, 11, 0.35); }
  .chip-role[data-role='viewer']   { color: var(--pos);      background: var(--pos-bg);      border-color: rgba(34, 197, 94, 0.35); }
</style>
