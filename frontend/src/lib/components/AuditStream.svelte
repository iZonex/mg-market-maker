<script>
  /*
   * Unified audit feed across the fleet.
   *
   * Audit lives on each agent's disk (engine writes to
   * `data/audit/{symbol}.jsonl` in the agent's CWD). We fan out
   * across the fleet, fetch each deployment's audit tail via the
   * per-deployment details endpoint, and merge by `seq` (audit
   * entries are monotonic, but seqs can collide across agents so
   * the key includes the source).
   *
   * Chain inspection + severity mapping live in
   * ./audit/audit-chain-analyzer.js; the verify-report panel
   * lives in ./audit/VerifyReportBanner.svelte.
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'
  import { Button } from '../primitives/index.js'
  import VerifyReportBanner from './audit/VerifyReportBanner.svelte'
  import { analyzeChain, severityFor } from './audit/audit-chain-analyzer.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const MAX = 200
  let events = $state([])
  let error = $state('')
  let filter = $state('')
  let pausePoll = $state(false)

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
              .then((resp) => (resp.payload?.events || []).map((ev) => ({
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
      const byId = new Map(events.map((e) => [`${e._source_agent}#${e.seq}`, e]))
      for (const ev of merged) {
        byId.set(`${ev._source_agent}#${ev.seq}`, ev)
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

  // Fix #2 — real SHA-256 chain verify (backend). Button triggers
  // POST /api/v1/audit/verify; controller fan-outs to every
  // running deployment so each agent verifies its own file.
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

  const chainStatus = $derived(analyzeChain(events))

  const filtered = $derived.by(() => {
    const src = chainStatus.events
    if (!filter) return src
    const q = filter.toLowerCase()
    return src.filter(
      (e) =>
        (e.event_type || '').toLowerCase().includes(q) ||
        (e.symbol || '').toLowerCase().includes(q) ||
        (e.detail || '').toLowerCase().includes(q),
    )
  })

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
      {#if chainStatus.brokenRowCount > 0}
        <span class="chain-chip chain-broken" title={`${chainStatus.brokenSources} source(s) have a broken hash chain`}>
          chain broken: {chainStatus.brokenRowCount}
        </span>
      {:else if events.length > 0}
        <span class="chain-chip chain-ok" title="Every audit source has a continuous hash chain">
          chain ok
        </span>
      {/if}
    </div>
    <div class="head-actions">
      <Button variant="primary" onclick={runVerify} disabled={verifyBusy}
        title="Run real SHA-256 hash-chain verify across every deployment's audit file">
        {#snippet children()}<Icon name="shield" size={12} />
        <span>{verifyBusy ? 'Verifying…' : 'Verify chain'}</span>{/snippet}
      </Button>
      <Button variant="ghost" onclick={() => (pausePoll = !pausePoll)} aria-label={pausePoll ? 'Resume' : 'Pause'}>
        {#snippet children()}{#if pausePoll}
          <Icon name="pulse" size={12} />
          <span>Resume</span>
        {:else}
          <Icon name="clock" size={12} />
          <span>Pause</span>
        {/if}{/snippet}
      </Button>
    </div>
  </header>

  <VerifyReportBanner report={verifyReport} />

  <div class="search">
    <span class="search-icon"><Icon name="search" size={13} /></span>
    <input
      type="text"
      class="input search-input"
      placeholder="filter by type / symbol / detail…"
      bind:value={filter}
    />
    {#if filter}
      <Button variant="ghost" size="sm" iconOnly onclick={() => (filter = '')} aria-label="clear">
        {#snippet children()}<Icon name="close" size={12} />{/snippet}
      </Button>
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
        <div class="log-row" data-sev={severityFor(e.event_type)} class:chain-break={e._chain_broken}>
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
  .head-actions { display: flex; gap: var(--s-2); }
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

  .alert-bar {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--neg-bg);
    border: 1px solid color-mix(in srgb, var(--neg) 30%, transparent);
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
