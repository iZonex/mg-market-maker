<script>
  /*
   * 23-UX-2 — per-leg inventory stacked-area chart.
   *
   * Shows inventory split by (venue, symbol) over time — critical
   * for a pro MM desk running the same base asset across multiple
   * venues (Binance spot + Bybit spot + Binance perp). The
   * aggregate InventoryChart hides which leg is soaking the
   * imbalance; this one makes it obvious.
   *
   * Backfills from /api/v1/history/inventory/per_leg on mount
   * (backend ring buffer keeps up to 4 hours at 1s resolution).
   * Polls every 10 s for updates — same cadence as
   * CrossVenuePortfolio.
   */
  import { onMount } from 'svelte'
  import { createApiClient } from '../api.svelte.js'
  import { baseChartOptions, seriesColor } from '../chart-theme.js'

  let { auth, base = null } = $props()
  const api = $derived(createApiClient(auth))

  let container
  let chart
  let series = {}
  let initialized = false

  let legs = $state([]) // [{venue, symbol, base_asset, points}]
  let error = $state('')

  async function refresh() {
    try {
      const q = base ? `?base=${encodeURIComponent(base)}` : ''
      legs = await api.getJson(`/api/v1/history/inventory/per_leg${q}`)
      error = ''
      if (chart) applyLegs()
    } catch (e) {
      error = String(e)
    }
  }

  /**
   * Hash a leg key into one of the chart-series palette slots. Same
   * leg always gets the same colour across page reloads — stable
   * comparison.
   */
  function legColor(key) {
    let h = 0
    for (let i = 0; i < key.length; i++) {
      h = ((h << 5) - h + key.charCodeAt(i)) | 0
    }
    return seriesColor(Math.abs(h))
  }

  function applyLegs() {
    if (!chart) return
    // Drop series that disappeared.
    for (const key of Object.keys(series)) {
      if (!legs.some((l) => `${l.venue}:${l.symbol}` === key)) {
        chart.removeSeries(series[key])
        delete series[key]
      }
    }
    // Add or update each leg.
    for (const leg of legs) {
      const key = `${leg.venue}:${leg.symbol}`
      let ser = series[key]
      if (!ser) {
        ser = chart.addSeries(window.__mmLineSeries, {
          color: legColor(key),
          lineWidth: 2,
          title: `${leg.venue}:${leg.symbol}`,
        })
        series[key] = ser
      }
      const seen = new Set()
      const pts = []
      for (const p of leg.points) {
        const t = Math.floor(p.timestamp_ms / 1000)
        if (seen.has(t)) continue
        seen.add(t)
        pts.push({ time: t, value: parseFloat(p.value || 0) })
      }
      try { ser.setData(pts) } catch(_) {}
    }
    initialized = true
  }

  onMount(async () => {
    const { createChart, LineSeries } = await import('lightweight-charts')
    window.__mmLineSeries = LineSeries
    chart = createChart(container, {
      ...baseChartOptions(),
      width: container.clientWidth,
      height: 220,
    })
    const ro = new ResizeObserver(() => {
      if (chart) chart.applyOptions({ width: container.clientWidth })
    })
    ro.observe(container)

    await refresh()

    const id = setInterval(refresh, 10_000)
    return () => { ro.disconnect(); clearInterval(id); chart.remove() }
  })
</script>

<div class="wrap">
  {#if error}
    <div class="alert-bar">{error}</div>
  {:else if legs.length === 0}
    <div class="empty-state">
      <span class="empty-state-title">No legs yet</span>
      <span class="empty-state-hint">
        Appears once the engine has published at least one
        venue-inventory sample. History grows from engine boot
        (up to 4 h at 1 s resolution).
      </span>
    </div>
  {:else}
    <div class="legend">
      {#each legs as l (l.venue + ':' + l.symbol)}
        <span class="leg-chip">
          <span class="dot" style="background: {legColor(l.venue + ':' + l.symbol)}"></span>
          <span class="venue">{l.venue}</span>
          <span class="sym">{l.symbol}</span>
        </span>
      {/each}
    </div>
  {/if}
  <div bind:this={container} class="chart-container"></div>
</div>

<style>
  .wrap { display: flex; flex-direction: column; gap: var(--s-2); }
  .chart-container { width: 100%; min-height: 220px; }
  .legend {
    display: flex; flex-wrap: wrap; gap: var(--s-2);
    padding: var(--s-2); border-bottom: 1px solid var(--border-subtle);
  }
  .leg-chip {
    display: inline-flex; align-items: center; gap: 4px;
    padding: 2px var(--s-2); border-radius: var(--r-sm);
    background: var(--bg-chip);
    font-size: var(--fs-2xs);
    font-family: var(--font-mono);
  }
  .dot { width: 8px; height: 8px; border-radius: 50%; }
  .venue { color: var(--accent); }
  .sym { color: var(--fg-muted); }
  .alert-bar {
    padding: var(--s-2) var(--s-3);
    background: var(--neg-bg);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: var(--r-md);
    color: var(--neg);
    font-size: var(--fs-xs);
  }
</style>
