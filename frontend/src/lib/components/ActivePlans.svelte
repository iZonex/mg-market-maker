<script>
  /*
   * UI-1 — Active execution plans panel.
   *
   * Polls /api/v1/plans/active every REFRESH_MS. Renders one row
   * per Plan.* node with a live progress bar, elapsed time, and
   * aborted flag. `qty_emitted` is absolute — we don't have the
   * target on the API yet (graph config would give it), so the
   * bar width is relative to the largest qty_emitted in the
   * current list. Operators see relative progress; absolute
   * values live in the config panel.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 3_000

  let plans = $state([])
  let error = $state(null)
  let lastFetch = $state(null)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/plans/active')
      plans = (data?.plans ?? []).map(p => ({
        ...p,
        qty_emitted: Number(p.qty_emitted),
      }))
      error = null
      lastFetch = new Date()
    } catch (e) {
      error = e?.message || String(e)
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(t)
  })

  function elapsedFmt(started_ms) {
    if (!started_ms) return '—'
    const dt = Math.max(0, Date.now() - started_ms)
    const secs = Math.floor(dt / 1000)
    if (secs < 60) return `${secs}s`
    if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`
    return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`
  }

  const maxEmit = $derived(
    plans.reduce((m, p) => Math.max(m, Math.abs(p.qty_emitted || 0)), 0.0001)
  )
</script>

<div class="plans">
  <div class="toolbar">
    <div class="title">Active plans</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if lastFetch}
        <span class="stale">{plans.length} plan(s) · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>
  {#if plans.length === 0}
    <div class="empty">no active execution plans</div>
  {:else}
    <div class="rows">
      {#each plans as p (p.node_id)}
        {@const pct = Math.min(100, Math.max(0, (Math.abs(p.qty_emitted) / maxEmit) * 100))}
        <div class="row" class:aborted={p.aborted}>
          <div class="meta-col">
            <div class="kind">{p.kind}</div>
            <div class="sym">{p.symbol}</div>
          </div>
          <div class="bar-col">
            <div class="bar-track">
              <div class="bar-fill" style:width="{pct}%"
                   style:background={p.aborted ? 'var(--danger)' : 'var(--accent)'}></div>
            </div>
            <div class="values">
              <span class="mono">emitted {p.qty_emitted}</span>
              <span class="mono">· elapsed {elapsedFmt(p.started_at_ms)}</span>
              {#if p.aborted}
                <span class="tag abort">aborted</span>
              {/if}
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .plans { display: flex; flex-direction: column; gap: var(--s-2); }
  .toolbar {
    display: flex; align-items: center; justify-content: space-between;
    padding: 0 var(--s-2); font-size: var(--fs-xs);
  }
  .title { font-weight: 600; color: var(--fg-primary); }
  .meta .error { color: var(--danger); }
  .empty { color: var(--fg-muted); font-size: var(--fs-xs); padding: var(--s-4); text-align: center; }
  .rows { display: flex; flex-direction: column; gap: var(--s-2); }
  .row {
    display: grid;
    grid-template-columns: 160px 1fr;
    gap: var(--s-3);
    align-items: center;
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
  }
  .row.aborted { opacity: 0.7; }
  .kind { font-family: var(--font-mono); font-size: var(--fs-xs); color: var(--fg-primary); }
  .sym { font-family: var(--font-mono); font-size: 10px; color: var(--fg-muted); }
  .bar-col { display: flex; flex-direction: column; gap: 4px; }
  .bar-track {
    height: 8px; background: var(--bg-raised); border-radius: var(--r-pill); overflow: hidden;
  }
  .bar-fill { height: 100%; border-radius: var(--r-pill); transition: width var(--dur-base) var(--ease-out); }
  .values { display: flex; gap: var(--s-2); font-size: 10px; color: var(--fg-muted); align-items: center; }
  .mono { font-family: var(--font-mono); }
  .tag.abort {
    color: var(--danger);
    font-weight: 600;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
  }
</style>
