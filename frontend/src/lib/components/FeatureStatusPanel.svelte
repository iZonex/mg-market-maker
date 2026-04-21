<script>
  /*
   * Feature status panel — drilldown edition.
   *
   * Shape contract:
   *   { row: DeploymentStateRow, onPatch: (patch) => Promise, canControl: bool }
   *
   * Reads the `features` map + `variables` map off the row. Runtime
   * toggles shape a variables PATCH (e.g. `momentum_enabled: true`)
   * that the agent's `translate_variable_override` maps to the
   * matching `ConfigOverride` variant. Unknown keys (config-only
   * features, TOML-driven toggles) render as status rows with a
   * breadcrumb — operators see which flags are truly runtime-
   * togglable vs which require a redeploy.
   */

  import Icon from './Icon.svelte'

  let { row, onPatch, canControl = false } = $props()

  const features = $derived(row?.features || {})
  const variables = $derived(row?.variables || {})

  let busyKey = $state('')
  let statusLine = $state('')

  async function toggle(varKey, value) {
    busyKey = varKey
    statusLine = ''
    try {
      await onPatch({ [varKey]: value })
      statusLine = `${varKey} → ${value}`
    } catch (e) {
      statusLine = `Error: ${e.message || e}`
    } finally {
      busyKey = ''
    }
  }

  // Boolean helper — features map wins over variables map for
  // the current state because the agent mirrors `variables.*_enabled`
  // keys into `features` exactly so the dashboard has one source
  // of truth. Variables fallback is in case the feature didn't
  // make it into the harvested map.
  function readBool(varKey) {
    if (typeof features[varKey] === 'boolean') return features[varKey]
    const v = variables[varKey]
    if (typeof v === 'boolean') return v
    return false
  }

  // Runtime-togglable features — variable keys recognised by the
  // agent's `translate_variable_override()`. Keep in sync with the
  // match arms in `crates/agent/src/registry.rs`.
  const RUNTIME_TOGGLES = [
    {
      varKey: 'momentum_enabled',
      label: 'Momentum alpha',
      hint: 'Book imbalance + trade-flow + microprice signal blend.',
    },
    {
      varKey: 'market_resilience_enabled',
      label: 'Market resilience widening',
      hint: 'Auto-widen spreads on liquidity-shock detection.',
    },
    {
      varKey: 'amend_enabled',
      label: 'Amend in-place',
      hint: 'Preserve queue priority via venue amend; off = cancel+replace.',
    },
    {
      varKey: 'otr_enabled',
      label: 'OTR snapshots',
      hint: 'Order-to-trade ratio audit writes (MiCA surveillance).',
    },
  ]

  // Extra features the agent surfaces through the `features` map
  // but that are not runtime-togglable. Rendered read-only with a
  // breadcrumb to where they get configured. Deployment-level
  // features the template opted into stay in this list regardless
  // of whether they currently report `true`.
  const FEATURE_HINTS = {
    momentum_ofi: { label: 'OFI tracker (CKS)', source: 'template: momentum.ofi' },
    bvc_classifier: { label: 'BVC volume classifier', source: 'toxicity.bvc_enabled' },
    sor_inline: { label: 'SOR inline dispatch', source: 'market_maker.sor_inline_enabled' },
  }
</script>

<div class="panel">
  <section class="section">
    <h4 class="section-title">Runtime-togglable</h4>
    <p class="section-hint">
      Flips on the next engine tick via <code>ConfigOverride</code>.
    </p>
    <ul class="rows">
      {#each RUNTIME_TOGGLES as t (t.varKey)}
        {@const v = readBool(t.varKey)}
        {@const disabled = !canControl || busyKey === t.varKey}
        <li class="row">
          <div class="row-left">
            <span class="row-label">{t.label}</span>
            <span class="row-hint">{t.hint}</span>
          </div>
          <div class="row-right">
            <span class="chip" class:chip-pos={v} class:chip-muted={!v}>
              {v ? 'ON' : 'OFF'}
            </span>
            <label class="switch" aria-label={`toggle ${t.label}`}>
              <input
                type="checkbox"
                checked={v}
                {disabled}
                onchange={(e) => toggle(t.varKey, e.currentTarget.checked)}
              />
              <span class="track" class:on={v}></span>
            </label>
          </div>
        </li>
      {/each}
    </ul>
  </section>

  <section class="section">
    <h4 class="section-title">Template features</h4>
    <p class="section-hint">
      Set at deploy time — change the template config and redeploy to flip.
    </p>
    <ul class="rows">
      {#each Object.entries(FEATURE_HINTS) as [key, meta] (key)}
        {@const state = features[key]}
        <li class="row">
          <div class="row-left">
            <span class="row-label">{meta.label}</span>
            <code class="row-src">{meta.source}</code>
          </div>
          <div class="row-right">
            {#if state === true}
              <span class="chip chip-pos">ACTIVE</span>
            {:else if state === false}
              <span class="chip chip-muted">IDLE</span>
            {:else}
              <span class="chip chip-muted">—</span>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  </section>

  {#if statusLine}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>{statusLine}</span>
    </div>
  {/if}

  {#if !canControl}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>Read-only — operator role required to flip toggles.</span>
    </div>
  {/if}
</div>

<style>
  .panel { display: flex; flex-direction: column; gap: var(--s-3); }

  .section { display: flex; flex-direction: column; gap: var(--s-2); }
  .section-title {
    margin: 0;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    font-weight: 600;
  }
  .section-hint {
    margin: 0;
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    line-height: var(--lh-snug);
  }
  .section-hint code {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    padding: 1px 4px;
    background: var(--bg-base);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
  }

  .rows {
    list-style: none; margin: 0; padding: 0;
    display: flex; flex-direction: column; gap: var(--s-1);
  }
  .row {
    display: flex; align-items: center; justify-content: space-between;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
  }
  .row-left { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .row-label {
    font-size: var(--fs-xs);
    font-weight: 500;
    color: var(--fg-primary);
  }
  .row-hint {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    line-height: var(--lh-snug);
  }
  .row-src {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    color: var(--fg-secondary);
  }
  .row-right { display: flex; align-items: center; gap: var(--s-2); }

  .chip {
    font-family: var(--font-mono); font-size: 10px;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-weight: 600; padding: 2px 6px; border-radius: var(--r-sm);
    border: 1px solid currentColor;
  }
  .chip-pos { color: var(--pos); }
  .chip-muted { color: var(--fg-muted); }

  .switch {
    position: relative;
    display: inline-block;
    width: 32px;
    height: 18px;
    cursor: pointer;
  }
  .switch input { opacity: 0; width: 100%; height: 100%; margin: 0; cursor: pointer; }
  .switch input:disabled ~ .track { opacity: 0.4; cursor: not-allowed; }
  .track {
    position: absolute;
    inset: 0;
    background: var(--border-subtle);
    border-radius: 9999px;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .track::before {
    content: "";
    position: absolute;
    width: 14px; height: 14px;
    left: 2px; top: 2px;
    background: var(--fg-primary);
    border-radius: 50%;
    transition: transform var(--dur-fast) var(--ease-out);
  }
  .track.on { background: var(--pos); }
  .track.on::before { transform: translateX(14px); }

  .status-line {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    color: var(--fg-secondary);
    word-break: break-word;
  }
</style>
