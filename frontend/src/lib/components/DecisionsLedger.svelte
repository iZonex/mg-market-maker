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
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 4_000

  const MAX_ROWS = 200

  let decisions = $state([])
  let totalBeforeSlice = $state(0)
  let error = $state(null)
  let lastFetch = $state(null)
  let onlyResolved = $state(false)
  let loading = $state(true)

  // Decisions live in each engine's DecisionLedger. The engine
  // mirrors its recent-N snapshot into a shared process-global
  // store on every publish tick; agents expose the snapshot
  // over the per-deployment details endpoint. We fan out across
  // the fleet and flatten results here.
  async function refresh() {
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const fetches = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if (!d.running) continue
          if (symbol && d.symbol !== symbol) continue
          const path = `/api/v1/agents/${encodeURIComponent(a.agent_id)}`
            + `/deployments/${encodeURIComponent(d.deployment_id)}`
            + `/details/decisions_recent`
          fetches.push(
            api.getJson(path)
              .then(resp => ((resp.payload?.decisions || []).map(r => ({ row: r, sym: d.symbol }))))
              .catch(() => []),
          )
        }
      }
      const all = (await Promise.all(fetches)).flat()
      const flat = []
      for (const { row: r, sym } of all) {
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
      flat.sort((a, b) => (b.fill_ts ?? b.tick_ms) - (a.fill_ts ?? a.tick_ms))
      totalBeforeSlice = flat.length
      decisions = flat.slice(0, MAX_ROWS)
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
  {#if loading}
    <div class="empty"><span class="spinner" aria-hidden="true"></span>loading decisions…</div>
  {:else if filtered.length === 0}
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
    {#if totalBeforeSlice > MAX_ROWS}
      <div class="truncated">
        {totalBeforeSlice - MAX_ROWS} older row(s) truncated · per-deployment drilldown shows the full ledger
      </div>
    {/if}
  {/if}
</div>

<style>
  .ledger { display: flex; flex-direction: column; gap: var(--s-2); height: 100%; }
  .toolbar { display: flex; align-items: center; justify-content: space-between; padding: 0 var(--s-2); font-size: var(--fs-xs); }
  .toggle { display: inline-flex; align-items: center; gap: var(--s-2); color: var(--fg-secondary); }
  .meta .error { color: var(--danger); }
  .empty {
    color: var(--fg-muted);
    font-size: var(--fs-xs);
    padding: var(--s-4);
    text-align: center;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--s-2);
  }
  .spinner {
    display: inline-block;
    width: 12px; height: 12px;
    border: 2px solid var(--border-subtle);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
  .table-wrap { overflow-y: auto; max-height: calc(100% - 32px); }
  table { width: 100%; border-collapse: collapse; font-size: var(--fs-xs); }
  thead { position: sticky; top: 0; background: var(--bg-raised); z-index: 1; }
  th, td { padding: var(--s-1) var(--s-2); text-align: left; white-space: nowrap; border-bottom: 1px solid var(--border-subtle); }
  th { color: var(--fg-muted); font-weight: 500; letter-spacing: var(--tracking-label); text-transform: uppercase; font-size: 10px; }
  .r { text-align: right; }  tr.unresolved td { opacity: 0.6; }
  .truncated {
    padding: var(--s-1) var(--s-2);
    font-size: 10px;
    color: var(--fg-muted);
    font-style: italic;
    text-align: center;
    border-top: 1px dashed var(--border-subtle);
  }
</style>
