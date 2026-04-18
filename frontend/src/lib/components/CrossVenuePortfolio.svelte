<script>
  /*
   * MV-UI-1 — Cross-venue portfolio panel.
   *
   * Polls /api/v1/portfolio/cross_venue every REFRESH_MS. One
   * row per base asset with the aggregated net delta + a
   * collapsible per-venue breakdown so the operator sees
   * "BTC = +0.3 (Binance +0.5 · Bybit -0.2)" at a glance.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 4_000

  let assets = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/portfolio/cross_venue')
      assets = (data?.assets ?? []).map(a => ({
        ...a,
        net_delta: Number(a.net_delta),
        legs: (a.legs ?? []).map(l => ({ ...l, inventory: Number(l.inventory) })),
      }))
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

  function deltaColour(v) {
    if (Math.abs(v) < 1e-12) return 'var(--fg-muted)'
    return v > 0 ? 'var(--accent)' : 'var(--danger)'
  }
</script>

<div class="cvp">
  <div class="toolbar">
    <div class="title">Cross-venue portfolio</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{assets.length} asset(s) · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && assets.length === 0}
    <div class="empty">no engines registered — start at least one engine</div>
  {:else}
    <div class="rows">
      {#each assets as a (a.base)}
        <div class="asset">
          <div class="asset-head">
            <span class="base">{a.base}</span>
            <span class="net mono" style:color={deltaColour(a.net_delta)}>
              {a.net_delta > 0 ? '+' : ''}{a.net_delta}
            </span>
          </div>
          <div class="legs">
            {#each a.legs as leg}
              <div class="leg">
                <span class="venue">{leg.venue}</span>
                <span class="sym">{leg.symbol}</span>
                <span class="leg-val mono" style:color={deltaColour(leg.inventory)}>
                  {leg.inventory > 0 ? '+' : ''}{leg.inventory}
                </span>
              </div>
            {/each}
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .cvp { display: flex; flex-direction: column; gap: var(--s-3); }
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

  .rows { display: flex; flex-direction: column; gap: var(--s-3); }
  .asset {
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
  }
  .asset-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--s-3);
    margin-bottom: var(--s-2);
  }
  .base {
    font-family: var(--font-mono);
    font-weight: 600;
    font-size: var(--fs-md);
    letter-spacing: var(--tracking-tight);
    color: var(--fg-primary);
  }
  .net {
    font-family: var(--font-mono);
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .legs {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .leg {
    display: grid;
    grid-template-columns: 90px 1fr auto;
    gap: var(--s-2);
    font-size: var(--fs-xs);
    align-items: baseline;
    padding-left: var(--s-2);
  }
  .venue { font-family: var(--font-mono); color: var(--fg-secondary); }
  .sym { font-family: var(--font-mono); color: var(--fg-muted); font-size: 10px; }
  .leg-val { font-family: var(--font-mono); font-variant-numeric: tabular-nums; text-align: right; }
  .mono { font-family: var(--font-mono); }
</style>
