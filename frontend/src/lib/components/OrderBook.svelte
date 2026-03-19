<script>
  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
  const bids = $derived(d.bids || [])
  const asks = $derived(d.asks || [])

  // Take top 10 levels.
  const topAsks = $derived([...asks].slice(0, 10).reverse())
  const topBids = $derived([...bids].slice(0, 10))

  const maxQty = $derived(
    Math.max(...[...topAsks, ...topBids].map(l => parseFloat(l[1] || l.qty || 0)), 1)
  )
</script>

<div class="book">
  <h3>Order Book</h3>

  <div class="header-row">
    <span>Price</span>
    <span>Size</span>
  </div>

  <div class="asks">
    {#each topAsks as level}
      {@const price = level[0] || level.price || '0'}
      {@const qty = parseFloat(level[1] || level.qty || 0)}
      <div class="row ask">
        <div class="bar ask-bar" style="width: {(qty / maxQty * 100).toFixed(1)}%"></div>
        <span class="price">{price}</span>
        <span class="qty">{qty.toFixed(4)}</span>
      </div>
    {/each}
  </div>

  <div class="spread-line">
    <span>Spread: {d.spread_bps ? `${parseFloat(d.spread_bps).toFixed(1)} bps` : '—'}</span>
  </div>

  <div class="bids">
    {#each topBids as level}
      {@const price = level[0] || level.price || '0'}
      {@const qty = parseFloat(level[1] || level.qty || 0)}
      <div class="row bid">
        <div class="bar bid-bar" style="width: {(qty / maxQty * 100).toFixed(1)}%"></div>
        <span class="price">{price}</span>
        <span class="qty">{qty.toFixed(4)}</span>
      </div>
    {/each}
  </div>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.5px; }
  .header-row {
    display: flex; justify-content: space-between;
    font-size: 10px; color: #484f58; padding: 2px 4px;
  }
  .row {
    display: flex; justify-content: space-between; align-items: center;
    padding: 1px 4px; position: relative; font-size: 12px;
  }
  .bar {
    position: absolute; left: 0; top: 0; bottom: 0; opacity: 0.15;
  }
  .ask-bar { background: #f85149; }
  .bid-bar { background: #3fb950; }
  .ask .price { color: #f85149; }
  .bid .price { color: #3fb950; }
  .qty { color: #8b949e; }
  .spread-line {
    text-align: center; padding: 4px; font-size: 11px;
    color: #58a6ff; border-top: 1px solid #21262d; border-bottom: 1px solid #21262d;
    margin: 4px 0;
  }
</style>
