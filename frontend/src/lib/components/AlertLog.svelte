<script>
  let { data } = $props()
  const alerts = $derived(data.state.alerts || [])

  function severityIcon(sev) {
    switch (sev) {
      case 'Critical': return '🚨'
      case 'Warning': return '⚠️'
      default: return 'ℹ️'
    }
  }
</script>

<div>
  <h3>Alerts <span class="count">{alerts.length}</span></h3>

  <div class="scroll">
    {#each alerts as alert}
      <div class="alert-row" class:critical={alert.severity === 'Critical'} class:warning={alert.severity === 'Warning'}>
        <span class="icon">{severityIcon(alert.severity)}</span>
        <span class="title">{alert.title}</span>
        <span class="msg">{alert.message}</span>
      </div>
    {/each}
    {#if alerts.length === 0}
      <div class="empty">No alerts</div>
    {/if}
  </div>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.5px; }
  .count { color: #58a6ff; margin-left: 6px; }
  .scroll { max-height: 200px; overflow-y: auto; }
  .alert-row {
    display: flex; gap: 8px; align-items: flex-start;
    padding: 4px 0; border-bottom: 1px solid #0a0e17; font-size: 11px;
  }
  .icon { flex-shrink: 0; }
  .title { font-weight: 600; color: #e1e4e8; white-space: nowrap; }
  .msg { color: #8b949e; overflow: hidden; text-overflow: ellipsis; }
  .critical .title { color: #f85149; }
  .warning .title { color: #d29922; }
  .empty { color: #484f58; text-align: center; padding: 20px; }
</style>
