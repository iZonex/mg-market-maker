<script>
  /*
   * MV-UI-2 — Per-venue health panel.
   *
   * Reads /api/v1/venues/status (already lists all symbols with
   * kill level + SLA + live orders), groups client-side by
   * the venue field, surfaces aggregated health per venue.
   *
   * Scope kept tight: no new backend endpoint beyond what
   * venues_status already returns. Operator sees one row per
   * venue with symbol count, aggregated kill level (max across
   * symbols), total live orders, min SLA uptime.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 3_000

  let rows = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/venues/status')
      rows = Array.isArray(data) ? data : []
      error = null
      lastFetch = new Date()
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

  // Client-side group by venue. Each row's state.venue is
  // already populated from the SymbolState via WS; here we read
  // it off the /api/v1/status payload. Fallback "-" when absent
  // so the table keeps rendering.
  const grouped = $derived(() => {
    const byV = new Map()
    for (const r of rows) {
      const v = r.venue || '-'
      if (!byV.has(v)) byV.set(v, [])
      byV.get(v).push(r)
    }
    return Array.from(byV.entries()).map(([venue, items]) => ({
      venue,
      symbols: items.length,
      max_kill: items.reduce((m, i) => Math.max(m, Number(i.kill_level || 0)), 0),
      live_orders: items.reduce((s, i) => s + Number(i.live_orders || 0), 0),
      min_uptime: items.reduce((m, i) => Math.min(m, Number(i.sla_uptime_pct || 100)), 100),
      halted: items.some(i => i.quoting_halted),
      venue_items: items,
    })).sort((a, b) => a.venue.localeCompare(b.venue))
  })

  function killColour(level) {
    if (level >= 3) return 'var(--danger)'
    if (level === 2) return 'var(--warn)'
    if (level === 1) return 'var(--warn)'
    return 'var(--accent)'
  }
  const killLabel = {
    0: 'NORMAL', 1: 'WIDEN', 2: 'STOP', 3: 'CANCEL', 4: 'FLATTEN', 5: 'DISCONNECT',
  }
</script>

<div class="vh">
  <div class="toolbar">
    <div class="title">Venues health</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{grouped().length} venue(s) · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && grouped().length === 0}
    <div class="empty">no venue data — is the server running?</div>
  {:else}
    <div class="rows">
      {#each grouped() as v (v.venue)}
        <div class="venue" class:halted={v.halted}>
          <div class="venue-head">
            <span class="name">{v.venue}</span>
            <span class="chip" style:color={killColour(v.max_kill)}>
              {killLabel[v.max_kill] || 'UNKNOWN'}
            </span>
          </div>
          <div class="stats">
            <div class="stat">
              <span class="k">symbols</span>
              <span class="v mono">{v.symbols}</span>
            </div>
            <div class="stat">
              <span class="k">live orders</span>
              <span class="v mono">{v.live_orders}</span>
            </div>
            <div class="stat">
              <span class="k">min SLA</span>
              <span class="v mono">{v.min_uptime.toFixed(2)}%</span>
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .vh { display: flex; flex-direction: column; gap: var(--s-3); }
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
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .rows { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: var(--s-3); }
  .venue {
    padding: var(--s-2) var(--s-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
  }
  .venue.halted { border-color: var(--danger); }
  .venue-head {
    display: flex; align-items: baseline; justify-content: space-between;
    margin-bottom: var(--s-2);
  }
  .name {
    font-family: var(--font-mono);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: var(--tracking-tight);
    color: var(--fg-primary);
  }
  .chip {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-weight: 600;
  }
  .stats {
    display: flex; flex-direction: column; gap: 2px;
    font-size: var(--fs-xs);
  }
  .stat { display: flex; justify-content: space-between; }
  .k { color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; font-size: 10px; }
  .v { color: var(--fg-primary); }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
</style>
