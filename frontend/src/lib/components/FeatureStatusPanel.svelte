<script>
  /*
   * Feature status panel (UX-3).
   *
   * One-stop board for every engine-level feature: shows
   * current state, splits features into "runtime-togglable"
   * (ConfigOverride wire is in place) and "config-only"
   * (reads `config.toml` at startup — requires restart to
   * flip). Runtime toggles fire through the existing
   * `/api/admin/config/{symbol}` path used by `ParamTuner`;
   * config-only features are read-only status rows with a
   * breadcrumb to the TOML key so the operator knows where
   * to look.
   *
   * This is deliberately honest — surfacing a fake toggle
   * on a feature that doesn't listen to it would strand
   * operators. Stage-2b wave can wire ConfigOverride paths
   * for the remaining features one by one.
   */

  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { data, auth } = $props()
  const api = createApiClient(auth)

  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})
  const tunable = $derived(d.tunable_config || {})

  let busy = $state('')
  let status = $state('')

  async function toggle(field, value) {
    busy = field
    status = ''
    try {
      await api.postJson(`/api/admin/config/${encodeURIComponent(sym)}`, {
        field,
        value: String(value),
      })
      status = `${field} set to ${value}`
    } catch (e) {
      status = `Error: ${e.message}`
    } finally {
      busy = ''
    }
  }

  // Runtime-togglable features — these map to
  // `ConfigOverride::*` variants the engine already honors.
  const RUNTIME_TOGGLES = [
    {
      field: 'MomentumEnabled',
      label: 'Momentum alpha',
      hint: 'Book imbalance + trade-flow + microprice signal blend.',
      read: 'momentum_enabled',
    },
    {
      field: 'MarketResilienceEnabled',
      label: 'Market resilience widening',
      hint: 'Auto-widen spreads on liquidity-shock detection.',
      read: 'market_resilience_enabled',
    },
    {
      field: 'AmendEnabled',
      label: 'Amend in-place',
      hint: 'Preserve queue priority via venue amend; off = cancel+replace.',
      read: 'amend_enabled',
    },
    {
      field: 'OtrEnabled',
      label: 'OTR snapshots',
      hint: 'Order-to-trade ratio audit writes (MiCA surveillance).',
      read: 'otr_enabled',
    },
  ]

  // Config-only features — surfaced read-only. Each entry
  // carries the TOML key operators flip + the expected
  // process-restart consequence.
  //
  // Runtime state is pulled from `symData` / `tunable` where
  // the engine publishes it; missing fields render as "—".
  const CONFIG_ONLY_ROWS = $derived([
    {
      label: 'OFI tracker (CKS)',
      toml: 'market_maker.momentum_ofi_enabled',
      value: d.momentum_ofi_ewma !== undefined && d.momentum_ofi_ewma !== null,
    },
    {
      label: 'Learned microprice (offline fit)',
      toml: 'market_maker.momentum_learned_microprice_path',
      value: d.momentum_learned_mp_drift !== undefined
        && d.momentum_learned_mp_drift !== null,
    },
    {
      label: 'Online lMP refit',
      toml: 'market_maker.momentum_learned_microprice_online',
      value: null, // Not surfaced on dashboard state yet.
    },
    {
      label: 'BVC volume classifier',
      toml: 'toxicity.bvc_enabled',
      value: null,
    },
    {
      label: 'SOR inline dispatch',
      toml: 'market_maker.sor_inline_enabled',
      value: null,
    },
    {
      label: 'Margin guard (perp)',
      toml: 'margin.*',
      value: d.margin_ratio !== undefined && d.margin_ratio !== null,
    },
    {
      label: 'Funding accrual (perp)',
      toml: 'exchange.product',
      // Surfaced when engine_product publishes a perp tag.
      value: s.engineProduct === 'linear_perp' || s.engineProduct === 'inverse_perp',
    },
    {
      label: 'Pair screener',
      toml: 'pair_screener',
      value: null,
    },
    {
      label: 'Listing sniper — observer',
      toml: 'listing_sniper.enabled',
      value: null,
    },
    {
      label: 'Listing sniper — auto entry',
      toml: 'listing_sniper_entry.enter_on_discovery',
      value: null,
    },
  ])

  const canControl = $derived(auth?.canControl?.() ?? false)
</script>

<div class="panel">
  <header class="head">
    <span class="label">Engine features</span>
    <span class="sym num">{sym || '—'}</span>
  </header>

  <section class="section">
    <h3 class="section-title">Runtime-togglable</h3>
    <p class="section-hint">
      Flip from the UI — change takes effect on the next
      engine tick via <code>ConfigOverride</code>.
    </p>
    <ul class="rows">
      {#each RUNTIME_TOGGLES as t (t.field)}
        {@const v = !!tunable[t.read]}
        {@const disabled = !canControl || busy === t.field}
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
                onchange={(e) => toggle(t.field, e.currentTarget.checked)}
              />
              <span class="track" class:on={v}></span>
            </label>
          </div>
        </li>
      {/each}
    </ul>
  </section>

  <section class="section">
    <h3 class="section-title">Config-only</h3>
    <p class="section-hint">
      Read at process start from <code>config.toml</code> —
      edit the listed key and restart <code>mm-server</code>
      to apply.
    </p>
    <ul class="rows">
      {#each CONFIG_ONLY_ROWS as r}
        <li class="row">
          <div class="row-left">
            <span class="row-label">{r.label}</span>
            <code class="row-toml">{r.toml}</code>
          </div>
          <div class="row-right">
            {#if r.value === true}
              <span class="chip chip-pos">ACTIVE</span>
            {:else if r.value === false}
              <span class="chip chip-muted">IDLE</span>
            {:else}
              <span class="chip chip-muted">—</span>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  </section>

  {#if status}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>{status}</span>
    </div>
  {/if}

  {#if !canControl}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>Read-only — log in as an operator or admin to flip runtime toggles.</span>
    </div>
  {/if}
</div>

<style>
  .panel { display: flex; flex-direction: column; gap: var(--s-4); }

  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--s-3);
  }
  .sym {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--accent);
  }

  .section { display: flex; flex-direction: column; gap: var(--s-2); }
  .section-title {
    margin: 0;
    font-size: var(--fs-xs);
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
    background: var(--bg-chip);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
  }

  .rows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: var(--s-1); }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .row-left { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .row-label {
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--fg-primary);
  }
  .row-hint {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    line-height: var(--lh-snug);
  }
  .row-toml {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    color: var(--fg-secondary);
  }
  .row-right { display: flex; align-items: center; gap: var(--s-2); }

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
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    word-break: break-word;
  }
</style>
