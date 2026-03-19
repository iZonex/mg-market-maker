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
      layout: { background: { color: '#161b22' }, textColor: '#8b949e' },
      grid: { vertLines: { color: '#21262d' }, horzLines: { color: '#21262d' } },
      width: container.clientWidth,
      height: 220,
      timeScale: { timeVisible: true, secondsVisible: false },
      rightPriceScale: { borderColor: '#21262d' },
    })

    series = chart.addSeries(LineSeries, { color: '#3fb950', lineWidth: 2 })

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

<div class="chart-panel">
  <h3>PnL ($)</h3>
  <div bind:this={container} class="chart-container"></div>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.5px; }
  .chart-container { width: 100%; }
</style>
