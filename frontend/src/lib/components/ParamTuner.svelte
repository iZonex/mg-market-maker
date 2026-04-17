<script>
  import { createApiClient } from '../api.svelte.js'

  let { data, auth } = $props()
  const api = createApiClient(auth)

  const s = $derived(data.state)
  const sym = $derived(s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
  const tunable = $derived(d.tunable_config || {})

  // Local pending edits, keyed by ConfigOverride field name. We
  // only POST when the operator clicks "apply" — a silent
  // slider drag should not stream 100 updates into the engine.
  let pending = $state({})
  let busy = $state(false)
  let lastStatus = $state('')

  const FIELDS = [
    { key: 'Gamma',           label: 'Gamma (γ)',          tooltip: 'Risk aversion. Higher → tighter spread / less inventory risk.',   min: 0.001, max: 5,   step: 0.001,  read: 'gamma' },
    { key: 'MinSpreadBps',    label: 'Min spread (bps)',   tooltip: 'Floor on the quoted spread. Ignored when computed spread wider.', min: 0,     max: 100, step: 0.5,    read: 'min_spread_bps' },
    { key: 'MaxDistanceBps',  label: 'Max distance (bps)', tooltip: 'Outermost ladder level cap distance from mid.',                   min: 1,     max: 500, step: 1,      read: 'max_distance_bps' },
    { key: 'OrderSize',       label: 'Order size',         tooltip: 'Base-asset qty per level.',                                       min: 0,     max: 10,  step: 0.0001, read: 'order_size' },
    { key: 'NumLevels',       label: 'Levels per side',    tooltip: 'Ladder depth per side.',                                          min: 1,     max: 10,  step: 1,      read: 'num_levels',       isInt: true },
    { key: 'MaxInventory',    label: 'Max inventory',      tooltip: 'Absolute base-asset inventory cap. 0 disables.',                  min: 0,     max: 100, step: 0.0001, read: 'max_inventory' },
    { key: 'AmendMaxTicks',   label: 'Amend tick budget',  tooltip: '0 disables amend. Higher = more aggressive queue-priority preservation.', min: 0, max: 20, step: 1, read: 'amend_max_ticks', isInt: true },
  ]

  const TOGGLES = [
    { key: 'MomentumEnabled',          label: 'Momentum alpha',     read: 'momentum_enabled' },
    { key: 'MarketResilienceEnabled',  label: 'Market resilience',  read: 'market_resilience_enabled' },
    { key: 'AmendEnabled',             label: 'Amend in-place',     read: 'amend_enabled' },
    { key: 'OtrEnabled',               label: 'OTR snapshots',      read: 'otr_enabled' },
  ]

  function currentValue(f) {
    return pending[f.key] ?? tunable[f.read] ?? ''
  }

  function currentToggle(t) {
    return pending[t.key] ?? tunable[t.read] ?? false
  }

  function onSlide(f, v) {
    const parsed = f.isInt ? parseInt(v, 10) : parseFloat(v)
    pending = { ...pending, [f.key]: parsed }
  }

  function onToggle(t, checked) {
    pending = { ...pending, [t.key]: checked }
  }

  async function applyOne(field, value) {
    const body = { field, value: String(value) }
    return api.postJson(`/api/admin/config/${encodeURIComponent(sym)}`, body)
  }

  async function applyAll() {
    if (!sym) return
    busy = true
    lastStatus = ''
    const applied = []
    const failed = []
    try {
      for (const [field, value] of Object.entries(pending)) {
        try {
          await applyOne(field, value)
          applied.push(field)
        } catch (e) {
          failed.push(`${field}: ${e.message}`)
        }
      }
      if (failed.length) {
        lastStatus = `applied ${applied.length}, failed ${failed.length} — ${failed.join('; ')}`
      } else {
        lastStatus = `applied ${applied.length} override(s) to ${sym}`
      }
      pending = {}
    } finally {
      busy = false
    }
  }

  function revertOne(key) {
    const clone = { ...pending }
    delete clone[key]
    pending = clone
  }

  function hasPending() {
    return Object.keys(pending).length > 0
  }
</script>

<div>
  <h3>
    Live Tuning <span class="sym">{sym}</span>
    {#if hasPending()}<span class="dirty">• {Object.keys(pending).length} pending</span>{/if}
  </h3>

  {#if !tunable || !tunable.gamma}
    <div class="empty">engine has not published a config snapshot yet</div>
  {:else}
    <div class="fields">
      {#each FIELDS as f (f.key)}
        {@const v = currentValue(f)}
        {@const pendingThis = pending[f.key] !== undefined}
        <div class="row">
          <label title={f.tooltip}>{f.label}</label>
          <input
            type="range"
            min={f.min}
            max={f.max}
            step={f.step}
            value={v}
            oninput={(e) => onSlide(f, e.currentTarget.value)}
          />
          <input
            type="number"
            class="num"
            min={f.min}
            max={f.max}
            step={f.step}
            value={v}
            oninput={(e) => onSlide(f, e.currentTarget.value)}
          />
          {#if pendingThis}
            <button class="btn-revert" onclick={() => revertOne(f.key)} title="revert to engine value">↺</button>
          {/if}
        </div>
      {/each}
    </div>

    <div class="toggles">
      {#each TOGGLES as t (t.key)}
        {@const v = currentToggle(t)}
        {@const pendingThis = pending[t.key] !== undefined}
        <label class="toggle">
          <input
            type="checkbox"
            checked={v}
            onchange={(e) => onToggle(t, e.currentTarget.checked)}
          />
          {t.label}
          {#if pendingThis}<span class="dirty-dot">●</span>{/if}
        </label>
      {/each}
    </div>

    <div class="actions">
      <button class="btn-apply" onclick={applyAll} disabled={busy || !hasPending()}>
        {busy ? 'applying…' : `apply ${Object.keys(pending).length} override(s)`}
      </button>
      {#if hasPending()}
        <button class="btn-discard" onclick={() => (pending = {})} disabled={busy}>discard</button>
      {/if}
    </div>
    {#if lastStatus}
      <div class="status">{lastStatus}</div>
    {/if}
  {/if}
</div>

<style>
  h3 {
    font-size: 12px; color: #8b949e; margin-bottom: 12px;
    text-transform: uppercase; letter-spacing: 0.5px;
    display: flex; align-items: center; gap: 8px;
  }
  .sym { font-size: 10px; color: #79c0ff; font-weight: 700; }
  .dirty { font-size: 10px; color: #d29922; margin-left: auto; }
  .empty { color: #8b949e; font-size: 11px; padding: 12px 0; }
  .fields { display: flex; flex-direction: column; gap: 6px; margin-bottom: 12px; }
  .row {
    display: grid; grid-template-columns: 130px 1fr 80px 24px;
    align-items: center; gap: 8px;
  }
  label { font-size: 11px; color: #8b949e; cursor: help; }
  input[type="range"] { width: 100%; }
  input.num {
    background: #0d1117; color: #e1e4e8;
    border: 1px solid #21262d; padding: 3px 6px; border-radius: 3px;
    font-family: inherit; font-size: 11px;
    font-variant-numeric: tabular-nums; text-align: right;
  }
  .btn-revert {
    background: none; border: 1px solid #30363d; color: #8b949e;
    padding: 1px 6px; border-radius: 3px; cursor: pointer;
    font-family: inherit; font-size: 11px;
  }
  .btn-revert:hover { border-color: #d29922; color: #d29922; }
  .toggles {
    display: flex; flex-wrap: wrap; gap: 10px;
    margin-bottom: 14px; padding: 8px;
    border-top: 1px solid #1b1f27; border-bottom: 1px solid #1b1f27;
  }
  .toggle {
    font-size: 11px; color: #e1e4e8; cursor: pointer;
    display: flex; align-items: center; gap: 4px;
  }
  .dirty-dot { color: #d29922; font-size: 10px; }
  .actions { display: flex; gap: 8px; }
  .btn-apply {
    background: #238636; color: #fff; border: none;
    padding: 6px 14px; border-radius: 3px; cursor: pointer;
    font-family: inherit; font-size: 11px; font-weight: 700;
  }
  .btn-apply:hover:not(:disabled) { background: #2ea043; }
  .btn-apply:disabled { opacity: 0.4; cursor: not-allowed; }
  .btn-discard {
    background: none; border: 1px solid #30363d; color: #8b949e;
    padding: 6px 10px; border-radius: 3px; cursor: pointer;
    font-family: inherit; font-size: 11px;
  }
  .btn-discard:hover:not(:disabled) { border-color: #f85149; color: #f85149; }
  .status {
    margin-top: 10px; font-size: 10px; color: #8b949e;
    background: #0d1117; border: 1px solid #21262d;
    padding: 6px 8px; border-radius: 3px;
    word-break: break-all;
  }
</style>
