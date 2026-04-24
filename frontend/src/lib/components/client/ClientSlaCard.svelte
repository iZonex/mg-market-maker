<script>
  /*
   * Per-client SLA rollup — avg/min presence + two-sided quoting
   * + compliance flag + signed certificate hash. Per-symbol table
   * rounds it out.
   */
  import Card from '../Card.svelte'
  import { fmtDec, slaTone, SLA_LEGEND } from './client-helpers.js'

  let { sla, cert } = $props()
</script>

<Card title="SLA" subtitle="fleet presence · two-sided · spread compliance" span={1}>
  {#snippet children()}
    {#if !sla}
      <div class="muted">No SLA data yet.</div>
    {:else}
      <div class="kv-row">
        <div class="kv-cell" title={SLA_LEGEND}>
          <span class="k">avg presence</span>
          <span class="v mono tone-{slaTone(sla.avg_presence_pct)}">
            {fmtDec(sla.avg_presence_pct, 2)}%
          </span>
        </div>
        <div class="kv-cell" title={SLA_LEGEND}>
          <span class="k">avg two-sided</span>
          <span class="v mono tone-{slaTone(sla.avg_two_sided_pct)}">
            {fmtDec(sla.avg_two_sided_pct, 2)}%
          </span>
        </div>
        <div class="kv-cell" title={SLA_LEGEND}>
          <span class="k">min presence</span>
          <span class="v mono tone-{slaTone(sla.min_presence_pct)}">
            {fmtDec(sla.min_presence_pct, 2)}%
          </span>
        </div>
        <div class="kv-cell">
          <span class="k">compliant</span>
          <span class="v chip tone-{sla.is_compliant ? 'ok' : 'bad'}">
            {sla.is_compliant ? 'YES' : 'NO'}
          </span>
        </div>
      </div>
      <div class="legend muted" title={SLA_LEGEND}>
        Compliance bands: ≥99% ok · 95–99% warn · &lt;95% breach
      </div>
      {#if sla.symbols?.length > 0}
        <table class="sym-table">
          <thead>
            <tr>
              <th>symbol</th>
              <th class="num">presence</th>
              <th class="num">two-sided</th>
              <th class="num">spread cmp</th>
              <th class="num">minutes</th>
            </tr>
          </thead>
          <tbody>
            {#each sla.symbols as r (r.symbol)}
              <tr>
                <td class="mono">{r.symbol}</td>
                <td class="num mono tone-{slaTone(r.presence_pct)}">{fmtDec(r.presence_pct, 2)}%</td>
                <td class="num mono tone-{slaTone(r.two_sided_pct)}">{fmtDec(r.two_sided_pct, 2)}%</td>
                <td class="num mono tone-{slaTone(r.spread_compliance_pct)}">{fmtDec(r.spread_compliance_pct, 2)}%</td>
                <td class="num mono">{r.minutes_with_data ?? 0}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
      {#if cert?.signature}
        <div class="cert-line">
          Signed certificate: <code class="mono sig">{cert.signature.slice(0, 24)}…</code>
          <span class="muted">· generated {cert.generated_at}</span>
        </div>
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

  .legend { font-size: 10px; margin-top: var(--s-2); }
  .cert-line {
    margin-top: var(--s-2); font-size: var(--fs-xs);
    color: var(--fg-secondary);
  }
  .sig { color: var(--accent); }
</style>
