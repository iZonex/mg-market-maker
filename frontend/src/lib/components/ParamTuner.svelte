<script>
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { data, auth } = $props()
  const api = createApiClient(auth)

  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
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

{#if !tunable || !tunable.gamma}
  <div class="empty-state">
    <span class="empty-state-icon"><Icon name="clock" size={18} /></span>
    <span class="empty-state-title">Waiting for engine snapshot</span>
    <span class="empty-state-hint">Tunable config publishes on every refresh tick.</span>
  </div>
{:else}
<div class="tuner">
  <header class="tuner-head">
    <div>
      <span class="label">Live tuning</span>
      <span class="sym num">{sym}</span>
    </div>
    {#if hasPending()}
      <span class="chip chip-warn">{Object.keys(pending).length} pending</span>
    {/if}
  </header>

  <div class="fields">
    {#each FIELDS as f (f.key)}
      {@const v = currentValue(f)}
      {@const pendingThis = pending[f.key] !== undefined}
      {@const pct = f.max > f.min ? ((parseFloat(v || 0) - f.min) / (f.max - f.min)) * 100 : 0}
      <div class="row" class:pending={pendingThis}>
        <label class="f-label" title={f.tooltip}>{f.label}</label>
        <input
          type="range"
          class="slider"
          min={f.min}
          max={f.max}
          step={f.step}
          value={v}
          style:--value="{Math.max(0, Math.min(100, pct))}%"
          oninput={(e) => onSlide(f, e.currentTarget.value)}
        />
        <input
          type="number"
          class="num-input"
          min={f.min}
          max={f.max}
          step={f.step}
          value={v}
          oninput={(e) => onSlide(f, e.currentTarget.value)}
        />
        <button
          type="button"
          class="btn btn-icon btn-sm btn-ghost"
          class:revert-visible={pendingThis}
          onclick={() => revertOne(f.key)}
          title="Revert to engine value"
          disabled={!pendingThis}
        >
          <Icon name="clock" size={12} />
        </button>
      </div>
    {/each}
  </div>

  <div class="toggles">
    {#each TOGGLES as t (t.key)}
      {@const v = currentToggle(t)}
      {@const pendingThis = pending[t.key] !== undefined}
      <label class="toggle" class:pending={pendingThis}>
        <input
          type="checkbox"
          class="checkbox"
          checked={v}
          onchange={(e) => onToggle(t, e.currentTarget.checked)}
        />
        <span>{t.label}</span>
        {#if pendingThis}<span class="chip chip-warn">edited</span>{/if}
      </label>
    {/each}
  </div>

  <div class="actions">
    <button type="button" class="btn btn-primary" onclick={applyAll} disabled={busy || !hasPending()}>
      {#if busy}
        <span class="spinner"></span>
        <span>Applying…</span>
      {:else}
        <Icon name="check" size={14} />
        <span>Apply {Object.keys(pending).length} override{Object.keys(pending).length === 1 ? '' : 's'}</span>
      {/if}
    </button>
    {#if hasPending()}
      <button type="button" class="btn btn-ghost" onclick={() => (pending = {})} disabled={busy}>
        Discard
      </button>
    {/if}
  </div>

  {#if lastStatus}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>{lastStatus}</span>
    </div>
  {/if}
</div>
{/if}

<style>
  .tuner {
    display: flex;
    flex-direction: column;
    gap: var(--s-5);
  }
  .tuner-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
  }
  .tuner-head > div {
    display: flex;
    align-items: baseline;
    gap: var(--s-3);
  }
  .sym {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--accent);
  }

  .fields {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
  }
  .row {
    display: grid;
    grid-template-columns: 140px 1fr 90px 28px;
    align-items: center;
    gap: var(--s-3);
  }
  .f-label {
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    cursor: help;
    font-weight: 500;
    user-select: none;
  }
  .row.pending .f-label { color: var(--warn); }
  .btn.revert-visible { opacity: 1; }
  .btn:disabled { opacity: 0; pointer-events: none; }
  .revert-visible { opacity: 1 !important; }

  .toggles {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-4);
    padding: var(--s-3);
    background: var(--bg-chip);
    border-radius: var(--r-lg);
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    font-size: var(--fs-sm);
    color: var(--fg-secondary);
    cursor: pointer;
    user-select: none;
    font-weight: 500;
  }
  .toggle:hover { color: var(--fg-primary); }
  .toggle.pending { color: var(--warn); }

  .actions {
    display: flex;
    gap: var(--s-2);
  }
  .spinner {
    width: 12px; height: 12px;
    border: 2px solid rgba(0, 0, 0, 0.25);
    border-top-color: #001510;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

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
