/*
 * chart-theme — bridge between the CSS token layer and the
 * TradingView Lightweight Charts library (which wants hex strings
 * at config time, not CSS custom properties).
 *
 * Reads values straight off `document.documentElement` at call
 * time so a theme-swap (edit `tokens.css`) automatically rethemes
 * every chart the next time it mounts. Callers should invoke
 * `readChartTheme()` inside the chart's init effect, not at
 * module-scope.
 *
 * Rule: no chart component should hardcode a hex colour. Every
 * colour comes through this module.
 */

function cssVar(name, fallback = '') {
  if (typeof document === 'undefined') return fallback
  const v = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim()
  return v || fallback
}

/** Main text + axis colour + background; crosshair is series-1 hue. */
export function readChartTheme() {
  return {
    text: cssVar('--chart-grid', '#a8acb5'),
    bg: 'transparent',
    accent: cssVar('--chart-accent', '#00d09c'),
    series: [
      cssVar('--chart-series-1', '#8b5cf6'),
      cssVar('--chart-series-2', '#10b981'),
      cssVar('--chart-series-3', '#f59e0b'),
      cssVar('--chart-series-4', '#ef4444'),
      cssVar('--chart-series-5', '#3b82f6'),
      cssVar('--chart-series-6', '#ec4899'),
      cssVar('--chart-series-7', '#14b8a6'),
      cssVar('--chart-series-8', '#eab308'),
      cssVar('--chart-series-9', '#f97316'),
      cssVar('--chart-series-10', '#6366f1'),
    ],
  }
}

/** Series colour picker — rotates through the 10-entry palette. */
export function seriesColor(idx) {
  const t = readChartTheme()
  return t.series[idx % t.series.length]
}

/**
 * Build the standard layout/grid/crosshair config shared by every
 * Lightweight-Charts chart in the app. Callers spread this into
 * their `createChart` options.
 */
export function baseChartOptions() {
  const t = readChartTheme()
  return {
    layout: {
      background: { color: t.bg },
      textColor: t.text,
      fontFamily: 'JetBrains Mono, monospace',
      fontSize: 11,
    },
    grid: {
      vertLines: { visible: false },
      horzLines: { visible: false },
    },
    crosshair: {
      vertLine: { color: `${t.accent}70`, labelBackgroundColor: t.accent },
      horzLine: { color: `${t.accent}70`, labelBackgroundColor: t.accent },
    },
    rightPriceScale: { borderVisible: false },
    timeScale: { borderVisible: false, timeVisible: true, secondsVisible: false },
  }
}
