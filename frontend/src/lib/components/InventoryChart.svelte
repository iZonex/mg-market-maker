<script>
  /*
   * UX-7 — inventory-over-time chart. The engine pushes one
   * sample per run-symbol tick into `DashboardState`, the
   * frontend mirrors it into `ws.state.inventoryHistory` and
   * backfills the last 1440 points from
   * `/api/v1/inventory/timeseries` on page load so operators
   * see real inventory drift instead of a blank panel.
   *
   * Zero line is highlighted — inventory can go negative on
   * spot-short or perp, so the chart renders as a two-tone
   * area symmetric about zero.
   */
  import { onMount } from 'svelte'
  import { baseChartOptions, seriesColor } from '../chart-theme.js'

  let { data } = $props()
  let container
  let chart
  let series
  let initialized = false
  let lastLen = 0

  onMount(async () => {
    const { createChart, AreaSeries } = await import('lightweight-charts')
    const hue = seriesColor(1)  // series-2 emerald

    chart = createChart(container, {
      ...baseChartOptions(),
      width: container.clientWidth,
      height: 200,
      crosshair: {
        vertLine: { color: `${hue}73`, labelBackgroundColor: hue },
        horzLine: { color: `${hue}73`, labelBackgroundColor: hue },
      },
    })

    series = chart.addSeries(AreaSeries, {
      lineColor: hue,
      topColor: `${hue}47`,
      bottomColor: `${hue}00`,
      lineWidth: 2,
      baseLineVisible: true,
      baseLineWidth: 1,
    })

    const ro = new ResizeObserver(() => {
      if (chart) chart.applyOptions({ width: container.clientWidth })
    })
    ro.observe(container)
    return () => { ro.disconnect(); chart.remove() }
  })

  $effect(() => {
    const history = data.state.inventoryHistory
    if (!series || !history || history.length === 0) return

    if (!initialized && history.length > 10) {
      const seen = new Set()
      const deduped = []
      for (const p of history) {
        const t = Math.floor(p.time / 1000)
        if (!seen.has(t)) {
          seen.add(t)
          deduped.push({ time: t, value: p.value })
        }
      }
      try { series.setData(deduped); initialized = true; lastLen = history.length } catch(e) { console.warn('inventory setData:', e) }
    } else if (initialized && history.length > lastLen) {
      const latest = history[history.length - 1]
      try { series.update({ time: Math.floor(latest.time / 1000), value: latest.value }) } catch(_) {}
      lastLen = history.length
    }
  })
</script>

<div bind:this={container} class="chart-container"></div>

<style>
  .chart-container { width: 100%; min-height: 200px; }
</style>
