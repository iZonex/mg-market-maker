<script>
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
    const hue = seriesColor(0)  // series-1 purple

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
    })

    const ro = new ResizeObserver(() => {
      if (chart) chart.applyOptions({ width: container.clientWidth })
    })
    ro.observe(container)
    return () => { ro.disconnect(); chart.remove() }
  })

  $effect(() => {
    const history = data.state.spreadHistory
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
      try { series.setData(deduped); initialized = true; lastLen = history.length } catch(e) { console.warn('spread setData:', e) }
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
