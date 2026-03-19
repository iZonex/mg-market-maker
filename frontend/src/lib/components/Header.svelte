<script>
  let { data } = $props()
  const s = $derived(data.state)

  const firstSymbol = $derived(s.symbols[0] || '')
  const d = $derived(s.data[firstSymbol] || {})
</script>

<header>
  <div class="left">
    <h1>MG Market Maker</h1>
    {#if firstSymbol}
      <span class="symbol-badge">{firstSymbol}</span>
    {/if}
    <span class="status" class:online={s.connected}>
      {s.connected ? 'LIVE' : 'OFFLINE'}
    </span>
  </div>

  <div class="metrics">
    <div class="metric">
      <span class="label">Mid</span>
      <span class="value">{d.mid_price || '—'}</span>
    </div>
    <div class="metric">
      <span class="label">Spread</span>
      <span class="value">{d.spread_bps ? `${parseFloat(d.spread_bps).toFixed(1)} bps` : '—'}</span>
    </div>
    <div class="metric">
      <span class="label">PnL</span>
      <span class="value" class:positive={parseFloat(d.pnl?.total || 0) > 0} class:negative={parseFloat(d.pnl?.total || 0) < 0}>
        {d.pnl?.total ? `$${parseFloat(d.pnl.total).toFixed(2)}` : '—'}
      </span>
    </div>
    <div class="metric">
      <span class="label">Inventory</span>
      <span class="value">{d.inventory || '0'}</span>
    </div>
    <div class="metric">
      <span class="label">Orders</span>
      <span class="value">{d.live_orders || 0}</span>
    </div>
    <div class="metric">
      <span class="label">Kill</span>
      <span class="value" class:danger={d.kill_level > 0}>L{d.kill_level || 0}</span>
    </div>
    <div class="metric">
      <span class="label">SLA</span>
      <span class="value" class:positive={parseFloat(d.sla_uptime_pct || 0) >= 95}>
        {d.sla_uptime_pct ? `${parseFloat(d.sla_uptime_pct).toFixed(1)}%` : '—'}
      </span>
    </div>
    <div class="metric">
      <span class="label">Regime</span>
      <span class="value regime">{d.regime || '—'}</span>
    </div>
  </div>
</header>

<style>
  header {
    background: #161b22;
    border: 1px solid #21262d;
    border-radius: 6px;
    padding: 12px 16px;
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .left {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  h1 {
    font-size: 16px;
    font-weight: 600;
    color: #58a6ff;
  }
  .status {
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    font-weight: 700;
    background: #da3633;
    color: #fff;
  }
  .status.online {
    background: #238636;
  }
  .symbol-badge {
    padding: 2px 8px; border-radius: 4px; font-size: 12px;
    font-weight: 700; background: #30363d; color: #58a6ff;
    letter-spacing: 0.5px;
  }
  .metrics {
    display: flex;
    gap: 20px;
  }
  .metric {
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .label {
    font-size: 10px;
    color: #8b949e;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }
  .value {
    font-size: 14px;
    font-weight: 600;
    color: #e1e4e8;
  }
  .positive { color: #3fb950; }
  .negative { color: #f85149; }
  .danger { color: #f85149; }
  .regime { color: #d2a8ff; font-size: 12px; }
</style>
