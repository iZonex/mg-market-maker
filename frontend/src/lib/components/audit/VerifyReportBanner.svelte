<script>
  /*
   * Fix #2 — render the aggregate SHA-256 chain-verify report
   * returned by POST /api/v1/audit/verify. Controller fan-outs
   * to every running deployment; body includes per-(agent,
   * deployment) row with valid/broken/missing + error kind.
   */
  let { report } = $props()
</script>

{#if report}
  <div class="verify-report" class:err={report.phase === 'err' || (report.broken ?? 0) > 0}>
    {#if report.phase === 'pending'}
      <span>Verifying…</span>
    {:else if report.phase === 'err'}
      <span>verify failed: {report.error}</span>
    {:else}
      <span>
        ✓ {report.valid}/{report.total_deployments} valid
        {#if report.broken > 0}· <strong>{report.broken} broken</strong>{/if}
        {#if report.missing > 0}· {report.missing} missing{/if}
      </span>
      {#if report.broken > 0}
        <div class="verify-broken-list">
          {#each report.rows.filter((r) => r.exists && !r.valid) as r (r.agent_id + '/' + r.deployment_id)}
            <div class="verify-broken">
              <span class="mono">{r.agent_id}/{r.symbol}</span>
              <span class="err-kind">{r.error_kind}</span>
              {#if r.break_row}<span class="mono">row #{r.break_row}</span>{/if}
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .verify-report {
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--ok) 12%, transparent);
    color: var(--ok); border-radius: var(--r-sm);
    font-size: var(--fs-xs); font-family: var(--font-mono);
  }
  .verify-report.err {
    background: color-mix(in srgb, var(--danger) 15%, transparent);
    color: var(--danger);
  }
  .verify-broken-list { display: flex; flex-direction: column; gap: 2px; margin-top: 4px; }
  .verify-broken { display: flex; gap: var(--s-2); font-size: 10px; }
  .err-kind { color: var(--danger); font-weight: 600; text-transform: uppercase; }
</style>
