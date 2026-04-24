<script>
  /*
   * Custom-graph paste area + "load JSON file" helper for
   * deploying a graph exported from the Strategy page. Parent
   * owns `graphText` + `filename`; this component surfaces the
   * textarea + file picker and reports parse errors inline.
   */
  import Icon from '../Icon.svelte'

  let {
    graphText = $bindable(''),
    filename = $bindable(''),
    disabled = false,
    onError,
  } = $props()

  async function loadFromFile(event) {
    const file = event.target.files?.[0]
    if (!file) return
    const text = await file.text()
    try {
      JSON.parse(text)
    } catch (e) {
      onError?.(`graph file is not valid JSON: ${e.message}`)
      return
    }
    graphText = text
    filename = file.name
    onError?.(null)
  }
</script>

<div class="graph-actions">
  <label class="file-btn">
    <input type="file" accept=".json,application/json" onchange={loadFromFile} {disabled} />
    Load JSON file…
  </label>
  {#if filename}
    <span class="graph-file mono">{filename}</span>
  {/if}
</div>
<textarea
  class="graph-text"
  rows="12"
  spellcheck="false"
  bind:value={graphText}
  placeholder={`{\n  "nodes": [ ... ],\n  "edges": [ ... ]\n}`}
  {disabled}
></textarea>
<div class="tpl-note">
  <Icon name="info" size={12} />
  <span>Author the graph visually on Strategy page, use the Export button, then paste here (or load the exported file).</span>
</div>

<style>
  .graph-actions { display: flex; align-items: center; gap: var(--s-3); margin-bottom: var(--s-2); }
  .file-btn {
    display: inline-flex; align-items: center; gap: var(--s-2);
    padding: 4px 10px;
    font-size: var(--fs-xs);
    border-radius: var(--r-sm);
    border: 1px solid var(--border-subtle);
    color: var(--fg-primary);
    background: transparent;
    cursor: pointer;
    position: relative;
    overflow: hidden;
  }
  .file-btn:hover { background: var(--bg-chip-hover); border-color: var(--border-default); }
  .file-btn input[type="file"] {
    position: absolute; inset: 0; opacity: 0; cursor: pointer;
  }
  .graph-file { font-size: var(--fs-xs); color: var(--fg-secondary); }

  .graph-text {
    width: 100%; resize: vertical;
    padding: 9px 12px;
    background: color-mix(in srgb, var(--bg-raised) 50%, transparent);
    color: var(--fg-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    line-height: 1.5;
    min-height: 220px;
    outline: none;
  }
  .graph-text:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-ring); }

  .tpl-note {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 6px 10px;
    font-size: var(--fs-xs);
    color: var(--fg-muted);
  }
</style>
