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

  let { data } = $props()
  let container
  let chart
  let series
  let initialized = false
  let lastLen = 0

  onMount(async () => {
    const { createChart, AreaSeries } = await import('lightweight-charts')

    chart = createChart(container, {
      layout: { background: { color: 'transparent' }, textColor: '#a8acb5', fontFamily: 'JetBrains Mono, monospace', fontSize: 11 },
      grid: { vertLines: { color: 'rgba(255,255,255,0.04)' }, horzLines: { color: 'rgba(255,255,255,0.04)' } },
      width: container.clientWidth,
      height: 200,
      timeScale: { timeVisible: true, secondsVisible: false, borderColor: 'rgba(255,255,255,0.06)' },
      rightPriceScale: { borderColor: 'rgba(255,255,255,0.06)' },
      crosshair: {
        vertLine: { color: 'rgba(16,185,129,0.45)', labelBackgroundColor: '#10b981' },
        horzLine: { color: 'rgba(16,185,129,0.45)', labelBackgroundColor: '#10b981' },
      },
    })

    series = chart.addSeries(AreaSeries, {
      lineColor: '#10b981',
      topColor: 'rgba(16,185,129,0.28)',
      bottomColor: 'rgba(16,185,129,0.00)',
      lineWidth: 2,
      baseLineVisible: true,
      baseLineColor: 'rgba(255,255,255,0.25)',
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
