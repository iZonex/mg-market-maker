<script>
  /*
   * R2.6 — CEX-side manipulation detector panel.
   *
   * Polls /api/v1/manipulation/scores. Each row is a symbol
   * with four sub-scores: pump_dump, wash, thin_book, combined.
   * Combined > 0.5 highlights the row — that's where the
   * RAVE-style rug pattern is forming and the engine should
   * widen / pause on its own via the graph gate.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 5_000

  let rows = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/manipulation/scores')
      rows = data?.rows ?? []
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
