<script>
  /*
   * Epic H — node palette.
   *
   * Single-column list grouped by `meta.group`. Groups are
   * collapsible + have sticky headers so an operator can scan
   * the whole catalog without losing track of which section
   * they're in. Each chip is a two-line card (label + summary)
   * — never a raw kind string, that's tooltip-only.
   */

  import Icon from './Icon.svelte'

  let { catalog = [], onAdd } = $props()

  let query = $state('')

  const grouped = $derived.by(() => {
    const q = query.trim().toLowerCase()
    const match = (e) =>
      !q ||
      `${e.label ?? ''} ${e.summary ?? ''} ${e.kind}`.toLowerCase().includes(q)

    const g = new Map()
    for (const e of catalog) {
      if (!match(e)) continue
      const bucket = e.group ?? e.kind.split('.')[0]
      if (!g.has(bucket)) g.set(bucket, [])
      g.get(bucket).push(e)
    }
    const order = ['Sources', 'Indicators', 'Math', 'Logic', 'Risk', 'Exec', 'Sinks']
    return [...g.entries()].sort((a, b) => {
      const ia = order.indexOf(a[0])
      const ib = order.indexOf(b[0])
      if (ia === -1 && ib === -1) return a[0].localeCompare(b[0])
      if (ia === -1) return 1
      if (ib === -1) return -1
      return ia - ib
    })
  })

  let collapsed = $state({})
  function toggle(bucket) {
    collapsed = { ...collapsed, [bucket]: !collapsed[bucket] }
  }

  function startDrag(e, kind) {
    e.dataTransfer?.setData('mm/strategy-kind', kind)
    e.dataTransfer.effectAllowed = 'copy'
  }
</script>

<div class="palette">
  <div class="head">
    <span class="title">Nodes</span>
    <span class="count">{catalog.length}</span>
  </div>

  <div class="search">
    <Icon name="search" size={12} />
    <input
      type="search"
      placeholder="Filter nodes…"
      bind:value={query}
      autocomplete="off"
      spellcheck="false"
    />
    {#if query}
      <button type="button" class="clear" onclick={() => (query = '')} aria-label="Clear">
        <Icon name="close" size={10} />
      </button>
    {/if}
  </div>

  <div class="scroll">
    {#each grouped as [bucket, entries] (bucket)}
      <section class="group" data-group={bucket}>
        <button type="button" class="group-head" onclick={() => toggle(bucket)}>
          <Icon name={collapsed[bucket] ? 'chevronR' : 'chevronDown'} size={10} />
          <span class="group-label">{bucket}</span>
          <span class="group-count">{entries.length}</span>
        </button>
        {#if !collapsed[bucket]}
          <div class="group-body">
            {#each entries as e (e.kind)}
              <button
                type="button"
                class="chip"
                class:restricted={e.restricted}
                draggable="true"
                ondragstart={(ev) => startDrag(ev, e.kind)}
                onclick={() => onAdd?.(e.kind)}
                title={`${e.label ?? e.kind}
${e.summary ?? ''}
${e.kind} · ${e.inputs.length} in · ${e.outputs.length} out${e.restricted ? ' · restricted' : ''}`}
              >
                <span class="chip-name">{e.label ?? e.kind.split('.').slice(1).join('.')}</span>
                <span class="chip-shape">
                  {e.inputs.length}<span class="sep">→</span>{e.outputs.length}
                </span>
              </button>
            {/each}
          </div>
        {/if}
      </section>
    {/each}

    {#if grouped.length === 0}
      <div class="empty">No nodes match “{query}”</div>
    {/if}
  </div>
</div>

<style>
  /* Sidebar column: fixed layout, its own scroll. */
  .palette {
    display: flex; flex-direction: column;
    height: 100%;
    background: var(--bg-raised);
  }

  .head {
    display: flex; align-items: center; justify-content: space-between;
    padding: var(--s-3) var(--s-3) var(--s-2);
  }
  .title {
    font-size: 11px; font-weight: 600; color: var(--fg-primary);
    text-transform: uppercase; letter-spacing: var(--tracking-label);
  }
  .count {
    font-family: var(--font-mono); font-size: 10px; color: var(--fg-muted);
    padding: 2px 6px; background: var(--bg-chip);
    border: 1px solid var(--border-subtle); border-radius: var(--r-pill);
  }

  .search {
    display: flex; align-items: center; gap: var(--s-2);
    margin: 0 var(--s-3) var(--s-3);
    padding: 0 var(--s-2);
    height: 28px;
    background: var(--bg-base); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-muted);
  }
  .search:focus-within { border-color: var(--accent); color: var(--accent); }
  .search input {
    flex: 1; min-width: 0;
    background: transparent; border: 0; outline: none;
    color: var(--fg-primary);
    font-family: var(--font-sans); font-size: 12px;
    height: 100%;
  }
  .search input::placeholder { color: var(--fg-muted); }
  .clear {
    display: inline-flex; align-items: center; justify-content: center;
    width: 16px; height: 16px;
    background: transparent; border: 0; cursor: pointer;
    color: var(--fg-muted); border-radius: var(--r-sm);
  }
  .clear:hover { color: var(--fg-primary); background: var(--bg-chip); }

  .scroll {
    flex: 1; overflow-y: auto; overflow-x: hidden;
    border-top: 1px solid var(--border-subtle);
  }
  .scroll::-webkit-scrollbar { width: 8px; }
  .scroll::-webkit-scrollbar-thumb {
    background: var(--border-subtle); border-radius: 4px;
  }

  .group { border-bottom: 1px solid var(--border-subtle); }
  .group-head {
    position: sticky; top: 0; z-index: 1;
    display: flex; align-items: center; gap: var(--s-2);
    width: 100%;
    padding: var(--s-2) var(--s-3);
    height: 28px;
    background: var(--bg-raised); border: 0; cursor: pointer;
    color: var(--fg-secondary);
    font-family: var(--font-sans); font-size: 11px; font-weight: 600;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
  }
  .group-head:hover { color: var(--fg-primary); }
  .group-label { letter-spacing: var(--tracking-label); }
  .group-count {
    margin-left: auto;
    font-family: var(--font-mono); font-size: 10px; font-weight: 400;
    color: var(--fg-muted); text-transform: none; letter-spacing: 0;
  }
  /* Category accent strip on the group header's left edge. */
  .group[data-group="Sources"]     .group-head { box-shadow: inset 2px 0 0 #7dd3fc; }
  .group[data-group="Indicators"]  .group-head { box-shadow: inset 2px 0 0 #fde68a; }
  .group[data-group="Math"]        .group-head { box-shadow: inset 2px 0 0 #d4d4d8; }
  .group[data-group="Logic"]       .group-head { box-shadow: inset 2px 0 0 #d4d4d8; }
  .group[data-group="Risk"]        .group-head { box-shadow: inset 2px 0 0 #fb923c; }
  .group[data-group="Exec"]        .group-head { box-shadow: inset 2px 0 0 #86efac; }
  .group[data-group="Sinks"]       .group-head { box-shadow: inset 2px 0 0 #ef4444; }

  .group-body {
    display: flex; flex-direction: column; gap: 2px;
    padding: 2px var(--s-2) var(--s-2);
  }

  /* Single-row chip: label (truncated) on the left, shape pill on
   * the right. Summary lives in the title tooltip so it never breaks
   * the layout, and the whole thing has one explicit height. */
  .chip {
    display: flex; align-items: center; gap: 8px;
    width: 100%; min-width: 0; box-sizing: border-box;
    height: 28px;
    padding: 0 8px;
    background: var(--bg-chip); border: 1px solid transparent;
    border-radius: var(--r-sm); color: var(--fg-primary);
    cursor: grab; user-select: none; text-align: left;
    transition: background 0.08s, border-color 0.08s;
  }
  .chip:hover { background: var(--bg-base); border-color: var(--accent); }
  .chip:active { cursor: grabbing; }
  .chip.restricted { border-color: var(--neg); }

  .chip-name {
    flex: 1; min-width: 0;
    font-family: var(--font-sans); font-size: 12px; font-weight: 500;
    line-height: 1;
    color: var(--fg-primary);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .chip-shape {
    flex-shrink: 0;
    display: inline-flex; align-items: center; gap: 1px;
    font-family: var(--font-mono); font-size: 10px; line-height: 1;
    color: var(--fg-muted);
    padding: 2px 5px; background: var(--bg-base);
    border: 1px solid var(--border-subtle); border-radius: 3px;
  }
  .chip-shape .sep { opacity: 0.55; margin: 0 1px; }

  .empty {
    padding: var(--s-4) var(--s-3);
    color: var(--fg-muted); font-size: 12px;
    text-align: center;
  }
</style>
