<script>
  import { onMount } from 'svelte'

  let { data } = $props()
  let container
  let chart
  let series

  onMount(async () => {
    const { createChart } = await import('lightweight-charts')

    chart = createChart(container, {
      layout: {
        background: { color: '#161b22' },
        textColor: '#8b949e',
      },
      grid: {
        vertLines: { color: '#21262d' },
        horzLines: { color: '#21262d' },
      },
      width: container.clientWidth,
      height: 230,
      timeScale: { timeVisible: true, secondsVisible: true },
      rightPriceScale: { borderColor: '#21262d' },
    })

    series = chart.addLineSeries({
      color: '#3fb950',
      lineWidth: 2,
    })

    // Resize observer.
    const ro = new ResizeObserver(() => {
      chart.applyOptions({ width: container.clientWidth })
    })
    ro.observe(container)

    return () => { ro.disconnect(); chart.remove() }
  })

  // Update chart when new data arrives.
  $effect(() => {
    const history = data.state.pnlHistory
    if (series && history.length > 0) {
      const chartData = history.map(p => ({
        time: Math.floor(p.time / 1000),
        value: p.value,
      }))
      series.setData(chartData)
    }
  })
</script>

<div class="chart-panel">
  <h3>PnL</h3>
  <div bind:this={container} class="chart-container"></div>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.5px; }
  .chart-container { width: 100%; }
</style>
