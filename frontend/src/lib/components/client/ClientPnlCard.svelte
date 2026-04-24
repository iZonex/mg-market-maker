<script>
  /*
   * Per-client fleet-aggregated PnL attribution.
   *
   * Top row shows total PnL + volume + round-trips; the table
   * breaks it down by symbol.
   */
  import Card from '../Card.svelte'
  import { fmtDec } from './client-helpers.js'

  let { selected, pnl } = $props()
</script>

<Card title="PnL attribution" subtitle={`tenant ${selected}`} span={1}>
  {#snippet children()}
    {#if !pnl}
      <div class="muted">No data yet — deploy a strategy on an agent tagged with this client_id.</div>
    {:else}
      <div class="kv-row">
        <div class="kv-cell">
          <span class="k">total PnL</span>
          <span class="v mono" class:pos={Number(pnl.total_pnl) > 0} class:neg={Number(pnl.total_pnl) < 0}>
            {fmtDec(pnl.total_pnl, 4)}
          </span>
        </div>
        <div class="kv-cell">
          <span class="k">volume</span>
          <span class="v mono">{fmtDec(pnl.total_volume)}</span>
        </div>
        <div class="kv-cell">
          <span class="k">round trips</span>
          <span class="v mono">{pnl.total_fills ?? 0}</span>
        </div>
      </div>
      {#if pnl.symbols?.length > 0}
        <table class="sym-table">
          <thead>
            <tr><th>symbol</th><th class="num">PnL</th><th class="num">volume</th><th class="num">fills</th></tr>
          </thead>
          <tbody>
            {#each pnl.symbols as r (r.symbol)}
              <tr>
                <td class="mono">{r.symbol}</td>
                <td class="num mono" class:pos={Number(r.pnl) > 0} class:neg={Number(r.pnl) < 0}>
                  {fmtDec(r.pnl, 4)}
                </td>
                <td class="num mono">{fmtDec(r.volume)}</td>
                <td class="num mono">{r.fills}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {/if}
  {/snippet}
</Card>

<style>
  .kv-row {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: var(--s-2);
    margin-bottom: var(--s-2);
  }
  .kv-cell {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2); background: var(--bg-raised);
    border-radius: var(--r-sm);
  }
  .k { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .v { font-size: var(--fs-sm); color: var(--fg-primary); }
  .v.pos { color: var(--pos); }
  .v.neg { color: var(--neg); }

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
</style>
