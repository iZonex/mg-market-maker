<script>
  // Adaptive-calibration state (Epic 30 + 31).
  //
  // Reads `SymbolState.adaptive_state` published every symbol-status
  // tick. Shows pair-class badge, online-tuner status, current γ
  // factor with a bipolar bar (0.25× → 4×), and last adjustment
  // reason as a coloured pill.

  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
  const a = $derived(d.adaptive_state || null)

  const pairClass = $derived(a?.pair_class || 'unclassified')
  const enabled = $derived(!!a?.enabled)
  const gammaFactor = $derived(parseFloat(a?.gamma_factor || '1'))
  const reason = $derived(a?.last_reason || 'no_op')

  // Geometric bar: log2(factor) mapped to [-1, 1] across 0.25..4.
  const barPct = $derived((() => {
    const f = Math.max(0.25, Math.min(4.0, gammaFactor))
    const normalised = Math.log2(f) / 2
    return normalised * 50
  })())

  const reasonLabel = $derived(reason.replace(/_/g, ' '))
  const reasonSeverity = $derived((() => {
    switch (reason) {
      case 'tighten_for_fills': return 'pos'
      case 'rate_limited':      return 'info'
      case 'clamped':           return 'warn'
      case 'widen_for_inventory':
      case 'widen_for_negative_edge':
      case 'widen_for_adverse': return 'neg'
      default:                  return 'muted'
    }
  })())
</script>

{#if !a}
  <div class="empty-state">
    <span class="empty-state-icon">
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="12" r="9"/>
        <polyline points="12 7 12 12 15 14"/>
      </svg>
    </span>
    <span class="empty-state-title">Adaptive state warming up</span>
    <span class="empty-state-hint">The engine will publish a snapshot on the next refresh tick.</span>
  </div>
{:else}
  <div class="adaptive">
    <div class="row">
      <span class="label">Pair class</span>
      <span class="chip chip-pc" data-pc={pairClass}>
        {pairClass.replace(/_/g, ' ')}
      </span>
    </div>

    <div class="row">
      <span class="label">Online tuner</span>
      <span class="chip" class:chip-pos={enabled} class:chip-muted={!enabled}>
        {enabled ? 'ENABLED' : 'DISABLED'}
      </span>
    </div>

    <div class="gamma-block">
      <div class="row">
        <span class="label">γ factor</span>
        <span class="val num"
              class:pos={gammaFactor < 1}
              class:neg={gammaFactor > 1}>
          {gammaFactor.toFixed(3)}×
        </span>
      </div>
      <div class="bar-track" aria-hidden="true">
        <div class="bar-center"></div>
        <div
          class="bar-fill"
          class:widen={gammaFactor >= 1}
          class:tighten={gammaFactor < 1}
          style="
            width: {Math.abs(barPct)}%;
            {gammaFactor >= 1 ? 'left: 50%' : 'right: 50%'};
          "></div>
      </div>
      <div class="bar-scale">
        <span>0.25×</span>
        <span>1.0×</span>
        <span>4.0×</span>
      </div>
    </div>

    <div class="row">
      <span class="label">Last reason</span>
      <span class="chip chip-{reasonSeverity}">{reasonLabel}</span>
    </div>

    <p class="hint">
      γ multiplies the base from config on top of the regime-based
      <code>AutoTuner</code> factor. 1.0 = neutral.
    </p>
  </div>
{/if}

<style>
  .adaptive {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--s-3);
  }
  .label {
    font-size: var(--fs-xs);
    color: var(--fg-muted);
    font-weight: 500;
  }
  .val {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }

  .gamma-block {
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
  }
  .bar-track {
    position: relative;
    height: 6px;
    background: var(--bg-chip);
    border-radius: var(--r-pill);
    overflow: visible;
    margin-top: var(--s-2);
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
    margin-top: var(--s-1);
    font-size: var(--fs-2xs);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
  }

  .chip-muted { color: var(--fg-muted); }

  .chip-pc[data-pc^='major']  { color: var(--pc-major-perp); background: rgba(96, 165, 250, 0.14); border-color: rgba(96, 165, 250, 0.35); }
  .chip-pc[data-pc^='alt']    { color: var(--pc-alt-perp);   background: rgba(251, 191, 36, 0.14); border-color: rgba(251, 191, 36, 0.35); }
  .chip-pc[data-pc^='meme']   { color: var(--pc-meme-spot);  background: rgba(236, 72, 153, 0.14); border-color: rgba(236, 72, 153, 0.35); }
  .chip-pc[data-pc^='stable'] { color: var(--pc-stable-stable); background: rgba(16, 185, 129, 0.14); border-color: rgba(16, 185, 129, 0.35); }
  .chip-pc[data-pc='unclassified'] { color: var(--fg-muted); }

  .hint {
    margin: 0;
    font-size: var(--fs-2xs);
    line-height: var(--lh-snug);
    color: var(--fg-muted);
  }
  .hint code {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    padding: 1px 4px;
    background: var(--bg-chip);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
  }
</style>
