<script>
  /*
   * Top context bar — route crumb + client scope + symbol
   * switcher + rx-freshness pill + global kill badge + context
   * chips + operator avatar menu.
   *
   * Sits above the main content area so the operator's context
   * never scrolls off-screen. Freshness, kill badge, and user
   * menu each own their own state and live in ./topbar/*.
   */
  import Icon from './Icon.svelte'
  import { createApiClient } from '../api.svelte.js'
  import { Button } from '../primitives/index.js'
  import FreshnessIndicator from './topbar/FreshnessIndicator.svelte'
  import KillBadge from './topbar/KillBadge.svelte'
  import UserMenu from './topbar/UserMenu.svelte'

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
    // Callback for nav items that live inside the user menu
    // (profile). Parent owns routing; we just signal.
    onNavigate = () => {},
  } = $props()

  // 23-UX-12 — client scope selector.
  const api = $derived(createApiClient(auth))
  let clients = $state([])
  let activeClient = $state('')
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
    if (id) {
      const c = clients.find((c) => c.id === id)
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

  const routeLabel = $derived({
    overview: 'Overview',
    orderbook: 'Orderbook',
    calibration: 'Calibration',
    compliance: 'Compliance',
    admin: 'Admin',
  }[route] || 'Overview')

  let symbolMenuOpen = $state(false)
  function pickSymbol(s) {
    onSymbolChange?.(s)
    symbolMenuOpen = false
  }
  function closeMenus() { symbolMenuOpen = false; clientMenuOpen = false }
  function onGlobalClick(e) {
    if (!(e.target.closest('.symbol-picker') || e.target.closest('.client-picker'))) {
      closeMenus()
    }
  }
  $effect(() => {
    window.addEventListener('mousedown', onGlobalClick)
    return () => window.removeEventListener('mousedown', onGlobalClick)
  })
</script>

<header class="topbar">
  <div class="context">
    <span class="crumb-route">{routeLabel}</span>
    <span class="crumb-sep">/</span>

    {#if clients.length > 0}
      <div class="client-picker">
        <Button variant="primary" onclick={() => (clientMenuOpen = !clientMenuOpen)} aria-haspopup="listbox" aria-expanded={clientMenuOpen} title="Client scope">
          {#snippet children()}<span class="client-tag">client:</span>
          <span class="client-name num">{clientLabel}</span>
          <Icon name="chevronDown" size={14} />{/snippet}
        </Button>
        {#if clientMenuOpen}
          <div class="topbar-menu card-glass scroll">
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
      <Button variant="primary" onclick={() => (symbolMenuOpen = !symbolMenuOpen)} aria-haspopup="listbox" aria-expanded={symbolMenuOpen}>
        {#snippet children()}<span class="sym-ticker num">{activeSymbol || '—'}</span>
        <Icon name="chevronDown" size={14} />{/snippet}
      </Button>
      {#if symbolMenuOpen && symbols.length > 0}
        <div class="topbar-menu card-glass scroll">
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

    <FreshnessIndicator {rxMs} {connected} />

    <KillBadge level={maxKillLevel} onClick={onKillClick} />

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

  <UserMenu {auth} {onNavigate} />
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
  .small { font-size: var(--fs-2xs); }
  .sym-ticker {
    font-weight: 600;
    font-size: var(--fs-md);
    color: var(--fg-primary);
  }
  .topbar-menu {
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
</style>
