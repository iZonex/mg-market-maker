<script>
  /*
   * S5.3 — Adverse-selection tracker panel.
   *
   * Polls /api/v1/adverse-selection. Each row is a symbol with
   * its running `adverse_bps` and per-side Cartea probabilities
   * (`as_prob_bid`, `as_prob_ask`). Probabilities above 0.55 /
   * below 0.45 highlight — anything tilted against a side means
   * Cartea widens there, so operators spot toxic-flow symbols at
   * a glance.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 6_000

  let rows = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)

  async function refresh() {
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const fetches = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if (!d.running) continue
          const path = `/api/v1/agents/${encodeURIComponent(a.agent_id)}`
            + `/deployments/${encodeURIComponent(d.deployment_id)}`
            + `/details/adverse_selection`
          fetches.push(
            api.getJson(path)
              .then(resp => resp.payload?.row ? [resp.payload.row] : [])
              .catch(() => []),
          )
        }
      }
      const all = (await Promise.all(fetches)).flat()
      // One row per symbol — dedup (same symbol may appear on
      // multiple deployments; keep the one with highest
      // adverse_bps, worst-case signal).
      const bySymbol = new Map()
      for (const r of all) {
        const prev = bySymbol.get(r.symbol)
        if (!prev || Number(r.adverse_bps) > Number(prev.adverse_bps)) {
          bySymbol.set(r.symbol, r)
        }
      }
      rows = Array.from(bySymbol.values()).sort((a, b) =>
        a.symbol.localeCompare(b.symbol),
      )
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

  function fmtProb(v) {
    if (v === null || v === undefined) return '—'
    return Number(v).toFixed(2)
  }

  function probColour(v) {
    if (v === null || v === undefined) return 'var(--fg-muted)'
    const n = Number(v)
    if (n >= 0.55) return 'var(--warn)'
    if (n <= 0.45) return 'var(--warn)'
    return 'var(--fg-primary)'
  }

  function bpsColour(v) {
    const n = Number(v)
    if (Math.abs(n) >= 5) return 'var(--warn)'
    if (Math.abs(n) >= 2) return 'var(--accent)'
    return 'var(--fg-primary)'
  }
</script>

<div class="as">
  <div class="toolbar">
    <div class="title">Adverse-selection</div>
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
    <div class="empty">no symbol state yet — engine hasn't ticked</div>
  {:else}
    <div class="header row">
      <span class="sym">symbol</span>
      <span class="bps">adv bps</span>
      <span class="pbid">ρ bid</span>
      <span class="pask">ρ ask</span>
    </div>
    <div class="rows">
      {#each rows as r, i (i)}
        <div class="row data">
          <span class="sym mono">{r.symbol}</span>
          <span class="bps mono" style:color={bpsColour(r.adverse_bps)}>
            {Number(r.adverse_bps).toFixed(2)}
          </span>
          <span class="pbid mono" style:color={probColour(r.as_prob_bid)}>
            {fmtProb(r.as_prob_bid)}
          </span>
          <span class="pask mono" style:color={probColour(r.as_prob_ask)}>
            {fmtProb(r.as_prob_ask)}
          </span>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .as { display: flex; flex-direction: column; gap: var(--s-2); }
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
    grid-template-columns: 2fr 1fr 1fr 1fr;
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
  .bps, .pbid, .pask { text-align: right; font-variant-numeric: tabular-nums; }</style>
