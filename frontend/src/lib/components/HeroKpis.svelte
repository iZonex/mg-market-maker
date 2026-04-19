<script>
  // Hero ticker strip — compact density, single row of KPIs.
  // Style-reference: Hyperliquid / Drift top bar. No huge numbers,
  // no billboard feel; just a clean information strip the operator
  // reads left-to-right in one glance.

  let { data } = $props()

  let lastMid = null
  let lastPnl = null
  let midDir = $state('flat')
  let pnlDir = $state('flat')

  $effect(() => {
    const mid = parseFloat(data?.mid_price || 0)
    if (lastMid !== null && Math.abs(mid - lastMid) > 1e-9) {
      midDir = mid > lastMid ? 'up' : 'down'
      setTimeout(() => { midDir = 'flat' }, 700)
    }
    lastMid = mid
  })
  $effect(() => {
    const p = parseFloat(data?.pnl?.total || 0)
    if (lastPnl !== null && Math.abs(p - lastPnl) > 1e-9) {
      pnlDir = p > lastPnl ? 'up' : 'down'
      setTimeout(() => { pnlDir = 'flat' }, 700)
    }
    lastPnl = p
  })

  const mid = $derived(parseFloat(data?.mid_price || 0))
  // Venue spread (top-of-book) — on Binance BTC/USDT with $0.01
  // tick this is ~0.0013 bps at $77 k, effectively meaningless.
  const venueSpreadBps = $derived(parseFloat(data?.spread_bps || 0))
  // Quoted spread — our own best bid vs best ask in bps. This is
  // THE spread a market-maker operator wants to see: "am I
  // actually trading the spread I configured?"
  const quotedSpreadBps = $derived.by(() => {
    const orders = data?.open_orders || []
    if (!orders.length || mid === 0) return null
    let bestBid = -Infinity, bestAsk = Infinity
    for (const o of orders) {
      const p = parseFloat(o.price || 0)
      const side = (o.side || '').toLowerCase()
      if (side === 'buy'  && p > bestBid) bestBid = p
      if (side === 'sell' && p < bestAsk) bestAsk = p
    }
    if (!Number.isFinite(bestBid) || !Number.isFinite(bestAsk) || bestAsk <= bestBid) return null
    return ((bestAsk - bestBid) / mid) * 10_000
  })
  // Pick the meaningful value for the ticker strip — prefer our
  // quote if we have one, fall back to venue.
  const spreadBps = $derived(quotedSpreadBps ?? venueSpreadBps)
  const spreadKind = $derived(quotedSpreadBps !== null ? 'quoted' : 'venue')
  const pnlTotal = $derived(parseFloat(data?.pnl?.total || 0))
  const volume = $derived(parseFloat(data?.pnl?.volume || 0))
  const fills = $derived(parseInt(data?.total_fills || 0, 10))
  const inventory = $derived(parseFloat(data?.inventory || 0))
  const invValue = $derived(parseFloat(data?.inventory_value || 0))
  const killLevel = $derived(parseInt(data?.kill_level || 0, 10))
  const sla = $derived(parseFloat(data?.sla_uptime_pct || 0))
  const regime = $derived(data?.regime || '—')
  const pairClass = $derived(data?.pair_class || null)
  const liveOrders = $derived(parseInt(data?.live_orders || 0, 10))

  // 23-UX-8 — shared formatters. Locale-aware `en-US` thousands
  // separators preserved by falling through to toLocaleString in
  // the HeroKpis-specific prices + volume cases below.
  import { fmtBps } from '../format.js'
  function fmt(n, d = 2) {
    if (!Number.isFinite(n)) return '—'
    return n.toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d })
  }
  // Shadow local fmtSigned with the locale-formatted version
  // HeroKpis needs for large PnL readings ($1,234.56 vs 1234.56).
  function fmtSigned(n, d = 2) {
    if (!Number.isFinite(n)) return '—'
    return (n > 0 ? '+' : '') + n.toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d })
  }

  const pnlClass = $derived(pnlTotal > 0.005 ? 'pos' : pnlTotal < -0.005 ? 'neg' : '')
  const invClass = $derived(inventory > 0 ? 'pos' : inventory < 0 ? 'neg' : '')
  const killClass = $derived(
    killLevel === 0 ? 'pos' : killLevel === 1 ? 'warn' : 'neg'
  )
  const killText = $derived({
    0: 'nominal', 1: 'widen', 2: 'stop-new', 3: 'cancel', 4: 'flatten', 5: 'disc.',
  }[killLevel] || '—')
  const slaClass = $derived(
    sla === 0 ? '' : sla >= 95 ? 'pos' : sla >= 90 ? 'warn' : 'neg'
  )
</script>

<section class="hero" aria-label="Session KPIs">
  <div class="cell cell-mid">
    <span class="label">Mid</span>
    <div class="val-row">
      <span class="val num ticker-{midDir}">{fmt(mid, 2)}</span>
      <span class="unit">USDT</span>
    </div>
  </div>

  <div class="cell" title="Our quoted spread (best bid → best ask in bps). Falls back to venue spread when we have no live orders.">
    <span class="label">Spread · {spreadKind}</span>
    <div class="val-row">
      <span class="val num">{spreadBps > 0 ? fmtBps(spreadBps) : '—'}</span>
      <span class="unit">bps</span>
    </div>
  </div>

  <div class="cell">
    <span class="label">PnL</span>
    <div class="val-row">
      <span class="val num {pnlClass} ticker-{pnlDir}">${fmtSigned(pnlTotal, 4)}</span>
    </div>
    <span class="sub num">vol ${fmt(volume, 0)} · {fills} fills</span>
  </div>

  <div class="cell">
    <span class="label">Inventory</span>
    <div class="val-row">
      <span class="val num {invClass}">{fmtSigned(inventory, 6)}</span>
    </div>
    <span class="sub num">≈ ${fmt(Math.abs(invValue), 2)}</span>
  </div>

  <div class="cell">
    <span class="label">Kill</span>
    <div class="val-row">
      <span class="kl-badge kl-{killClass}">L{killLevel}</span>
      <span class="sub kill-sub">{killText}</span>
    </div>
  </div>

  <div class="cell">
    <span class="label">SLA 24h</span>
    <div class="val-row">
      <span class="val num {slaClass}">{sla > 0 ? `${fmt(sla, 1)}%` : '—'}</span>
    </div>
    <span class="sub">{liveOrders} live</span>
  </div>

  <div class="cell cell-regime">
    <span class="label">Regime</span>
    <div class="val-row">
      <span class="chip"
            class:chip-info={regime === 'Quiet'}
            class:chip-warn={regime === 'Volatile'}
            class:chip-pos={regime === 'Trending'}
            class:chip-neg={regime === 'MeanReverting'}>
        {regime}
      </span>
      {#if pairClass}
        <span class="chip chip-pc" data-pc={pairClass}>{pairClass.replace(/_/g, ' ')}</span>
      {/if}
    </div>
  </div>
</section>

<style>
  .hero {
    display: grid;
    grid-template-columns: 1.3fr repeat(5, minmax(0, 1fr)) 1.2fr;
    gap: var(--s-4);
    align-items: center;
    padding: var(--s-3) var(--s-5);
    background:
      linear-gradient(90deg, rgba(0, 208, 156, 0.045) 0%, transparent 35%),
      var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-xl);
  }

  .cell {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    padding: var(--s-1) var(--s-2);
    border-right: 1px solid var(--border-subtle);
  }
  .cell:last-child { border-right: none; }

  .label {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    font-weight: 500;
  }
  .val-row {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    line-height: var(--lh-tight);
  }
  .val {
    font-size: var(--fs-xl);
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--fg-primary);
    transition: color var(--dur-slow) var(--ease-out);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .cell-mid .val { font-size: var(--fs-2xl); }
  .unit {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    font-family: var(--font-sans);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .sub {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    margin-top: 1px;
  }

  .pos  { color: var(--pos); }
  .neg  { color: var(--neg); }
  .warn { color: var(--warn); }

  .ticker-up   { color: var(--pos); }
  .ticker-down { color: var(--neg); }

  /* Kill-level pill, sized to fit the ticker strip */
  .kl-badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 32px;
    padding: 2px var(--s-2);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .kl-pos  { background: var(--pos-bg); color: var(--pos); }
  .kl-warn { background: var(--warn-bg); color: var(--warn); }
  .kl-neg  { background: var(--neg-bg); color: var(--neg); }
  .kill-sub { color: var(--fg-muted); }

  /* Regime cell — chips in a row */
  .cell-regime .val-row {
    flex-wrap: wrap;
    gap: var(--s-1);
  }
  .chip-pc[data-pc^='Major']  { color: var(--pc-major-perp); background: rgba(96, 165, 250, 0.14); border-color: rgba(96, 165, 250, 0.35); }
  .chip-pc[data-pc^='Alt']    { color: var(--pc-alt-perp);   background: rgba(251, 191, 36, 0.14); border-color: rgba(251, 191, 36, 0.35); }
  .chip-pc[data-pc^='Meme']   { color: var(--pc-meme-spot);  background: rgba(236, 72, 153, 0.14); border-color: rgba(236, 72, 153, 0.35); }
  .chip-pc[data-pc^='Stable'] { color: var(--pc-stable-stable); background: rgba(16, 185, 129, 0.14); border-color: rgba(16, 185, 129, 0.35); }

  @media (max-width: 1200px) {
    .hero {
      grid-template-columns: repeat(3, 1fr);
      row-gap: var(--s-3);
    }
    .cell {
      border-right: none;
      border-bottom: 1px solid var(--border-subtle);
      padding-bottom: var(--s-2);
    }
  }
  @media (max-width: 720px) {
    .hero { grid-template-columns: repeat(2, 1fr); }
  }
</style>
