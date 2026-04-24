<script>
  /*
   * <StatTile> — one KPI. Label on top, big value in the middle,
   * optional delta / hint at the bottom. Used by Overview, Fleet,
   * DeploymentDrilldown, ClientPortal — anywhere you're showing
   * "mid = $50,123", "PnL = +$1,234", etc.
   *
   * Design-system contract:
   *   - Tokens only.
   *   - `tone` colours the VALUE (not the container), matching
   *     the semantic semantics of `<Chip>` / `<StatusPill>`.
   *   - Value + delta both support mono (tabular-nums) via `mono`.
   *   - If a value is currently unknown, pass `—` (or null — we'll
   *     render an em-dash in `--fg-muted`).
   */

  let {
    /** Small uppercase label shown above the value. */
    label = '',
    /** Primary display value. Pre-formatted string. */
    value = null,
    /** Optional small meta under the value (unit, "24h", etc.). */
    meta = '',
    /** Optional signed delta display; rendered in `--pos`/`--neg` based on prefix. */
    delta = null,
    /** @type {'neutral'|'accent'|'positive'|'warn'|'danger'|'info'|'muted'} */
    tone = 'neutral',
    /** Apply tabular-mono font to value+delta. Default on. */
    mono = true,
    /** Render as a compact inline strip instead of the default stacked tile. */
    inline = false,
  } = $props()

  const displayValue = $derived(value === null || value === undefined || value === '' ? '—' : value)
  const isMissing = $derived(value === null || value === undefined || value === '')
  const deltaTone = $derived.by(() => {
    if (delta == null || delta === '') return 'neutral'
    const s = String(delta).trim()
    if (s.startsWith('+')) return 'positive'
    if (s.startsWith('-') || s.startsWith('−')) return 'negative'
    return 'neutral'
  })
</script>

<div class="tile tone-{tone}" class:inline class:missing={isMissing}>
  {#if label}<span class="label">{label}</span>{/if}
  <span class="value" class:mono>{displayValue}</span>
  {#if delta != null && delta !== ''}
    <span class="delta delta-{deltaTone}" class:mono>{delta}</span>
  {:else if meta}
    <span class="meta">{meta}</span>
  {/if}
</div>

<style>
  .tile {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--s-3);
    background: var(--bg-raised);
    border-radius: var(--r-md);
    border: 1px solid var(--border-subtle);
    min-width: 0;
  }
  .tile.inline {
    flex-direction: row;
    align-items: baseline;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
  }
  .label {
    font-size: 10px;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    color: var(--fg-muted);
  }
  .value {
    font-size: var(--fs-lg);
    font-weight: 600;
    color: var(--fg-primary);
    line-height: 1.2;
  }
  .tile.inline .value { font-size: var(--fs-md); }
  .value.mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .meta { font-size: var(--fs-xs); color: var(--fg-muted); }
  .delta { font-size: var(--fs-xs); }
  .delta.mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .delta-positive { color: var(--pos); }
  .delta-negative { color: var(--danger); }
  .delta-neutral  { color: var(--fg-muted); }

  .missing .value { color: var(--fg-muted); }

  /* Tone colourises the value, keeping the tile chrome neutral. */
  .tile.tone-accent   .value { color: var(--accent); }
  .tile.tone-positive .value { color: var(--pos); }
  .tile.tone-warn     .value { color: var(--warn); }
  .tile.tone-danger   .value { color: var(--danger); }
  .tile.tone-info     .value { color: var(--info); }
  .tile.tone-muted    .value { color: var(--fg-muted); }
</style>
