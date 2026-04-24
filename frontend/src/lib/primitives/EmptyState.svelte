<script>
  /*
   * <EmptyState> — consistent "there's nothing here yet" placeholder.
   *
   * Every panel used to render its own "No X yet" with slightly
   * different icon sizes + copy styles; this gives a single source
   * of truth so compliance / orderbook / surveillance feel like one
   * app.
   *
   * Design-system contract:
   *   - Tokens only.
   *   - `variant` maps to a semantic tone (waiting=info, done=pos,
   *     muted=neutral). Everything else follows.
   */

  import Icon from '../components/Icon.svelte'

  let {
    icon = 'info',
    title = '',
    hint = '',
    /** @type {'waiting' | 'done' | 'muted'} */
    variant = 'muted',
  } = $props()
</script>

<div class="empty-state" data-variant={variant}>
  <span class="ico"><Icon name={icon} size={20} /></span>
  {#if title}<span class="title">{title}</span>{/if}
  {#if hint}<span class="hint">{hint}</span>{/if}
</div>

<style>
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--s-2);
    padding: var(--s-5);
    min-height: 120px;
    text-align: center;
    color: var(--fg-muted);
  }
  .ico {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    background: var(--bg-chip);
    color: var(--fg-muted);
    margin-bottom: var(--s-1);
  }
  .empty-state[data-variant='waiting'] .ico { color: var(--info); background: var(--info-bg); }
  .empty-state[data-variant='done']    .ico { color: var(--pos); background: var(--pos-bg); }
  .title {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-secondary);
  }
  .hint {
    font-size: var(--fs-xs);
    color: var(--fg-muted);
    max-width: 340px;
    line-height: 1.4;
  }
</style>
