<script>
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'
  import { Button } from '../primitives/index.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let rows = $state([])
  let error = $state('')
  let busy = $state(false)

  async function refresh() {
    try {
      rows = await api.getJson('/api/v1/clients/loss-state')
      error = ''
    } catch (e) {
      error = e.message
    }
  }

  async function resetClient(cid) {
    if (!confirm(`Reset per-client loss circuit for ${cid}?\n\nNote: engine kill switches must be reset separately via Fleet → deployment drilldown → Ops → Reset; this only clears the aggregate breaker.`)) {
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

  function fmtPnl(s) {
    const n = parseFloat(s || 0)
    if (!Number.isFinite(n)) return '—'
    return (n > 0 ? '+' : '') + n.toFixed(2)
  }
  function pnlClass(s) {
    const n = parseFloat(s || 0)
    if (n > 0.005)  return 'pos'
    if (n < -0.005) return 'neg'
    return ''
  }
</script>

{#if error}
  <div class="empty-state">
    <span class="empty-state-icon" style="color: var(--neg)"><Icon name="alert" size={18} /></span>
    <span class="empty-state-title">Failed to load clients</span>
    <span class="empty-state-hint">{error}</span>
  </div>
{:else if rows.length === 0}
  <div class="empty-state">
    <span class="empty-state-icon"><Icon name="shield" size={18} /></span>
    <span class="empty-state-title">No clients registered</span>
    <span class="empty-state-hint">Per-client loss circuits appear once a client is registered. Use the <strong>Client onboarding</strong> panel on the Admin page.</span>
  </div>
{:else}
  <table class="tbl">
    <thead>
      <tr>
        <th>Client</th>
        <th class="right">Daily PnL</th>
        <th class="right">Limit</th>
        <th>Status</th>
        <th class="right"></th>
      </tr>
    </thead>
    <tbody>
      {#each rows as r (r.client_id)}
        <tr class:tripped={r.tripped}>
          <td class="cid">{r.client_id}</td>
          <td class="num-cell right"><span class={pnlClass(r.daily_pnl)}>${fmtPnl(r.daily_pnl)}</span></td>
          <td class="num-cell right">
            {#if r.limit_usd}${r.limit_usd}{:else}—{/if}
          </td>
          <td>
            {#if r.tripped}
              <span class="chip chip-neg">Tripped</span>
            {:else if !r.limit_usd}
              <span class="chip">Unlimited</span>
            {:else}
              <span class="chip chip-pos">OK</span>
            {/if}
          </td>
          <td class="right">
            {#if r.tripped}
              <Button variant="primary" onclick={() => resetClient(r.client_id)} disabled={busy}>
          {#snippet children()}<Icon name="check" size={12} />
                <span>Reset</span>{/snippet}
        </Button>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  .cid {
    font-family: var(--font-mono);
    font-weight: 500;
    color: var(--fg-primary);
  }
  tr.tripped {
    background: rgba(239, 68, 68, 0.06);
  }
  tr.tripped td.cid { color: var(--neg); }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }
</style>
