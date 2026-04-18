<script>
  import { createApiClient } from '../api.svelte.js'
  import KillConfirmModal from './KillConfirmModal.svelte'
  import Icon from './Icon.svelte'

  let { data, auth } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
  const pnl = $derived(d.pnl || {})
  const killLevel = $derived(d.kill_level || 0)
  const liveOrders = $derived(d.live_orders || 0)
  const inventory = $derived(d.inventory || '0')
  const api = createApiClient(auth)

  let busy = $state(false)
  let lastAction = $state('')
  let modalOpen = $state(false)
  let modalLevel = $state('L4')
  let modalAction = $state('FLATTEN')
  let modalPreview = $state('')
  let modalPath = $state('')
  let modalReason = $state('')

  async function dispatch(path, reason) {
    busy = true
    lastAction = ''
    try {
      const res = await api.postJson(path, reason ? { reason } : {})
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

  async function lowRiskOp(path, confirmMsg, reason) {
    if (!confirm(confirmMsg)) return
    await dispatch(path, reason)
  }

  function stage(level, action, path, reason, preview) {
    modalLevel = level
    modalAction = action
    modalPath = path
    modalReason = reason
    modalPreview = preview
    modalOpen = true
  }

  function onModalConfirm() { modalOpen = false; dispatch(modalPath, modalReason) }
  function onModalCancel()  { modalOpen = false }

  function widen()    { lowRiskOp(`/api/v1/ops/widen/${encodeURIComponent(sym)}`,   `Widen spreads on ${sym}? (Kill L1)`,       'dashboard widen') }
  function stopNew()  { lowRiskOp(`/api/v1/ops/stop/${encodeURIComponent(sym)}`,    `Stop new orders on ${sym}? (Kill L2)`,      'dashboard stop-new') }
  function reset()    { lowRiskOp(`/api/v1/ops/reset/${encodeURIComponent(sym)}`,   `Reset kill switch on ${sym}?`,              'dashboard reset') }
  function cancelAll(){ stage('L3', 'CANCEL ALL', `/api/v1/ops/cancel-all/${encodeURIComponent(sym)}`, 'dashboard cancel-all',
    `Will cancel ${liveOrders} open order${liveOrders === 1 ? '' : 's'} on ${sym}. Inventory stays at ${inventory}.`) }
  function flatten()  { stage('L4', 'FLATTEN',    `/api/v1/ops/flatten/${encodeURIComponent(sym)}`,    'dashboard flatten',
    `Cancels all ${liveOrders} open order${liveOrders === 1 ? '' : 's'} AND sells the ${inventory} ${sym} inventory as market-takers. Irreversible until kill switch is reset.`) }

  const killText = $derived({
    0: 'Nominal', 1: 'Widening', 2: 'Stop-new', 3: 'Cancel-all', 4: 'Flattening', 5: 'Disconnect',
  }[killLevel] || '—')
  const killSeverity = $derived(killLevel === 0 ? 'ok' : killLevel === 1 ? 'warn' : 'neg')

  function fmt(n, d = 4) {
    const f = parseFloat(n || 0)
    return Number.isFinite(f) ? f.toFixed(d) : '—'
  }
  function fmtSigned(n, d = 4) {
    const f = parseFloat(n || 0)
    if (!Number.isFinite(f)) return '—'
    return (f > 0 ? '+' : '') + f.toFixed(d)
  }
</script>

<KillConfirmModal
  open={modalOpen}
  level={modalLevel}
  action={modalAction}
  symbol={sym}
  preview={modalPreview}
  onConfirm={onModalConfirm}
  onCancel={onModalCancel}
/>

<div class="controls">
  <!-- Current kill switch state strip -->
  <div class="state-strip" data-sev={killSeverity}>
    <div class="state-main">
      <span class="kl-badge kl-{killSeverity}">L{killLevel}</span>
      <div class="state-text">
        <span class="state-label">Kill switch</span>
        <span class="state-value">{killText}</span>
      </div>
    </div>
    {#if killLevel > 0}
      <button type="button" class="btn btn-sm btn-ghost" onclick={reset} disabled={busy}>
        <Icon name="check" size={12} />
        Reset
      </button>
    {/if}
  </div>

  <!-- PnL attribution mini-grid -->
  <div class="pnl-grid">
    <div class="pnl-cell">
      <span class="label">Spread</span>
      <span class="pnl-val num pos">{fmtSigned(pnl.spread)}</span>
    </div>
    <div class="pnl-cell">
      <span class="label">Inventory</span>
      <span class="pnl-val num" class:pos={parseFloat(pnl.inventory || 0) >= 0} class:neg={parseFloat(pnl.inventory || 0) < 0}>
        {fmtSigned(pnl.inventory)}
      </span>
    </div>
    <div class="pnl-cell">
      <span class="label">Rebates</span>
      <span class="pnl-val num pos">{fmt(pnl.rebates)}</span>
    </div>
    <div class="pnl-cell">
      <span class="label">Fees</span>
      <span class="pnl-val num neg">−{fmt(pnl.fees)}</span>
    </div>
    <div class="pnl-cell">
      <span class="label">Trips</span>
      <span class="pnl-val num">{pnl.round_trips || 0}</span>
    </div>
    <div class="pnl-cell">
      <span class="label">Volume</span>
      <span class="pnl-val num">${fmt(pnl.volume, 2)}</span>
    </div>
  </div>

  <!-- Kill switch action buttons -->
  <div class="actions">
    <button type="button" class="kill-btn kl-L1" onclick={widen} disabled={busy}>
      <span class="kl-tag">L1</span>
      <span class="kl-lbl">Widen</span>
    </button>
    <button type="button" class="kill-btn kl-L2" onclick={stopNew} disabled={busy}>
      <span class="kl-tag">L2</span>
      <span class="kl-lbl">Stop</span>
    </button>
    <button type="button" class="kill-btn kl-L3" onclick={cancelAll} disabled={busy}>
      <span class="kl-tag">L3</span>
      <span class="kl-lbl">Cancel</span>
    </button>
    <button type="button" class="kill-btn kl-L4" onclick={flatten} disabled={busy}>
      <span class="kl-tag">L4</span>
      <span class="kl-lbl">Flatten</span>
    </button>
  </div>

  {#if lastAction}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>{lastAction}</span>
    </div>
  {/if}
</div>

<style>
  .controls {
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
  }

  /* ── State strip ────────────────────────────────────────── */
  .state-strip {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-lg);
  }
  .state-strip[data-sev='ok']   { border-color: rgba(34, 197, 94, 0.2); }
  .state-strip[data-sev='warn'] { border-color: rgba(245, 158, 11, 0.28); background: rgba(245, 158, 11, 0.05); }
  .state-strip[data-sev='neg']  { border-color: rgba(239, 68, 68, 0.35);  background: rgba(239, 68, 68, 0.06); }
  .state-main { display: flex; align-items: center; gap: var(--s-3); }
  .kl-badge {
    display: inline-flex; align-items: center; justify-content: center;
    min-width: 34px;
    padding: 2px var(--s-2);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--fs-md);
    font-weight: 700;
  }
  .kl-badge.kl-ok   { background: var(--pos-bg);  color: var(--pos); }
  .kl-badge.kl-warn { background: var(--warn-bg); color: var(--warn); }
  .kl-badge.kl-neg  { background: var(--neg-bg);  color: var(--neg); }
  .state-text { display: flex; flex-direction: column; gap: 1px; }
  .state-label {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
  }
  .state-value { font-size: var(--fs-sm); font-weight: 500; color: var(--fg-primary); }

  /* ── PnL grid ───────────────────────────────────────────── */
  .pnl-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--s-2);
    padding: var(--s-3);
    background: var(--bg-chip);
    border-radius: var(--r-lg);
  }
  .pnl-cell {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .pnl-val {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }

  /* ── Kill switch buttons ───────────────────────────────── */
  .actions {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: var(--s-2);
  }
  .kill-btn {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s-1);
    padding: var(--s-3) var(--s-2);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-lg);
    color: var(--fg-secondary);
    font-family: var(--font-sans);
    cursor: pointer;
    transition:
      background var(--dur-fast) var(--ease-out),
      border-color var(--dur-fast) var(--ease-out),
      transform var(--dur-fast) var(--ease-out),
      color var(--dur-fast) var(--ease-out);
  }
  .kill-btn:hover {
    background: var(--bg-chip-hover);
    border-color: var(--border-strong);
    color: var(--fg-primary);
  }
  .kill-btn:active:not(:disabled) { transform: translateY(1px); }
  .kill-btn:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-ring);
  }
  .kill-btn:disabled { opacity: 0.45; cursor: not-allowed; }
  .kl-tag {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    font-weight: 700;
    letter-spacing: 0.05em;
    padding: 1px 5px;
    border-radius: var(--r-sm);
  }
  .kl-lbl {
    font-size: var(--fs-sm);
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  /* Severity shading per level */
  .kill-btn.kl-L1 .kl-tag { background: var(--warn-bg); color: var(--warn); }
  .kill-btn.kl-L1:hover   { border-color: rgba(245, 158, 11, 0.4); }

  .kill-btn.kl-L2 .kl-tag { background: rgba(245, 158, 11, 0.22); color: #fbbf24; }
  .kill-btn.kl-L2:hover   { border-color: rgba(245, 158, 11, 0.55); }

  .kill-btn.kl-L3 .kl-tag { background: var(--neg-bg); color: var(--neg); }
  .kill-btn.kl-L3:hover   { border-color: rgba(239, 68, 68, 0.45); }

  .kill-btn.kl-L4 .kl-tag { background: var(--grad-danger); color: #fff; }
  .kill-btn.kl-L4:hover {
    background: rgba(239, 68, 68, 0.08);
    border-color: rgba(239, 68, 68, 0.5);
    color: var(--neg);
  }

  /* ── Status line ───────────────────────────────────────── */
  .status-line {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    word-break: break-all;
  }
</style>
