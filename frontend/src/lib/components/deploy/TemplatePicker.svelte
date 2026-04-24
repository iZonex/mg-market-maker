<script>
  /*
   * Template picker — catalogue rows grouped by category, each
   * card shows name + risk band + description + recommended-for
   * + caveats. Parent owns `selected` template name and receives
   * onPick(name) on click.
   */

  let {
    templates = [],
    selected = '',
    disabled = false,
    onPick,
  } = $props()

  const grouped = $derived.by(() => {
    const by = new Map()
    for (const t of templates) {
      const k = t.category || 'other'
      if (!by.has(k)) by.set(k, [])
      by.get(k).push(t)
    }
    return Array.from(by.entries()).map(([cat, items]) => ({ category: cat, items }))
  })
</script>

<div class="templates">
  {#each grouped as g (g.category)}
    <div class="template-group">
      <div class="group-label">{g.category}</div>
      <div class="group-items">
        {#each g.items as t (t.name)}
          <button
            type="button"
            class="template-card"
            class:selected={selected === t.name}
            onclick={() => onPick(t.name)}
            {disabled}
          >
            <div class="t-head">
              <span class="t-name mono">{t.name}</span>
              {#if t.risk_band}
                <span class="risk-chip risk-{t.risk_band}">{t.risk_band}</span>
              {/if}
            </div>
            <span class="t-desc">{t.description}</span>
            {#if t.recommended_for}
              <div class="t-tip">
                <span class="tip-k">for</span>
                <span class="tip-v">{t.recommended_for}</span>
              </div>
            {/if}
            {#if t.caveats}
              <div class="t-tip caveat">
                <span class="tip-k">⚠</span>
                <span class="tip-v">{t.caveats}</span>
              </div>
            {/if}
          </button>
        {/each}
      </div>
    </div>
  {/each}
</div>

<style>
  .templates { display: flex; flex-direction: column; gap: var(--s-3); }
  .template-group { display: flex; flex-direction: column; gap: 6px; }
  .group-label {
    font-size: 10px; color: var(--fg-faint);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .group-items {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: var(--s-2);
  }
  .template-card {
    display: flex; flex-direction: column; align-items: flex-start; gap: 4px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    cursor: pointer;
    text-align: left;
    transition: border-color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
  }
  .template-card:hover { border-color: var(--border-default); background: var(--bg-chip-hover, var(--bg-raised)); }
  .template-card.selected { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 8%, transparent); }
  .template-card:disabled { opacity: 0.5; cursor: not-allowed; }
  .t-head { display: flex; align-items: center; gap: var(--s-2); width: 100%; }
  .t-name { font-size: var(--fs-sm); font-weight: 600; color: var(--fg-primary); flex: 1; }
  .t-desc { font-size: 11px; color: var(--fg-muted); line-height: 1.4; }
  .risk-chip {
    padding: 1px 6px; font-size: 9px;
    text-transform: uppercase; letter-spacing: var(--tracking-label); font-weight: 600;
    border-radius: var(--r-sm); font-family: var(--font-mono);
  }
  .risk-chip.risk-low    { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .risk-chip.risk-medium { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .risk-chip.risk-high   { background: color-mix(in srgb, var(--danger) 22%, transparent); color: var(--danger); }
  .t-tip {
    display: flex; gap: 6px;
    margin-top: 4px; padding: 4px 8px;
    background: var(--bg-chip); border-radius: var(--r-sm);
    font-size: 10px; line-height: 1.45;
  }
  .t-tip .tip-k { color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); font-weight: 600; flex-shrink: 0; }
  .t-tip .tip-v { color: var(--fg-secondary); }
  .t-tip.caveat { background: color-mix(in srgb, var(--warn) 10%, transparent); }
  .t-tip.caveat .tip-k { color: var(--warn); }
</style>
