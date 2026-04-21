<script>
  /*
   * Adaptive-state panel — drilldown edition.
   *
   * Shape contract: { row: DeploymentStateRow }
   *
   * Surfaces the trio the adaptive calibration loop publishes via
   * Prometheus gauges + the agent's telemetry harvest:
   *   - regime             (volatility regime label)
   *   - adaptive_gamma     (γ the tuner landed on, decimal string)
   *   - adaptive_reason    (one-line why of the last adjustment)
   *
   * Falls back to a warming-up placeholder while any of the
   * fields is unpopulated — agents sample these at engine cadence
   * so a freshly-started deployment legitimately has "—" for a
   * few seconds before the gauges write.
   */

  let { row } = $props()

  const regime = $derived(row?.regime || '')
  const gammaStr = $derived(row?.adaptive_gamma || '')
  const reason = $derived(row?.adaptive_reason || '')
  const killLevel = $derived(row?.kill_level ?? 0)

  const gammaValue = $derived.by(() => {
    const n = parseFloat(gammaStr)
    return Number.isFinite(n) && n > 0 ? n : null
  })

  const hasAny = $derived(Boolean(regime) || gammaValue !== null || Boolean(reason))

  // Geometric bar: log2(γ / 1) mapped to [-1, 1] across 0.25..4×,
  // which covers the adaptive tuner's typical clamp band.
  const barPct = $derived.by(() => {
    if (gammaValue === null) return 0
    const ref = 1.0
    const f = Math.max(0.25, Math.min(4.0, gammaValue / ref))
    return (Math.log2(f) / 2) * 50
  })

  const regimeTone = $derived.by(() => {
    switch ((regime || '').toLowerCase()) {
      case 'quiet': return 'muted'
      case 'trending': return 'info'
      case 'volatile': return 'warn'
      case 'meanreverting': case 'mean-reverting': return 'pos'
      default: return 'muted'
    }
  })

  const reasonTone = $derived.by(() => {
    const r = (reason || '').toLowerCase()
    if (r.includes('tighten') || r.includes('fill')) return 'pos'
    if (r.includes('clamp') || r.includes('rate')) return 'warn'
    if (r.includes('widen') || r.includes('adverse') || r.includes('toxic')) return 'neg'
    return 'muted'
  })
</script>

{#if !hasAny}
  <div class="empty">
    <span class="empty-title">Adaptive state warming up</span>
    <span class="empty-hint">
      The engine publishes regime + γ + reason every tick. Give it
      a few seconds after spawn.
    </span>
  </div>
{:else}
  <div class="adaptive">
    <div class="row">
      <span class="label">Regime</span>
      <span class="chip tone-{regimeTone}">{regime || '—'}</span>
    </div>

    <div class="row">
      <span class="label">Kill level</span>
      {#if killLevel === 0}
        <span class="chip tone-muted">NORMAL</span>
      {:else}
        <span class="chip tone-danger">LEVEL {killLevel}</span>
      {/if}
    </div>

    <div class="gamma-block">
      <div class="row">
        <span class="label">γ (adaptive)</span>
        <span class="val num"
              class:pos={gammaValue !== null && gammaValue < 1}
              class:neg={gammaValue !== null && gammaValue > 1}>
          {gammaValue !== null ? gammaValue.toFixed(4) : '—'}
        </span>
      </div>
      {#if gammaValue !== null}
        <div class="bar-track" aria-hidden="true">
          <div class="bar-center"></div>
          <div
            class="bar-fill"
            class:widen={gammaValue >= 1}
            class:tighten={gammaValue < 1}
            style="
              width: {Math.abs(barPct)}%;
              {gammaValue >= 1 ? 'left: 50%' : 'right: 50%'};
            "></div>
        </div>
        <div class="bar-scale">
          <span>0.25×</span>
          <span>1.0×</span>
          <span>4.0×</span>
        </div>
      {/if}
    </div>

    <div class="row">
      <span class="label">Last reason</span>
      <span class="chip tone-{reasonTone}">{reason || 'no_op'}</span>
    </div>

    <p class="hint">
      γ is multiplicative on top of the regime-based baseline. Live
      tuning overrides from ParamTuner flow through the same
      channel.
    </p>
  </div>
{/if}

<style>
  .empty {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
  }
  .empty-title { font-size: var(--fs-xs); color: var(--fg-primary); font-weight: 500; }
  .empty-hint  { font-size: var(--fs-2xs); color: var(--fg-muted); line-height: var(--lh-snug); }

  .adaptive { display: flex; flex-direction: column; gap: var(--s-2); }
  .row {
    display: flex; justify-content: space-between; align-items: center;
    gap: var(--s-3);
  }
  .label {
    font-size: var(--fs-xs);
    color: var(--fg-muted);
    font-weight: 500;
  }
  .val {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-primary);
    font-family: var(--font-mono); font-variant-numeric: tabular-nums;
  }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }
  .num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }

  .chip {
    font-family: var(--font-mono); font-size: 10px;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-weight: 600; padding: 2px 6px; border-radius: var(--r-sm);
    border: 1px solid currentColor;
  }
  .tone-pos { color: var(--pos); }
  .tone-neg { color: var(--neg); }
  .tone-danger { color: var(--neg); }
  .tone-warn { color: var(--warn); }
  .tone-info { color: var(--accent); }
  .tone-muted { color: var(--fg-muted); }

  .gamma-block {
    display: flex; flex-direction: column; gap: 4px;
    margin-top: var(--s-1);
  }
  .bar-track {
    position: relative;
    height: 6px;
    background: var(--bg-base);
    border-radius: var(--r-pill);
    margin-top: var(--s-1);
  }
  .bar-center {
    position: absolute;
    left: 50%; top: -3px; bottom: -3px;
    width: 2px;
    background: var(--border-strong);
    border-radius: 1px;
  }
  .bar-fill {
    position: absolute;
    top: 0;
    height: 100%;
    border-radius: var(--r-pill);
    transition: width 320ms var(--ease-out), background 200ms var(--ease-out);
  }
  .bar-fill.widen   { background: var(--neg); }
  .bar-fill.tighten { background: var(--pos); }
  .bar-scale {
    display: flex;
    justify-content: space-between;
    margin-top: 2px;
    font-size: 10px;
    font-family: var(--font-mono);
    color: var(--fg-muted);
  }

  .hint {
    margin: 0;
    font-size: var(--fs-2xs);
    line-height: var(--lh-snug);
    color: var(--fg-muted);
  }
</style>
