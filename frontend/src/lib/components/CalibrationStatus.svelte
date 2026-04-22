<script>
  /*
   * S5.4 — GLFT-style calibration status panel.
   *
   * Polls /api/v1/calibration/status. Each row is a symbol
   * whose active strategy publishes live calibration (GLFT
   * today). Shows fitted (a, k), sample count, and time since
   * the last retune. `< 50` samples flags as "seeded" — the
   * strategy is still running on constructor defaults.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 10_000

  let rows = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)
  let now = $state(Date.now())

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/calibration/status')
      rows = data?.rows ?? []
      error = null
      lastFetch = new Date()
      now = Date.now()
      loading = false
    } catch (e) {
      error = e?.message || String(e)
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(t)
  })

  function fmtAgo(ms) {
    if (!ms) return 'never'
    const s = Math.max(0, Math.floor((now - ms) / 1000))
    if (s < 60) return `${s}s ago`
    if (s < 3600) return `${Math.floor(s / 60)}m ago`
    return `${Math.floor(s / 3600)}h ago`
  }

  function samplesLabel(n) {
    if (n < 50) return 'seeded'
    return `${n} samples`
  }

  function samplesColour(n) {
    return n < 50 ? 'var(--warn)' : 'var(--fg-primary)'
  }
</script>

<div class="cal">
  <div class="toolbar">
    <div class="title">Live calibration</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{rows.length} row(s) · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && rows.length === 0}
    <div class="empty">no calibrating strategy active — stateless quoter (grid / basis / AS)</div>
  {:else}
    <div class="header row">
      <span class="sym">symbol</span>
      <span class="strat">strategy</span>
      <span class="val">a</span>
      <span class="val">k</span>
      <span class="samp">samples</span>
      <span class="ago">retune</span>
    </div>
    <div class="rows">
      {#each rows as r, i (i)}
        <div class="row data">
          <span class="sym mono">{r.symbol}</span>
          <span class="strat mono">{r.strategy}</span>
          <span class="val mono">{Number(r.a).toFixed(3)}</span>
          <span class="val mono">{Number(r.k).toFixed(3)}</span>
          <span class="samp mono" style:color={samplesColour(r.samples)}>
            {samplesLabel(r.samples)}
          </span>
          <span class="ago mono">{fmtAgo(r.last_recalibrated_ms)}</span>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .cal { display: flex; flex-direction: column; gap: var(--s-2); }
  .toolbar {
    display: flex; align-items: center; justify-content: space-between;
    padding: 0 var(--s-2); font-size: var(--fs-xs);
  }
  .title { font-weight: 600; color: var(--fg-primary); }
  .meta { color: var(--fg-muted); display: flex; align-items: center; gap: var(--s-2); }
  .meta .error { color: var(--danger); }
  .empty {
    color: var(--fg-muted); font-size: var(--fs-xs);
    padding: var(--s-4); text-align: center;
  }
  .spinner {
    display: inline-block; width: 10px; height: 10px;
    border: 2px solid var(--border-subtle);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    margin-right: var(--s-1);
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .row {
    display: grid;
    grid-template-columns: 1.5fr 1fr 1fr 1fr 1.2fr 1fr;
    gap: var(--s-2);
    padding: var(--s-1) var(--s-2);
    font-size: var(--fs-xs);
    align-items: baseline;
  }
  .header {
    color: var(--fg-muted);
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    border-bottom: 1px solid var(--border-subtle);
  }
  .rows { display: flex; flex-direction: column; }
  .row.data + .row.data { border-top: 1px solid var(--border-subtle); }
  .sym { color: var(--fg-primary); font-weight: 600; }
  .strat { color: var(--fg-secondary); text-transform: uppercase; font-size: 10px; }
  .val, .samp, .ago { text-align: right; font-variant-numeric: tabular-nums; }
  .mono { font-family: var(--font-mono); }
</style>
