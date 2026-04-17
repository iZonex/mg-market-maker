<script>
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  let rows = $state([])
  let error = $state('')
  let lastUpdated = $state(0)
  let busy = $state(false)

  async function refresh() {
    try {
      rows = await api.getJson('/api/v1/clients/loss-state')
      error = ''
      lastUpdated = Date.now()
    } catch (e) {
      error = e.message
    }
  }

  async function resetClient(cid) {
    if (!confirm(`Reset per-client loss circuit for ${cid}?\n\nNOTE: each engine's kill switch must be reset separately via /api/v1/ops/reset/{symbol} — this only clears the aggregate breaker.`)) {
      return
    }
    busy = true
    try {
      await api.postJson(`/api/v1/ops/client-reset/${encodeURIComponent(cid)}`, {})
      await refresh()
    } catch (e) {
      error = e.message
    } finally {
      busy = false
    }
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 5000)
    return () => clearInterval(id)
  })

  function formatPnl(s) {
    const n = parseFloat(s || 0)
    return n.toFixed(2)
  }

  function pnlClass(s) {
    const n = parseFloat(s || 0)
    if (n > 0) return 'pos'
    if (n < 0) return 'neg'
    return ''
  }
</script>

<div>
  <h3>
    Per-Client Loss Circuit
    <span class="count">({rows.length})</span>
  </h3>

  {#if error}
    <div class="error">error: {error}</div>
  {/if}

  {#if rows.length === 0 && !error}
    <div class="empty">no clients registered</div>
  {:else}
    <table>
      <thead>
        <tr>
          <th>Client</th>
          <th class="num">Daily PnL</th>
          <th class="num">Limit</th>
          <th>Status</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each rows as r (r.client_id)}
          <tr class:tripped={r.tripped}>
            <td class="cid">{r.client_id}</td>
            <td class="num {pnlClass(r.daily_pnl)}">{formatPnl(r.daily_pnl)}</td>
            <td class="num">
              {#if r.limit_usd}-{r.limit_usd}{:else}—{/if}
            </td>
            <td>
              {#if r.tripped}
                <span class="badge bad">TRIPPED</span>
              {:else if !r.limit_usd}
                <span class="badge dim">UNLIMITED</span>
              {:else}
                <span class="badge ok">OK</span>
              {/if}
            </td>
            <td>
              {#if r.tripped}
                <button
                  class="btn-reset"
                  onclick={() => resetClient(r.client_id)}
                  disabled={busy}
                >reset</button>
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
    display: flex; align-items: center; gap: 8px;
  }
  .count { font-size: 10px; color: #484f58; }
  .error { color: #f85149; font-size: 11px; padding: 4px; }
  .empty { color: #8b949e; font-size: 11px; padding: 8px 0; }
  table { width: 100%; border-collapse: collapse; font-size: 11px; }
  th {
    text-align: left; color: #8b949e; font-weight: 500;
    padding: 4px 6px; border-bottom: 1px solid #21262d;
    font-size: 10px; text-transform: uppercase;
  }
  th.num, td.num { text-align: right; font-variant-numeric: tabular-nums; }
  td { padding: 6px; border-bottom: 1px solid #1b1f27; }
  tr.tripped { background: rgba(248, 81, 73, 0.08); }
  td.cid { font-weight: 600; color: #e1e4e8; }
  .pos { color: #3fb950; }
  .neg { color: #f85149; }
  .badge {
    padding: 2px 6px; border-radius: 3px; font-size: 10px;
    font-weight: 700; letter-spacing: 0.5px;
  }
  .badge.ok { background: #238636; color: #fff; }
  .badge.bad { background: #f85149; color: #fff; }
  .badge.dim { background: #30363d; color: #8b949e; }
  .btn-reset {
    background: none; border: 1px solid #30363d; color: #8b949e;
    padding: 2px 10px; border-radius: 3px; cursor: pointer;
    font-family: inherit; font-size: 10px;
  }
  .btn-reset:hover { border-color: #3fb950; color: #3fb950; }
  .btn-reset:disabled { opacity: 0.4; cursor: not-allowed; }
</style>
