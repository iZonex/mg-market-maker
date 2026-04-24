<script>
  /*
   * <SectionHeader> — consistent header row for a section inside
   * a Card or standalone block. Title on the left, optional actions
   * on the right, optional subtitle beneath. Used wherever you
   * have a "labelled group of controls / rows".
   *
   * Differs from `<Card>` header: SectionHeader is for NESTED groups
   * inside a card or page section; Card owns the top-level chrome.
   *
   * Design-system contract: tokens only.
   */

  let {
    title = '',
    subtitle = '',
    /** @type {'sm' | 'md'} — `sm` uses the uppercase-label style, `md` is a regular heading. */
    size = 'sm',
    /** Optional action row (buttons, selects). */
    actions,
  } = $props()
</script>

<header class="section-header size-{size}">
  <div class="titles">
    {#if title}<span class="title">{title}</span>{/if}
    {#if subtitle}<span class="subtitle">{subtitle}</span>{/if}
  </div>
  {#if actions}
    <div class="actions">{@render actions()}</div>
  {/if}
</header>

<style>
  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
    min-width: 0;
  }
  .titles {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    min-width: 0;
  }
  .size-sm .title {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    color: var(--fg-muted);
  }
  .size-md .title {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .subtitle {
    font-size: var(--fs-xs);
    color: var(--fg-muted);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    flex-shrink: 0;
  }
</style>
