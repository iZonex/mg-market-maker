<script>
  import Sidebar from './lib/components/Sidebar.svelte'
  import TopBar from './lib/components/TopBar.svelte'
  import Login from './lib/components/Login.svelte'
  import Overview from './lib/pages/Overview.svelte'
  import OrderbookPage from './lib/pages/OrderbookPage.svelte'
  import HistoryPage from './lib/pages/HistoryPage.svelte'
  import CalibrationPage from './lib/pages/CalibrationPage.svelte'
  import CompliancePage from './lib/pages/CompliancePage.svelte'
  import SurveillancePage from './lib/pages/SurveillancePage.svelte'
  import UsersPage from './lib/pages/UsersPage.svelte'
  import StrategyPage from './lib/pages/StrategyPage.svelte'
  import VenuesPage from './lib/pages/VenuesPage.svelte'
  import RulesPage from './lib/pages/RulesPage.svelte'
  import KillSwitchPage from './lib/pages/KillSwitchPage.svelte'
  import FleetPage from './lib/pages/FleetPage.svelte'
  import ClientPage from './lib/pages/ClientPage.svelte'
  import ClientPortalPage from './lib/pages/ClientPortalPage.svelte'
  import ClientSignupPage from './lib/pages/ClientSignupPage.svelte'
  import PasswordResetPage from './lib/pages/PasswordResetPage.svelte'
  import ReconciliationPage from './lib/pages/ReconciliationPage.svelte'
  import IncidentsPage from './lib/pages/IncidentsPage.svelte'
  import VaultPage from './lib/pages/VaultPage.svelte'
  import PlatformPage from './lib/pages/PlatformPage.svelte'
  import ProfilePage from './lib/pages/ProfilePage.svelte'
  import LoginAuditPage from './lib/pages/LoginAuditPage.svelte'
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

  // M2-GOBS — `?live=<agentId>/<deploymentId>` opens StrategyPage
  // in Live mode with the given deployment's graph under observation.
  // Operator lands here from DeploymentDrilldown → "Open graph (live)".
  // The route itself is `strategy` — the query string carries the
  // target so the main-app URL doesn't pattern-proliferate.
  let liveTarget = $state.raw(parseLiveTarget())
  // M4-GOBS — `?tick=<tick_num>` deep link pre-pins a frame
  // so operators landing from Incidents see the exact tick.
  let liveTick = $state.raw(parseLiveTick())
  let route = $state(liveTarget ? 'strategy' : 'overview')
  function parseLiveTarget() {
    try {
      const q = new URL(window.location.href).searchParams.get('live')
      if (!q) return null
      const [agentId, deploymentId] = q.split('/')
      if (!agentId || !deploymentId) return null
      return { agentId, deploymentId }
    } catch {
      return null
    }
  }
  function parseLiveTick() {
    try {
      const q = new URL(window.location.href).searchParams.get('tick')
      if (!q) return null
      const n = Number(q)
      return Number.isFinite(n) ? n : null
    } catch {
      return null
    }
  }
  function navigateLiveGraph(agentId, deploymentId, tickNum = null) {
    const url = new URL(window.location.href)
    url.searchParams.set('live', `${agentId}/${deploymentId}`)
    if (tickNum != null) url.searchParams.set('tick', String(tickNum))
    else url.searchParams.delete('tick')
    window.history.replaceState(null, '', url.toString())
    liveTarget = { agentId, deploymentId }
    liveTick = tickNum
    route = 'strategy'
  }
  function clearLiveTargetOnRouteChange() {
    const url = new URL(window.location.href)
    if (url.searchParams.has('live')) url.searchParams.delete('live')
    if (url.searchParams.has('tick')) url.searchParams.delete('tick')
    window.history.replaceState(null, '', url.toString())
    liveTarget = null
    liveTick = null
  }
  $effect(() => {
    // Leaving the strategy page cancels the live binding so a
    // subsequent click on Strategy opens plain Authoring mode.
    if (route !== 'strategy' && liveTarget) clearLiveTargetOnRouteChange()
  })

  const activeSymbol = $derived(ws.state.activeSymbol || ws.state.symbols[0] || '')
  const symData = $derived(ws.state.data[activeSymbol] || {})
  const rxMs = $derived(symData._rx_ms ?? null)

  // 23-UX-10 — global kill-switch state for TopBar indicator.
  // Takes the max kill_level across every symbol the WS has
  // seen — if ANY leg is in cancel/flatten/disconnect we want
  // that visible from every page, not buried on Admin.
  const maxKillLevel = $derived.by(() => {
    let max = 0
    for (const s of Object.values(ws.state.data || {})) {
      const lvl = parseInt(s?.kill_level ?? 0, 10)
      if (Number.isFinite(lvl) && lvl > max) max = lvl
    }
    return max
  })

  // Detect engine mode from the status payload if the backend
  // surfaces it; default to "paper" so the chip matches the
  // default run mode.
  const mode = $derived(symData.mode || 'paper')

  function onSymbolChange(s) { ws.setActiveSymbol?.(s) }
</script>

{#if !auth.state.loggedIn && (() => {
  const m = window.location.pathname.match(/^\/client-signup\/(.+)$/);
  return m ? m[1] : null;
})()}
  <!-- Wave E4 — signup page bypasses Login when URL carries an
       invite token. After successful signup the auth store is
       populated and we fall into the ClientReader branch below. -->
  <ClientSignupPage
    {auth}
    inviteToken={window.location.pathname.replace(/^\/client-signup\//, '')}
  />
{:else if !auth.state.loggedIn && (() => {
  const m = window.location.pathname.match(/^\/password-reset\/(.+)$/);
  return m ? m[1] : null;
})()}
  <!-- Wave H1 — password-reset page bypasses Login when URL
       carries a reset token. After success the user hits the
       normal login form; we don't auto-login on purpose so the
       audit trail captures a fresh LoginSucceeded row from
       the user's browser with the new credential. -->
  <PasswordResetPage
    resetToken={window.location.pathname.replace(/^\/password-reset\//, '')}
  />
{:else if !auth.state.loggedIn}
  <Login {auth} />
{:else if auth.state.role === 'clientreader'}
  <!-- Wave E3 — tenant-scoped client portal shell.
       No Sidebar / TopBar / symbol picker. Just the portal.
       Server-side tenant_scope_middleware blocks any attempt
       to hit operator/admin routes regardless. -->
  <div class="portal-shell">
    <ClientPortalPage {auth} />
  </div>
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
        {maxKillLevel}
        onKillClick={() => (route = 'kill-switch')}
        onNavigate={(r) => (route = r)}
      />
      {#if demo}
        <div class="demo-banner">
          <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
          DEMO MODE — simulated data
        </div>
      {/if}
      <div class="content">
        {#if route === 'overview'}
          <Overview {ws} {auth} onNavigate={(r) => (route = r)} />
        {:else if route === 'orderbook'}
          <OrderbookPage {ws} {auth} />
        {:else if route === 'history'}
          <HistoryPage {ws} {auth} />
        {:else if route === 'calibration'}
          <CalibrationPage {ws} {auth} />
        {:else if route === 'compliance'}
          <CompliancePage {ws} {auth} />
        {:else if route === 'surveillance' && auth.state.role === 'admin'}
          <SurveillancePage {auth} />
        {:else if route === 'strategy' && auth.canControl()}
          <StrategyPage
            {auth}
            liveAgent={liveTarget?.agentId ?? null}
            liveDeployment={liveTarget?.deploymentId ?? null}
            liveTick={liveTick}
          />
        {:else if route === 'rules' && auth.canControl()}
          <RulesPage {auth} />
        {:else if route === 'venues' && auth.canControl()}
          <VenuesPage {auth} />
        {:else if route === 'kill-switch' && auth.state.role === 'admin'}
          <KillSwitchPage {auth} onNavigate={(r) => (route = r)} />
        {:else if route === 'users' && auth.state.role === 'admin'}
          <UsersPage {auth} />
        {:else if route === 'fleet'}
          <FleetPage
            {auth}
            onNavigate={(r) => (route = r)}
            onOpenGraphLive={(a, d) => navigateLiveGraph(a, d)}
          />
        {:else if route === 'clients'}
          <ClientPage {auth} onNavigate={(r) => (route = r)} />
        {:else if route === 'reconciliation'}
          <ReconciliationPage {auth} />
        {:else if route === 'incidents'}
          <IncidentsPage
            {auth}
            onOpenGraphLive={(a, d, t) => navigateLiveGraph(a, d, t)}
          />
        {:else if route === 'vault' && auth.state.role === 'admin'}
          <VaultPage {auth} />
        {:else if route === 'platform' && auth.state.role === 'admin'}
          <PlatformPage {auth} />
        {:else if route === 'profile'}
          <ProfilePage {auth} />
        {:else if route === 'login-audit' && auth.state.role === 'admin'}
          <LoginAuditPage {auth} />
        {:else}
          <Overview {ws} {auth} />
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
  .portal-shell {
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
