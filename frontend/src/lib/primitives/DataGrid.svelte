<script>
  /*
   * <DataGrid> — dense tabular data with consistent header /
   * row / cell styling. Accepts a column spec + row objects;
   * each row's cell can be overridden via a per-column snippet.
   *
   * Design-system contract:
   *   - Tokens only.
   *   - Columns declare { key, label, align?, width?, mono? }.
   *   - Rows render via the `row` snippet if provided, else a
   *     simple `row[col.key]` lookup.
   *
   * Usage:
   *   <DataGrid
   *     columns={[
   *       { key: 'symbol', label: 'Symbol', mono: true },
   *       { key: 'pnl',    label: 'PnL',    align: 'right', mono: true },
   *     ]}
   *     rows={list}
   *     onRowClick={(r) => open(r.id)}
   *   />
   */

  let {
    /** @type {{ key: string, label: string, align?: 'left'|'right'|'center', width?: string, mono?: boolean }[]} */
    columns = [],
    /** @type {any[]} */
    rows = [],
    /** Key extractor; defaults to index. */
    rowKey = (r, i) => r?.id ?? i,
    /** Optional click handler on a row (makes rows interactive). */
    onRowClick = null,
    /** Empty-state text when rows.length === 0. */
    emptyText = 'no data',
    /** Per-row cell override. Receives `{ row, column }`. */
    cell,
  } = $props()
</script>

<div class="grid" role="table">
  <div class="head" role="row">
    {#each columns as c}
      <div
        class="th align-{c.align ?? 'left'}"
        role="columnheader"
        style={c.width ? `flex-basis: ${c.width}; flex-grow: 0;` : ''}
      >
        {c.label}
      </div>
    {/each}
  </div>

  {#if rows.length === 0}
    <div class="empty">{emptyText}</div>
  {:else}
    {#each rows as row, i (rowKey(row, i))}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="row"
        class:clickable={onRowClick}
        role="row"
        onclick={() => onRowClick?.(row)}
      >
        {#each columns as c}
          <div
            class="td align-{c.align ?? 'left'}"
            class:mono={c.mono}
            role="cell"
            style={c.width ? `flex-basis: ${c.width}; flex-grow: 0;` : ''}
          >
            {#if cell}
              {@render cell({ row, column: c })}
            {:else}
              {row?.[c.key] ?? '—'}
            {/if}
          </div>
        {/each}
      </div>
    {/each}
  {/if}
</div>

<style>
  .grid {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-raised);
    overflow: hidden;
  }
  .head {
    display: flex;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-raised-2);
    border-bottom: 1px solid var(--border-subtle);
    font-size: 10px;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    color: var(--fg-muted);
    font-weight: 600;
  }
  .th, .td {
    flex: 1;
    padding: 0 var(--s-2);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row {
    display: flex;
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--fs-sm);
    color: var(--fg-primary);
  }
  .row:last-child { border-bottom: none; }
  .row.clickable { cursor: pointer; }
  .row.clickable:hover { background: var(--bg-chip-hover); }
  .td.mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .align-left   { text-align: left; }
  .align-right  { text-align: right;  justify-content: flex-end; }
  .align-center { text-align: center; justify-content: center; }
  .empty {
    padding: var(--s-4);
    text-align: center;
    color: var(--fg-muted);
    font-size: var(--fs-xs);
  }
</style>
