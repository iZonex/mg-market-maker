<script>
  /*
   * Kill switch — L5 emergency dashboard.
   *
   * Top-level admin entry (was a card on the old AdminPage).
   * Polls fleet every 5 s, lists every deployment with
   * kill_level > 0, one-click to Fleet drilldown for reset /
   * widen / cancel-all / flatten. Also exposes the raw ops
   * quick-links for preflight / diagnostics / metrics.
   */
  import Card from '../components/Card.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth, onNavigate = () => {} } = $props()
  const api = $derived(createApiClient(auth))

  let escalated = $state([])
  let lastKillFetch = $state(null)

  async function refreshEscalated() {
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const rows = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if ((d.kill_level || 0) > 0) {
            rows.push({ agent_id: a.agent_id, deployment: d })
          }
        }
      }
      escalated = rows
      lastKillFetch = new Date()
    } catch { /* retry next tick */ }
  }

  $effect(() => {
    refreshEscalated()
    const iv = setInterval(refreshEscalated, 5000)
    return () => clearInterval(iv)
  })
</script>

<div class="page scroll">
  <div class="grid">
    <Card
      title="Kill switch"
      subtitle={escalated.length > 0 ? `${escalated.length} deployment(s) escalated` : 'no escalations · all clean'}
      span={2}
    >
      {#snippet children()}
        {#if escalated.length === 0}
          <div class="empty">
            <strong>All clean.</strong>
            No deployments are currently kill-escalated. Controls live on
            each deployment's drilldown — open Fleet and click the row.
            <div class="empty-actions">
              <button type="button" class="linklike" onclick={() => onNavigate('fleet')}>
                Open Fleet →
              </button>
            </div>
          </div>
        {:else}
          <div class="esc-list">
            {#each escalated as r (r.agent_id + '/' + r.deployment.deployment_id)}
              <button
                type="button"
                class="esc-row"
                onclick={() => onNavigate('fleet')}
                title="Opens Fleet — click the deployment to drill down"
              >
                <span class="esc-kill">L{r.deployment.kill_level}</span>
                <span class="esc-sym mono">{r.deployment.symbol}</span>
                <span class="esc-agent mono">{r.agent_id}</span>
                <span class="esc-chev">›</span>
              </button>
            {/each}
          </div>
          <div class="muted">
            Click any row to open the Fleet page. The deployment
            drilldown's Ops section carries reset + widen /
            cancel-all / flatten controls.
          </div>
        {/if}
        {#if lastKillFetch}
          <div class="muted small">
            refreshed {lastKillFetch.toLocaleTimeString()}
          </div>
        {/if}
      {/snippet}
    </Card>
    <Card title="System links" subtitle="preflight · metrics · diagnostics" span={1}>
      {#snippet children()}
        <div class="links">
          <a href="/metrics" target="_blank" rel="noopener">Prometheus /metrics</a>
          <a href="/api/v1/system/preflight" target="_blank" rel="noopener">Preflight JSON</a>
          <a href="/api/v1/system/diagnostics" target="_blank" rel="noopener">Diagnostics</a>
        </div>
      {/snippet}
    </Card>
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }
  .empty {
    padding: var(--s-3);
    background: color-mix(in srgb, var(--ok) 8%, transparent);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
    font-size: var(--fs-sm);
    line-height: 1.5;
  }
  .empty strong { color: var(--ok); display: block; margin-bottom: 4px; }
  .empty-actions { margin-top: var(--s-2); }
  .muted { color: var(--fg-muted); font-size: var(--fs-xs); line-height: 1.55; margin-top: var(--s-2); }
  .muted.small { font-size: 10px; }
  .links { display: flex; flex-direction: column; gap: var(--s-2); }
  .links a {
    padding: var(--s-2) var(--s-3);
    color: var(--fg-secondary);
    text-decoration: none;
    border-radius: var(--r-md);
    font-size: var(--fs-sm);
    transition: background var(--dur-fast) var(--ease-out), color var(--dur-fast) var(--ease-out);
  }
  .links a:hover { background: var(--bg-chip); color: var(--accent); }
  .esc-list { display: flex; flex-direction: column; gap: 4px; margin-bottom: var(--s-2); }
  .esc-row {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 30%, transparent);
    border-radius: var(--r-sm);
    cursor: pointer; color: inherit; text-align: left; font-size: var(--fs-xs);
  }
  .esc-row:hover { background: color-mix(in srgb, var(--danger) 18%, transparent); }
  .esc-kill {
    padding: 2px 6px; border-radius: var(--r-sm);
    background: var(--danger); color: var(--bg-base);
    font-family: var(--font-mono); font-weight: 600; font-size: 10px;
  }
  .esc-sym { flex: 1; }
  .esc-agent { color: var(--fg-muted); font-size: 10px; }
  .esc-chev { font-size: var(--fs-md); color: var(--fg-muted); }
  .linklike {
    background: none; border: 0; padding: 0;
    color: var(--accent); cursor: pointer; font-size: inherit;
    font-family: inherit;
  }
  .linklike:hover { text-decoration: underline; }
</style>
