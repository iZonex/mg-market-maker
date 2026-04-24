<script>
  /*
   * Live ParamTuner — drilldown edition.
   *
   * Shape contract:
   *   { row: DeploymentStateRow, onPatch: (patch) => Promise, canControl: bool }
   *
   * Reads current tunable values off `row.variables` (the
   * authoritative effective config the deployment is running
   * with). Operator edits accumulate in a local `pending` map
   * keyed by variable name; "Apply" ships the whole batch as a
   * single PATCH so the agent's translator maps each known key
   * to a `ConfigOverride` in one round-trip.
   *
   * Variable keys here MUST match the match arms in
   * `crates/agent/src/registry.rs::translate_variable_override()`
   * — if a new tunable is added, both sides change together.
   */

  import Icon from './Icon.svelte'

  import { Button } from '../primitives/index.js'

  let { row, onPatch, canControl = false } = $props()

  const variables = $derived(row?.variables || {})

  let pending = $state({})
  let busy = $state(false)
  let lastStatus = $state('')

  // Numeric / integer knobs — slider + direct input.
  const FIELDS = [
    { key: 'gamma',            label: 'Gamma (γ)',          tooltip: 'Risk aversion. Higher → tighter spread / less inventory risk.',   min: 0.001, max: 5,   step: 0.001 },
    { key: 'min_spread_bps',   label: 'Min spread (bps)',   tooltip: 'Floor on the quoted spread. Ignored when computed spread wider.', min: 0,     max: 100, step: 0.5 },
    { key: 'max_distance_bps', label: 'Max distance (bps)', tooltip: 'Outermost ladder level cap distance from mid.',                   min: 1,     max: 500, step: 1 },
    { key: 'order_size',       label: 'Order size',         tooltip: 'Base-asset qty per level.',                                       min: 0,     max: 10,  step: 0.0001 },
    { key: 'num_levels',       label: 'Levels per side',    tooltip: 'Ladder depth per side.',                                          min: 1,     max: 10,  step: 1, isInt: true },
    { key: 'max_inventory',    label: 'Max inventory',      tooltip: 'Absolute base-asset inventory cap. 0 disables.',                  min: 0,     max: 100, step: 0.0001 },
    { key: 'amend_max_ticks',  label: 'Amend tick budget',  tooltip: '0 disables amend. Higher = more aggressive queue-priority preservation.', min: 0, max: 20, step: 1, isInt: true },
  ]

  // Boolean knobs.
  const TOGGLES = [
    { key: 'momentum_enabled',          label: 'Momentum alpha' },
    { key: 'market_resilience_enabled', label: 'Market resilience' },
    { key: 'amend_enabled',             label: 'Amend in-place' },
    { key: 'otr_enabled',               label: 'OTR snapshots' },
  ]

  function parseNumber(v) {
    if (typeof v === 'number') return v
    if (typeof v === 'string') {
      const n = parseFloat(v)
      return Number.isFinite(n) ? n : null
    }
    return null
  }

  function fieldValue(f) {
    if (pending[f.key] !== undefined) return pending[f.key]
    const raw = variables[f.key]
    const n = parseNumber(raw)
    return n !== null ? n : (f.isInt ? f.min : f.min)
  }

  function toggleValue(t) {
    if (pending[t.key] !== undefined) return pending[t.key]
    return Boolean(variables[t.key])
  }

  function onSlide(f, raw) {
    const parsed = f.isInt ? parseInt(raw, 10) : parseFloat(raw)
    if (!Number.isFinite(parsed)) return
    pending = { ...pending, [f.key]: parsed }
  }

  function onCheck(t, checked) {
    pending = { ...pending, [t.key]: checked }
  }

  function revert(key) {
    const clone = { ...pending }
    delete clone[key]
    pending = clone
  }

  const pendingCount = $derived(Object.keys(pending).length)

  async function applyAll() {
    if (pendingCount === 0) return
    busy = true
    lastStatus = ''
    // Build the patch map. Numeric values go over the wire as
    // strings so the agent's Decimal parser gets stable precision
    // — the agent translator accepts both shapes but string form
    // matches how the existing ConfigOverride path serialises.
    const patch = {}
    for (const [k, v] of Object.entries(pending)) {
      if (typeof v === 'boolean') {
        patch[k] = v
      } else if (typeof v === 'number' && Number.isInteger(v)) {
        patch[k] = v
      } else {
        patch[k] = String(v)
      }
    }
    try {
      await onPatch(patch)
      lastStatus = `applied ${Object.keys(patch).length} override(s)`
      pending = {}
    } catch (e) {
      lastStatus = `failed: ${e?.message || e}`
    } finally {
      busy = false
    }
  }

  function pct(f) {
    const v = fieldValue(f)
    if (f.max <= f.min) return 0
    const p = ((v - f.min) / (f.max - f.min)) * 100
    return Math.max(0, Math.min(100, p))
  }
</script>

<div class="tuner">
  <div class="fields">
    {#each FIELDS as f (f.key)}
      {@const v = fieldValue(f)}
      {@const pendingThis = pending[f.key] !== undefined}
      <div class="row" class:pending={pendingThis}>
        <label class="f-label" for={`tuner-${f.key}`} title={f.tooltip}>{f.label}</label>
        <input
          type="range"
          class="slider"
          min={f.min}
          max={f.max}
          step={f.step}
          value={v}
          disabled={!canControl}
          style:--value="{pct(f)}%"
          oninput={(e) => onSlide(f, e.currentTarget.value)}
          aria-label={f.label}
        />
        <input
          id={`tuner-${f.key}`}
          type="number"
          class="num-input mono"
          min={f.min}
          max={f.max}
          step={f.step}
          value={v}
          disabled={!canControl}
          oninput={(e) => onSlide(f, e.currentTarget.value)}
        />
        <span class:revert-visible={pendingThis}>
          <Button variant="ghost" size="sm" iconOnly
            onclick={() => revert(f.key)}
            title="Revert"
            disabled={!pendingThis}>
            {#snippet children()}<Icon name="clock" size={12} />{/snippet}
          </Button>
        </span>
      </div>
    {/each}
  </div>

  <div class="toggles">
    {#each TOGGLES as t (t.key)}
      {@const v = toggleValue(t)}
      {@const pendingThis = pending[t.key] !== undefined}
      <label class="toggle" class:pending={pendingThis}>
        <input
          type="checkbox"
          checked={v}
          disabled={!canControl}
          onchange={(e) => onCheck(t, e.currentTarget.checked)}
        />
        <span>{t.label}</span>
        {#if pendingThis}<span class="chip chip-warn">edited</span>{/if}
      </label>
    {/each}
  </div>

  <div class="actions">
    <Button variant="primary" onclick={applyAll}
 disabled={busy || !canControl || pendingCount === 0}>
          {#snippet children()}{#if busy}
        <span class="spinner"></span>
        <span>Applying…</span>
      {:else}
        <Icon name="check" size={14} />
        <span>Apply {pendingCount} override{pendingCount === 1 ? '' : 's'}</span>
      {/if}{/snippet}
        </Button>
    {#if pendingCount > 0}
      <Button variant="primary" onclick={() => (pending = {})}
 disabled={busy}>
          {#snippet children()}Discard{/snippet}
        </Button>
    {/if}
  </div>

  {#if lastStatus}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>{lastStatus}</span>
    </div>
  {/if}

  {#if !canControl}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>Read-only — operator role required to push overrides.</span>
    </div>
  {/if}
</div>

<style>
  .tuner { display: flex; flex-direction: column; gap: var(--s-3); }

  .fields { display: flex; flex-direction: column; gap: var(--s-2); }
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

  .slider {
    width: 100%;
    accent-color: var(--accent);
  }
  .num-input {
    padding: 2px 6px;
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    font-size: var(--fs-xs);
    width: 90px;
  }

  .btn-icon { padding: 4px; width: 24px; height: 24px; justify-content: center; }
  .btn-primary {
    color: var(--bg-base);
    background: var(--accent);
    border-color: var(--accent);
  }
  .btn-primary:hover:not(:disabled) { opacity: 0.92; }
  .btn-ghost { color: var(--fg-muted); border-color: var(--border-subtle); }
  .btn-ghost:hover:not(:disabled) { color: var(--fg-primary); background: var(--bg-base); }
  .revert-visible { opacity: 1 !important; pointer-events: auto !important; }

  .toggles {
    display: flex; flex-wrap: wrap; gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border-radius: var(--r-sm);
  }
  .toggle {
    display: flex; align-items: center; gap: var(--s-2);
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    cursor: pointer; user-select: none;
  }
  .toggle:hover { color: var(--fg-primary); }
  .toggle.pending { color: var(--warn); }

  .chip {
    font-family: var(--font-mono); font-size: 10px;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-weight: 600; padding: 2px 6px; border-radius: var(--r-sm);
    border: 1px solid currentColor;
  }
  .chip-warn { color: var(--warn); }

  .actions { display: flex; gap: var(--s-2); }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid rgba(0, 0, 0, 0.25);
    border-top-color: var(--bg-base);
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .status-line {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    color: var(--fg-secondary);
    word-break: break-word;
  }

  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
</style>
