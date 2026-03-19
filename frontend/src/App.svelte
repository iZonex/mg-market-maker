<script>
  import Header from './lib/components/Header.svelte'
  import PnlChart from './lib/components/PnlChart.svelte'
  import SpreadChart from './lib/components/SpreadChart.svelte'
  import OrderBook from './lib/components/OrderBook.svelte'
  import InventoryPanel from './lib/components/InventoryPanel.svelte'
  import OpenOrders from './lib/components/OpenOrders.svelte'
  import FillHistory from './lib/components/FillHistory.svelte'
  import Controls from './lib/components/Controls.svelte'
  import AlertLog from './lib/components/AlertLog.svelte'
  import Login from './lib/components/Login.svelte'
  import { createWsStore } from './lib/ws.js'
  import { createAuthStore } from './lib/auth.js'
  import { isDemoMode, createDemoStore } from './lib/demo.js'

  const demo = isDemoMode()
  const auth = createAuthStore()
  const ws = demo ? createDemoStore() : createWsStore()

  // In demo mode, auto-login as operator.
  if (demo && !auth.state.loggedIn) {
    auth.state.loggedIn = true
    auth.state.name = 'Demo Operator'
    auth.state.role = 'operator'
    auth.state.token = 'demo'
  }
</script>

{#if !auth.state.loggedIn}
  <Login {auth} />
{:else}
  <div class="app">
    {#if demo}
      <div class="demo-banner">DEMO MODE — simulated data</div>
    {/if}
    <Header data={ws} {auth} />

    <div class="grid">
      <div class="panel span-2">
        <PnlChart data={ws} />
      </div>
      <div class="panel">
        <OrderBook data={ws} />
      </div>

      <div class="panel">
        <SpreadChart data={ws} />
      </div>
      <div class="panel">
        <InventoryPanel data={ws} />
      </div>
      <div class="panel">
        {#if auth.canControl()}
          <Controls data={ws} />
        {:else}
          <div class="viewer-pnl">
            <h3>PnL Attribution</h3>
            {@const sym = ws.state.symbols[0] || ''}
            {@const pnl = ws.state.data[sym]?.pnl || {}}
            <div class="pnl-row"><span>Spread</span><span class="pos">${parseFloat(pnl.spread || 0).toFixed(4)}</span></div>
            <div class="pnl-row"><span>Inventory</span><span>${parseFloat(pnl.inventory || 0).toFixed(4)}</span></div>
            <div class="pnl-row"><span>Rebates</span><span class="pos">${parseFloat(pnl.rebates || 0).toFixed(4)}</span></div>
            <div class="pnl-row"><span>Fees</span><span class="neg">-${parseFloat(pnl.fees || 0).toFixed(4)}</span></div>
            <div class="pnl-row total"><span>Total</span><span>${parseFloat(pnl.total || 0).toFixed(4)}</span></div>
          </div>
        {/if}
      </div>

      <div class="panel">
        <OpenOrders data={ws} />
      </div>
      <div class="panel">
        <FillHistory data={ws} />
      </div>
      <div class="panel">
        {#if auth.canViewInternals()}
          <AlertLog data={ws} />
        {:else}
          <div class="viewer-sla">
            <h3>SLA Compliance</h3>
            {@const sym = ws.state.symbols[0] || ''}
            {@const d = ws.state.data[sym] || {}}
            <div class="sla-big">{parseFloat(d.sla_uptime_pct || 0).toFixed(1)}%</div>
            <div class="sla-label">Uptime</div>
          </div>
        {/if}
      </div>
    </div>

    <footer>
      <span class="user-info">
        <span class="role-badge {auth.state.role}">{auth.state.role}</span>
        {auth.state.name}
      </span>
      <button class="logout" onclick={() => auth.logout()}>Logout</button>
    </footer>
  </div>
{/if}

<style>
  :global(*) { margin: 0; padding: 0; box-sizing: border-box; }
  :global(body) {
    background: #0a0e17; color: #e1e4e8;
    font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', monospace;
    font-size: 13px;
  }
  .app { min-height: 100vh; padding: 8px; display: flex; flex-direction: column; }
  .demo-banner {
    background: #d29922; color: #000; text-align: center;
    padding: 4px; font-size: 11px; font-weight: 700;
    letter-spacing: 1px; border-radius: 4px; margin-bottom: 4px;
  }
  .grid {
    display: grid; grid-template-columns: 1fr 1fr 1fr;
    gap: 8px; margin-top: 8px; flex: 1;
  }
  .panel {
    background: #161b22; border: 1px solid #21262d;
    border-radius: 6px; padding: 12px; min-height: 250px;
  }
  .span-2 { grid-column: span 2; }
  footer {
    display: flex; justify-content: space-between; align-items: center;
    padding: 8px 12px; margin-top: 8px;
    background: #161b22; border: 1px solid #21262d; border-radius: 6px;
  }
  .user-info { display: flex; align-items: center; gap: 8px; font-size: 12px; color: #8b949e; }
  .role-badge {
    padding: 2px 6px; border-radius: 3px; font-size: 10px;
    font-weight: 700; text-transform: uppercase;
  }
  .role-badge.admin { background: #da3633; color: #fff; }
  .role-badge.operator { background: #d29922; color: #000; }
  .role-badge.viewer { background: #238636; color: #fff; }
  .logout {
    background: none; border: 1px solid #30363d; color: #8b949e;
    padding: 4px 12px; border-radius: 4px; cursor: pointer; font-family: inherit; font-size: 11px;
  }
  .logout:hover { border-color: #f85149; color: #f85149; }
  .viewer-pnl h3, .viewer-sla h3 {
    font-size: 12px; color: #8b949e; margin-bottom: 12px;
    text-transform: uppercase; letter-spacing: 0.5px;
  }
  .pnl-row { display: flex; justify-content: space-between; padding: 4px 0; font-size: 13px; }
  .pnl-row.total {
    border-top: 1px solid #21262d; margin-top: 8px; padding-top: 8px;
    font-weight: 700; font-size: 15px;
  }
  .pos { color: #3fb950; }
  .neg { color: #f85149; }
  .viewer-sla { text-align: center; padding-top: 40px; }
  .sla-big { font-size: 48px; font-weight: 700; color: #3fb950; }
  .sla-label { font-size: 14px; color: #8b949e; margin-top: 8px; }
</style>
