<script>
  /*
   * Pending-approval list — shown when any approval row is in
   * the `pending` state. Each row has Accept / Reject / Skip
   * controls; parent owns the actual state transitions.
   */
  import Card from '../Card.svelte'
  import { Button } from '../../primitives/index.js'
  import { fmtDuration as formatAge } from '../../format.js'

  let {
    rows = [],
    busyFp = {},
    skippedFp = {},
    nowMs = Date.now(),
    onAccept,
    onReject,
    onSkip,
  } = $props()

  const pending = $derived(
    rows.filter((r) => r.state === 'pending' && !skippedFp[r.fingerprint]),
  )
</script>

{#if pending.length > 0}
  <Card title="Pending approvals" subtitle="new agents awaiting admission" span={3}>
    {#snippet children()}
      <div class="pending-list">
        {#each pending as r (r.fingerprint)}
          <div class="pending-row">
            <div class="pending-info">
              <div class="row-line">
                <span class="fp mono">{r.fingerprint}</span>
                <span class="chip tone-warn">PENDING</span>
                {#if r.connected}<span class="chip tone-ok">CONNECTED</span>{/if}
              </div>
              <div class="row-meta">
                advertised id <span class="mono">{r.agent_id}</span>
                · first seen {formatAge(nowMs - (r.first_seen_ms || nowMs))} ago
              </div>
            </div>
            <div class="actions">
              <Button variant="ok" disabled={busyFp[r.fingerprint]} onclick={() => onAccept(r.fingerprint)}>
                {#snippet children()}Accept{/snippet}
              </Button>
              <Button variant="danger" disabled={busyFp[r.fingerprint]} onclick={() => onReject(r.fingerprint)}>
                {#snippet children()}Reject{/snippet}
              </Button>
              <Button variant="ghost" onclick={() => onSkip(r.fingerprint)}>
                {#snippet children()}Skip{/snippet}
              </Button>
            </div>
          </div>
        {/each}
      </div>
    {/snippet}
  </Card>
{/if}

<style>
  .pending-list { display: flex; flex-direction: column; gap: var(--s-2); }
  .pending-row {
    display: flex; align-items: center; justify-content: space-between;
    padding: var(--s-3);
    border: 1px solid var(--warn);
    background: var(--warn-bg);
    border-radius: var(--r-md);
    gap: var(--s-3);
  }
  .pending-info { display: flex; flex-direction: column; gap: 4px; flex: 1; min-width: 0; }
  .row-line { display: flex; align-items: center; gap: var(--s-2); }
  .row-meta { font-size: var(--fs-xs); color: var(--fg-muted); }
  .actions { display: flex; gap: var(--s-1); }
  .fp { font-weight: 600; color: var(--fg-primary); font-size: var(--fs-sm); }
</style>
