<script>
  /*
   * S5.1 — Rebalance recommendations panel.
   *
   * Polls /api/v1/rebalance/recommendations. Dashboard groups
   * VenueBalanceSnapshot rows by (venue, asset), runs the
   * rebalancer against the configured thresholds, and returns
   * advisory transfer rows. Empty result means everything is
   * balanced OR the [rebalancer] config section is absent.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 15_000

  let recs = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/rebalance/recommendations')
      recs = data?.recommendations ?? []
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
</script>

<div class="rebal">
  <div class="toolbar">
    <div class="title">Rebalance recommendations</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{recs.length} advisory · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && recs.length === 0}
    <div class="empty">balances within thresholds — no transfers recommended</div>
  {:else}
    <div class="rows">
      {#each recs as r, i (i)}
        <div class="rec">
          <div class="head">
            <span class="asset mono">{r.asset}</span>
            <span class="qty mono">{r.qty}</span>
            <span class="route mono">{r.from_venue}<span class="arrow"> → </span>{r.to_venue}</span>
          </div>
          <div class="reason">{r.reason}</div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .rebal { display: flex; flex-direction: column; gap: var(--s-3); }
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

  .rows { display: flex; flex-direction: column; gap: var(--s-2); }
  .rec {
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
    display: flex; flex-direction: column; gap: var(--s-1);
  }
  .head {
    display: grid;
    grid-template-columns: 80px 1fr 2fr;
    gap: var(--s-2);
    align-items: baseline;
    font-size: var(--fs-xs);
  }
  .asset { color: var(--fg-primary); font-weight: 600; text-transform: uppercase; }
  .qty { color: var(--fg-secondary); text-align: right; font-variant-numeric: tabular-nums; }
  .route { color: var(--fg-primary); text-transform: uppercase; text-align: right; }
  .arrow { color: var(--accent); font-weight: 700; }
  .reason { color: var(--fg-muted); font-size: var(--fs-2xs); }
  .mono { font-family: var(--font-mono); }
</style>
