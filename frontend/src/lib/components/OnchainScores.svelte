<script>
  /*
   * R3.8 — On-chain surveillance panel.
   *
   * Polls /api/v1/onchain/scores. Each row is a symbol with:
   *   - holder concentration %: fraction of supply held by
   *     top-N wallets on-chain. 0.9+ is RAVE-territory.
   *   - CEX inflow total + event count: raw token notional
   *     moving from suspect wallets to known-CEX deposits
   *     over the tracker window. Non-zero = early warning
   *     that the team is loading exchanges for a sell.
   *
   * The panel stays hidden (empty state) when no onchain
   * provider is configured — the endpoint returns an empty
   * rows array.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 15_000

  let rows = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)
  let now = $state(Date.now())

  async function refresh() {
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const fetches = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if (!d.running) continue
          const path = `/api/v1/agents/${encodeURIComponent(a.agent_id)}`
            + `/deployments/${encodeURIComponent(d.deployment_id)}`
            + `/details/onchain_scores`
          fetches.push(
            api.getJson(path)
              .then(resp => resp.payload?.snapshots || [])
              .catch(() => []),
          )
        }
      }
      const all = (await Promise.all(fetches)).flat()
      // Dedup by symbol (same chain data may appear on multiple
      // deployments). Keep the latest-fetched entry.
      const bySymbol = new Map()
      for (const r of all) {
        const key = r.symbol
        const prev = bySymbol.get(key)
        if (!prev || (r.fetched_at_ms || 0) > (prev.fetched_at_ms || 0)) {
          bySymbol.set(key, r)
        }
      }
      rows = Array.from(bySymbol.values())
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

  function concentrationColour(v) {
    const n = Number(v)
    if (n >= 0.8) return 'var(--danger)'
    if (n >= 0.5) return 'var(--warn)'
    return 'var(--fg-primary)'
  }

  function inflowColour(v) {
    const n = Number(v)
    if (n > 0) return 'var(--danger)'
    return 'var(--fg-primary)'
  }

  function fmtPct(v) {
    return `${(Number(v) * 100).toFixed(1)}%`
  }

  function fmtAgo(ms) {
    if (!ms) return '—'
    const s = Math.max(0, Math.floor((now - ms) / 1000))
    if (s < 60) return `${s}s`
    if (s < 3600) return `${Math.floor(s / 60)}m`
    return `${Math.floor(s / 3600)}h`
  }
</script>

<div class="onchain">
  <div class="toolbar">
    <div class="title">On-chain surveillance</div>
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
    <div class="empty">no on-chain snapshots — configure [onchain] + provider key to enable</div>
  {:else}
    <div class="header row">
      <span class="sym">symbol</span>
      <span class="chain">chain</span>
      <span class="val">holders (top-N)</span>
      <span class="val">CEX inflow</span>
      <span class="val">events</span>
      <span class="val">age</span>
    </div>
    <div class="rows">
      {#each rows as r, i (i)}
        <div class="row data">
          <span class="sym mono">{r.symbol}</span>
          <span class="chain mono">{r.chain}</span>
          <span class="val mono" style:color={concentrationColour(r.concentration_pct)}>
            <b>{fmtPct(r.concentration_pct)}</b>
            <small>({r.top_n})</small>
          </span>
          <span class="val mono" style:color={inflowColour(r.inflow_total)}>
            {Number(r.inflow_total).toLocaleString(undefined, { maximumFractionDigits: 2 })}
          </span>
          <span class="val mono" style:color={inflowColour(r.inflow_events)}>
            {r.inflow_events}
          </span>
          <span class="val mono">{fmtAgo(r.computed_at_ms)}</span>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .onchain { display: flex; flex-direction: column; gap: var(--s-2); }
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
    grid-template-columns: 1.5fr 1fr 1.3fr 1.5fr 0.8fr 0.7fr;
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
  .chain { color: var(--fg-secondary); text-transform: lowercase; font-size: 10px; }
  .val { text-align: right; font-variant-numeric: tabular-nums; }
  .val small { color: var(--fg-muted); margin-left: 4px; }
  .mono { font-family: var(--font-mono); }
</style>
