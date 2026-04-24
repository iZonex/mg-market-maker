<script>
  /*
   * C7 GOBS — fleet-wide rollup card. Every KPI is a pure client
   * derivation from the `fleet` + `approvals` snapshots the parent
   * already polls, so no new endpoint needed.
   */
  import Card from '../Card.svelte'

  let { rows = [] } = $props()

  const stats = $derived.by(() => {
    const accepted = rows.filter((r) => r.state === 'accepted').length
    const online = rows.filter((r) => r.connected).length
    const allDeps = rows.flatMap((r) => r.live?.deployments || [])
    const runningDeps = allDeps.filter((d) => d.running).length
    const liveOrders = allDeps.reduce((s, d) => s + Number(d.live_orders || 0), 0)
    const totalPnl = allDeps.reduce((s, d) => s + Number(d.unrealized_pnl_quote || 0), 0)
    const killed = allDeps.filter((d) => (d.kill_level || 0) > 0).length
    const nowMs = Date.now()
    const oldestTick = allDeps
      .filter((d) => d.running)
      .reduce((m, d) => {
        const age = nowMs - Number(d.last_tick_ms || 0)
        return Number.isFinite(age) && age < m ? m : (age >= 0 && age > m ? age : m)
      }, 0)
    return { accepted, online, allDeps, runningDeps, liveOrders, totalPnl, killed, oldestTick }
  })
</script>

<Card title="Fleet rollup" subtitle="live totals across every accepted agent" span={3}>
  {#snippet children()}
    <div class="rollup-grid">
      <div class="rollup-cell">
        <span class="rollup-k">agents</span>
        <span class="rollup-v mono">{stats.online}/{stats.accepted}</span>
        <span class="rollup-sub">online/accepted</span>
      </div>
      <div class="rollup-cell">
        <span class="rollup-k">deployments</span>
        <span class="rollup-v mono">{stats.runningDeps}/{stats.allDeps.length}</span>
        <span class="rollup-sub">running/total</span>
      </div>
      <div class="rollup-cell">
        <span class="rollup-k">live orders</span>
        <span class="rollup-v mono">{stats.liveOrders}</span>
      </div>
      <div class="rollup-cell" class:pos={stats.totalPnl > 0} class:neg={stats.totalPnl < 0}>
        <span class="rollup-k">total PnL</span>
        <span class="rollup-v mono">{stats.totalPnl !== 0 ? stats.totalPnl.toFixed(2) : '—'}</span>
        <span class="rollup-sub">unrealized · quote</span>
      </div>
      <div class="rollup-cell" class:alert={stats.killed > 0}>
        <span class="rollup-k">kill-escalated</span>
        <span class="rollup-v mono">{stats.killed}</span>
      </div>
      <div class="rollup-cell">
        <span class="rollup-k">oldest tick</span>
        <span class="rollup-v mono">
          {#if stats.runningDeps === 0}—
          {:else if stats.oldestTick < 1000}&lt;1s
          {:else if stats.oldestTick < 60_000}{Math.round(stats.oldestTick / 1000)}s
          {:else}{Math.round(stats.oldestTick / 60_000)}m
          {/if}
        </span>
        <span class="rollup-sub">across running deployments</span>
      </div>
    </div>
  {/snippet}
</Card>

<style>
  .rollup-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: var(--s-2);
  }
  .rollup-cell {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-raised);
    border-radius: var(--r-sm);
  }
  .rollup-cell.pos .rollup-v { color: var(--pos); }
  .rollup-cell.neg .rollup-v { color: var(--neg); }
  .rollup-cell.alert { background: color-mix(in srgb, var(--danger) 15%, transparent); }
  .rollup-cell.alert .rollup-v { color: var(--danger); }
  .rollup-k {
    font-size: 10px;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    color: var(--fg-muted);
  }
  .rollup-v {
    font-size: var(--fs-lg);
    color: var(--fg-primary);
    font-weight: 500;
  }
  .rollup-sub {
    font-size: 10px;
    color: var(--fg-muted);
  }
</style>
