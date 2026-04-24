<script>
  /*
   * Left-column client selector — list of registered tenants.
   * Click a row to drill into the rollup cards on the right.
   */
  import Card from '../Card.svelte'

  let {
    clients = [],
    selected = null,
    loading = false,
    error = null,
    onSelect,
  } = $props()
</script>

<Card title="Clients" subtitle="registered tenants · click to drill down" span={1}>
  {#snippet children()}
    {#if error}
      <div class="error">{error}</div>
    {:else if loading}
      <div class="muted">Loading…</div>
    {:else if clients.length === 0}
      <div class="empty">No clients registered.</div>
    {:else}
      <div class="client-list">
        {#each clients as c (c.client_id || c.id)}
          {@const id = c.client_id || c.id}
          <button
            type="button"
            class="client-row"
            class:selected={selected === id}
            onclick={() => onSelect(id)}
          >
            <span class="c-name">{c.name || id}</span>
            <span class="c-id mono">{id}</span>
            {#if c.jurisdiction}<span class="c-tag">{c.jurisdiction}</span>{/if}
          </button>
        {/each}
      </div>
    {/if}
  {/snippet}
</Card>

<style>
  .error { color: var(--neg); font-size: var(--fs-sm); }
  .empty {
    padding: var(--s-3); color: var(--fg-muted);
    font-size: var(--fs-sm); text-align: center;
  }
  .client-list { display: flex; flex-direction: column; gap: 4px; }
  .client-row {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: inherit;
    cursor: pointer; text-align: left;
  }
  .client-row:hover { border-color: var(--accent); }
  .client-row.selected { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .c-name { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 500; }
  .c-id { font-size: 10px; color: var(--fg-muted); }
  .c-tag { font-size: 10px; color: var(--fg-muted); }
</style>
