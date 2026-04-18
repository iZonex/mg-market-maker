<script>
  import Sidebar from './lib/components/Sidebar.svelte'
  import TopBar from './lib/components/TopBar.svelte'
  import Login from './lib/components/Login.svelte'
  import Overview from './lib/pages/Overview.svelte'
  import OrderbookPage from './lib/pages/OrderbookPage.svelte'
  import HistoryPage from './lib/pages/HistoryPage.svelte'
  import CalibrationPage from './lib/pages/CalibrationPage.svelte'
  import CompliancePage from './lib/pages/CompliancePage.svelte'
  import SettingsPage from './lib/pages/SettingsPage.svelte'
  import UsersPage from './lib/pages/UsersPage.svelte'
  import StrategyPage from './lib/pages/StrategyPage.svelte'
  import AdminPage from './lib/pages/AdminPage.svelte'
  import { createWsStore } from './lib/ws.svelte.js'
  import { createAuthStore } from './lib/auth.svelte.js'
  import { isDemoMode, createDemoStore } from './lib/demo.svelte.js'

  const demo = isDemoMode()
  const auth = createAuthStore()
  const ws = demo ? createDemoStore() : createWsStore(auth)

  if (demo && !auth.state.loggedIn) {
    auth.state.loggedIn = true
    auth.state.name = 'Demo Operator'
    auth.state.role = 'operator'
    auth.state.token = 'demo'
  }

  let route = $state('overview')

  const activeSymbol = $derived(ws.state.activeSymbol || ws.state.symbols[0] || '')
  const symData = $derived(ws.state.data[activeSymbol] || {})
  const rxMs = $derived(symData._rx_ms ?? null)

  // Detect engine mode from the status payload if the backend
  // surfaces it; default to "paper" so the chip matches the
  // default run mode.
  const mode = $derived(symData.mode || 'paper')

  function onSymbolChange(s) { ws.setActiveSymbol?.(s) }
</script>

{#if !auth.state.loggedIn}
  <Login {auth} />
{:else}
  <div class="shell">
    <Sidebar bind:route {auth} connected={ws.state.connected} {mode} />
    <div class="main">
      <TopBar
        symbols={ws.state.symbols}
        {activeSymbol}
        {onSymbolChange}
        {rxMs}
        connected={ws.state.connected}
        {auth}
        {route}
        {symData}
      />
      {#if demo}
        <div class="demo-banner">
          <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
          DEMO MODE — simulated data
        </div>
      {/if}
      <div class="content">
        {#if route === 'overview'}
          <Overview {ws} />
        {:else if route === 'orderbook'}
          <OrderbookPage {ws} {auth} />
        {:else if route === 'history'}
          <HistoryPage {ws} {auth} />
        {:else if route === 'calibration'}
          <CalibrationPage {ws} {auth} />
        {:else if route === 'compliance'}
          <CompliancePage {ws} {auth} />
        {:else if route === 'strategy' && auth.canControl()}
          <StrategyPage {auth} />
        {:else if route === 'settings' && auth.canControl()}
          <SettingsPage {ws} {auth} />
        {:else if route === 'users' && auth.state.role === 'admin'}
          <UsersPage {auth} />
        {:else if route === 'admin' && auth.canControl()}
          <AdminPage {ws} {auth} />
        {:else}
          <Overview {ws} />
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .shell {
    display: flex;
    min-height: 100vh;
    background: var(--bg-base);
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .content {
    flex: 1;
    min-height: 0;
  }
  .demo-banner {
    display: flex; align-items: center; justify-content: center;
    gap: var(--s-2);
    padding: var(--s-2);
    background: var(--warn-bg);
    color: var(--warn);
    border-bottom: 1px solid rgba(245, 158, 11, 0.3);
    font-size: var(--fs-xs);
    font-weight: 600;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
  }
</style>
