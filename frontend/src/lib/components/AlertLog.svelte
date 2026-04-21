<script>
  /*
   * AlertLog — shows alerts from both sources:
   *   - Local ws DashboardState (single-engine fallback).
   *   - Wave D4 fleet dedup endpoint `/api/v1/alerts/fleet` —
   *     one row per collapsed `(severity, title)` group with
   *     occurrence count + distinct agents. Preferred when
   *     `auth` prop is provided so we can hit HTTP.
   */
  import Icon from './Icon.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { data = null, auth = null } = $props()
  const api = auth ? createApiClient(auth) : null

  let fleetAlerts = $state([])

  async function refreshFleet() {
    if (!api) return
    try {
      const r = await api.getJson('/api/v1/alerts/fleet')
      fleetAlerts = Array.isArray(r) ? r : []
    } catch {
      fleetAlerts = []
    }
  }

  $effect(() => {
    refreshFleet()
    if (!api) return
    const iv = setInterval(refreshFleet, 5000)
    return () => clearInterval(iv)
  })

  // Prefer the fleet dedup feed when available + non-empty.
  // Falls back to ws local state so single-engine tests still
  // render something.
  const alerts = $derived.by(() => {
    if (fleetAlerts.length > 0) {
      return fleetAlerts.map(a => ({
        title: a.title,
        message: a.message,
        severity: a.severity,
        symbol: a.symbol,
        timestamp: new Date(a.ts_ms).toISOString(),
        agents: a.agents || [],
        count: a.count || 1,
      }))
    }
    return (data?.state?.alerts || []).map(a => ({ ...a, agents: [], count: 1 }))
  })

  function sev(severity) {
    const s = (severity || '').toLowerCase()
    if (s === 'critical') return 'neg'
    if (s === 'warning' || s === 'high')  return 'warn'
    return 'info'
  }
  function icon(severity) {
    const s = (severity || '').toLowerCase()
    if (s === 'critical') return 'alert'
    if (s === 'warning' || s === 'high')  return 'bolt'
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
            {#if a.count > 1}
              <span class="count-chip" title={`${a.count} occurrences in dedup window from ${a.agents.length} agent(s)`}>
                ×{a.count}
              </span>
            {/if}
            <span class="alert-time num">{fmtTime(a.timestamp)}</span>
          </div>
          {#if a.message}
            <div class="alert-msg">{a.message}</div>
          {/if}
          <div class="alert-tags">
            {#if a.symbol}
              <span class="chip">{a.symbol}</span>
            {/if}
            {#each a.agents as ag (ag)}
              <span class="chip mono">{ag}</span>
            {/each}
          </div>
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
  .alert-tags { display: flex; gap: 4px; flex-wrap: wrap; margin-top: 2px; }
  .count-chip {
    font-size: 10px; font-family: var(--font-mono);
    padding: 1px 6px; border-radius: var(--r-sm);
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    color: var(--accent);
  }
  .mono { font-family: var(--font-mono); }
</style>
