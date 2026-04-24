<script>
  /*
   * Agents joined to this tenant via profile.client_id — shows
   * one row per matched agent with region, environment,
   * deployment counts, and live-order sum.
   */
  import Card from '../Card.svelte'

  let { selected, agents = [] } = $props()
</script>

<Card title="Agents carrying this tenant" subtitle={`${agents.length} approved · matched by profile.client_id`} span={3}>
  {#snippet children()}
    {#if agents.length === 0}
      <div class="empty">
        No agent has <code>profile.client_id = "{selected}"</code> — set it in Fleet → Edit on the agent card.
      </div>
    {:else}
      <table class="agent-table">
        <thead>
          <tr>
            <th>agent</th>
            <th>region</th>
            <th>environment</th>
            <th class="num">deployments</th>
            <th class="num">live orders</th>
            <th>state</th>
          </tr>
        </thead>
        <tbody>
          {#each agents as a (a.agent_id)}
            {@const live = (a.deployments || []).filter((d) => d.running).length}
            {@const orders = (a.deployments || []).reduce((n, d) => n + (d.live_orders || 0), 0)}
            <tr>
              <td class="mono">{a.agent_id}</td>
              <td>{a.approval?.profile?.region || '—'}</td>
              <td>{a.approval?.profile?.environment || '—'}</td>
              <td class="num mono">{live}/{(a.deployments || []).length}</td>
              <td class="num mono">{orders}</td>
              <td>
                <span class="chip tone-{a.approval_state === 'accepted' ? 'ok' : 'muted'}">
                  {a.approval_state || 'unknown'}
                </span>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/snippet}
</Card>

<style>
  .empty {
    padding: var(--s-3); color: var(--fg-muted);
    font-size: var(--fs-sm); text-align: center;
  }
  .empty code { font-family: var(--font-mono); background: var(--bg-chip); padding: 0 4px; border-radius: 3px; }
  .agent-table { width: 100%; border-collapse: collapse; margin-top: var(--s-2); }
  .agent-table th, .agent-table td {
    padding: var(--s-2);
    font-size: var(--fs-xs);
    text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .agent-table th {
    color: var(--fg-muted); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-size: 10px;
  }
  .num { text-align: right; }
</style>
