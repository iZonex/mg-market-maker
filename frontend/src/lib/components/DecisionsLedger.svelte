<script>
  /*
   * UI-2 — Decision cost ledger table.
   *
   * Polls /api/v1/decisions/recent every REFRESH_MS, renders
   * per-decision rows with realized vs expected cost deltas.
   * Filterable by symbol + "resolved only" toggle.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth, symbol = null } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 4_000

  let decisions = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let onlyResolved = $state(false)

  async function refresh() {
    const base = '/api/v1/decisions/recent?limit=200'
    const q = symbol ? `${base}&symbol=${encodeURIComponent(symbol)}` : base
    try {
      const data = await api.getJson(q)
      const flat = []
      for (const [sym, rows] of Object.entries(data?.symbols ?? {})) {
        for (const r of rows) {
          // One row per resolved fill; when no fills, emit one placeholder
          // row so operators see the decision exists.
          if (r.resolved && r.resolved.length > 0) {
            for (const f of r.resolved) {
              flat.push({
                symbol: sym,
                id: r.id,
                tick_ms: r.tick_ms,
                side: r.side,
                target_qty: Number(r.target_qty),
                mid: Number(r.mid_at_decision),
                expected_bps: r.expected_cost_bps !== null && r.expected_cost_bps !== undefined
                  ? Number(r.expected_cost_bps)
                  : null,
                fill_ts: f.tick_ms,
                fill_price: Number(f.fill_price),
                fill_qty: Number(f.fill_qty),
                realized_bps: Number(f.realized_cost_bps),
                vs_expected_bps: f.vs_expected_bps !== null && f.vs_expected_bps !== undefined
                  ? Number(f.vs_expected_bps)
                  : null,
                resolved: true,
              })
            }
          } else {
            flat.push({
              symbol: sym,
              id: r.id,
              tick_ms: r.tick_ms,
              side: r.side,
              target_qty: Number(r.target_qty),
              mid: Number(r.mid_at_decision),
              expected_bps: r.expected_cost_bps !== null && r.expected_cost_bps !== undefined
                ? Number(r.expected_cost_bps)
                : null,
              resolved: false,
            })
          }
        }
      }
      flat.sort((a, b) => (b.fill_ts ?? b.tick_ms) - (a.fill_ts ?? a.tick_ms))
      decisions = flat
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

  const filtered = $derived(onlyResolved ? decisions.filter(d => d.resolved) : decisions)

  function fmtBps(v) {
    if (v === null || v === undefined || Number.isNaN(v)) return '—'
    return `${v.toFixed(2)}`
  }
  function bpsColour(v) {
    if (v === null || v === undefined || Number.isNaN(v)) return 'var(--fg-muted)'
    if (v <= -5) return 'var(--accent)'
    if (v >= 5) return 'var(--danger)'
    return 'var(--fg-secondary)'
  }
  function fmtTs(ms) {
    if (!ms) return '—'
    const d = new Date(ms)
    return `${d.toLocaleTimeString()}.${String(d.getMilliseconds()).padStart(3, '0')}`
  }
</script>

<div class="ledger">
  <div class="toolbar">
    <label class="toggle">
      <input type="checkbox" bind:checked={onlyResolved} />
      <span>resolved only</span>
    </label>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if lastFetch}
        <span class="stale">{filtered.length} rows · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>
  {#if filtered.length === 0}
    <div class="empty">no decisions yet</div>
  {:else}
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>time</th>
            <th>sym</th>
            <th>side</th>
            <th class="r">target</th>
            <th class="r">mid</th>
            <th class="r">expected</th>
            <th class="r">realized</th>
            <th class="r">Δ vs exp</th>
          </tr>
        </thead>
        <tbody>
          {#each filtered as d (d.id + (d.fill_ts ?? ''))}
            <tr class:unresolved={!d.resolved}>
              <td class="mono">{fmtTs(d.fill_ts ?? d.tick_ms)}</td>
              <td class="mono">{d.symbol}</td>
              <td class="mono">{d.side}</td>
              <td class="r mono">{d.target_qty}</td>
              <td class="r mono">{d.mid}</td>
              <td class="r mono" style:color={bpsColour(d.expected_bps)}>{fmtBps(d.expected_bps)}</td>
              <td class="r mono" style:color={d.resolved ? bpsColour(d.realized_bps) : 'var(--fg-muted)'}>
                {d.resolved ? fmtBps(d.realized_bps) : 'pending'}
              </td>
              <td class="r mono" style:color={d.resolved ? bpsColour(d.vs_expected_bps) : 'var(--fg-muted)'}>
                {d.resolved ? fmtBps(d.vs_expected_bps) : '—'}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .ledger { display: flex; flex-direction: column; gap: var(--s-2); height: 100%; }
  .toolbar { display: flex; align-items: center; justify-content: space-between; padding: 0 var(--s-2); font-size: var(--fs-xs); }
  .toggle { display: inline-flex; align-items: center; gap: var(--s-2); color: var(--fg-secondary); }
  .meta .error { color: var(--danger); }
  .empty { color: var(--fg-muted); font-size: var(--fs-xs); padding: var(--s-4); text-align: center; }
  .table-wrap { overflow-y: auto; max-height: calc(100% - 32px); }
  table { width: 100%; border-collapse: collapse; font-size: var(--fs-xs); }
  thead { position: sticky; top: 0; background: var(--bg-raised); z-index: 1; }
  th, td { padding: var(--s-1) var(--s-2); text-align: left; white-space: nowrap; border-bottom: 1px solid var(--border-subtle); }
  th { color: var(--fg-muted); font-weight: 500; letter-spacing: var(--tracking-label); text-transform: uppercase; font-size: 10px; }
  .r { text-align: right; }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  tr.unresolved td { opacity: 0.6; }
</style>
