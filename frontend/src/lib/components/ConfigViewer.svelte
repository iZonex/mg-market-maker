<script>
  /*
   * UX-5 — read-only visualisation of the effective `AppConfig`
   * the server booted with. Answers the operator question
   * "what's configured, what's on defaults, what's disabled"
   * without shelling into the host to `cat` the TOML.
   *
   * Data comes from `/api/v1/config/snapshot`. Secrets never
   * land in `AppConfig` (they're env-only), so the full struct
   * is safe to render. For optional sections (hedge, margin,
   * rebalancer, ...) we show a status chip — the operator sees
   * at a glance which product features are actually wired.
   */

  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  const api = createApiClient(auth)

  let snapshot = $state(null)
  let error = $state('')
  let loading = $state(true)
  let expanded = $state(false)

  async function load() {
    loading = true
    error = ''
    try {
      snapshot = await api.getJson('/api/v1/config/snapshot')
    } catch (e) {
      error = e?.message || String(e)
      snapshot = null
    } finally {
      loading = false
    }
  }

  $effect(() => { load() })

  // Optional top-level sections we surface as "wired / not wired"
  // chips. Each entry is [label, key, hint]. The backend hands
  // back `null` for the ones left unset in TOML.
  const optionalSections = [
    ['Hedge connector',   'hedge',                 'cross-product strategies'],
    ['Funding arb driver','funding_arb',           'atomic basis-shifted quoting'],
    ['Paper fill sim',    'paper_fill',            'probabilistic filler params'],
    ['Cross-venue rebalancer', 'rebalancer',       'auto-transfer between venues'],
    ['Portfolio risk',    'portfolio_risk',        'factor limits + delta guard'],
    ['Margin guard',      'margin',                'perp margin-ratio kill switch'],
    ['Listing sniper entry', 'listing_sniper_entry', 'auto-enter new listings'],
    ['Pair screener',     'pair_screener',         'stat-arb candidate discovery'],
  ]

  // Arrays we summarise as a count (expandable detail below).
  const arraySections = [
    ['Symbols',           'symbols'],
    ['Clients',           'clients'],
    ['Users',             'users'],
    ['SOR extra venues',  'sor_extra_venues'],
  ]

  // Top-level top-N scalars worth surfacing.
  const flags = [
    ['Mode',              'mode'],
    ['Dashboard port',    'dashboard_port'],
    ['Checkpoint restore','checkpoint_restore'],
    ['Record market data','record_market_data'],
    ['Log file',          'log_file'],
    ['Checkpoint path',   'checkpoint_path'],
  ]

  function sectionStatus(key) {
    if (!snapshot) return 'unknown'
    const v = snapshot[key]
    if (v === null || v === undefined) return 'off'
    if (Array.isArray(v)) return v.length > 0 ? 'on' : 'off'
    if (typeof v === 'object' && Object.keys(v).length === 0) return 'off'
    return 'on'
  }

  function loansCount() {
    const l = snapshot?.loans
    if (!l || typeof l !== 'object') return 0
    return Object.keys(l).length
  }
</script>

<div class="viewer">
  <div class="top">
    <div class="header">
      <span class="label">Config snapshot</span>
      <span class="hint">what's wired vs on defaults</span>
    </div>
    <div class="actions">
      <button type="button" class="btn" onclick={load} disabled={loading}>
        <Icon name="refresh" size={14} />
        <span>{loading ? 'Loading…' : 'Reload'}</span>
      </button>
      <button type="button" class="btn ghost" onclick={() => expanded = !expanded}>
        <span>{expanded ? 'Hide raw JSON' : 'Show raw JSON'}</span>
      </button>
    </div>
  </div>

  {#if error}
    <div class="error">Failed to load: {error}</div>
  {:else if loading && !snapshot}
    <div class="note">Loading config…</div>
  {:else if snapshot}
    <div class="section">
      <div class="section-title">Runtime flags</div>
      <div class="rows">
        {#each flags as [label, key] (key)}
          {@const raw = snapshot[key]}
          <div class="row">
            <span class="row-label">{label}</span>
            <span class="row-value {raw === '' || raw == null ? 'muted' : ''}">
              {raw === '' || raw == null ? '—' : String(raw)}
            </span>
          </div>
        {/each}
        <div class="row">
          <span class="row-label">Telegram alerts</span>
          <span class="row-value {snapshot?.telegram?.enabled ? 'on' : 'muted'}">
            {snapshot?.telegram?.enabled ? 'enabled' : 'disabled'}
          </span>
        </div>
        <div class="row">
          <span class="row-label">Loan agreements</span>
          <span class="row-value">{loansCount()} configured</span>
        </div>
      </div>
    </div>

    <div class="section">
      <div class="section-title">Optional subsystems</div>
      <div class="chips">
        {#each optionalSections as [label, key, hint] (key)}
          {@const st = sectionStatus(key)}
          <div class="chip" class:on={st === 'on'} class:off={st === 'off'}>
            <span class="dot"></span>
            <div class="chip-body">
              <span class="chip-label">{label}</span>
              <span class="chip-hint">{hint}</span>
            </div>
            <span class="chip-state">{st === 'on' ? 'wired' : 'off'}</span>
          </div>
        {/each}
      </div>
    </div>

    <div class="section">
      <div class="section-title">Collections</div>
      <div class="rows">
        {#each arraySections as [label, key] (key)}
          {@const arr = snapshot[key] ?? []}
          <div class="row">
            <span class="row-label">{label}</span>
            <span class="row-value">
              {arr.length}
              {#if key === 'symbols' && arr.length > 0}
                <span class="muted"> — {arr.slice(0, 4).join(', ')}{arr.length > 4 ? '…' : ''}</span>
              {/if}
            </span>
          </div>
        {/each}
      </div>
    </div>

    {#if expanded}
      <div class="section">
        <div class="section-title">Raw snapshot</div>
        <pre class="raw">{JSON.stringify(snapshot, null, 2)}</pre>
      </div>
    {/if}
  {/if}
</div>

<style>
  .viewer { display: flex; flex-direction: column; gap: var(--s-4); }
  .top {
    display: flex; justify-content: space-between; align-items: center;
    gap: var(--s-3);
  }
  .header { display: flex; flex-direction: column; gap: 2px; }
  .label {
    font-size: var(--fs-sm); font-weight: 600; color: var(--fg-primary);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .hint { font-size: var(--fs-xs); color: var(--fg-muted); }
  .actions { display: flex; gap: var(--s-2); }
  .btn {
    display: inline-flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md); color: var(--fg-primary);
    font-size: var(--fs-xs); cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .btn:hover:not(:disabled) { background: var(--bg-raised); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.ghost { background: transparent; }

  .error {
    padding: var(--s-3); background: var(--danger-bg);
    border-radius: var(--r-md); color: var(--danger); font-size: var(--fs-sm);
  }
  .note { padding: var(--s-3); color: var(--fg-muted); font-size: var(--fs-sm); }

  .section { display: flex; flex-direction: column; gap: var(--s-2); }
  .section-title {
    font-size: var(--fs-xs); font-weight: 600; color: var(--fg-muted);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }

  .rows { display: flex; flex-direction: column; gap: 1px; background: var(--border-subtle);
    border-radius: var(--r-md); overflow: hidden; }
  .row {
    display: flex; justify-content: space-between; align-items: center;
    padding: var(--s-2) var(--s-3); background: var(--bg-raised);
    font-size: var(--fs-sm);
  }
  .row-label { color: var(--fg-secondary); }
  .row-value { color: var(--fg-primary); font-family: var(--font-mono); font-size: var(--fs-xs); }
  .row-value.muted { color: var(--fg-muted); }
  .row-value.on { color: var(--success); }
  .muted { color: var(--fg-muted); }

  .chips { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: var(--s-2); }
  .chip {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-raised); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .chip .dot {
    width: 8px; height: 8px; border-radius: 50%; background: var(--fg-muted);
    flex-shrink: 0;
  }
  .chip.on .dot { background: var(--success); box-shadow: 0 0 0 2px rgba(16, 185, 129, 0.18); }
  .chip.off .dot { background: var(--fg-muted); }
  .chip-body { display: flex; flex-direction: column; gap: 1px; min-width: 0; flex: 1; }
  .chip-label { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 500; }
  .chip-hint { font-size: var(--fs-xs); color: var(--fg-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .chip-state {
    font-size: var(--fs-xs); font-weight: 600; text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .chip.on .chip-state { color: var(--success); }
  .chip.off .chip-state { color: var(--fg-muted); }

  .raw {
    max-height: 480px; overflow: auto;
    padding: var(--s-3); background: var(--bg-base);
    border: 1px solid var(--border-subtle); border-radius: var(--r-md);
    font-family: var(--font-mono); font-size: var(--fs-xs);
    color: var(--fg-secondary); white-space: pre;
  }
</style>
