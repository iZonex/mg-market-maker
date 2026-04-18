<script>
  /*
   * Epic H — custom node renderer.
   *
   * Each node in the DAG renders as a titled card with one
   * Handle per declared input (left side) + output (right side).
   * Handle id must match the port name so svelte-flow's
   * sourceHandle/targetHandle round-trips cleanly into the
   * backend edge schema.
   */

  import { Handle, Position } from '@xyflow/svelte'
  let { data, selected } = $props()

  const isSink = $derived(data.kind.startsWith('Out.'))
  const isSource = $derived(data.inputs.length === 0)
</script>

<div
  class="node"
  class:selected
  class:sink={isSink}
  class:source={isSource}
  class:restricted={data.restricted}
>
  <header>
    <span class="kind">{data.kind}</span>
  </header>
  <div class="ports">
    <div class="in-col">
      {#each data.inputs as p (p.name)}
        <div class="port-row">
          <Handle type="target" position={Position.Left} id={p.name} />
          <span class="port-label">{p.name}</span>
        </div>
      {/each}
    </div>
    <div class="out-col">
      {#each data.outputs as p (p.name)}
        <div class="port-row right">
          <span class="port-label">{p.name}</span>
          <Handle type="source" position={Position.Right} id={p.name} />
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .node {
    min-width: 180px;
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    font-family: var(--font-sans);
    font-size: var(--fs-xs);
    color: var(--fg-primary);
    overflow: hidden;
  }
  .node.selected { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-dim); }
  .node.sink { border-left: 3px solid var(--pos); }
  .node.source { border-left: 3px solid var(--accent); }
  .node.restricted { border-color: var(--neg); }

  header {
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border-bottom: 1px solid var(--border-subtle);
  }
  .kind { font-family: var(--font-mono); font-weight: 600; }
  .ports {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
  }
  .in-col, .out-col { display: flex; flex-direction: column; gap: 4px; }
  .port-row { display: flex; align-items: center; gap: var(--s-2); position: relative; }
  .port-row.right { justify-content: flex-end; }
  .port-label {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    color: var(--fg-secondary);
  }
  /* svelte-flow injects handle circles; keep their positioning tidy. */
  .node :global(.svelte-flow__handle) {
    width: 8px;
    height: 8px;
    background: var(--fg-muted);
    border: 1px solid var(--border-strong);
  }
  .node :global(.svelte-flow__handle:hover) { background: var(--accent); }
</style>
