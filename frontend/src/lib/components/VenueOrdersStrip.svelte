<script>
  /*
   * Per-venue live-order count — single strip under HeroKpis.
   *
   * The UX audit flagged: HeroKpis shows aggregate live-order
   * count ("15 live") but no per-venue breakdown, so the
   * operator must navigate to Admin → VenuesHealth to see
   * "Binance: 10, Bybit: 5". This strip pulls the same
   * per-symbol state the WS store already has and groups
   * by (venue, product), emitting one chip per venue with
   * the aggregate order count, kill level, and fills-today.
   *
   * Hidden when fewer than 2 venues are active — on a single-
   * venue deploy the HeroKpis readout already covers it.
   */
  let { data } = $props()

  // Build a `{ venue → { orders, fills, symbols, maxKill } }`
  // aggregate off the WS store's per-symbol map. Venue-less
  // rows (fresh engine, no tick yet) land in an "unknown"
  // bucket which we hide from the strip.
  const venues = $derived.by(() => {
    const out = new Map()
    const states = Object.values(data?.state?.data ?? {})
    for (const s of states) {
      const venue = (s.venue || '').trim()
      const product = (s.product || '').trim()
      if (!venue) continue
      const key = product ? `${venue}·${product}` : venue
      const entry = out.get(key) ?? {
        label: key,
        venue,
        product,
        orders: 0,
        fills: 0,
        symbols: 0,
        maxKill: 0,
      }
      entry.orders += parseInt(s.live_orders || 0, 10) || 0
      entry.fills += parseInt(s.total_fills || 0, 10) || 0
      entry.symbols += 1
      const kl = parseInt(s.kill_level || 0, 10) || 0
      if (kl > entry.maxKill) entry.maxKill = kl
      out.set(key, entry)
    }
    return Array.from(out.values()).sort((a, b) => a.label.localeCompare(b.label))
  })

  function killColour(level) {
    if (level >= 3) return 'var(--neg)'
    if (level >= 1) return 'var(--warn)'
    return 'var(--pos)'
  }

  // Only render when we have ≥ 2 venues active — single-venue
  // deploy already sees the aggregate on HeroKpis.
  const show = $derived(venues.length >= 2)
</script>

{#if show}
  <div class="strip" aria-label="Per-venue order breakdown">
    <div class="label">Venues</div>
    <div class="chips">
      {#each venues as v (v.label)}
        <div class="chip" title="{v.symbols} symbol{v.symbols === 1 ? '' : 's'} · kill L{v.maxKill}">
          <span class="dot" style:background={killColour(v.maxKill)} aria-hidden="true"></span>
          <span class="venue-label">{v.venue}</span>
          {#if v.product}
            <span class="sep">·</span>
            <span class="product">{v.product}</span>
          {/if}
          <span class="sep">·</span>
          <span class="metric" title="live orders">
            <span class="num">{v.orders}</span>
            <span class="unit">live</span>
          </span>
          <span class="sep">·</span>
          <span class="metric" title="fills this session">
            <span class="num">{v.fills}</span>
            <span class="unit">fills</span>
          </span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .strip {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    font-size: var(--fs-xs);
  }
  .label {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    font-weight: 600;
    flex-shrink: 0;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-2);
    flex: 1;
  }  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .venue-label {
    font-weight: 700;
    color: var(--fg-primary);
    text-transform: uppercase;
  }
  .product {
    color: var(--fg-secondary);
    font-size: 10px;
  }
  .sep {
    color: var(--fg-faint);
  }
  .metric {
    display: inline-flex;
    align-items: baseline;
    gap: 2px;
  }
  .metric .num {
    color: var(--fg-primary);
    font-variant-numeric: tabular-nums;
  }
  .metric .unit {
    color: var(--fg-muted);
    font-size: 10px;
  }
</style>
