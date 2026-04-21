<script>
  /*
   * R2.6 — CEX-side manipulation detector panel.
   *
   * Source: `/api/v1/surveillance/fleet` (fleet aggregate).
   * Each returned row is one deployment with pump_dump, wash,
   * thin_book, and combined sub-scores. We group by symbol so
   * an operator running the same pair on multiple agents still
   * sees one line — taking max per score across agents because
   * any single agent reporting > 0.5 warrants attention.
   *
   * LEGACY-1 (2026-04-21) — was polling `/api/v1/manipulation/scores`
   * which reads controller-local DashboardState. In distributed
   * mode nothing writes to that store, so the panel stayed empty
   * even when per-agent detectors were firing. Switched to the
   * fleet endpoint which fans out per-deployment.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 5_000

  let rows = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)

  function groupBySymbol(fleetRows) {
    const bySym = new Map()
    for (const r of fleetRows) {
      const key = r.symbol
      const existing = bySym.get(key)
      const num = (v) => {
        const n = Number(v)
        return Number.isFinite(n) ? n : 0
      }
      const next = {
        symbol: key,
        pump_dump: num(r.pump_dump),
        wash: num(r.wash),
        thin_book: num(r.thin_book),
        combined: num(r.combined),
      }
      if (!existing) {
        bySym.set(key, next)
      } else {
        bySym.set(key, {
          symbol: key,
          pump_dump: Math.max(existing.pump_dump, next.pump_dump),
          wash: Math.max(existing.wash, next.wash),
          thin_book: Math.max(existing.thin_book, next.thin_book),
          combined: Math.max(existing.combined, next.combined),
        })
      }
    }
    return Array.from(bySym.values()).sort((a, b) => b.combined - a.combined)
  }

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/surveillance/fleet')
      rows = groupBySymbol(Array.isArray(data) ? data : [])
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

  function scoreColour(v) {
    const n = Number(v)
    if (n >= 0.5) return 'var(--danger)'
    if (n >= 0.25) return 'var(--warn)'
    return 'var(--fg-primary)'
  }

  function fmt(v) {
    return Number(v).toFixed(3)
  }
</script>

<div class="manip">
  <div class="toolbar">
    <div class="title">Manipulation detector</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{rows.length} symbol(s) · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && rows.length === 0}
    <div class="empty">no symbol snapshots yet — engine hasn't ticked</div>
  {:else}
    <div class="header row">
      <span class="sym">symbol</span>
      <span class="val">pump / dump</span>
      <span class="val">wash</span>
      <span class="val">thin book</span>
      <span class="val combined-col">combined</span>
    </div>
    <div class="rows">
      {#each rows as r, i (i)}
        <div class="row data" class:danger={Number(r.combined) >= 0.5}>
          <span class="sym mono">{r.symbol}</span>
          <span class="val mono" style:color={scoreColour(r.pump_dump)}>{fmt(r.pump_dump)}</span>
          <span class="val mono" style:color={scoreColour(r.wash)}>{fmt(r.wash)}</span>
          <span class="val mono" style:color={scoreColour(r.thin_book)}>{fmt(r.thin_book)}</span>
          <span class="val mono combined-col" style:color={scoreColour(r.combined)}>
            <b>{fmt(r.combined)}</b>
          </span>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .manip { display: flex; flex-direction: column; gap: var(--s-2); }
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
    grid-template-columns: 2fr 1fr 1fr 1fr 1fr;
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
  .row.data.danger { background: var(--danger-dim, rgba(255, 80, 80, 0.08)); }
  .sym { color: var(--fg-primary); font-weight: 600; }
  .val { text-align: right; font-variant-numeric: tabular-nums; }
  .combined-col { font-weight: 700; }
  .mono { font-family: var(--font-mono); }
</style>
