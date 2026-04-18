<script>
  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
  const orders = $derived(d.open_orders || [])
</script>

{#if orders.length === 0}
  <div class="empty-state">
    <span class="empty-state-icon">
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="12" r="9"/>
        <line x1="12" y1="8" x2="12" y2="12"/>
        <line x1="12" y1="16" x2="12.01" y2="16"/>
      </svg>
    </span>
    <span class="empty-state-title">No open orders</span>
    <span class="empty-state-hint">Orders will appear once the strategy completes a refresh tick.</span>
  </div>
{:else}
  <table class="orders">
    <thead>
      <tr>
        <th>Side</th>
        <th>Price</th>
        <th class="right">Size</th>
        <th>Status</th>
      </tr>
    </thead>
    <tbody>
      {#each orders as order}
        <tr>
          <td>
            <span class="side" data-side={order.side?.toLowerCase()}>{order.side?.toUpperCase()}</span>
          </td>
          <td class="num">{order.price}</td>
          <td class="num right">{order.qty}</td>
          <td><span class="status">{order.status}</span></td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  .orders {
    width: 100%;
    border-collapse: collapse;
    font-family: var(--font-sans);
    font-size: var(--fs-sm);
  }
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
  th.right { text-align: right; }
  td {
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    color: var(--fg-primary);
  }
  td.num {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--fs-sm);
  }
  td.right { text-align: right; }
  .side {
    display: inline-flex;
    align-items: center;
    padding: 2px var(--s-2);
    border-radius: var(--r-sm);
    font-size: var(--fs-2xs);
    font-weight: 700;
    letter-spacing: var(--tracking-label);
    font-family: var(--font-mono);
  }
  .side[data-side='buy']  { background: var(--pos-bg); color: var(--pos); }
  .side[data-side='sell'] { background: var(--neg-bg); color: var(--neg); }
  .status {
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
  }
</style>
