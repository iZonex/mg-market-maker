<script>
  // Card primitive — wraps every panel with consistent header,
  // body, and empty/skeleton state. Three slots: actions (right
  // of title), default (body), and footer.

  let {
    title = '',
    subtitle = '',
    loading = false,
    empty = false,
    emptyTitle = 'No data yet',
    emptyHint = '',
    variant = 'default',   // default | glass | hero
    span = 1,              // 1 | 2 | 3 columns
    children,
    actions,
    footer,
  } = $props()

  const klass = $derived(
    variant === 'glass' ? 'card-glass'
      : variant === 'hero' ? 'card-hero'
      : 'card'
  )
</script>

<section class="card-wrap {klass}" style:grid-column="span {span}">
  {#if title}
    <header class="card-header">
      <div class="card-title-group">
        <h3 class="card-title">{title}</h3>
        {#if subtitle}<span class="card-subtitle">{subtitle}</span>{/if}
      </div>
      {#if actions}
        <div class="card-actions">
          {@render actions()}
        </div>
      {/if}
    </header>
  {/if}

  <div class="card-body">
    {#if loading}
      <div class="skeleton-stack">
        <div class="skeleton" style="height: 20px; width: 70%;"></div>
        <div class="skeleton" style="height: 14px; width: 45%;"></div>
        <div class="skeleton" style="height: 14px; width: 60%;"></div>
      </div>
    {:else if empty}
      <div class="empty-state">
        <span class="empty-state-icon">
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="10"/>
            <line x1="12" y1="8" x2="12" y2="12"/>
            <line x1="12" y1="16" x2="12.01" y2="16"/>
          </svg>
        </span>
        <span class="empty-state-title">{emptyTitle}</span>
        {#if emptyHint}<span class="empty-state-hint">{emptyHint}</span>{/if}
      </div>
    {:else if children}
      {@render children()}
    {/if}
  </div>

  {#if footer}
    <footer class="card-footer">
      {@render footer()}
    </footer>
  {/if}
</section>

<style>
  .card-wrap {
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
    min-width: 0;
  }
  .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-4);
  }
  .card-title-group {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
  }
  .card-title {
    font-size: var(--fs-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    margin: 0;
  }
  .card-subtitle {
    font-size: var(--fs-2xs);
    color: var(--fg-faint);
  }
  .card-actions {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  .card-body {
    flex: 1;
    min-height: 0;
  }
  .card-footer {
    padding-top: var(--s-3);
    border-top: 1px solid var(--border-subtle);
    font-size: var(--fs-xs);
    color: var(--fg-muted);
  }
  .skeleton-stack {
    display: flex; flex-direction: column; gap: var(--s-3);
  }
</style>
