<script>
  /*
   * Epic H — node palette.
   *
   * Reads the catalog (delivered by the parent page — one network
   * call per session) and renders draggable chips grouped by
   * prefix (`Math.*`, `Logic.*`, …). Click adds to canvas at a
   * default position; drag emits the `mm/strategy-kind` data type
   * the canvas handler consumes.
   */

  let { catalog = [], onAdd } = $props()

  // Group by the substring before `.` — "Math.Add" → "Math".
  const grouped = $derived(() => {
    const g = new Map()
    for (const e of catalog) {
      const bucket = e.kind.split('.')[0]
      if (!g.has(bucket)) g.set(bucket, [])
      g.get(bucket).push(e)
    }
    return [...g.entries()].sort((a, b) => a[0].localeCompare(b[0]))
  })

  function startDrag(e, kind) {
    e.dataTransfer?.setData('mm/strategy-kind', kind)
    e.dataTransfer.effectAllowed = 'copy'
  }
</script>

<div class="palette">
  <header>
    <span class="label">Nodes</span>
    <span class="hint">drag or click</span>
  </header>
  {#each grouped() as [bucket, entries] (bucket)}
    <div class="group">
      <div class="group-label">{bucket}</div>
      {#each entries as e (e.kind)}
        <button
          type="button"
          class="chip"
          class:restricted={e.restricted}
          draggable="true"
          ondragstart={(ev) => startDrag(ev, e.kind)}
          onclick={() => onAdd?.(e.kind)}
          title={`${e.kind} — ${e.inputs.length} in, ${e.outputs.length} out${e.restricted ? ' (restricted)' : ''}`}
        >
          <span class="chip-kind">{e.kind.split('.').slice(1).join('.')}</span>
          <span class="chip-shape">{e.inputs.length}→{e.outputs.length}</span>
        </button>
      {/each}
    </div>
  {/each}
</div>

<style>
  .palette { padding: var(--s-3); display: flex; flex-direction: column; gap: var(--s-3); }
  header { display: flex; justify-content: space-between; align-items: baseline; padding: 0 var(--s-2); }
  .label { font-size: var(--fs-xs); text-transform: uppercase; letter-spacing: var(--tracking-label); color: var(--fg-primary); font-weight: 600; }
  .hint { font-size: var(--fs-2xs); color: var(--fg-muted); }
  .group { display: flex; flex-direction: column; gap: 2px; }
  .group-label {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    padding: var(--s-2) var(--s-2) 2px;
  }
  .chip {
    display: flex; justify-content: space-between; align-items: center;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md); color: var(--fg-primary);
    font-family: var(--font-mono); font-size: var(--fs-xs);
    cursor: grab; user-select: none; text-align: left;
  }
  .chip:hover { background: var(--bg-base); border-color: var(--accent); color: var(--accent); }
  .chip:active { cursor: grabbing; }
  .chip.restricted { border-color: var(--neg); }
  .chip-kind { font-weight: 600; }
  .chip-shape { font-size: var(--fs-2xs); color: var(--fg-muted); }
</style>
