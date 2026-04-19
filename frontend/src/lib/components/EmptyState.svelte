<script>
  /*
   * 23-UX-9 — reusable empty-state component.
   *
   * Every panel was rendering its own version of "No X yet" with
   * slightly different icon sizes + copy styles. This gives a
   * single source of truth so compliance / orderbook /
   * surveillance feel like one app.
   *
   * Usage:
   *   <EmptyState
   *     icon="history"
   *     title="No events yet"
   *     hint="Event stream appears once the engine starts ticking."
   *     variant="waiting"|"done"|"muted" />
   */
  import Icon from './Icon.svelte'
  let {
    icon = 'info',
    title = '',
    hint = '',
    variant = 'muted',
  } = $props()
</script>

<div class="empty" data-variant={variant}>
  <span class="ico"><Icon name={icon} size={20} /></span>
  {#if title}<span class="title">{title}</span>{/if}
  {#if hint}<span class="hint">{hint}</span>{/if}
</div>

<style>
  .empty {
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
  .empty[data-variant='waiting'] .ico { color: var(--info); background: rgba(59, 130, 246, 0.1); }
  .empty[data-variant='done']    .ico { color: var(--pos); background: var(--pos-bg); }
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
