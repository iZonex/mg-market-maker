<script>
  let { data } = $props()
  const fills = $derived(data.state.fills || [])
</script>

<div>
  <h3>Fill History <span class="count">{fills.length}</span></h3>

  <div class="scroll">
    <table>
      <thead>
        <tr>
          <th>Time</th>
          <th>Side</th>
          <th>Price</th>
          <th>Qty</th>
          <th>Role</th>
        </tr>
      </thead>
      <tbody>
        {#each fills.slice(0, 20) as fill}
          <tr>
            <td class="time">{new Date(fill.timestamp || Date.now()).toLocaleTimeString()}</td>
            <td class:buy={fill.side === 'buy'} class:sell={fill.side === 'sell'}>
              {fill.side?.toUpperCase()}
            </td>
            <td>{fill.price}</td>
            <td>{fill.qty}</td>
            <td class="role">{fill.is_maker ? 'MAKER' : 'TAKER'}</td>
          </tr>
        {/each}
        {#if fills.length === 0}
          <tr><td colspan="5" class="empty">No fills yet</td></tr>
        {/if}
      </tbody>
    </table>
  </div>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.5px; }
  .count { color: #58a6ff; margin-left: 6px; }
  .scroll { max-height: 200px; overflow-y: auto; }
  table { width: 100%; border-collapse: collapse; }
  th { font-size: 10px; color: #484f58; text-align: left; padding: 4px; border-bottom: 1px solid #21262d; position: sticky; top: 0; background: #161b22; }
  td { font-size: 11px; padding: 2px 4px; border-bottom: 1px solid #0a0e17; }
  .time { color: #8b949e; }
  .buy { color: #3fb950; font-weight: 600; }
  .sell { color: #f85149; font-weight: 600; }
  .role { color: #d2a8ff; font-size: 10px; }
  .empty { color: #484f58; text-align: center; padding: 20px; }
</style>
