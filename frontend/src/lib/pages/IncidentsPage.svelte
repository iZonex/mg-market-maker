<script>
  /*
   * Wave G2/G4 — incident lifecycle UI.
   *
   * Operator opens incidents from ViolationsPanel; this page
   * lists them by state (open / acked / resolved) and drives
   * the ack + resolve forms. On resolve the operator fills a
   * post-mortem (root cause / action taken / preventive) that
   * stamps onto the record.
   *
   * Backend endpoints:
   *   GET  /api/v1/incidents
   *   POST /api/v1/incidents
   *   POST /api/v1/incidents/{id}/ack
   *   POST /api/v1/incidents/{id}/resolve
   */
  import Card from '../components/Card.svelte'
  import Icon from '../components/Icon.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 5000

  let incidents = $state([])
  let loading = $state(true)
  let error = $state(null)
  let selectedId = $state(null)
  let resolveForm = $state({ root_cause: '', action_taken: '', preventive: '' })
  let busyId = $state({})
  let filter = $state('open')  // 'open' | 'acked' | 'resolved' | 'all'

  async function refresh() {
    try {
      const r = await api.getJson('/api/v1/incidents')
      incidents = Array.isArray(r) ? r : []
      error = null
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const iv = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(iv)
  })

  async function ack(id) {
    if (busyId[id]) return
    busyId[id] = true; busyId = { ...busyId }
    try {
      const r = await api.authedFetch(`/api/v1/incidents/${encodeURIComponent(id)}/ack`, {
        method: 'POST',
        body: JSON.stringify({}),
      })
      if (!r.ok) throw new Error(`${r.status} ${await r.text()}`)
      await refresh()
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      delete busyId[id]; busyId = { ...busyId }
    }
  }

  async function resolve(id) {
    if (busyId[id]) return
    if (!resolveForm.root_cause.trim()) {
      error = 'Root cause is required to resolve'
      return
    }
    busyId[id] = true; busyId = { ...busyId }
    try {
      const r = await api.authedFetch(`/api/v1/incidents/${encodeURIComponent(id)}/resolve`, {
        method: 'POST',
        body: JSON.stringify({
          root_cause: resolveForm.root_cause.trim(),
          action_taken: resolveForm.action_taken.trim() || null,
          preventive: resolveForm.preventive.trim() || null,
        }),
      })
      if (!r.ok) throw new Error(`${r.status} ${await r.text()}`)
      selectedId = null
      resolveForm = { root_cause: '', action_taken: '', preventive: '' }
      error = null
      await refresh()
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      delete busyId[id]; busyId = { ...busyId }
    }
  }

  const filtered = $derived.by(() => {
    if (filter === 'all') return incidents
    return incidents.filter(i => i.state === filter)
  })

  const counts = $derived.by(() => {
    const c = { open: 0, acked: 0, resolved: 0 }
    for (const i of incidents) {
      if (c[i.state] !== undefined) c[i.state]++
    }
    return c
  })

  function fmtTime(ms) {
    if (!ms) return '—'
    return new Date(ms).toLocaleString()
  }

  function ageMs(ms) {
    if (!ms) return ''
    const dt = Date.now() - ms
    if (dt < 60_000) return `${Math.round(dt / 1000)}s ago`
    if (dt < 3600_000) return `${Math.round(dt / 60_000)}m ago`
    return `${Math.round(dt / 3600_000)}h ago`
  }
</script>

<div class="page scroll">
  <Card title="Incidents" subtitle={`${counts.open} open · ${counts.acked} acked · ${counts.resolved} resolved`} span={3}>
    {#snippet children()}
      <div class="filter-row">
        <button class="filter-btn" class:on={filter === 'open'}     onclick={() => (filter = 'open')}>Open ({counts.open})</button>
        <button class="filter-btn" class:on={filter === 'acked'}    onclick={() => (filter = 'acked')}>Acked ({counts.acked})</button>
        <button class="filter-btn" class:on={filter === 'resolved'} onclick={() => (filter = 'resolved')}>Resolved ({counts.resolved})</button>
        <button class="filter-btn" class:on={filter === 'all'}      onclick={() => (filter = 'all')}>All</button>
      </div>

      {#if error}
        <div class="err-banner">{error}</div>
      {/if}

      {#if loading}
        <div class="muted">Loading…</div>
      {:else if filtered.length === 0}
        <div class="empty">
          <Icon name="check" size={14} />
          <span>No incidents in <strong>{filter}</strong> state.</span>
        </div>
      {:else}
        <div class="rows">
          {#each filtered as inc (inc.id)}
            {@const isOpen = selectedId === inc.id}
            <div class="inc-card state-{inc.state}">
              <div class="inc-head" onclick={() => (selectedId = isOpen ? null : inc.id)}>
                <span class="sev sev-{inc.severity}">{inc.severity}</span>
                <span class="cat mono">{inc.category}</span>
                <span class="target mono">{inc.target}</span>
                <span class="metric mono">{inc.metric}</span>
                <span class="state-chip state-chip-{inc.state}">{inc.state}</span>
                <span class="age">{ageMs(inc.opened_at_ms)}</span>
                <span class="chev">{isOpen ? '▾' : '▸'}</span>
              </div>
              {#if isOpen}
                <div class="inc-body">
                  <div class="inc-detail">{inc.detail}</div>
                  <div class="inc-meta">
                    <span>opened {fmtTime(inc.opened_at_ms)} by <code>{inc.opened_by}</code></span>
                    {#if inc.acked_at_ms}
                      <span>· acked {fmtTime(inc.acked_at_ms)} by <code>{inc.acked_by}</code></span>
                    {/if}
                    {#if inc.resolved_at_ms}
                      <span>· resolved {fmtTime(inc.resolved_at_ms)} by <code>{inc.resolved_by}</code></span>
                    {/if}
                  </div>

                  {#if inc.state === 'resolved'}
                    <div class="postmortem">
                      <div class="pm-row"><span class="pm-k">Root cause</span><span class="pm-v">{inc.root_cause || '—'}</span></div>
                      {#if inc.action_taken}
                        <div class="pm-row"><span class="pm-k">Action taken</span><span class="pm-v">{inc.action_taken}</span></div>
                      {/if}
                      {#if inc.preventive}
                        <div class="pm-row"><span class="pm-k">Preventive</span><span class="pm-v">{inc.preventive}</span></div>
                      {/if}
                    </div>
                  {:else}
                    <div class="inc-actions">
                      {#if inc.state === 'open'}
                        <button class="btn ghost small" disabled={busyId[inc.id]} onclick={() => ack(inc.id)}>Acknowledge</button>
                      {/if}
                      <details class="resolve-details">
                        <summary class="btn ok small">Resolve…</summary>
                        <div class="resolve-form">
                          <label>
                            <span>Root cause <span class="req">*</span></span>
                            <textarea rows="2" bind:value={resolveForm.root_cause} placeholder="What actually caused this?"></textarea>
                          </label>
                          <label>
                            <span>Action taken</span>
                            <textarea rows="2" bind:value={resolveForm.action_taken} placeholder="What did you do to fix it?"></textarea>
                          </label>
                          <label>
                            <span>Preventive</span>
                            <textarea rows="2" bind:value={resolveForm.preventive} placeholder="How do we stop this from happening again?"></textarea>
                          </label>
                          <div class="resolve-actions">
                            <button class="btn ok" disabled={busyId[inc.id]} onclick={() => resolve(inc.id)}>
                              {busyId[inc.id] ? 'Resolving…' : 'Confirm resolve'}
                            </button>
                          </div>
                        </div>
                      </details>
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    {/snippet}
  </Card>
</div>

<style>
  .page { padding: var(--s-4); }
  .scroll { overflow-y: auto; }
  .muted { color: var(--fg-muted); font-size: var(--fs-sm); padding: var(--s-3); }
  .empty {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-3);
    background: color-mix(in srgb, var(--ok) 10%, transparent);
    color: var(--ok); border-radius: var(--r-sm); font-size: var(--fs-sm);
  }
  .err-banner {
    padding: var(--s-2); background: color-mix(in srgb, var(--danger) 15%, transparent);
    color: var(--danger); border-radius: var(--r-sm); font-size: var(--fs-xs);
    margin-bottom: var(--s-2);
  }

  .filter-row { display: flex; gap: 4px; margin-bottom: var(--s-3); }
  .filter-btn {
    padding: 4px 12px; font-size: var(--fs-xs);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-secondary);
    cursor: pointer;
  }
  .filter-btn.on { background: var(--accent); color: var(--bg-base); border-color: var(--accent); }

  .rows { display: flex; flex-direction: column; gap: var(--s-2); }
  .inc-card { border: 1px solid var(--border-subtle); border-radius: var(--r-md); background: var(--bg-raised); }
  .inc-card.state-open { border-left: 3px solid var(--danger); }
  .inc-card.state-acked { border-left: 3px solid var(--warn); }
  .inc-card.state-resolved { border-left: 3px solid var(--ok); opacity: 0.8; }

  .inc-head {
    display: grid;
    grid-template-columns: 70px 100px 1fr 140px 90px 90px 20px;
    gap: var(--s-2); align-items: center;
    padding: var(--s-2) var(--s-3);
    cursor: pointer;
    font-size: var(--fs-xs);
  }
  .inc-head:hover { background: var(--bg-chip); }
  .sev { padding: 2px 8px; border-radius: var(--r-sm); font-weight: 600; font-size: 10px; text-transform: uppercase; letter-spacing: var(--tracking-label); text-align: center; }
  .sev-high { background: color-mix(in srgb, var(--danger) 25%, transparent); color: var(--danger); }
  .sev-med  { background: color-mix(in srgb, var(--warn) 22%, transparent); color: var(--warn); }
  .sev-low  { background: color-mix(in srgb, var(--accent) 18%, transparent); color: var(--accent); }
  .cat, .target, .metric { color: var(--fg-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .state-chip { padding: 1px 6px; border-radius: var(--r-sm); font-size: 10px; font-family: var(--font-mono); background: var(--bg-chip); color: var(--fg-secondary); text-align: center; }
  .state-chip-open { color: var(--danger); }
  .state-chip-acked { color: var(--warn); }
  .state-chip-resolved { color: var(--ok); }
  .age { color: var(--fg-muted); font-size: 10px; font-family: var(--font-mono); text-align: right; }
  .chev { color: var(--fg-muted); }

  .inc-body {
    padding: var(--s-3);
    border-top: 1px solid var(--border-subtle);
    display: flex; flex-direction: column; gap: var(--s-2);
  }
  .inc-detail { font-size: var(--fs-sm); color: var(--fg-secondary); }
  .inc-meta { font-size: 10px; color: var(--fg-muted); font-family: var(--font-mono); }
  .inc-meta code { background: var(--bg-chip); padding: 0 4px; border-radius: 2px; }
  .inc-actions { display: flex; gap: var(--s-2); align-items: flex-start; }

  .resolve-details summary { list-style: none; cursor: pointer; }
  .resolve-details summary::-webkit-details-marker { display: none; }
  .resolve-form {
    display: flex; flex-direction: column; gap: var(--s-2);
    padding: var(--s-3);
    background: var(--bg-chip); border-radius: var(--r-sm);
    margin-top: var(--s-2);
  }
  .resolve-form label { display: flex; flex-direction: column; gap: 4px; }
  .resolve-form label > span { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .resolve-form .req { color: var(--danger); }
  .resolve-form textarea {
    padding: var(--s-2); background: var(--bg-raised);
    border: 1px solid var(--border-subtle); border-radius: var(--r-sm);
    color: var(--fg-primary); font-size: var(--fs-xs); font-family: inherit;
    resize: vertical;
  }
  .resolve-actions { display: flex; justify-content: flex-end; }

  .postmortem { display: flex; flex-direction: column; gap: 4px; }
  .pm-row { display: flex; gap: var(--s-2); }
  .pm-k { font-size: 10px; color: var(--fg-muted); min-width: 100px; letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .pm-v { font-size: var(--fs-xs); color: var(--fg-primary); line-height: 1.5; }
</style>
