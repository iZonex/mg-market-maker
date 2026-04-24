<script>
  /*
   * S1.3/S1.4 — SOR routing decision panel.
   *
   * Polls /api/v1/sor/decisions/recent. For each recent
   * decision, renders the winning leg(s) (which venue(s) the
   * router picked + their cost in bps) and the runner-ups it
   * considered but rejected. Operators use this to answer
   * "why did this order go to Bybit and not Binance today?"
   * without scraping Prometheus or grepping audit.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 4_000

  let decisions = $state([])
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
            + `/details/sor_decisions_recent`
          fetches.push(
            api.getJson(path)
              .then(resp => resp.payload?.decisions || [])
              .catch(() => []),
          )
        }
      }
      const all = (await Promise.all(fetches)).flat()
      all.sort((a, b) => (b.ts_ms ?? 0) - (a.ts_ms ?? 0))
      decisions = all.slice(0, 30)
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

  function fmtTs(ms) {
    if (!ms) return '—'
    return new Date(ms).toLocaleTimeString()
  }

  function sideColour(side) {
    return side === 'buy' ? 'var(--accent)' : 'var(--warn)'
  }
</script>

<div class="sor">
  <div class="toolbar">
    <div class="title">SOR routing decisions</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{decisions.length} decision(s) · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && decisions.length === 0}
    <div class="empty">no SOR decisions recorded yet — engine hasn't routed anything</div>
  {:else}
    <div class="rows">
      {#each decisions as d, i (i)}
        <div class="dec">
          <div class="dec-head">
            <span class="ts mono">{fmtTs(d.ts_ms)}</span>
            <span class="sym mono">{d.symbol}</span>
            <span class="side mono" style:color={sideColour(d.side)}>
              {d.side?.toString?.().toUpperCase?.() || d.side}
            </span>
            <span class="qty mono">
              {d.filled_qty}/{d.target_qty}
              {#if !d.is_complete}<span class="partial">partial</span>{/if}
            </span>
          </div>
          <div class="legs">
            {#each d.winners as w, wi (wi)}
              <div class="leg winner">
                <span class="badge">WIN</span>
                <span class="venue mono">{w.venue}</span>
                <span class="lqty mono">{w.qty}</span>
                <span class="mode mono">{w.is_taker ? 'taker' : 'maker'}</span>
                <span class="cost mono">{Number(w.cost_bps).toFixed(2)} bps</span>
              </div>
            {/each}
            {#each d.considered as c, ci (ci)}
              <div class="leg loser">
                <span class="badge dim">—</span>
                <span class="venue mono">{c.venue}</span>
                <span class="lqty mono">{c.qty}</span>
                <span class="mode mono">{c.is_taker ? 'taker' : 'maker'}</span>
                <span class="cost mono">{Number(c.cost_bps).toFixed(2)} bps</span>
              </div>
            {/each}
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .sor { display: flex; flex-direction: column; gap: var(--s-3); }
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

  .rows { display: flex; flex-direction: column; gap: var(--s-2); max-height: 420px; overflow: auto; }
  .dec {
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
    display: flex; flex-direction: column; gap: var(--s-1);
  }
  .dec-head {
    display: grid;
    grid-template-columns: 80px 1fr 50px 1fr;
    gap: var(--s-2);
    align-items: baseline;
    font-size: var(--fs-xs);
  }
  .ts { color: var(--fg-muted); font-size: 10px; }
  .sym { color: var(--fg-primary); font-weight: 600; }
  .side { font-weight: 700; font-size: 10px; letter-spacing: var(--tracking-label); }
  .qty { color: var(--fg-secondary); font-size: 10px; text-align: right; }
  .partial { color: var(--warn); margin-left: var(--s-1); font-size: 9px; }

  .legs { display: flex; flex-direction: column; gap: 1px; }
  .leg {
    display: grid;
    grid-template-columns: 36px 80px 1fr 60px 70px;
    gap: var(--s-2);
    font-size: var(--fs-2xs);
    padding-left: var(--s-2);
    align-items: baseline;
  }
  .winner { color: var(--fg-primary); }
  .loser { color: var(--fg-muted); }
  .badge {
    font-family: var(--font-mono); font-size: 9px;
    padding: 0 4px; border-radius: var(--r-pill);
    background: var(--accent-dim); color: var(--accent);
    text-align: center;
  }
  .badge.dim { background: transparent; color: var(--fg-muted); }
  .venue { text-transform: uppercase; }
  .lqty { text-align: right; }
  .mode { font-size: 9px; text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .cost { text-align: right; font-variant-numeric: tabular-nums; }</style>
