<script>
  /*
   * 23-UX-3 — per-leg PnL attribution table.
   *
   * Answers "when total PnL dropped $100, which leg caused it?"
   * One row per (venue, symbol, product) with spread / inventory
   * / rebates / fees / efficiency columns. Compares with
   * Controls.svelte's pnl-grid which is session-aggregate.
   */
  import { createApiClient } from '../api.svelte.js'
  import { fmtPnl, fmtFixed, fmtBps } from '../format.js'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let rows = $state([])
  let error = $state('')

  async function refresh() {
    try {
      rows = await api.getJson('/api/v1/pnl/per_leg')
      error = ''
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 4000)
    return () => clearInterval(id)
  })

  // Sort leg rows by abs(total) so the biggest PnL movers show
  // first. Ties broken by venue alpha.
  const sorted = $derived.by(() => {
    return [...rows].sort((a, b) => {
      const ap = Math.abs(parseFloat(a.total || 0))
      const bp = Math.abs(parseFloat(b.total || 0))
      if (bp !== ap) return bp - ap
      return (a.venue || '').localeCompare(b.venue || '')
    })
  })
</script>

<div class="perleg">
  {#if error}
    <div class="alert-bar">
      <Icon name="alert" size={14} />
      <span>{error}</span>
    </div>
  {:else if sorted.length === 0}
    <div class="empty-state">
      <span class="empty-state-title">No legs reporting yet</span>
      <span class="empty-state-hint">
        A row appears per running engine (one per venue+symbol).
      </span>
    </div>
  {:else}
    <table class="grid">
      <thead>
        <tr>
          <th>Venue</th>
          <th>Symbol</th>
          <th>Product</th>
          <th class="right">Total</th>
          <th class="right">Spread</th>
          <th class="right">Inv</th>
          <th class="right">Rebates</th>
          <th class="right">Fees</th>
          <th class="right">Vol $</th>
          <th class="right">Eff bps</th>
          <th class="right">Fills</th>
        </tr>
      </thead>
      <tbody>
        {#each sorted as r (r.venue + ':' + r.symbol + ':' + r.product)}
          <tr>
            <td class="venue">{r.venue || '—'}</td>
            <td class="sym num">{r.symbol}</td>
            <td><span class="chip">{r.product || '—'}</span></td>
            <td class="num right" class:pos={parseFloat(r.total) > 0} class:neg={parseFloat(r.total) < 0}>
              {fmtPnl(r.total)}
            </td>
            <td class="num right" class:pos={parseFloat(r.spread_capture) > 0}>
              {fmtPnl(r.spread_capture)}
            </td>
            <td class="num right" class:pos={parseFloat(r.inventory_pnl) > 0} class:neg={parseFloat(r.inventory_pnl) < 0}>
              {fmtPnl(r.inventory_pnl)}
            </td>
            <td class="num right pos">{fmtFixed(r.rebate_income, 2)}</td>
            <td class="num right neg">−{fmtFixed(r.fees_paid, 2)}</td>
            <td class="num right">{fmtFixed(r.volume, 0)}</td>
            <td class="num right">{fmtBps(r.efficiency_bps)}</td>
            <td class="num right">{r.fills}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .perleg { display: flex; flex-direction: column; gap: var(--s-2); }
  table { width: 100%; border-collapse: collapse; font-family: var(--font-sans); }
  th {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    text-align: left;
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    font-weight: 500;
  }
  th.right, td.right { text-align: right; }
  td {
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    color: var(--fg-primary);
  }
  td.num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .venue { font-family: var(--font-mono); font-size: var(--fs-xs); color: var(--accent); }
  .sym { font-size: var(--fs-xs); }
  .chip {
    display: inline-block;
    padding: 2px var(--s-2);
    border-radius: var(--r-sm);
    font-size: var(--fs-2xs);
    background: var(--bg-chip);
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }
  .alert-bar {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--neg-bg);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: var(--r-md);
    color: var(--neg);
    font-size: var(--fs-xs);
  }
</style>
