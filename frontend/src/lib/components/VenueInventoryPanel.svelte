<script>
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { data, auth } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
  const api = createApiClient(auth)

  let httpRows = $state([])
  let lastError = $state('')
  let loading = $state(false)

  async function refresh() {
    if (!sym) return
    loading = true
    try {
      httpRows = await api.getJson(`/api/v1/inventory/venues/${encodeURIComponent(sym)}`)
      lastError = ''
    } catch (e) {
      lastError = String(e?.message || e)
    } finally {
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 15000)
    return () => clearInterval(id)
  })

  const wsRows = $derived(s.venueBalances?.[sym] || [])
  const rows = $derived(wsRows.length > 0 ? wsRows : httpRows)

  const groups = $derived.by(() => {
    const m = new Map()
    for (const r of rows) {
      const key = `${r.venue}:${r.product}`
      if (!m.has(key)) m.set(key, [])
      m.get(key).push(r)
    }
    return Array.from(m.entries())
  })

  function fmtAmt(n) {
    const f = parseFloat(n || 0)
    if (!Number.isFinite(f)) return '—'
    if (Math.abs(f) < 1e-9) return '0'
    if (Math.abs(f) >= 1000) return f.toLocaleString('en-US', { maximumFractionDigits: 2 })
    return f.toFixed(6)
  }
</script>

{#if lastError && rows.length === 0}
  <div class="empty-state">
    <span class="empty-state-icon" style="color: var(--neg)"><Icon name="alert" size={18} /></span>
    <span class="empty-state-title">Failed to load balances</span>
    <span class="empty-state-hint">{lastError}</span>
  </div>
{:else if loading && rows.length === 0}
  <div class="empty-state">
    <span class="empty-state-icon"><Icon name="clock" size={18} /></span>
    <span class="empty-state-title">Loading…</span>
  </div>
{:else if rows.length === 0}
  <div class="empty-state">
    <span class="empty-state-icon"><Icon name="emptyList" size={18} /></span>
    <span class="empty-state-title">No balance snapshots yet</span>
    <span class="empty-state-hint">Engine publishes per-venue balances on every refresh tick.</span>
  </div>
{:else}
  <div class="venues">
    {#each groups as [venueKey, entries] (venueKey)}
      <section class="group">
        <header class="group-head">
          <span class="venue-name">{venueKey.split(':')[0]}</span>
          <span class="venue-sep">·</span>
          <span class="venue-product">{venueKey.split(':')[1]}</span>
        </header>
        <table class="tbl">
          <thead>
            <tr>
              <th>Asset</th>
              <th>Wallet</th>
              <th class="right">Total</th>
              <th class="right">Available</th>
              <th class="right">Locked</th>
            </tr>
          </thead>
          <tbody>
            {#each entries as e}
              <tr>
                <td class="asset">{e.asset}</td>
                <td class="wallet">{e.wallet}</td>
                <td class="num-cell right">{fmtAmt(e.total)}</td>
                <td class="num-cell right">{fmtAmt(e.available)}</td>
                <td class="num-cell right" class:warn={parseFloat(e.locked) > 0}>{fmtAmt(e.locked)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>
    {/each}
  </div>
{/if}

<style>
  .venues {
    display: flex;
    flex-direction: column;
    gap: var(--s-5);
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  .group-head {
    display: flex;
    align-items: center;
    gap: var(--s-1);
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    padding: 0 var(--s-1);
  }
  .venue-name { color: var(--accent); font-weight: 700; }
  .venue-sep  { color: var(--fg-faint); }
  .venue-product { font-weight: 600; color: var(--fg-secondary); }

  .asset {
    font-family: var(--font-mono);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: 0.02em;
  }
  .wallet {
    color: var(--fg-muted);
    font-size: var(--fs-xs);
    text-transform: lowercase;
  }
  .warn { color: var(--warn); }
</style>
