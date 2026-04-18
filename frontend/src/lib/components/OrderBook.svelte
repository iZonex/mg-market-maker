<script>
  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})

  // Prefer live L2 arrays if the WS pushes them; fall back to the
  // aggregated `book_depth_levels` from /api/status so this panel
  // never looks empty when only REST polling is alive.
  const rawBids = $derived(d.bids || [])
  const rawAsks = $derived(d.asks || [])
  const depthLevels = $derived(d.book_depth_levels || [])

  const hasLive = $derived(rawBids.length > 0 || rawAsks.length > 0)
  const hasAgg  = $derived(depthLevels.length > 0)

  const topAsks = $derived([...rawAsks].slice(0, 10).reverse())
  const topBids = $derived([...rawBids].slice(0, 10))

  const maxQty = $derived(
    hasLive
      ? Math.max(...[...topAsks, ...topBids].map(l => parseFloat(l[1] ?? l.qty ?? 0)), 1)
      : Math.max(...depthLevels.map(l => Math.max(
          parseFloat(l.bid_depth_quote || 0),
          parseFloat(l.ask_depth_quote || 0),
        )), 1)
  )

  // Two spreads are meaningful: ours (the strategy's quoted
  // bid/ask pair) and the venue's (top-of-book tick distance).
  // On majors the venue spread is tick-limited (~0.0013 bps for
  // BTCUSDT at $77 k) so we headline the quoted spread and show
  // venue as a secondary annotation.
  const venueSpreadBps = $derived(parseFloat(d.spread_bps || 0))
  const mid = $derived(parseFloat(d.mid_price || 0))
  const quotedSpreadBps = $derived.by(() => {
    const orders = d.open_orders || []
    if (!orders.length || mid === 0) return null
    let bb = -Infinity, ba = Infinity
    for (const o of orders) {
      const p = parseFloat(o.price || 0)
      const side = (o.side || '').toLowerCase()
      if (side === 'buy'  && p > bb) bb = p
      if (side === 'sell' && p < ba) ba = p
    }
    if (!Number.isFinite(bb) || !Number.isFinite(ba) || ba <= bb) return null
    return ((ba - bb) / mid) * 10_000
  })
  const spreadBps = $derived(quotedSpreadBps ?? venueSpreadBps)
  const spreadKind = $derived(quotedSpreadBps !== null ? 'quoted' : 'venue')
  const spreadLabel = $derived(spreadBps > 0 ? `${spreadBps.toFixed(2)} bps` : '—')

  // Dedup identical depth rows — on majors the top-20 stream
  // covers ≤ 0.5 % from mid, so ±1/2/5 % bands often collapse to
  // the same cumulative sum. Showing repeats reads like a bug.
  const uniqueDepthLevels = $derived.by(() => {
    const seen = new Set()
    const out = []
    for (const l of depthLevels) {
      const key = `${parseFloat(l.bid_depth_quote || 0).toFixed(1)}|${parseFloat(l.ask_depth_quote || 0).toFixed(1)}`
      if (seen.has(key)) continue
      seen.add(key)
      out.push(l)
    }
    return out
  })

  function fmtPx(n) {
    const f = parseFloat(n)
    if (!Number.isFinite(f)) return '—'
    return f.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
  }
  function fmtQty(n) {
    const f = parseFloat(n)
    if (!Number.isFinite(f)) return '—'
    return f.toFixed(4)
  }
  function fmtNotional(n) {
    const f = parseFloat(n)
    if (!Number.isFinite(f) || f === 0) return '—'
    if (f >= 1e6) return `${(f / 1e6).toFixed(2)}M`
    if (f >= 1e3) return `${(f / 1e3).toFixed(1)}k`
    return f.toFixed(0)
  }
</script>

{#if hasLive}
  <div class="book">
    <div class="head">
      <span>Price</span>
      <span>Size</span>
    </div>

    <div class="side asks">
      {#each topAsks as level}
        {@const price = level[0] ?? level.price ?? '0'}
        {@const qty   = parseFloat(level[1] ?? level.qty ?? 0)}
        <div class="row ask">
          <div class="bar" style="width: {(qty / maxQty * 100).toFixed(1)}%"></div>
          <span class="px num">{fmtPx(price)}</span>
          <span class="qty num">{fmtQty(qty)}</span>
        </div>
      {/each}
    </div>

    <div class="spread-line">
      <span>spread</span>
      <span class="spread-val num">{spreadLabel}</span>
    </div>

    <div class="side bids">
      {#each topBids as level}
        {@const price = level[0] ?? level.price ?? '0'}
        {@const qty   = parseFloat(level[1] ?? level.qty ?? 0)}
        <div class="row bid">
          <div class="bar" style="width: {(qty / maxQty * 100).toFixed(1)}%"></div>
          <span class="px num">{fmtPx(price)}</span>
          <span class="qty num">{fmtQty(qty)}</span>
        </div>
      {/each}
    </div>
  </div>
{:else if hasAgg}
  <div class="depth">
    <div class="head">
      <span>Depth (%)</span>
      <span>Bid ($)</span>
      <span>Ask ($)</span>
    </div>
    {#each uniqueDepthLevels as l, i}
      <div class="depth-row">
        <span class="pct label">
          {#if i === uniqueDepthLevels.length - 1 && uniqueDepthLevels.length < depthLevels.length}
            ≥±{parseFloat(l.pct_from_mid || 0).toFixed(1)}%
          {:else}
            ±{parseFloat(l.pct_from_mid || 0).toFixed(1)}%
          {/if}
        </span>
        <span class="num pos">{fmtNotional(l.bid_depth_quote)}</span>
        <span class="num neg">{fmtNotional(l.ask_depth_quote)}</span>
      </div>
    {/each}
    {#if uniqueDepthLevels.length < depthLevels.length}
      <div class="depth-note">
        <span>top-20 book — depth flattens beyond ±0.5%</span>
      </div>
    {/if}
    <div class="spread-line">
      <span>spread · {spreadKind}</span>
      <span class="spread-val num">{spreadLabel}</span>
    </div>
  </div>
{:else}
  <div class="empty-state">
    <span class="empty-state-icon">
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <line x1="4" y1="6"  x2="20" y2="6"/>
        <line x1="4" y1="12" x2="16" y2="12"/>
        <line x1="4" y1="18" x2="12" y2="18"/>
      </svg>
    </span>
    <span class="empty-state-title">Waiting for book data</span>
    <span class="empty-state-hint">The orderbook will render once the venue sends the first L2 snapshot.</span>
  </div>
{/if}

<style>
  .book, .depth {
    display: flex;
    flex-direction: column;
    gap: 1px;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .head {
    display: grid;
    grid-template-columns: 1fr 1fr;
    padding: 0 var(--s-2) var(--s-2);
    font-size: var(--fs-2xs);
    color: var(--fg-faint);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-family: var(--font-sans);
  }
  .head > *:last-child { text-align: right; }
  .depth .head { grid-template-columns: 1fr 1fr 1fr; }
  .depth .head > :nth-child(2),
  .depth .head > :nth-child(3) { text-align: right; }

  .row, .depth-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    align-items: center;
    padding: 3px var(--s-2);
    font-size: var(--fs-xs);
    position: relative;
    border-radius: 2px;
  }
  .depth-row { grid-template-columns: 1fr 1fr 1fr; }
  .row .bar {
    position: absolute;
    top: 0; bottom: 0;
    border-radius: 2px;
    opacity: 0.14;
    z-index: 0;
  }
  .row.ask .bar { right: 0; background: var(--neg); }
  .row.bid .bar { right: 0; background: var(--pos); }
  .row .px, .row .qty, .pct, .depth-row .num { position: relative; z-index: 1; }
  .row.ask .px { color: var(--neg); font-weight: 500; }
  .row.bid .px { color: var(--pos); font-weight: 500; }
  .row .qty, .depth-row .num { text-align: right; color: var(--fg-secondary); }
  .pct { font-family: var(--font-sans); }

  .spread-line {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--s-2) var(--s-3);
    margin: var(--s-1) 0;
    background: var(--bg-chip);
    border-top: 1px solid var(--border-subtle);
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .spread-val {
    color: var(--accent);
    font-weight: 600;
    text-transform: none;
    letter-spacing: 0;
  }

  .depth-note {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    padding: var(--s-1) var(--s-2);
    font-style: italic;
    text-align: center;
    font-family: var(--font-sans);
  }
</style>
