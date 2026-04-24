<script>
  /*
   * Webhook test button + delivery log. Fires a test payload at
   * every configured URL, shows the rollup inline, then renders
   * the last N deliveries in a table.
   */
  import Card from '../Card.svelte'
  import { Button } from '../../primitives/index.js'

  let {
    deliveries = [],
    testBusy = false,
    testStatus = null,
    onTest,
  } = $props()
</script>

<Card title="Webhook deliveries" subtitle="last 50 · fires on SLA breaches, fills, kill events" span={3}>
  {#snippet children()}
    <div class="wh-actions">
      <Button variant="ok" size="sm" disabled={testBusy} onclick={onTest}>
        {#snippet children()}{testBusy ? 'Testing…' : 'Send test payload'}{/snippet}
      </Button>
      {#if testStatus}
        <span class="wh-status {testStatus.phase}">{testStatus.text}</span>
      {/if}
    </div>
    {#if deliveries.length === 0}
      <div class="empty">No deliveries logged yet. Fire a test, or wait for the next event.</div>
    {:else}
      <table class="sym-table">
        <thead>
          <tr>
            <th>when</th>
            <th>url</th>
            <th>event</th>
            <th class="num">status</th>
            <th class="num">latency</th>
          </tr>
        </thead>
        <tbody>
          {#each deliveries as d (d.timestamp + d.url)}
            <tr>
              <td class="mono">{new Date(d.timestamp).toLocaleTimeString()}</td>
              <td class="mono url-cell" title={d.url}>{d.url}</td>
              <td class="mono">{d.event_type}</td>
              <td class="num mono">
                {#if d.ok}
                  <span class="chip tone-ok">{d.http_status ?? 'ok'}</span>
                {:else}
                  <span class="chip tone-bad" title={d.error || ''}>{d.http_status ?? 'err'}</span>
                {/if}
              </td>
              <td class="num mono">{d.latency_ms ?? '—'}ms</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/snippet}
</Card>

<style>
  .wh-actions {
    display: flex; align-items: center; gap: var(--s-2);
    margin-bottom: var(--s-2);
  }
  .wh-status {
    font-size: 11px; font-family: var(--font-mono);
    padding: 2px 8px; border-radius: var(--r-sm);
  }
  .wh-status.pending { background: var(--bg-raised); color: var(--fg-muted); }
  .wh-status.ok      { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .wh-status.warn    { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .wh-status.info    { background: var(--bg-raised); color: var(--fg-secondary); }
  .wh-status.err     { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }

  .empty {
    padding: var(--s-3); color: var(--fg-muted);
    font-size: var(--fs-sm); text-align: center;
  }

  .sym-table { width: 100%; border-collapse: collapse; margin-top: var(--s-2); }
  .sym-table th, .sym-table td {
    padding: var(--s-2);
    font-size: var(--fs-xs);
    text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .sym-table th {
    color: var(--fg-muted); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-size: 10px;
  }
  .num { text-align: right; }
  .url-cell { max-width: 340px; overflow: hidden; text-overflow: ellipsis; }
</style>
