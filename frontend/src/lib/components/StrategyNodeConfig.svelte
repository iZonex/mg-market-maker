<script>
  /*
   * Epic H — right-pane node config panel.
   *
   * Renders the selected node's kind + port declarations + an
   * auto-generated form for its `config` JSON. Phase 1 supports
   * only the two configurable nodes we ship (`Stats.EWMA`,
   * `Cast.ToBool`); unknown-kind configs fall back to a raw
   * textarea so power users aren't blocked.
   */

  let { node, onUpdate, onDelete } = $props()

  const kind = $derived(node?.data?.kind ?? '')
  const label = $derived(node?.data?.label ?? kind)
  const summary = $derived(node?.data?.summary ?? '')
  const cfg = $derived(node?.data?.config ?? {})
  const hasConfig = $derived(configurableKinds.has(kind))

  // Kinds whose `from_config` accepts user-authored fields; anything
  // else just shows the header + delete so the panel doesn't fake
  // configurability the node can't honour.
  const configurableKinds = new Set([
    'Stats.EWMA', 'Math.Const', 'Cast.ToBool', 'Cast.StrategyEq',
    'Cast.PairClassEq', 'Risk.ToxicityWiden', 'Risk.InventoryUrgency',
    'Risk.CircuitBreaker', 'Indicator.SMA', 'Indicator.EMA',
    'Indicator.HMA', 'Indicator.RSI', 'Indicator.ATR',
    'Indicator.Bollinger', 'Exec.TwapConfig', 'Exec.VwapConfig',
    'Exec.PovConfig', 'Exec.IcebergConfig',
  ])

  function update(field, v) {
    const next = { ...cfg, [field]: v }
    onUpdate?.(next)
  }
</script>

<div class="pane">
  {#if !node}
    <header>
      <span class="title">Node config</span>
    </header>
    <div class="empty">Select a node on the canvas to edit its config.</div>
  {:else}
    <header class="node-head">
      <span class="kind-label">{label}</span>
      {#if summary}<span class="kind-summary">{summary}</span>{/if}
    </header>

    {#if kind === 'Stats.EWMA'}
      <label class="field">
        <span>α (0,1]</span>
        <input
          type="text"
          value={cfg.alpha ?? '0.1'}
          oninput={(e) => update('alpha', e.currentTarget.value)}
        />
      </label>
    {:else if kind === 'Math.Const'}
      <label class="field">
        <span>value</span>
        <input
          type="text"
          value={cfg.value ?? '0'}
          oninput={(e) => update('value', e.currentTarget.value)}
        />
      </label>
    {:else if kind === 'Cast.ToBool'}
      <label class="field">
        <span>threshold</span>
        <input
          type="text"
          value={cfg.threshold ?? '0'}
          oninput={(e) => update('threshold', e.currentTarget.value)}
        />
      </label>
      <label class="field">
        <span>comparator</span>
        <select
          value={cfg.cmp ?? 'ge'}
          onchange={(e) => update('cmp', e.currentTarget.value)}
        >
          <option value="ge">≥</option>
          <option value="gt">&gt;</option>
          <option value="le">≤</option>
          <option value="lt">&lt;</option>
          <option value="eq">=</option>
        </select>
      </label>
    {:else if kind === 'Cast.StrategyEq'}
      <label class="field">
        <span>target strategy</span>
        <select
          value={cfg.target ?? 'AvellanedaStoikov'}
          onchange={(e) => update('target', e.currentTarget.value)}
        >
          <option value="AvellanedaStoikov">AvellanedaStoikov</option>
          <option value="GLFT">GLFT</option>
          <option value="Grid">Grid</option>
          <option value="Basis">Basis</option>
          <option value="CrossExchange">CrossExchange</option>
        </select>
      </label>
    {:else if kind === 'Cast.PairClassEq'}
      <label class="field">
        <span>target pair class</span>
        <input
          type="text"
          value={cfg.target ?? 'MajorSpot'}
          oninput={(e) => update('target', e.currentTarget.value)}
        />
      </label>
    {:else if kind === 'Risk.ToxicityWiden'}
      <label class="field">
        <span>scale (mult at vpin=1)</span>
        <input
          type="text"
          value={cfg.scale ?? '2'}
          oninput={(e) => update('scale', e.currentTarget.value)}
        />
      </label>
    {:else if kind === 'Risk.InventoryUrgency'}
      <label class="field">
        <span>cap</span>
        <input
          type="text"
          value={cfg.cap ?? '1'}
          oninput={(e) => update('cap', e.currentTarget.value)}
        />
      </label>
      <label class="field">
        <span>exponent</span>
        <input
          type="text"
          value={cfg.exponent ?? '2'}
          oninput={(e) => update('exponent', e.currentTarget.value)}
        />
      </label>
    {:else if kind === 'Risk.CircuitBreaker'}
      <label class="field">
        <span>wide spread (bps)</span>
        <input
          type="text"
          value={cfg.wide_bps ?? '100'}
          oninput={(e) => update('wide_bps', e.currentTarget.value)}
        />
      </label>
    {:else if kind === 'Indicator.SMA' || kind === 'Indicator.EMA' || kind === 'Indicator.HMA' || kind === 'Indicator.RSI' || kind === 'Indicator.ATR'}
      <label class="field">
        <span>period</span>
        <input
          type="number"
          min="1"
          max="10000"
          value={cfg.period ?? 14}
          oninput={(e) => update('period', Number(e.currentTarget.value))}
        />
      </label>
    {:else if kind === 'Indicator.Bollinger'}
      <label class="field">
        <span>period</span>
        <input
          type="number"
          min="1"
          max="10000"
          value={cfg.period ?? 20}
          oninput={(e) => update('period', Number(e.currentTarget.value))}
        />
      </label>
      <label class="field">
        <span>k (std dev)</span>
        <input
          type="text"
          value={cfg.k_stddev ?? '2'}
          oninput={(e) => update('k_stddev', e.currentTarget.value)}
        />
      </label>
    {:else if kind === 'Exec.TwapConfig'}
      <label class="field">
        <span>duration (sec)</span>
        <input type="number" min="1" value={cfg.duration_secs ?? 120}
          oninput={(e) => update('duration_secs', Number(e.currentTarget.value))} />
      </label>
      <label class="field">
        <span>slice count</span>
        <input type="number" min="1" max="1000" value={cfg.slice_count ?? 5}
          oninput={(e) => update('slice_count', Number(e.currentTarget.value))} />
      </label>
    {:else if kind === 'Exec.VwapConfig'}
      <label class="field">
        <span>duration (sec)</span>
        <input type="number" min="1" value={cfg.duration_secs ?? 300}
          oninput={(e) => update('duration_secs', Number(e.currentTarget.value))} />
      </label>
    {:else if kind === 'Exec.PovConfig'}
      <label class="field">
        <span>target participation (%)</span>
        <input type="number" min="1" max="100" value={cfg.target_pct ?? 10}
          oninput={(e) => update('target_pct', Number(e.currentTarget.value))} />
      </label>
    {:else if kind === 'Exec.IcebergConfig'}
      <label class="field">
        <span>display qty</span>
        <input type="text" value={cfg.display_qty ?? '0.1'}
          oninput={(e) => update('display_qty', e.currentTarget.value)} />
      </label>
    {:else if !hasConfig}
      <div class="muted small">No parameters — this node takes its values from the engine.</div>
    {/if}

    <div class="actions">
      <button type="button" class="btn danger" onclick={onDelete}>
        Delete node
      </button>
    </div>
  {/if}
</div>

<style>
  .pane { padding: var(--s-3); display: flex; flex-direction: column; gap: var(--s-3); }
  header .title {
    font-size: 11px; text-transform: uppercase;
    letter-spacing: var(--tracking-label); color: var(--fg-muted); font-weight: 600;
  }
  .node-head {
    display: flex; flex-direction: column; gap: 3px;
    padding-bottom: var(--s-2);
    border-bottom: 1px solid var(--border-subtle);
  }
  .kind-label {
    font-family: var(--font-sans); font-size: 14px; font-weight: 600;
    color: var(--fg-primary);
  }
  .kind-summary {
    font-family: var(--font-sans); font-size: 11px; line-height: 1.4;
    color: var(--fg-muted);
  }
  .empty, .muted { color: var(--fg-muted); font-size: var(--fs-xs); }
  .small { font-size: var(--fs-2xs); }

  .field { display: flex; flex-direction: column; gap: 2px; }
  .field span { font-size: var(--fs-2xs); color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .field input, .field select {
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md); color: var(--fg-primary);
    font-family: var(--font-mono); font-size: var(--fs-xs);
  }

  .actions { display: flex; justify-content: flex-end; }
  .btn {
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md); color: var(--fg-primary);
    font-size: var(--fs-xs); cursor: pointer;
  }
  .btn.danger { border-color: var(--neg); color: var(--neg); }
  .btn.danger:hover { background: var(--neg); color: var(--bg-base); }
</style>
