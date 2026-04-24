<script>
  /*
   * M2-GOBS — per-node inspector sidebar for Live mode.
   *
   * Replaces StrategyNodeConfig when StrategyPage is observing a
   * deployed graph. For the selected node shows:
   *   - latest output + type
   *   - sparkline history (last 20 values if numeric)
   *   - hit rate % over the trace window
   *   - avg elapsed_ns per tick
   *   - status chip (ok / source / error / dormant)
   *   - a "Return to authoring" button to flip back to edit mode
   *
   * When no node is selected, renders a summary: total traces in
   * window, kill_level history, dead-node count from graphAnalysis.
   */

  import { formatValue } from '../graphLiveStore.svelte.js'

  import { Button } from '../primitives/index.js'

  let {
    node = null,
    stats = null, // nodeStats Map — see graphLiveStore.nodeStatsFromTraces
    graphAnalysis = null,
    traces = [],
    lastFetch = null,
    error = null,
    onReturnToAuthoring = () => {},
  } = $props()

  const row = $derived(node && stats ? stats.get(node.id) : null)

  const sparkline = $derived.by(() => {
    if (!row || !Array.isArray(row.history) || row.history.length === 0) return []
    // Pull numeric values only; non-numeric outputs render as flat 0.
    const nums = row.history.map((v) => {
      if (v && typeof v === 'object' && 'Number' in v) {
        const n = Number(v.Number)
        return Number.isFinite(n) ? n : null
      }
      if (typeof v === 'number') return v
      return null
    })
    return nums
  })

  const sparklineSvg = $derived.by(() => {
    const pts = sparkline
    const w = 180
    const h = 32
    if (!pts || pts.length === 0) return null
    const valid = pts.filter((n) => n !== null)
    if (valid.length === 0) return { w, h, path: `M0 ${h / 2} L${w} ${h / 2}`, flat: true }
    const min = Math.min(...valid)
    const max = Math.max(...valid)
    const range = max - min || 1
    const step = w / Math.max(pts.length - 1, 1)
    let d = ''
    pts.forEach((v, i) => {
      const x = i * step
      const y = v === null ? h / 2 : h - ((v - min) / range) * (h - 4) - 2
      d += `${i === 0 ? 'M' : 'L'}${x.toFixed(1)} ${y.toFixed(1)} `
    })
    return { w, h, path: d.trim(), flat: false, min, max }
  })

  const deadNodeIds = $derived(graphAnalysis?.dead_nodes ?? [])
  const isDead = $derived(node ? deadNodeIds.includes(node.id) : false)

  const latestOutput = $derived.by(() => {
    if (!row || !Array.isArray(row.history) || row.history.length === 0) return null
    return row.history[row.history.length - 1]
  })

  function statusLabel(s) {
    if (!s) return '—'
    return s
  }
  function statusTone(s) {
    switch (s) {
      case 'ok':
        return 'ok'
      case 'source':
        return 'info'
      case 'error':
        return 'bad'
      default:
        return 'muted'
    }
  }
</script>

<div class="inspector">
  <header class="insp-head">
    <span class="insp-title">Live inspector</span>
    <Button variant="ghost" size="sm" onclick={onReturnToAuthoring}>
          {#snippet children()}Back to authoring{/snippet}
        </Button>
  </header>

  {#if error}
    <div class="err">stream error: {error}</div>
  {/if}

  {#if node}
    <div class="node-panel">
      <div class="node-id-row">
        <code class="kind">{node.data?.kind ?? '?'}</code>
        {#if isDead}
          <span class="chip bad">dead — no path to sink</span>
        {/if}
      </div>

      <div class="kv-grid">
        <div class="kv">
          <span class="k">status</span>
          <span class="v"><span class="chip tone-{statusTone(row?.lastStatus)}">{statusLabel(row?.lastStatus)}</span></span>
        </div>
        <div class="kv">
          <span class="k">hit rate</span>
          <span class="v mono">{row ? (row.hitRate * 100).toFixed(0) + '%' : '—'}</span>
        </div>
        <div class="kv">
          <span class="k">avg elapsed</span>
          <span class="v mono">
            {row && row.avgElapsedNs > 0 ? `${(row.avgElapsedNs / 1000).toFixed(1)} µs` : '—'}
          </span>
        </div>
        <div class="kv">
          <span class="k">last output</span>
          <span class="v mono">{formatValue(latestOutput)}</span>
        </div>
      </div>

      {#if row?.lastError}
        <div class="err">error: {row.lastError}</div>
      {/if}

      {#if sparklineSvg}
        <div class="spark">
          <div class="spark-label">
            <span>history</span>
            {#if !sparklineSvg.flat}
              <span class="spark-range mono">
                {sparklineSvg.min.toFixed(2)} … {sparklineSvg.max.toFixed(2)}
              </span>
            {/if}
          </div>
          <svg viewBox="0 0 {sparklineSvg.w} {sparklineSvg.h}" width={sparklineSvg.w} height={sparklineSvg.h}>
            <path d={sparklineSvg.path} stroke="var(--accent)" stroke-width="1.5" fill="none" />
          </svg>
        </div>
      {:else}
        <div class="spark-empty muted small">no numeric history yet</div>
      {/if}
    </div>
  {:else}
    <div class="summary">
      <div class="summary-head">Fleet graph overview</div>
      <div class="kv-grid">
        <div class="kv">
          <span class="k">ticks in window</span>
          <span class="v mono">{traces.length}</span>
        </div>
        <div class="kv">
          <span class="k">dead nodes</span>
          <span class="v mono" class:alert={(graphAnalysis?.dead_nodes?.length ?? 0) > 0}>
            {graphAnalysis?.dead_nodes?.length ?? 0}
          </span>
        </div>
        <div class="kv">
          <span class="k">required sources</span>
          <span class="v mono">{graphAnalysis?.required_sources?.length ?? 0}</span>
        </div>
        <div class="kv">
          <span class="k">unconsumed</span>
          <span class="v mono" class:warn={(graphAnalysis?.unconsumed_outputs?.length ?? 0) > 0}>
            {graphAnalysis?.unconsumed_outputs?.length ?? 0}
          </span>
        </div>
      </div>
      <div class="summary-hint muted small">
        Select a node on the canvas to see its per-tick stats.
      </div>
      {#if lastFetch}
        <div class="foot muted small">last poll {lastFetch.toLocaleTimeString()}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .inspector {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    padding: var(--s-3);
    height: 100%;
    overflow-y: auto;
  }
  .insp-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .insp-title {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-primary);
  }

  .node-panel, .summary {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
  }
  .node-id-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-2);
    flex-wrap: wrap;
  }
  .kind {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    color: var(--fg-primary);
    padding: 2px 6px;
    background: var(--bg-chip);
    border-radius: var(--r-sm);
  }

  .kv-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s-2);
  }
  .kv {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--s-2);
    background: var(--bg-raised);
    border-radius: var(--r-sm);
  }
  .k {
    font-size: 10px;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    color: var(--fg-muted);
  }
  .v {
    font-size: var(--fs-xs);
    color: var(--fg-primary);
  }
  .v.alert { color: var(--danger); }
  .v.warn { color: var(--warn); }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }

  .chip {
    display: inline-block;
    padding: 1px 6px;
    font-size: 10px;
    font-family: var(--font-mono);
    border-radius: var(--r-sm);
    background: var(--bg-chip);
  }
  .chip.tone-ok { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .chip.tone-bad { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
  .chip.tone-info { background: color-mix(in srgb, var(--accent) 14%, transparent); color: var(--accent); }
  .chip.tone-muted { color: var(--fg-muted); }
  .chip.bad {
    background: color-mix(in srgb, var(--danger) 18%, transparent);
    color: var(--danger);
  }

  .err {
    padding: var(--s-2);
    font-size: var(--fs-xs);
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    border-radius: var(--r-sm);
  }

  .spark {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: var(--s-2);
    background: var(--bg-raised);
    border-radius: var(--r-sm);
  }
  .spark-label {
    display: flex;
    justify-content: space-between;
    font-size: 10px;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .spark-range { color: var(--fg-secondary); font-size: 10px; }
  .spark-empty { padding: var(--s-2); text-align: center; }

  .summary-head {
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--fg-primary);
  }
  .summary-hint { line-height: 1.4; }
  .foot { margin-top: var(--s-2); }
  .muted { color: var(--fg-muted); }
  .small { font-size: 10px; }

</style>
