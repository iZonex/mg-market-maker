<script>
  /*
   * Epic H — right-pane node config panel.
   *
   * Schema-driven: catalog ships a `config_schema` per kind, the
   * frontend renders one row per field automatically. Adding a new
   * configurable node becomes a pure-Rust change — no UI touch.
   *
   * The engine still validates the final config server-side on
   * deploy, so the form is a convenience, not a trust boundary.
   */

  let { node, onUpdate, onDelete } = $props()

  const kind = $derived(node?.data?.kind ?? '')
  const label = $derived(node?.data?.label ?? kind)
  const summary = $derived(node?.data?.summary ?? '')
  const cfg = $derived(node?.data?.config ?? {})
  const schema = $derived(node?.data?.configSchema ?? [])

  function update(field, v) {
    onUpdate?.({ ...cfg, [field]: v })
  }

  // Widget kinds use `snake_case` on the wire; match them exactly.
  function widgetOf(f) {
    return f.widget?.kind ?? 'text'
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
      {#if node.data.restricted}
        <span class="restricted-tag">RESTRICTED · pentest only</span>
      {/if}
    </header>

    {#if schema.length === 0}
      <div class="muted small">
        No parameters — this node takes its values from the engine.
      </div>
    {:else}
      {#each schema as f (f.name)}
        <label class="field stacked">
          <span class="field-label">{f.label}</span>
          {#if widgetOf(f) === 'number'}
            <input
              type="text"
              inputmode="decimal"
              value={cfg[f.name] ?? f.default}
              oninput={(e) => update(f.name, e.currentTarget.value)}
            />
          {:else if widgetOf(f) === 'integer'}
            <input
              type="number"
              step="1"
              min={f.widget.min ?? null}
              max={f.widget.max ?? null}
              value={cfg[f.name] ?? f.default}
              oninput={(e) => update(f.name, Number(e.currentTarget.value))}
            />
          {:else if widgetOf(f) === 'bool'}
            <label class="inline-bool">
              <input
                type="checkbox"
                checked={cfg[f.name] ?? f.default}
                onchange={(e) => update(f.name, e.currentTarget.checked)}
              />
              <span>{f.label}</span>
            </label>
          {:else if widgetOf(f) === 'enum'}
            <select
              value={cfg[f.name] ?? f.default}
              onchange={(e) => update(f.name, e.currentTarget.value)}
            >
              {#each f.widget.options as opt (opt.value)}
                <option value={opt.value}>{opt.label}</option>
              {/each}
            </select>
          {:else}
            <input
              type="text"
              value={cfg[f.name] ?? f.default}
              oninput={(e) => update(f.name, e.currentTarget.value)}
            />
          {/if}
          {#if f.hint}<span class="hint">{f.hint}</span>{/if}
        </label>
      {/each}
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
  .restricted-tag {
    margin-top: 4px;
    font-family: var(--font-mono); font-size: 10px; font-weight: 600;
    color: var(--neg); letter-spacing: var(--tracking-label);
  }
  .empty, .muted { color: var(--fg-muted); font-size: var(--fs-xs); }
  .small { font-size: var(--fs-2xs); }

  .field.stacked {
    display: flex; flex-direction: column; gap: 4px;
  }
  .field-label {
    font-family: var(--font-mono); font-size: 10px;
    color: var(--fg-muted); text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .field input, .field select {
    height: 28px;
    padding: 0 var(--s-3);
    background: var(--bg-base); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary);
    font-family: var(--font-mono); font-size: 12px;
  }
  .field input:focus, .field select:focus {
    outline: none; border-color: var(--accent);
  }
  .hint {
    font-size: 10px; color: var(--fg-muted); line-height: 1.3;
  }
  .inline-bool {
    display: flex; align-items: center; gap: 8px;
    font-size: 12px; color: var(--fg-primary);
  }

  .actions { display: flex; justify-content: flex-end; margin-top: var(--s-2); }
  .btn {
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary);
    font-size: var(--fs-xs); cursor: pointer;
  }
  .btn.danger { border-color: var(--neg); color: var(--neg); }
  .btn.danger:hover { background: var(--neg); color: var(--bg-base); }
</style>
