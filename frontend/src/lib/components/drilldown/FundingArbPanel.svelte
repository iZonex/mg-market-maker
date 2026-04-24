<script>
  /*
   * Funding-arb driver summary for the drilldown: state chip,
   * counter grid, and a lazy-loaded recent-events table. Parent
   * owns the fetcher + loading state.
   */

  let {
    row,
    events = [],
    loading = false,
    error = null,
    onRefresh,
  } = $props()
</script>

<div class="exec-grid">
  <div class="exec-cell">
    <span class="exec-k">State</span>
    {#if row.funding_arb_active}
      <span class="exec-v mono pos">ENGAGED</span>
    {:else}
      <span class="exec-v mono">IDLE</span>
    {/if}
  </div>
  <div class="exec-cell">
    <span class="exec-k">Entered</span>
    <span class="exec-v mono">{row.funding_arb_entered ?? 0}</span>
  </div>
  <div class="exec-cell">
    <span class="exec-k">Exited</span>
    <span class="exec-v mono">{row.funding_arb_exited ?? 0}</span>
  </div>
  <div class="exec-cell">
    <span class="exec-k">Taker rejected</span>
    <span class="exec-v mono">{row.funding_arb_taker_rejected ?? 0}</span>
  </div>
  <div class="exec-cell">
    <span class="exec-k">Pair break</span>
    <span class="exec-v mono">{row.funding_arb_pair_break ?? 0}</span>
  </div>
  <div class="exec-cell">
    <span class="exec-k">Pair break (uncomp.)</span>
    {#if (row.funding_arb_pair_break_uncompensated ?? 0) > 0}
      <span class="exec-v mono neg">{row.funding_arb_pair_break_uncompensated}</span>
    {:else}
      <span class="exec-v mono">0</span>
    {/if}
  </div>
</div>

<div class="events-head">
  <span class="events-title">Recent events</span>
  <button type="button" class="events-refresh" disabled={loading} onclick={onRefresh}>
    {loading ? 'Loading…' : 'Refresh'}
  </button>
</div>
{#if error}
  <p class="exec-hint err">Details fetch failed: {error}</p>
{:else if events.length === 0 && !loading}
  <p class="exec-hint">No recent events in the agent's ring buffer yet.</p>
{:else}
  <table class="events-table">
    <thead>
      <tr>
        <th>when</th>
        <th>outcome</th>
        <th>reason</th>
      </tr>
    </thead>
    <tbody>
      {#each events as ev, i (i)}
        <tr>
          <td class="mono">{new Date(ev.at_ms).toLocaleTimeString()}</td>
          <td class="mono">{ev.outcome}</td>
          <td class="mono">{ev.reason || '—'}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  .exec-grid { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-2); }
  .exec-cell {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
  }
  .exec-k {
    font-size: 10px;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .exec-v { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 600; }
  .exec-v.pos { color: var(--pos); }
  .exec-v.neg { color: var(--neg); }
  .exec-hint {
    margin: 0;
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    line-height: var(--lh-snug);
  }
  .exec-hint.err { color: var(--neg); }

  .events-head {
    display: flex; justify-content: space-between; align-items: center;
    margin-top: var(--s-2);
  }
  .events-title {
    font-size: 10px; color: var(--fg-muted);
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-weight: 600;
  }
  .events-refresh {
    padding: 2px 8px;
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
    font-size: var(--fs-2xs);
    cursor: pointer;
  }
  .events-refresh:disabled { opacity: 0.5; cursor: not-allowed; }
  .events-refresh:hover:not(:disabled) {
    color: var(--fg-primary); background: var(--bg-base);
  }

  .events-table {
    width: 100%; border-collapse: collapse;
    font-size: var(--fs-2xs);
    margin-top: var(--s-1);
  }
  .events-table th, .events-table td {
    padding: 4px var(--s-2);
    text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .events-table th {
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-size: 10px;
    font-weight: 600;
  }
  .events-table td.mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; color: var(--fg-primary); }
</style>
