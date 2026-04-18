<script>
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  const api = createApiClient(auth)

  const MAX = 200
  let events = $state([])
  let error = $state('')
  let filter = $state('')
  let pausePoll = $state(false)
  let lastSeq = 0

  async function refresh() {
    if (pausePoll) return
    try {
      const data = await api.getJson('/api/v1/audit/recent')
      const fresh = data.events || data || []
      const byId = new Map(events.map((e) => [e.seq, e]))
      for (const ev of fresh) {
        byId.set(ev.seq, ev)
        if (ev.seq > lastSeq) lastSeq = ev.seq
      }
      const sorted = Array.from(byId.values()).sort((a, b) => b.seq - a.seq)
      events = sorted.slice(0, MAX)
      error = ''
    } catch (e) {
      error = e.message
    }
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 3000)
    return () => clearInterval(id)
  })

  const filtered = $derived.by(() => {
    if (!filter) return events
    const q = filter.toLowerCase()
    return events.filter(
      (e) =>
        (e.event_type || '').toLowerCase().includes(q) ||
        (e.symbol || '').toLowerCase().includes(q) ||
        (e.detail || '').toLowerCase().includes(q)
    )
  })

  function severity(evtType) {
    const t = (evtType || '').toLowerCase()
    if (t.includes('kill') || t.includes('breaker') || t.includes('halt') || t.includes('fail')) return 'neg'
    if (t.includes('drift') || t.includes('resync') || t.includes('violation') || t.includes('delist') || t.includes('disconnect') || t.includes('shutdown')) return 'warn'
    if (t.includes('login') || t.includes('logout') || t.includes('escalated') || t.includes('reset')) return 'info'
    return 'muted'
  }
  function fmtTime(t) {
    if (!t) return '—'
    try { return new Date(t).toLocaleTimeString() } catch { return '—' }
  }
</script>

<div class="audit">
  <header class="head">
    <div class="head-meta">
      <span class="chip" class:chip-pos={!pausePoll} class:chip-warn={pausePoll}>
        {pausePoll ? 'PAUSED' : 'LIVE'}
      </span>
      <span class="count num">{filtered.length}</span>
      <span class="label">events</span>
    </div>
    <button
      type="button"
      class="btn btn-ghost btn-sm"
      onclick={() => (pausePoll = !pausePoll)}
      aria-label={pausePoll ? 'Resume' : 'Pause'}
    >
      {#if pausePoll}
        <Icon name="pulse" size={12} />
        <span>Resume</span>
      {:else}
        <Icon name="clock" size={12} />
        <span>Pause</span>
      {/if}
    </button>
  </header>

  <div class="search">
    <span class="search-icon"><Icon name="search" size={13} /></span>
    <input
      type="text"
      class="input search-input"
      placeholder="filter by type / symbol / detail…"
      bind:value={filter}
    />
    {#if filter}
      <button type="button" class="btn btn-icon btn-ghost btn-sm" onclick={() => (filter = '')} aria-label="clear">
        <Icon name="close" size={12} />
      </button>
    {/if}
  </div>

  {#if error}
    <div class="alert-bar">
      <Icon name="alert" size={14} />
      <span>{error}</span>
    </div>
  {/if}

  {#if filtered.length === 0 && !error}
    <div class="empty-state">
      <span class="empty-state-icon"><Icon name="history" size={18} /></span>
      <span class="empty-state-title">
        {filter ? 'No matching events' : 'No events yet'}
      </span>
      <span class="empty-state-hint">
        {filter ? 'Clear the filter to see the full stream.'
                : 'Order lifecycle, kill switch, and SLA events land here.'}
      </span>
    </div>
  {:else}
    <div class="log scroll">
      {#each filtered as e (e.seq)}
        <div class="log-row" data-sev={severity(e.event_type)}>
          <span class="dot"></span>
          <span class="ts num">{fmtTime(e.timestamp)}</span>
          <span class="type">{e.event_type}</span>
          {#if e.symbol}<span class="sym num">{e.symbol}</span>{/if}
          {#if e.detail}<span class="detail">{e.detail}</span>{/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .audit {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
  }
  .head-meta {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  .count {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
  }

  .search {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  .search-icon {
    position: absolute;
    left: var(--s-3);
    top: 50%;
    transform: translateY(-50%);
    color: var(--fg-muted);
    pointer-events: none;
  }
  .search-input {
    padding-left: 34px;
    font-family: var(--font-sans);
  }
  .search .btn {
    position: absolute;
    right: 4px;
    top: 50%;
    transform: translateY(-50%);
  }

  .alert-bar {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--neg-bg);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: var(--r-md);
    color: var(--neg);
    font-size: var(--fs-xs);
  }

  .log {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 340px;
    overflow-y: auto;
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
  }
  .log-row {
    display: grid;
    grid-template-columns: 14px 80px 1fr 80px 2fr;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-1) var(--s-2);
    border-radius: var(--r-sm);
    transition: background var(--dur-fast) var(--ease-out);
  }
  .log-row:hover { background: var(--bg-chip); }
  .dot {
    width: 6px; height: 6px;
    border-radius: 50%;
  }
  .log-row[data-sev='muted'] .dot { background: var(--fg-faint); }
  .log-row[data-sev='info']  .dot { background: var(--info); }
  .log-row[data-sev='warn']  .dot { background: var(--warn); }
  .log-row[data-sev='neg']   .dot { background: var(--neg); }

  .ts {
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .type {
    font-weight: 600;
    color: var(--fg-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .log-row[data-sev='warn'] .type { color: var(--warn); }
  .log-row[data-sev='neg']  .type { color: var(--neg); }
  .log-row[data-sev='info'] .type { color: var(--info); }

  .sym {
    font-weight: 500;
    color: var(--accent);
    font-variant-numeric: tabular-nums;
  }
  .detail {
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
