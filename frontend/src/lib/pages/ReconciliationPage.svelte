<script>
  /*
   * Wave C1 — fleet-wide order + balance reconciliation view.
   *
   * Backend: GET /api/v1/reconciliation/fleet — fans out the
   * `reconciliation_snapshot` details topic to every running
   * deployment and returns a flat list. Rows with drift (ghost
   * orders, phantom orders, balance mismatches, or a failed
   * order fetch) sort to the top.
   *
   * Page layout:
   *   - Summary card: cycle counts, drift rollup, fetch failures.
   *   - Drift table: every row with non-empty drift fields.
   *   - Clean table: every row with no drift (folded by default).
   */
  import Card from '../components/Card.svelte'
  import { createApiClient } from '../api.svelte.js'
  import { Button } from '../primitives/index.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 5000

  let rows = $state([])
  let error = $state(null)
  let loading = $state(true)
  let lastFetch = $state(null)
  let showClean = $state(null)

  async function refresh() {
    try {
      const r = await api.getJson('/api/v1/reconciliation/fleet')
      rows = Array.isArray(r) ? r : []
      error = null
      lastFetch = new Date()
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const iv = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(iv)
  })

  const driftRows = $derived(rows.filter(r => r.has_drift))
  const cleanRows = $derived(rows.filter(r => !r.has_drift))
  // Auto-expand clean rows when the fleet is small enough to
  // skim in one glance. Operator can still collapse manually.
  const cleanExpanded = $derived(
    showClean !== null ? showClean : cleanRows.length > 0 && cleanRows.length <= 20
  )

  const totals = $derived.by(() => {
    let ghosts = 0, phantoms = 0, balMm = 0, fetchFails = 0
    for (const r of rows) {
      ghosts += (r.ghost_orders || []).length
      phantoms += (r.phantom_orders || []).length
      balMm += (r.balance_mismatches || []).length
      if (r.orders_fetch_failed) fetchFails++
    }
    return { ghosts, phantoms, balMm, fetchFails }
  })

  function fmtAge(ms) {
    if (!ms) return '—'
    const delta = Date.now() - ms
    if (delta < 60_000) return `${Math.round(delta / 1000)}s ago`
    if (delta < 3600_000) return `${Math.round(delta / 60_000)}m ago`
    return `${Math.round(delta / 3600_000)}h ago`
  }
</script>

<div class="page scroll">
  <div class="grid">
    <Card title="Fleet reconciliation" subtitle={lastFetch ? `${rows.length} row(s) · refreshed ${lastFetch.toLocaleTimeString()}` : 'loading…'} span={3}>
      {#snippet children()}
        {#if error}
          <div class="error">error: {error}</div>
        {/if}

        <div class="totals">
          <div class="tot-cell">
            <span class="tot-k">deployments</span>
            <span class="tot-v mono">{rows.length}</span>
          </div>
          <div class="tot-cell" class:alert={totals.ghosts > 0}>
            <span class="tot-k">ghost orders</span>
            <span class="tot-v mono">{totals.ghosts}</span>
          </div>
          <div class="tot-cell" class:alert={totals.phantoms > 0}>
            <span class="tot-k">phantom orders</span>
            <span class="tot-v mono">{totals.phantoms}</span>
          </div>
          <div class="tot-cell" class:alert={totals.balMm > 0}>
            <span class="tot-k">balance mismatches</span>
            <span class="tot-v mono">{totals.balMm}</span>
          </div>
          <div class="tot-cell" class:alert={totals.fetchFails > 0}>
            <span class="tot-k">fetch failures</span>
            <span class="tot-v mono">{totals.fetchFails}</span>
          </div>
          <div class="tot-cell" class:alert={driftRows.length > 0}>
            <span class="tot-k">rows with drift</span>
            <span class="tot-v mono">{driftRows.length}</span>
          </div>
        </div>

        <div class="terms">
          <span class="term">
            <span class="term-k">ghost</span>
            <span class="term-v">tracked locally, absent on venue</span>
          </span>
          <span class="term">
            <span class="term-k">phantom</span>
            <span class="term-v">live on venue, not tracked locally</span>
          </span>
          <span class="term">
            <span class="term-k">fetch fail</span>
            <span class="term-v">get_open_orders errored — order reconciliation skipped this cycle</span>
          </span>
        </div>
      {/snippet}
    </Card>

    <Card
      title={`Rows with drift (${driftRows.length})`}
      subtitle="cycle outcomes requiring operator attention"
      span={3}
    >
      {#snippet children()}
        {#if loading}
          <div class="muted">loading…</div>
        {:else if driftRows.length === 0}
          <div class="empty">
            <strong>Clean</strong> — no drift detected across any running deployment.
          </div>
        {:else}
          <table class="rec-table">
            <thead>
              <tr>
                <th>agent</th>
                <th>deployment</th>
                <th>symbol</th>
                <th class="num">cycle</th>
                <th class="num">int / venue</th>
                <th>ghosts</th>
                <th>phantoms</th>
                <th>balance Δ</th>
                <th>last cycle</th>
              </tr>
            </thead>
            <tbody>
              {#each driftRows as r (`${r.agent_id}/${r.deployment_id}`)}
                <tr class:warn={r.orders_fetch_failed}>
                  <td class="mono">{r.agent_id}</td>
                  <td class="mono">{r.deployment_id}</td>
                  <td class="mono">{r.symbol}</td>
                  <td class="num mono">{r.cycle}</td>
                  <td class="num mono">{r.internal_orders} / {r.venue_orders}</td>
                  <td>
                    {#if r.ghost_orders?.length > 0}
                      <span class="chip tone-danger" title={r.ghost_orders.join(', ')}>
                        {r.ghost_orders.length}
                      </span>
                    {:else}—{/if}
                  </td>
                  <td>
                    {#if r.phantom_orders?.length > 0}
                      <span class="chip tone-warn" title={r.phantom_orders.join(', ')}>
                        {r.phantom_orders.length}
                      </span>
                    {:else}—{/if}
                  </td>
                  <td>
                    {#if r.balance_mismatches?.length > 0}
                      <span class="chip tone-warn" title={r.balance_mismatches.map(b => `${b.asset}: ${b.internal} vs ${b.exchange}`).join('\n')}>
                        {r.balance_mismatches.length}
                      </span>
                    {:else if r.orders_fetch_failed}
                      <span class="chip tone-danger">fetch fail</span>
                    {:else}—{/if}
                  </td>
                  <td class="mono">{fmtAge(r.last_cycle_ms)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/snippet}
    </Card>

    <Card
      title={`Clean rows (${cleanRows.length})`}
      subtitle="cycles with no drift detected"
      span={3}
    >
      {#snippet children()}
        {#if cleanRows.length === 0}
          <div class="muted">no clean rows</div>
        {:else if !cleanExpanded}
          <Button variant="ghost" onclick={() => (showClean = true)}>
          {#snippet children()}Show {cleanRows.length} clean row(s){/snippet}
        </Button>
        {:else}
          <div class="header-row">
            <span class="muted">{cleanRows.length} deployment(s) reconciled clean</span>
            <Button variant="ghost" size="sm" onclick={() => (showClean = false)}>
          {#snippet children()}Hide{/snippet}
        </Button>
          </div>
          <table class="rec-table">
            <thead>
              <tr>
                <th>agent</th>
                <th>deployment</th>
                <th>symbol</th>
                <th class="num">cycle</th>
                <th class="num">int / venue</th>
                <th>last cycle</th>
              </tr>
            </thead>
            <tbody>
              {#each cleanRows as r (`${r.agent_id}/${r.deployment_id}`)}
                <tr>
                  <td class="mono">{r.agent_id}</td>
                  <td class="mono">{r.deployment_id}</td>
                  <td class="mono">{r.symbol}</td>
                  <td class="num mono">{r.cycle}</td>
                  <td class="num mono">{r.internal_orders} / {r.venue_orders}</td>
                  <td class="mono">{fmtAge(r.last_cycle_ms)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-4); }
  .scroll { overflow-y: auto; }
  .grid { display: grid; grid-template-columns: 1fr; gap: var(--s-3); }
  .error { color: var(--neg); font-size: var(--fs-sm); margin-bottom: var(--s-2); }  .empty {
    padding: var(--s-3); color: var(--fg-muted);
    font-size: var(--fs-sm); text-align: center;
    background: color-mix(in srgb, var(--ok) 8%, transparent);
    border-radius: var(--r-sm);
  }

  .totals {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
    gap: var(--s-2);
    margin-bottom: var(--s-3);
  }
  .tot-cell {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2); background: var(--bg-raised);
    border-radius: var(--r-sm);
  }
  .tot-cell.alert { background: color-mix(in srgb, var(--danger) 15%, transparent); }
  .tot-k { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .tot-v { font-size: var(--fs-lg); color: var(--fg-primary); font-weight: 500; }

  .terms { display: flex; flex-wrap: wrap; gap: var(--s-3); font-size: 11px; color: var(--fg-secondary); }
  .term-k { color: var(--fg-primary); font-weight: 500; }
  .term-v { color: var(--fg-muted); }

  .rec-table { width: 100%; border-collapse: collapse; }
  .rec-table th, .rec-table td {
    padding: var(--s-2);
    font-size: var(--fs-xs);
    text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .rec-table th {
    color: var(--fg-muted); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-size: 10px;
  }
  .rec-table tr.warn { background: color-mix(in srgb, var(--warn) 6%, transparent); }
  .num { text-align: right; }
  .header-row {
    display: flex; align-items: center; justify-content: space-between;
    margin-bottom: var(--s-2);
  }
</style>
