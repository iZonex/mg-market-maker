<script>
  /*
   * Epic H — custom node renderer.
   *
   * One port per full-width row so each Handle sits flush to the
   * node's left or right edge (svelte-flow positions Handle against
   * its nearest block ancestor — a two-column grid pushed handles
   * into the middle of the card). Inputs come first (handles on the
   * left), then outputs (handles on the right). Port-type drives the
   * handle colour so the operator can see at a glance which outputs
   * can feed which inputs.
   */

  import { Handle, Position } from '@xyflow/svelte'
  let { data, selected } = $props()

  const isSink = $derived(data.kind.startsWith('Out.'))
  const isSource = $derived(data.inputs.length === 0)
  const category = $derived(data.group ?? data.kind.split('.')[0])
  // Prefer the human label delivered by the catalog; fall back to
  // the raw kind suffix so older graph payloads still render
  // something readable.
  const name = $derived(
    data.label || data.kind.split('.').slice(1).join('.') || data.kind
  )

  // Port-type → CSS class used by both the handle and its label so
  // a Bool port looks the same everywhere it appears.
  function typeClass(t) {
    return `t-${(t || 'unit').toLowerCase()}`
  }

  // Short glyph per port type — nothing fancy, just visible in the
  // tiny 10px label space next to the port name.
  function typeGlyph(t) {
    return {
      Number: '#',
      Bool: '✓',
      Unit: '·',
      String: 'A',
      KillLevel: '!',
      StrategyKind: 'S',
      PairClass: 'P',
    }[t] ?? '·'
  }
</script>

<div
  class="node"
  class:selected
  class:sink={isSink}
  class:source={isSource}
  class:restricted={data.restricted}
  data-category={category}
>
  <header>
    <span class="category">{category}</span>
    <span class="kind">{name}</span>
  </header>

  {#if data.inputs.length > 0}
    <div class="port-group inputs">
      {#each data.inputs as p (p.name)}
        <div class="port-row input">
          <Handle type="target" position={Position.Left} id={p.name} class={typeClass(p.type)} />
          <span class="port-glyph {typeClass(p.type)}" title={p.type}>{typeGlyph(p.type)}</span>
          <span class="port-label">{p.name}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if data.outputs.length > 0}
    <div class="port-group outputs">
      {#each data.outputs as p (p.name)}
        <div class="port-row output">
          <span class="port-label">{p.name}</span>
          <span class="port-glyph {typeClass(p.type)}" title={p.type}>{typeGlyph(p.type)}</span>
          <Handle type="source" position={Position.Right} id={p.name} class={typeClass(p.type)} />
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .node {
    min-width: 210px;
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-left: 4px solid var(--fg-muted);
    border-radius: var(--r-md);
    font-family: var(--font-sans);
    font-size: var(--fs-xs);
    color: var(--fg-primary);
    overflow: hidden;
    box-shadow: 0 2px 6px rgba(0, 0, 0, 0.35);
  }
  .node.selected {
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent-dim), 0 4px 10px rgba(0, 0, 0, 0.45);
  }
  .node.restricted { border-color: var(--neg); }

  /* Category colour band (left edge). */
  .node[data-category="Source"]     { border-left-color: #7dd3fc; }
  .node[data-category="Book"]       { border-left-color: #7dd3fc; }
  .node[data-category="Sentiment"]  { border-left-color: #c4b5fd; }
  .node[data-category="News"]       { border-left-color: #c4b5fd; }
  .node[data-category="Volatility"] { border-left-color: #fde68a; }
  .node[data-category="Inventory"]  { border-left-color: #fda4af; }
  .node[data-category="Toxicity"]   { border-left-color: #fda4af; }
  .node[data-category="Momentum"]   { border-left-color: #fda4af; }
  .node[data-category="Signal"]     { border-left-color: #fda4af; }
  .node[data-category="Regime"]     { border-left-color: #fda4af; }
  .node[data-category="Indicator"]  { border-left-color: #fde68a; }
  .node[data-category="Indicators"] { border-left-color: #fde68a; }
  .node[data-category="Stats"]      { border-left-color: #fde68a; }
  .node[data-category="Math"]       { border-left-color: #d4d4d8; }
  .node[data-category="Logic"]      { border-left-color: #d4d4d8; }
  .node[data-category="Cast"]       { border-left-color: #d4d4d8; }
  .node[data-category="PairClass"]  { border-left-color: #c4b5fd; }
  .node[data-category="Strategy"]   { border-left-color: #86efac; }
  .node[data-category="Risk"]       { border-left-color: #fb923c; }
  .node[data-category="Exec"]       { border-left-color: #86efac; }
  .node[data-category="Out"]        { border-left-color: #ef4444; }

  header {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border-bottom: 1px solid var(--border-subtle);
  }
  .category {
    font-family: var(--font-mono); font-size: 9px;
    color: var(--fg-muted); text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .kind {
    font-family: var(--font-mono); font-weight: 600; font-size: var(--fs-sm);
    color: var(--fg-primary);
  }

  .port-group {
    display: flex; flex-direction: column;
    padding: 4px 0;
  }
  .port-group.inputs { border-bottom: 1px dashed var(--border-subtle); }
  .port-group.outputs { background: rgba(255, 255, 255, 0.02); }

  .port-row {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: 3px var(--s-3);
    min-height: 22px;
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
  }
  .port-row.output { justify-content: flex-end; }
  .port-label { color: var(--fg-secondary); }
  .port-row:hover .port-label { color: var(--fg-primary); }

  .port-glyph {
    display: inline-flex; align-items: center; justify-content: center;
    width: 14px; height: 14px;
    border-radius: 3px;
    font-size: 9px; font-weight: 700;
    background: var(--bg-chip); color: var(--fg-muted);
    border: 1px solid var(--border-subtle);
  }

  /* Port-type palette — echoes the typeClass() output. */
  .t-number { --pt: #60a5fa; }  /* blue */
  .t-bool   { --pt: #34d399; }  /* emerald */
  .t-unit   { --pt: #9ca3af; }  /* gray */
  .t-string { --pt: #c084fc; }  /* purple */
  .t-killlevel   { --pt: #f87171; }  /* red */
  .t-strategykind{ --pt: #fbbf24; }  /* amber */
  .t-pairclass   { --pt: #fb7185; }  /* rose */

  .port-glyph.t-number,
  .port-glyph.t-bool,
  .port-glyph.t-unit,
  .port-glyph.t-string,
  .port-glyph.t-killlevel,
  .port-glyph.t-strategykind,
  .port-glyph.t-pairclass {
    border-color: var(--pt);
    color: var(--pt);
    background: color-mix(in srgb, var(--pt) 12%, transparent);
  }

  /* svelte-flow handle — colour matches its port type, sits flush to
   * the node edge since .port-row now spans full width. */
  .node :global(.svelte-flow__handle) {
    width: 11px; height: 11px;
    background: var(--bg-base);
    border: 2px solid var(--fg-muted);
    border-radius: 50%;
    transition: transform 0.1s, box-shadow 0.1s;
  }
  .node :global(.svelte-flow__handle.t-number)       { border-color: #60a5fa; }
  .node :global(.svelte-flow__handle.t-bool)         { border-color: #34d399; }
  .node :global(.svelte-flow__handle.t-unit)         { border-color: #9ca3af; }
  .node :global(.svelte-flow__handle.t-string)       { border-color: #c084fc; }
  .node :global(.svelte-flow__handle.t-killlevel)    { border-color: #f87171; }
  .node :global(.svelte-flow__handle.t-strategykind) { border-color: #fbbf24; }
  .node :global(.svelte-flow__handle.t-pairclass)    { border-color: #fb7185; }

  .node :global(.svelte-flow__handle:hover) {
    transform: scale(1.35);
    box-shadow: 0 0 0 4px rgba(125, 211, 252, 0.2);
  }
  /* Connected handle — xyflow adds `connectingfrom`/`connectable` but
   * to show "has an edge" we rely on data-handleid + a child selector
   * that xyflow exposes when connections exist. */
  .node :global(.svelte-flow__handle.connectionindicator) {
    background: currentColor;
  }
</style>
