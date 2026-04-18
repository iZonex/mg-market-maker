<script>
  import Icon from './Icon.svelte'
  let { data } = $props()
  const alerts = $derived(data.state.alerts || [])

  function sev(severity) {
    const s = (severity || '').toLowerCase()
    if (s === 'critical') return 'neg'
    if (s === 'warning')  return 'warn'
    return 'info'
  }
  function icon(severity) {
    const s = (severity || '').toLowerCase()
    if (s === 'critical') return 'alert'
    if (s === 'warning')  return 'bolt'
    return 'info'
  }
  function fmtTime(t) {
    if (!t) return '—'
    try { return new Date(t).toLocaleTimeString() } catch { return '—' }
  }
</script>

{#if alerts.length === 0}
  <div class="empty-state">
    <span class="empty-state-icon"><Icon name="check" size={18} /></span>
    <span class="empty-state-title">No alerts</span>
    <span class="empty-state-hint">Engine health alerts will appear here in real time.</span>
  </div>
{:else}
  <div class="alerts scroll">
    {#each alerts as a}
      <div class="alert" data-sev={sev(a.severity)}>
        <span class="alert-icon"><Icon name={icon(a.severity)} size={14} /></span>
        <div class="alert-body">
          <div class="alert-head">
            <span class="alert-title">{a.title}</span>
            <span class="alert-time num">{fmtTime(a.timestamp)}</span>
          </div>
          {#if a.message}
            <div class="alert-msg">{a.message}</div>
          {/if}
          {#if a.symbol}
            <span class="chip">{a.symbol}</span>
          {/if}
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .alerts {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    max-height: 280px;
    overflow-y: auto;
  }
  .alert {
    display: flex;
    gap: var(--s-3);
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-lg);
    transition: border-color var(--dur-fast) var(--ease-out);
  }
  .alert[data-sev='neg']  { border-color: rgba(239, 68, 68, 0.35); }
  .alert[data-sev='warn'] { border-color: rgba(245, 158, 11, 0.3); }
  .alert[data-sev='info'] { border-color: var(--border-subtle); }
  .alert-icon {
    flex-shrink: 0;
    width: 24px; height: 24px;
    display: flex; align-items: center; justify-content: center;
    border-radius: 50%;
  }
  .alert[data-sev='neg']  .alert-icon { background: var(--neg-bg);  color: var(--neg); }
  .alert[data-sev='warn'] .alert-icon { background: var(--warn-bg); color: var(--warn); }
  .alert[data-sev='info'] .alert-icon { background: var(--info-bg); color: var(--info); }

  .alert-body { display: flex; flex-direction: column; gap: var(--s-1); min-width: 0; }
  .alert-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--s-2);
  }
  .alert-title {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .alert-time {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    flex-shrink: 0;
  }
  .alert-msg {
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    line-height: var(--lh-snug);
  }
</style>
