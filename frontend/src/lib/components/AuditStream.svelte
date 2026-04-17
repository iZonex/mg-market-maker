<script>
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  // Keep the N most recent events so the panel does not grow
  // unbounded — the backend already caps at 100 per request,
  // but after several polls we would otherwise accumulate.
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
      // Incoming is newest-first; we flip so the list reads
      // top=most-recent AFTER merging with existing state.
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
    if (t.includes('kill') || t.includes('breaker') || t.includes('break') || t.includes('halt'))
      return 'crit'
    if (t.includes('drift') || t.includes('resync') || t.includes('violation') || t.includes('delist'))
      return 'warn'
    if (t.includes('shutdown') || t.includes('disconnect')) return 'warn'
    return 'info'
  }
</script>

<div>
  <h3>
    Audit Stream
    <span class="count">({filtered.length})</span>
    <button class="btn-pause" onclick={() => (pausePoll = !pausePoll)}>
      {pausePoll ? 'resume' : 'pause'}
    </button>
  </h3>

  <input
    type="text"
    class="filter"
    placeholder="filter by type / symbol / detail…"
    bind:value={filter}
  />

  {#if error}
    <div class="error">error: {error}</div>
  {/if}

  <div class="log">
    {#each filtered as e (e.seq)}
      <div class="row sev-{severity(e.event_type)}">
        <span class="ts">{new Date(e.timestamp).toLocaleTimeString()}</span>
        <span class="type">{e.event_type}</span>
        <span class="sym">{e.symbol}</span>
        {#if e.detail}<span class="detail">{e.detail}</span>{/if}
      </div>
    {/each}
    {#if filtered.length === 0 && !error}
      <div class="empty">no events</div>
    {/if}
  </div>
</div>

<style>
  h3 {
    font-size: 12px; color: #8b949e; margin-bottom: 8px;
    text-transform: uppercase; letter-spacing: 0.5px;
    display: flex; align-items: center; gap: 8px;
  }
  .count { font-size: 10px; color: #484f58; }
  .btn-pause {
    margin-left: auto; background: none; border: 1px solid #30363d;
    color: #8b949e; padding: 2px 8px; border-radius: 3px;
    cursor: pointer; font-family: inherit; font-size: 10px;
  }
  .btn-pause:hover { border-color: #d29922; color: #d29922; }
  .filter {
    width: 100%; background: #0d1117; color: #e1e4e8;
    border: 1px solid #21262d; padding: 5px 8px; border-radius: 3px;
    font-family: inherit; font-size: 11px; margin-bottom: 8px;
  }
  .error { color: #f85149; font-size: 11px; padding: 4px; }
  .empty { color: #8b949e; font-size: 11px; padding: 12px 0; text-align: center; }
  .log {
    font-size: 10px; max-height: 280px; overflow-y: auto;
    display: flex; flex-direction: column; gap: 2px;
  }
  .row {
    display: flex; gap: 6px; padding: 2px 4px; border-radius: 2px;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  .row.sev-info { color: #8b949e; }
  .row.sev-warn { color: #d29922; }
  .row.sev-crit { color: #f85149; background: rgba(248, 81, 73, 0.08); }
  .ts { color: #484f58; }
  .type { font-weight: 700; }
  .sym { color: #79c0ff; }
  .detail { color: #6e7681; overflow: hidden; text-overflow: ellipsis; }
</style>
