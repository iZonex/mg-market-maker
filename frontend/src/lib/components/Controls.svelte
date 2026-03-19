<script>
  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
  const pnl = $derived(d.pnl || {})

  async function killSwitch(level) {
    try {
      // TODO: POST to /api/v1/kill-switch when endpoint is added.
      alert(`Kill switch level ${level} triggered (API endpoint pending)`)
    } catch (e) {
      console.error(e)
    }
  }
</script>

<div>
  <h3>Controls & PnL</h3>

  <div class="pnl-grid">
    <div class="pnl-item">
      <span class="label">Spread</span>
      <span class="value positive">${parseFloat(pnl.spread || 0).toFixed(4)}</span>
    </div>
    <div class="pnl-item">
      <span class="label">Inventory</span>
      <span class="value" class:positive={parseFloat(pnl.inventory || 0) >= 0} class:negative={parseFloat(pnl.inventory || 0) < 0}>
        ${parseFloat(pnl.inventory || 0).toFixed(4)}
      </span>
    </div>
    <div class="pnl-item">
      <span class="label">Rebates</span>
      <span class="value positive">${parseFloat(pnl.rebates || 0).toFixed(4)}</span>
    </div>
    <div class="pnl-item">
      <span class="label">Fees</span>
      <span class="value negative">-${parseFloat(pnl.fees || 0).toFixed(4)}</span>
    </div>
    <div class="pnl-item">
      <span class="label">Trips</span>
      <span class="value">{pnl.round_trips || 0}</span>
    </div>
    <div class="pnl-item">
      <span class="label">Volume</span>
      <span class="value">${parseFloat(pnl.volume || 0).toFixed(2)}</span>
    </div>
  </div>

  <div class="buttons">
    <button class="btn btn-warning" onclick={() => killSwitch(1)}>Widen Spreads</button>
    <button class="btn btn-danger" onclick={() => killSwitch(3)}>Cancel All</button>
    <button class="btn btn-critical" onclick={() => killSwitch(4)}>FLATTEN</button>
  </div>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 12px; text-transform: uppercase; letter-spacing: 0.5px; }
  .pnl-grid {
    display: grid; grid-template-columns: 1fr 1fr; gap: 6px;
    margin-bottom: 16px;
  }
  .pnl-item { display: flex; justify-content: space-between; padding: 4px; }
  .label { color: #8b949e; font-size: 11px; }
  .value { font-weight: 600; font-size: 12px; }
  .positive { color: #3fb950; }
  .negative { color: #f85149; }
  .buttons { display: flex; gap: 8px; }
  .btn {
    flex: 1; padding: 8px; border: none; border-radius: 4px; cursor: pointer;
    font-family: inherit; font-size: 11px; font-weight: 700; text-transform: uppercase;
  }
  .btn-warning { background: #d29922; color: #000; }
  .btn-danger { background: #da3633; color: #fff; }
  .btn-critical { background: #f85149; color: #fff; }
  .btn:hover { opacity: 0.85; }
</style>
