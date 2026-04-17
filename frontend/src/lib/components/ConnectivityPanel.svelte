<script>
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  let rows = $state([])
  let error = $state('')
  let lastUpdated = $state(0)

  async function refresh() {
    try {
      rows = await api.getJson('/api/v1/venues/status')
      error = ''
      lastUpdated = Date.now()
    } catch (e) {
      error = e.message
    }
  }

  // Poll every 5 seconds. Cheap enough for a ~10-row table and
  // gives a "live feel" without the socket plumbing for a
  // rarely-changing table.
  $effect(() => {
    refresh()
    const id = setInterval(refresh, 5000)
    return () => clearInterval(id)
  })

  function agoSec() {
    if (!lastUpdated) return '-'
    return `${Math.round((Date.now() - lastUpdated) / 1000)}s ago`
  }
</script>

<div>
  <h3>Connectivity <span class="age">(updated {agoSec()})</span></h3>

  {#if error}
    <div class="error">error: {error}</div>
  {/if}

  {#if rows.length === 0 && !error}
    <div class="empty">no symbols tracked</div>
  {:else}
    <table>
      <thead>
        <tr>
          <th>Symbol</th>
          <th>Mid</th>
          <th>Live</th>
          <th>Fills</th>
          <th>SLA %</th>
          <th>Kill</th>
          <th>State</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as r}
          <tr>
            <td class="sym">{r.symbol}</td>
            <td>{r.has_data ? parseFloat(r.mid_price).toFixed(4) : '-'}</td>
            <td>{r.live_orders}</td>
            <td>{r.total_fills}</td>
            <td>{parseFloat(r.sla_uptime_pct).toFixed(1)}</td>
            <td>
              <span class="kill-badge level-{r.kill_level}">L{r.kill_level}</span>
            </td>
            <td>
              {#if !r.has_data}
                <span class="state bad">NO DATA</span>
              {:else if r.quoting_halted}
                <span class="state warn">HALTED</span>
              {:else}
                <span class="state ok">OK</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  h3 {
    font-size: 12px; color: #8b949e; margin-bottom: 12px;
    text-transform: uppercase; letter-spacing: 0.5px;
  }
  .age { font-size: 10px; color: #484f58; text-transform: none; letter-spacing: 0; }
  .error { color: #f85149; font-size: 11px; padding: 4px; }
  .empty { color: #8b949e; font-size: 11px; padding: 8px 0; }
  table { width: 100%; border-collapse: collapse; font-size: 11px; }
  th {
    text-align: left; color: #8b949e; font-weight: 500;
    padding: 4px 6px; border-bottom: 1px solid #21262d;
    font-size: 10px; text-transform: uppercase;
  }
  td { padding: 5px 6px; border-bottom: 1px solid #1b1f27; }
  td.sym { font-weight: 600; color: #e1e4e8; }
  .kill-badge {
    padding: 1px 5px; border-radius: 2px; font-size: 10px;
    font-weight: 700;
  }
  .kill-badge.level-0 { background: #238636; color: #fff; }
  .kill-badge.level-1 { background: #d29922; color: #000; }
  .kill-badge.level-2 { background: #bf8700; color: #fff; }
  .kill-badge.level-3 { background: #da3633; color: #fff; }
  .kill-badge.level-4, .kill-badge.level-5 { background: #f85149; color: #fff; }
  .state { font-size: 10px; font-weight: 700; padding: 1px 5px; border-radius: 2px; }
  .state.ok { color: #3fb950; }
  .state.warn { color: #d29922; }
  .state.bad { color: #f85149; }
</style>
