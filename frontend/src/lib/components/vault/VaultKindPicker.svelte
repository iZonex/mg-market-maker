<script>
  /*
   * Kind-picker gallery — operator picks what kind of secret
   * they want to add before the form shape is locked in.
   */
  import Card from '../Card.svelte'
  import { Button } from '../../primitives/index.js'
  import { KINDS } from '../../vault-kinds.js'

  let { onPick, onCancel } = $props()
</script>

<Card title="Pick a kind" subtitle="vault stores more than just exchange keys — any service credential lives here" span={1}>
  {#snippet children()}
    <div class="kind-gallery">
      {#each KINDS as k (k.value)}
        <button type="button" class="kind-card" onclick={() => onPick(k.value)}>
          <div class="kind-name">{k.label}</div>
          <div class="kind-hint">{k.hint}</div>
          <div class="kind-fields">
            {#each k.values as v (v.key)}
              <span class="kind-chip">{v.label}</span>
            {/each}
            {#if k.value === 'exchange'}
              <span class="kind-chip push">pushed to agents</span>
            {:else}
              <span class="kind-chip local">server-local</span>
            {/if}
          </div>
        </button>
      {/each}
    </div>
    <div class="actions">
      <Button variant="ghost" onclick={onCancel}>
        {#snippet children()}Cancel{/snippet}
      </Button>
    </div>
  {/snippet}
</Card>

<style>
  .kind-gallery {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: var(--s-3);
    margin-bottom: var(--s-3);
  }
  .kind-card {
    text-align: left;
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    cursor: pointer;
    display: flex; flex-direction: column; gap: 6px;
    transition: border-color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
    font-family: var(--font-sans);
    color: inherit;
  }
  .kind-card:hover {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 5%, transparent);
  }
  .kind-name { font-size: var(--fs-sm); font-weight: 600; color: var(--fg-primary); }
  .kind-hint { font-size: var(--fs-xs); color: var(--fg-muted); line-height: 1.4; }
  .kind-fields { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 2px; }
  .kind-chip {
    font-family: var(--font-mono); font-size: 10px;
    padding: 1px 6px;
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
  }
  .kind-chip.push { color: var(--accent); background: color-mix(in srgb, var(--accent) 8%, transparent); }
  .kind-chip.local { color: var(--fg-muted); }

  .actions { display: flex; gap: var(--s-2); justify-content: flex-end; }
</style>
