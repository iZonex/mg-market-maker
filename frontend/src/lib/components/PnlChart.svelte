<script>
  import { onMount } from 'svelte'
  import { baseChartOptions, readChartTheme } from '../chart-theme.js'

  let { data } = $props()
  let container
  let chart
  let series
  let initialized = false
  let lastLen = 0

  onMount(async () => {
    const { createChart, LineSeries } = await import('lightweight-charts')
    const theme = readChartTheme()

    chart = createChart(container, {
      ...baseChartOptions(),
      width: container.clientWidth,
      height: 200,
    })

    series = chart.addSeries(LineSeries, {
      color: theme.accent,
      lineWidth: 2,
      lineType: 0,
      crosshairMarkerRadius: 4,
      crosshairMarkerBorderWidth: 2,
      crosshairMarkerBackgroundColor: theme.accent,
    })

    const ro = new ResizeObserver(() => {
      if (chart) chart.applyOptions({ width: container.clientWidth })
    })
    ro.observe(container)
    return () => { ro.disconnect(); chart.remove() }
  })

  $effect(() => {
    const history = data.state.pnlHistory
    if (!series || history.length === 0) return

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
      try { series.setData(deduped); initialized = true; lastLen = history.length } catch(e) { console.warn('pnl setData:', e) }
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
