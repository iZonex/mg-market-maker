<script>
  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
</script>

<div>
  <h3>Open Orders <span class="count">{d.live_orders || 0}</span></h3>

  <table>
    <thead>
      <tr>
        <th>Side</th>
        <th>Price</th>
        <th>Size</th>
        <th>Status</th>
      </tr>
    </thead>
    <tbody>
      {#if d.open_orders}
        {#each d.open_orders as order}
          <tr>
            <td class:buy={order.side === 'buy'} class:sell={order.side === 'sell'}>
              {order.side?.toUpperCase()}
            </td>
            <td>{order.price}</td>
            <td>{order.qty}</td>
            <td>{order.status}</td>
          </tr>
        {/each}
      {:else}
        <tr><td colspan="4" class="empty">Waiting for data...</td></tr>
      {/if}
    </tbody>
  </table>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.5px; }
  .count { color: #58a6ff; margin-left: 6px; }
  table { width: 100%; border-collapse: collapse; }
  th { font-size: 10px; color: #484f58; text-align: left; padding: 4px; border-bottom: 1px solid #21262d; }
  td { font-size: 12px; padding: 3px 4px; border-bottom: 1px solid #161b22; }
  .buy { color: #3fb950; }
  .sell { color: #f85149; }
  .empty { color: #484f58; text-align: center; padding: 20px; }
</style>
