<script>
  import { onMount } from 'svelte'

  let { data } = $props()
  let container
  let chart
  let series
  let initialized = false
  let lastLen = 0

  onMount(async () => {
    const { createChart, LineSeries } = await import('lightweight-charts')

    chart = createChart(container, {
      layout: { background: { color: 'transparent' }, textColor: '#a8acb5', fontFamily: 'JetBrains Mono, monospace', fontSize: 11 },
      grid: { vertLines: { color: 'rgba(255,255,255,0.04)' }, horzLines: { color: 'rgba(255,255,255,0.04)' } },
      width: container.clientWidth,
      height: 200,
      timeScale: { timeVisible: true, secondsVisible: false, borderColor: 'rgba(255,255,255,0.06)' },
      rightPriceScale: { borderColor: 'rgba(255,255,255,0.06)' },
      crosshair: { vertLine: { color: 'rgba(0,208,156,0.4)', labelBackgroundColor: '#00d09c' }, horzLine: { color: 'rgba(0,208,156,0.4)', labelBackgroundColor: '#00d09c' } },
    })

    series = chart.addSeries(LineSeries, {
      color: '#00d09c',
      lineWidth: 2,
      lineType: 0,
      crosshairMarkerRadius: 4,
      crosshairMarkerBorderWidth: 2,
      crosshairMarkerBackgroundColor: '#00d09c',
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
