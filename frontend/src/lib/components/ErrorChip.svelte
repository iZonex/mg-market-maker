<script>
  /*
   * 23-UX-9 — reusable error chip with optional retry.
   *
   * Usage:
   *   <ErrorChip message={error} onRetry={refresh} />
   */
  import Icon from './Icon.svelte'
  let { message = '', severity = 'neg', onRetry = null } = $props()
</script>

{#if message}
  <div class="err" data-sev={severity}>
    <Icon name="alert" size={14} />
    <span class="msg">{message}</span>
    {#if onRetry}
      <button type="button" class="retry" onclick={onRetry} aria-label="Retry">
        <Icon name="refresh" size={12} />
        <span>Retry</span>
      </button>
    {/if}
  </div>
{/if}

<style>
  .err {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    border-radius: var(--r-md);
    font-size: var(--fs-xs);
  }
  .err[data-sev='neg']  { background: var(--neg-bg); border: 1px solid rgba(239,68,68,0.3);  color: var(--neg); }
  .err[data-sev='warn'] { background: var(--warn-bg); border: 1px solid rgba(245,158,11,0.3); color: var(--warn); }
  .msg { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .retry {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: transparent;
    border: 1px solid currentColor;
    color: inherit;
    border-radius: var(--r-sm);
    padding: 2px var(--s-2);
    font-size: var(--fs-2xs);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .retry:hover { background: rgba(255,255,255,0.08); }
</style>
