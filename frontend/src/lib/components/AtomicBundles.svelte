<script>
  /*
   * S2.2 — inflight atomic bundle monitor.
   *
   * Polls /api/v1/atomic-bundles/inflight every 3s. One card
   * per bundle id: maker venue:symbol:side + hedge ditto, ack
   * checkmarks per leg. Missing-side entries show "—" (an
   * originator mid-dispatch can see its own leg registered
   * before the hedge is acked by the remote engine).
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 3_000

  let bundles = $state([])
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
            + `/details/atomic_bundles_inflight`
          fetches.push(
            api.getJson(path)
              .then(resp => resp.payload?.bundles || [])
              .catch(() => []),
          )
        }
      }
      // Dedup by bundle_id — the same bundle's legs may be
      // registered on two different deployments (maker on one
      // agent, hedge on another). Prefer whichever copy has
      // more fields populated.
      const all = (await Promise.all(fetches)).flat()
      const byId = new Map()
      for (const b of all) {
        const existing = byId.get(b.bundle_id)
        if (!existing) {
          byId.set(b.bundle_id, b)
          continue
        }
        // Merge leg by leg — whichever side has a non-null entry
        // wins; if both do, ack=true wins over ack=false.
        const merged = { bundle_id: b.bundle_id, maker: null, hedge: null }
        for (const k of ['maker', 'hedge']) {
          const a = existing[k]
          const c = b[k]
          if (!a && !c) continue
          if (a && !c) merged[k] = a
          else if (!a && c) merged[k] = c
          else merged[k] = a.acked ? a : c
        }
        byId.set(b.bundle_id, merged)
      }
      bundles = Array.from(byId.values())
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

<div class="ab">
  <div class="toolbar">
    <div class="title">Atomic bundles inflight</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{bundles.length} inflight · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && bundles.length === 0}
    <div class="empty">no atomic bundles in flight — engine is quiet or all pairs have settled</div>
  {:else}
    <div class="rows">
      {#each bundles as b (b.bundle_id)}
        <div class="bundle">
          <div class="bundle-head">
            <span class="id mono">{b.bundle_id}</span>
          </div>
          <div class="legs">
            <div class="leg" class:acked={b.maker?.acked}>
              <span class="role">MAKER</span>
              {#if b.maker}
                <span class="venue mono">{b.maker.venue}</span>
                <span class="sym mono">{b.maker.symbol}</span>
                <span class="side mono" class:buy={b.maker.side === 'buy'} class:sell={b.maker.side === 'sell'}>
                  {b.maker.side}
                </span>
                <span class="px mono">@ {b.maker.price}</span>
                <span class="ack">{b.maker.acked ? '✓ acked' : '… pending'}</span>
              {:else}
                <span class="missing">— not registered</span>
              {/if}
            </div>
            <div class="leg" class:acked={b.hedge?.acked}>
              <span class="role">HEDGE</span>
              {#if b.hedge}
                <span class="venue mono">{b.hedge.venue}</span>
                <span class="sym mono">{b.hedge.symbol}</span>
                <span class="side mono" class:buy={b.hedge.side === 'buy'} class:sell={b.hedge.side === 'sell'}>
                  {b.hedge.side}
                </span>
                <span class="px mono">@ {b.hedge.price}</span>
                <span class="ack">{b.hedge.acked ? '✓ acked' : '… pending'}</span>
              {:else}
                <span class="missing">— not registered</span>
              {/if}
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .ab { display: flex; flex-direction: column; gap: var(--s-3); }
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

  .rows { display: flex; flex-direction: column; gap: var(--s-2); max-height: 360px; overflow: auto; }
  .bundle {
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
    display: flex; flex-direction: column; gap: var(--s-1);
  }
  .bundle-head { padding-bottom: 2px; border-bottom: 1px dashed var(--border-subtle); }
  .id { font-family: var(--font-mono); font-size: 10px; color: var(--fg-secondary); }
  .legs { display: flex; flex-direction: column; gap: 2px; }
  .leg {
    display: grid;
    grid-template-columns: 60px 90px 1fr 60px 1fr 80px;
    gap: var(--s-2);
    font-size: var(--fs-xs);
    align-items: baseline;
    padding: 4px var(--s-2);
    border-radius: var(--r-sm);
    background: var(--bg-raised);
  }
  .leg.acked { background: rgba(52, 211, 153, 0.08); }
  .role { font-size: 9px; letter-spacing: var(--tracking-label); color: var(--fg-muted); font-weight: 600; }
  .venue { text-transform: uppercase; font-weight: 600; color: var(--fg-primary); }
  .sym { color: var(--fg-secondary); }
  .side { font-size: 10px; text-transform: uppercase; }
  .side.buy { color: var(--accent); }
  .side.sell { color: var(--warn); }
  .px { color: var(--fg-secondary); font-size: 11px; }
  .ack { font-size: 10px; text-align: right; color: var(--fg-muted); }
  .leg.acked .ack { color: var(--accent); font-weight: 600; }
  .missing { grid-column: 2 / -1; color: var(--fg-muted); font-style: italic; font-size: 11px; }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
</style>
