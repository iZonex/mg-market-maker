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
      // Per-deployment details fan-out: find every deployment
      // running `sym` across the fleet and pull its `venue_inventory`
      // topic. Merge legs — same (venue, product, asset, wallet)
      // key may appear on multiple deployments (same agent
      // reports same spot balance through different bundles).
      const fleet = await api.getJson('/api/v1/fleet')
      const fetches = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if (!d.running) continue
          if (d.symbol !== sym) continue
          const path = `/api/v1/agents/${encodeURIComponent(a.agent_id)}`
            + `/deployments/${encodeURIComponent(d.deployment_id)}`
            + `/details/venue_inventory`
          fetches.push(
            api.getJson(path)
              .then(resp => resp.payload?.legs || [])
              .catch(() => []),
          )
        }
      }
      const all = (await Promise.all(fetches)).flat()
      const byKey = new Map()
      for (const r of all) {
        const key = `${r.venue}:${r.product}:${r.asset}:${r.wallet}`
        const prev = byKey.get(key)
        if (!prev || (r.ts_ms || 0) > (prev.ts_ms || 0)) {
          byKey.set(key, r)
        }
      }
      httpRows = Array.from(byKey.values())
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
