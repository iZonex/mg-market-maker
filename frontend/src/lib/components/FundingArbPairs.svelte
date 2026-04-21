<script>
  /*
   * S5.2 — Funding-arb pair monitor.
   *
   * Polls /api/v1/funding-arb/pairs. Each row shows a pair
   * (primary|hedge) with its per-event counters + the latest
   * event kind and reason. The `pair_break_uncompensated`
   * counter shows in red so operators spot unhedged breaks at
   * a glance.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 5_000

  let pairs = $state([])
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
            + `/details/funding_arb_pairs`
          fetches.push(
            api.getJson(path)
              .then(resp => resp.payload?.pairs || [])
              .catch(() => []),
          )
        }
      }
      // Dedup by pair key — same pair may appear on two
      // deployments (primary on one agent, hedge on another).
      // Max counters across copies and keep the latest-event
      // record.
      const all = (await Promise.all(fetches)).flat()
      const byPair = new Map()
      for (const p of all) {
        const key = p.pair
        const prev = byPair.get(key)
        if (!prev) {
          byPair.set(key, { ...p })
          continue
        }
        const m = { ...prev }
        for (const k of ['entered', 'exited', 'hold', 'taker_rejected', 'pair_break', 'pair_break_uncompensated', 'input_unavailable']) {
          m[k] = Math.max(prev[k] || 0, p[k] || 0)
        }
        if ((p.last_event_at_ms || 0) > (prev.last_event_at_ms || 0)) {
          m.last_event_at_ms = p.last_event_at_ms
          m.last_event = p.last_event
          m.last_reason = p.last_reason
        }
        byPair.set(key, m)
      }
      pairs = Array.from(byPair.values())
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

  function eventColour(tag) {
    if (tag === 'pair_break') return 'var(--danger)'
    if (tag === 'taker_rejected') return 'var(--warn)'
    if (tag === 'entered' || tag === 'exited') return 'var(--accent)'
    return 'var(--fg-muted)'
  }
</script>

<div class="fpairs">
  <div class="toolbar">
    <div class="title">Funding-arb pairs</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{pairs.length} pair(s) · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && pairs.length === 0}
    <div class="empty">no funding-arb driver active — strategy not configured</div>
  {:else}
    <div class="rows">
      {#each pairs as p, i (i)}
        <div class="pair">
          <div class="head">
            <span class="name mono">{p.pair}</span>
            <span class="last mono" style:color={eventColour(p.last_event)}>
              {p.last_event || 'idle'}
            </span>
            <span class="ts mono">{fmtTs(p.last_event_at_ms)}</span>
          </div>
          <div class="counts">
            <span class="count">enter <b class="mono">{p.entered}</b></span>
            <span class="count">exit <b class="mono">{p.exited}</b></span>
            <span class="count">hold <b class="mono">{p.hold}</b></span>
            <span class="count">rej <b class="mono">{p.taker_rejected}</b></span>
            <span class="count" class:danger={p.pair_break_uncompensated > 0}>
              break <b class="mono">{p.pair_break}</b>
              {#if p.pair_break_uncompensated > 0}
                <span class="uc">({p.pair_break_uncompensated} uncomp)</span>
              {/if}
            </span>
            <span class="count">in/a <b class="mono">{p.input_unavailable}</b></span>
          </div>
          {#if p.last_reason}
            <div class="reason">{p.last_reason}</div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .fpairs { display: flex; flex-direction: column; gap: var(--s-3); }
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
  .pair {
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
    display: flex; flex-direction: column; gap: var(--s-1);
  }
  .head {
    display: grid;
    grid-template-columns: 2fr 1fr 1fr;
    gap: var(--s-2);
    align-items: baseline;
    font-size: var(--fs-xs);
  }
  .name { color: var(--fg-primary); font-weight: 600; }
  .last { text-transform: uppercase; font-size: 10px; text-align: right; }
  .ts { color: var(--fg-muted); text-align: right; font-size: 10px; }
  .counts {
    display: flex; flex-wrap: wrap; gap: var(--s-3);
    font-size: var(--fs-2xs); color: var(--fg-muted);
  }
  .count b { color: var(--fg-primary); font-weight: 600; margin-left: 2px; }
  .count.danger { color: var(--danger); }
  .count.danger b { color: var(--danger); }
  .uc { color: var(--danger); margin-left: 4px; font-size: 9px; }
  .reason { color: var(--fg-secondary); font-size: var(--fs-2xs); }
  .mono { font-family: var(--font-mono); }
</style>
