<script>
  /*
   * Version history picker for a saved custom template. Parent
   * owns `state` ({ name, history }) and handles the load-version
   * dispatch on row click.
   */
  import { Button, Modal } from '../../primitives/index.js'

  let { state, onLoadVersion, onClose } = $props()

  const open = $derived(!!state)
</script>

<Modal {open} ariaLabel="Version history" maxWidth="720px" {onClose}>
  {#snippet children()}
    {#if state}
      <h3>{state.name} — version history</h3>
      {#if state.history?.length === 0}
        <div class="muted">no saved versions yet</div>
      {:else}
        <div class="versions-list">
          {#each state.history as v, i (v.hash)}
            <button type="button" class="version-row" onclick={() => onLoadVersion(v.hash)}>
              <span class="v-ix">v{state.history.length - i}</span>
              <code class="v-hash mono">{v.hash.slice(0, 12)}</code>
              <span class="v-when mono">{new Date(v.saved_at).toLocaleString()}</span>
              {#if v.saved_by}<span class="v-by">by <code>{v.saved_by}</code></span>{/if}
              {#if v.description}<span class="v-desc">· {v.description}</span>{/if}
              <span class="v-chev">›</span>
            </button>
          {/each}
        </div>
      {/if}
    {/if}
  {/snippet}
  {#snippet actions()}
    <Button variant="ghost" onclick={onClose}>
      {#snippet children()}Close{/snippet}
    </Button>
  {/snippet}
</Modal>

<style>
  h3 { margin: 0 0 var(--s-3); font-size: var(--fs-lg); font-weight: 600; color: var(--fg-primary); }
  .versions-list { display: flex; flex-direction: column; gap: 4px; max-height: 420px; overflow-y: auto; }
  .version-row {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    cursor: pointer;
    font-size: var(--fs-xs);
    text-align: left;
    transition: border-color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
  }
  .version-row:hover { border-color: var(--accent); background: var(--bg-raised); }
  .v-ix {
    font-family: var(--font-mono); font-weight: 600;
    padding: 1px 6px; background: var(--bg-raised);
    border-radius: var(--r-sm); color: var(--accent);
  }
  .v-hash, .v-when { color: var(--fg-secondary); }
  .v-by { color: var(--fg-muted); font-size: 10px; }
  .v-by code { font-family: var(--font-mono); background: var(--bg-raised); padding: 0 4px; border-radius: 3px; }
  .v-desc { color: var(--fg-muted); flex: 1; }
  .v-chev { color: var(--fg-faint); font-size: var(--fs-md); }
</style>
