<script>
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let rows = $state([])
  let error = $state('')
  let lastUpdated = $state(0)

  async function refresh() {
    try {
      rows = await api.getJson('/api/v1/venues/status')
      error = ''
      lastUpdated = Date.now()
    } catch (e) {
      error = e.message
    }
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 5000)
    return () => clearInterval(id)
  })

  let now = $state(Date.now())
  $effect(() => {
    const id = setInterval(() => { now = Date.now() }, 1000)
    return () => clearInterval(id)
  })
  const ageSecs = $derived(lastUpdated ? Math.round((now - lastUpdated) / 1000) : null)
</script>

{#if error}
  <div class="empty-state">
    <span class="empty-state-icon" style="color: var(--neg)"><Icon name="alert" size={18} /></span>
    <span class="empty-state-title">Failed to load venues</span>
    <span class="empty-state-hint">{error}</span>
  </div>
{:else if rows.length === 0}
  <div class="empty-state">
    <span class="empty-state-icon"><Icon name="emptyList" size={18} /></span>
    <span class="empty-state-title">No symbols tracked</span>
  </div>
{:else}
  <div class="conn">
    <div class="meta">
      <span class="label">updated</span>
      <span class="num">{ageSecs === null ? '—' : `${ageSecs}s ago`}</span>
    </div>

    <div class="rows">
      {#each rows as r}
        <div class="row" data-state={!r.has_data ? 'bad' : r.quoting_halted ? 'warn' : 'ok'}>
          <div class="row-main">
            <span class="sym num">{r.symbol}</span>
            <span class="state-dot"></span>
            <span class="state-label">
              {#if !r.has_data}no data
              {:else if r.quoting_halted}halted
              {:else}quoting{/if}
            </span>
          </div>
          <div class="row-stats">
            <div class="stat">
              <span class="label">Mid</span>
              <span class="num">{r.has_data ? parseFloat(r.mid_price).toFixed(2) : '—'}</span>
            </div>
            <div class="stat">
              <span class="label">Live</span>
              <span class="num">{r.live_orders}</span>
            </div>
            <div class="stat">
              <span class="label">Fills</span>
              <span class="num">{r.total_fills}</span>
            </div>
            <div class="stat">
              <span class="label">SLA</span>
              <span class="num" class:pos={parseFloat(r.sla_uptime_pct) >= 95}
                                class:warn={parseFloat(r.sla_uptime_pct) < 95 && parseFloat(r.sla_uptime_pct) >= 90}
                                class:neg={parseFloat(r.sla_uptime_pct) < 90 && parseFloat(r.sla_uptime_pct) > 0}>
                {parseFloat(r.sla_uptime_pct).toFixed(1)}%
              </span>
            </div>
            <div class="stat">
              <span class="label">Kill</span>
              <span class="kl-mini kl-{r.kill_level === 0 ? 'ok' : r.kill_level === 1 ? 'warn' : 'neg'}">L{r.kill_level}</span>
            </div>
          </div>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .conn { display: flex; flex-direction: column; gap: var(--s-3); }
  .meta {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--s-2);
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
  }

  .rows { display: flex; flex-direction: column; gap: var(--s-2); }
  .row {
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-lg);
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  .row[data-state='ok']   { border-color: rgba(34, 197, 94, 0.18); }
  .row[data-state='warn'] { border-color: rgba(245, 158, 11, 0.25); }
  .row[data-state='bad']  { border-color: rgba(239, 68, 68, 0.3); background: rgba(239, 68, 68, 0.04); }

  .row-main {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  .sym {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: 0.02em;
  }
  .state-dot {
    width: 6px; height: 6px; border-radius: 50%;
  }
  .row[data-state='ok']   .state-dot { background: var(--pos); animation: pulse 2s ease-in-out infinite; }
  .row[data-state='warn'] .state-dot { background: var(--warn); }
  .row[data-state='bad']  .state-dot { background: var(--neg); animation: pulse 0.7s ease-in-out infinite; }
  @keyframes pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.5; transform: scale(0.8); }
  }
  .state-label {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
  }
  .row[data-state='bad'] .state-label { color: var(--neg); }

  .row-stats {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: var(--s-2);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .stat {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .stat .num {
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--fg-primary);
  }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }
  .warn { color: var(--warn); }

  .kl-mini {
    display: inline-flex;
    align-items: center;
    justify-content: flex-start;
    padding: 1px var(--s-1);
    border-radius: var(--r-sm);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    font-weight: 700;
    width: fit-content;
  }
  .kl-ok   { background: var(--pos-bg);  color: var(--pos); }
  .kl-warn { background: var(--warn-bg); color: var(--warn); }
  .kl-neg  { background: var(--neg-bg);  color: var(--neg); }
</style>
