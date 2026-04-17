<script>
  import { createApiClient } from '../api.svelte.js'

  let { data, auth } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
  const pnl = $derived(d.pnl || {})
  const killLevel = $derived(d.kill_level || 0)
  const api = createApiClient(auth)

  let busy = $state(false)
  let lastAction = $state('')

  async function callOps(path, confirmMsg, reason) {
    if (!confirm(confirmMsg)) return
    busy = true
    lastAction = ''
    try {
      const body = reason ? { reason } : {}
      const res = await api.postJson(path, body)
      lastAction = res.applied
        ? `${path} applied on ${res.symbol}`
        : `${path} rejected (no channel for ${res.symbol})`
    } catch (e) {
      lastAction = `error: ${e.message}`
      console.error(e)
    } finally {
      busy = false
    }
  }

  function widen() {
    callOps(
      `/api/v1/ops/widen/${encodeURIComponent(sym)}`,
      `Widen spreads on ${sym}? (Kill L1)`,
      'dashboard widen'
    )
  }
  function stopNew() {
    callOps(
      `/api/v1/ops/stop/${encodeURIComponent(sym)}`,
      `Stop new orders on ${sym}? (Kill L2)`,
      'dashboard stop-new'
    )
  }
  function cancelAll() {
    callOps(
      `/api/v1/ops/cancel-all/${encodeURIComponent(sym)}`,
      `CANCEL ALL orders on ${sym}? (Kill L3)`,
      'dashboard cancel-all'
    )
  }
  function flatten() {
    callOps(
      `/api/v1/ops/flatten/${encodeURIComponent(sym)}`,
      `FLATTEN inventory on ${sym}? (Kill L4, irreversible until reset)`,
      'dashboard flatten'
    )
  }
  function reset() {
    callOps(
      `/api/v1/ops/reset/${encodeURIComponent(sym)}`,
      `Reset kill switch on ${sym}?`,
      'dashboard reset'
    )
  }
</script>

<div>
  <h3>Controls & PnL</h3>

  <div class="kill-row">
    <span class="label">Kill level</span>
    <span class="kill-badge level-{killLevel}">L{killLevel}</span>
    {#if killLevel > 0}
      <button class="btn-reset" onclick={reset} disabled={busy}>reset</button>
    {/if}
  </div>

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
    <button class="btn btn-warning" onclick={widen} disabled={busy}>L1 Widen</button>
    <button class="btn btn-stop" onclick={stopNew} disabled={busy}>L2 Stop</button>
    <button class="btn btn-danger" onclick={cancelAll} disabled={busy}>L3 Cancel</button>
    <button class="btn btn-critical" onclick={flatten} disabled={busy}>L4 FLATTEN</button>
  </div>
  {#if lastAction}
    <div class="status">{lastAction}</div>
  {/if}
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 12px; text-transform: uppercase; letter-spacing: 0.5px; }
  .kill-row { display: flex; align-items: center; gap: 8px; margin-bottom: 12px; }
  .kill-badge {
    padding: 3px 8px; border-radius: 3px; font-size: 11px;
    font-weight: 700; letter-spacing: 0.5px;
  }
  .kill-badge.level-0 { background: #238636; color: #fff; }
  .kill-badge.level-1 { background: #d29922; color: #000; }
  .kill-badge.level-2 { background: #bf8700; color: #fff; }
  .kill-badge.level-3 { background: #da3633; color: #fff; }
  .kill-badge.level-4, .kill-badge.level-5 { background: #f85149; color: #fff; }
  .btn-reset {
    background: none; border: 1px solid #30363d; color: #8b949e;
    padding: 2px 8px; border-radius: 3px; cursor: pointer;
    font-family: inherit; font-size: 10px;
  }
  .btn-reset:hover { border-color: #3fb950; color: #3fb950; }
  .btn-reset:disabled { opacity: 0.4; cursor: not-allowed; }
  .pnl-grid {
    display: grid; grid-template-columns: 1fr 1fr; gap: 6px;
    margin-bottom: 16px;
  }
  .pnl-item { display: flex; justify-content: space-between; padding: 4px; }
  .label { color: #8b949e; font-size: 11px; }
  .value { font-weight: 600; font-size: 12px; }
  .positive { color: #3fb950; }
  .negative { color: #f85149; }
  .buttons { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
  .btn {
    padding: 8px; border: none; border-radius: 4px; cursor: pointer;
    font-family: inherit; font-size: 11px; font-weight: 700; text-transform: uppercase;
  }
  .btn-warning { background: #d29922; color: #000; }
  .btn-stop    { background: #bf8700; color: #fff; }
  .btn-danger  { background: #da3633; color: #fff; }
  .btn-critical{ background: #f85149; color: #fff; }
  .btn:hover { opacity: 0.85; }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .status {
    margin-top: 10px; font-size: 11px; color: #8b949e;
    background: #0d1117; border: 1px solid #21262d;
    padding: 6px 8px; border-radius: 3px;
    font-family: inherit; word-break: break-all;
  }
</style>
