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
  const cfg = $derived(node?.data?.config ?? {})

  function update(field, v) {
    const next = { ...cfg, [field]: v }
    onUpdate?.(next)
  }
</script>

<div class="pane">
  <header>
    <span class="label">Node config</span>
  </header>

  {#if !node}
    <div class="empty">Select a node on the canvas to edit its config.</div>
  {:else}
    <div class="meta">
      <div class="meta-row"><span>Kind</span><code>{kind}</code></div>
      <div class="meta-row"><span>ID</span><code class="small">{node.id.slice(0, 8)}…</code></div>
      <div class="meta-row">
        <span>Ports</span>
        <code class="small">{node.data.inputs.length} in · {node.data.outputs.length} out</code>
      </div>
    </div>

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
    {:else}
      <div class="muted small">This node has no config parameters.</div>
    {/if}

    <div class="actions">
      <button type="button" class="btn danger" onclick={onDelete}>Delete node</button>
    </div>
  {/if}
</div>

<style>
  .pane { padding: var(--s-3); display: flex; flex-direction: column; gap: var(--s-3); }
  header .label {
    font-size: var(--fs-xs); text-transform: uppercase;
    letter-spacing: var(--tracking-label); color: var(--fg-primary); font-weight: 600;
  }
  .empty, .muted { color: var(--fg-muted); font-size: var(--fs-xs); }
  .small { font-size: var(--fs-2xs); }
  .meta { display: flex; flex-direction: column; gap: 2px; padding: var(--s-2) 0; }
  .meta-row {
    display: flex; justify-content: space-between; align-items: center;
    font-size: var(--fs-xs); color: var(--fg-muted);
    padding: var(--s-2) 0;
    border-bottom: 1px solid var(--border-subtle);
  }
  .meta-row span { text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .meta-row code { font-family: var(--font-mono); color: var(--fg-primary); }

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
