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

  // Audit lives on each agent's disk (engine writes to
  // `data/audit/{symbol}.jsonl` in the agent's CWD). To show a
  // unified feed we fan out across the fleet, fetch each
  // deployment's audit tail via the per-deployment details
  // endpoint, and merge by `seq` (audit entries are
  // monotonic). Controller serves the per-deployment fetch
  // over its existing request/reply protocol with a 5s
  // per-call timeout.
  async function refresh() {
    if (pausePoll) return
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const tailFetches = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if (!d.running) continue
          const path = `/api/v1/agents/${encodeURIComponent(a.agent_id)}`
            + `/deployments/${encodeURIComponent(d.deployment_id)}/details/audit_tail`
          tailFetches.push(
            api.getJson(path)
              .then(resp => (resp.payload?.events || []).map(ev => ({
                ...ev,
                _source_agent: a.agent_id,
                _source_deployment: d.deployment_id,
              })))
              .catch(() => []),
          )
        }
      }
      const perDeployment = await Promise.all(tailFetches)
      const merged = perDeployment.flat()
      // Dedup by seq within each source (seq is engine-scoped;
      // across agents seqs can collide so include source in key).
      const byId = new Map(events.map(e => [`${e._source_agent}#${e.seq}`, e]))
      for (const ev of merged) {
        byId.set(`${ev._source_agent}#${ev.seq}`, ev)
        if (ev.seq > lastSeq) lastSeq = ev.seq
      }
      // Sort newest-first by timestamp since seq isn't globally
      // comparable across agents.
      const sorted = Array.from(byId.values()).sort((a, b) => {
        const at = Date.parse(a.timestamp) || 0
        const bt = Date.parse(b.timestamp) || 0
        return bt - at
      })
      events = sorted.slice(0, MAX)
      error = ''
    } catch (e) {
      error = e.message || String(e)
    }
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 3000)
    return () => clearInterval(id)
  })

  // Fix #2 — real SHA-256 chain verify (backend). Button
  // triggers POST /api/v1/audit/verify; controller fan-outs to
  // every running deployment so each agent verifies its own
  // file. We render the aggregate result inline.
  let verifyReport = $state(null)
  let verifyBusy = $state(false)

  async function runVerify() {
    if (verifyBusy) return
    verifyBusy = true
    verifyReport = { phase: 'pending' }
    try {
      const resp = await api.authedFetch('/api/v1/audit/verify', { method: 'POST' })
      if (!resp.ok) throw new Error(`${resp.status} ${await resp.text()}`)
      const body = await resp.json()
      verifyReport = { phase: 'done', ...body }
    } catch (e) {
      verifyReport = { phase: 'err', error: e.message || String(e) }
    } finally {
      verifyBusy = false
    }
  }

  // Wave D1 — hash-chain verification. The audit JSONL carries
  // `prev_hash` on every row (risk::audit::AuditEvent). For each
  // (agent, deployment) source we walk the row sequence oldest-
  // first: a break shows when two consecutive rows share the
  // same prev_hash (missing row between), when a non-first row
  // carries `prev_hash: null` (truncation), or when the seq
  // jumps backwards. We annotate events with `_chain_broken`
  // so the UI renders them in a distinct band.
  const eventsWithChainStatus = $derived.by(() => {
    const bySource = new Map()
    for (const ev of events) {
      const key = `${ev._source_agent}#${ev._source_deployment}`
      if (!bySource.has(key)) bySource.set(key, [])
      bySource.get(key).push(ev)
    }
    const brokenKeys = new Set()
    let totalSources = 0
    let brokenSources = 0
    for (const [, list] of bySource) {
      totalSources += 1
      // sort oldest-first
      list.sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0))
      let thisBroken = false
      let prevPrev = undefined
      for (let i = 0; i < list.length; i++) {
        const curr = list[i]
        const k = `${curr._source_agent}#${curr._source_deployment}#${curr.seq}`
        // Non-first row with null prev_hash — truncation point.
        if (i > 0 && (curr.prev_hash === null || curr.prev_hash === undefined)) {
          brokenKeys.add(k); thisBroken = true
        }
        // Two consecutive rows sharing the same prev_hash —
        // implies a row was deleted between them.
        if (i > 0 && prevPrev === curr.prev_hash) {
          brokenKeys.add(k); thisBroken = true
        }
        prevPrev = curr.prev_hash
      }
      if (thisBroken) brokenSources += 1
    }
    return {
      events: events.map(ev => ({
        ...ev,
        _chain_broken: brokenKeys.has(`${ev._source_agent}#${ev._source_deployment}#${ev.seq}`),
      })),
      totalSources,
      brokenSources,
      brokenRowCount: brokenKeys.size,
    }
  })

  const filtered = $derived.by(() => {
    const src = eventsWithChainStatus.events
    if (!filter) return src
    const q = filter.toLowerCase()
    return src.filter(
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
      {#if eventsWithChainStatus.brokenRowCount > 0}
        <span class="chain-chip chain-broken" title={`${eventsWithChainStatus.brokenSources} source(s) have a broken hash chain`}>
          chain broken: {eventsWithChainStatus.brokenRowCount}
        </span>
      {:else if events.length > 0}
        <span class="chain-chip chain-ok" title="Every audit source has a continuous hash chain">
          chain ok
        </span>
      {/if}
    </div>
    <div class="head-actions">
      <button
        type="button"
        class="btn btn-ghost btn-sm"
        onclick={runVerify}
        disabled={verifyBusy}
        title="Run real SHA-256 hash-chain verify across every deployment's audit file"
      >
        <Icon name="shield" size={12} />
        <span>{verifyBusy ? 'Verifying…' : 'Verify chain'}</span>
      </button>
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
    </div>
  </header>

  {#if verifyReport}
    <div class="verify-report" class:err={verifyReport.phase === 'err' || (verifyReport.broken ?? 0) > 0}>
      {#if verifyReport.phase === 'pending'}
        <span>Verifying…</span>
      {:else if verifyReport.phase === 'err'}
        <span>verify failed: {verifyReport.error}</span>
      {:else}
        <span>
          ✓ {verifyReport.valid}/{verifyReport.total_deployments} valid
          {#if verifyReport.broken > 0}· <strong>{verifyReport.broken} broken</strong>{/if}
          {#if verifyReport.missing > 0}· {verifyReport.missing} missing{/if}
        </span>
        {#if verifyReport.broken > 0}
          <div class="verify-broken-list">
            {#each verifyReport.rows.filter(r => r.exists && !r.valid) as r (r.agent_id + '/' + r.deployment_id)}
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
      {#each filtered as e (e._source_agent + '#' + e.seq)}
        <div class="log-row" data-sev={severity(e.event_type)} class:chain-break={e._chain_broken}>
          <span class="dot"></span>
          <span class="ts num">{fmtTime(e.timestamp)}</span>
          {#if e._chain_broken}
            <span class="chain-break-chip" title="Hash chain break — prev_hash doesn't match the expected predecessor. Indicates insertion / deletion / truncation in the log.">
              ✗ chain
            </span>
          {/if}
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

  .chain-chip {
    font-size: 10px; font-family: var(--font-mono);
    padding: 2px 8px; border-radius: var(--r-sm);
  }
  .chain-chip.chain-ok     { background: color-mix(in srgb, var(--ok) 15%, transparent); color: var(--ok); }
  .chain-chip.chain-broken { background: color-mix(in srgb, var(--danger) 20%, transparent); color: var(--danger); font-weight: 600; }

  .log-row.chain-break {
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    border-left: 2px solid var(--danger);
  }
  .chain-break-chip {
    font-size: 10px; font-family: var(--font-mono);
    padding: 1px 5px; border-radius: var(--r-sm);
    background: color-mix(in srgb, var(--danger) 20%, transparent);
    color: var(--danger); font-weight: 600;
  }

  .head-actions { display: flex; gap: var(--s-2); }
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
