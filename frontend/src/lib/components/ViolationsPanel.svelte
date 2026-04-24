<script>
  /*
   * Wave D5 — compliance violations panel.
   *
   * Aggregates four signal streams into a single watch list:
   *   - SLA uptime < 95%           → /api/v1/sla
   *   - Kill ladder escalated      → fleet rows (kill_level > 0)
   *   - Reconciliation drift       → /api/v1/reconciliation/fleet
   *   - Manipulation score > 0.8   → /api/v1/surveillance/fleet
   *
   * Presents a single sorted list of open violations. Aggregation
   * logic lives in ./violations/aggregate-violations.js; row
   * rendering + actions live in ./violations/ViolationRow.svelte.
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'
  import ViolationRow from './violations/ViolationRow.svelte'
  import { aggregateViolations } from './violations/aggregate-violations.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 5000

  let slaRows = $state([])
  let fleetRows = $state([])
  let reconRows = $state([])
  let manipulationRows = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  // Wave G1 — per-row action state.
  // `acknowledged` is an in-memory set of violation keys the
  // operator clicked "Hide" on. They stay hidden locally until
  // the metric recovers or the page reloads.
  let acknowledged = $state({})
  let actionBusy = $state({})
  let actionStatus = $state({})  // key → { phase, text }

  async function refresh() {
    try {
      const [s, f, r, m] = await Promise.all([
        api.getJson('/api/v1/sla').catch(() => []),
        api.getJson('/api/v1/fleet').catch(() => []),
        api.getJson('/api/v1/reconciliation/fleet').catch(() => []),
        api.getJson('/api/v1/surveillance/fleet').catch(() => []),
      ])
      slaRows = Array.isArray(s) ? s : []
      fleetRows = Array.isArray(f) ? f : []
      reconRows = Array.isArray(r) ? r : []
      manipulationRows = Array.isArray(m) ? m : []
      error = null
      lastFetch = new Date()
    } catch (e) {
      error = e?.message || String(e)
    }
  }

  $effect(() => {
    refresh()
    const iv = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(iv)
  })

  function hideViolation(key) {
    acknowledged[key] = Date.now()
    acknowledged = { ...acknowledged }
    actionStatus[key] = { phase: 'ok', text: 'acknowledged' }
    actionStatus = { ...actionStatus }
  }

  // Wave G2 — open a persistent incident from this violation so
  // on-call has a paper trail + workflow (ack → resolve). Local
  // Hide just tidies the list; opening an incident records it
  // on the controller.
  async function openIncident(v) {
    if (actionBusy[v.key]) return
    actionBusy[v.key] = true
    actionBusy = { ...actionBusy }
    actionStatus[v.key] = { phase: 'pending', text: 'opening incident…' }
    actionStatus = { ...actionStatus }
    try {
      const r = await api.authedFetch('/api/v1/incidents', {
        method: 'POST',
        body: JSON.stringify({
          violation_key: v.key,
          severity: v.severity,
          category: v.category,
          target: v.target,
          metric: v.metric,
          detail: v.detail,
        }),
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      const inc = await r.json()
      actionStatus[v.key] = { phase: 'ok', text: `incident ${inc.id.slice(0, 6)}… opened` }
    } catch (e) {
      actionStatus[v.key] = { phase: 'err', text: e?.message || String(e) }
    } finally {
      actionStatus = { ...actionStatus }
      delete actionBusy[v.key]
      actionBusy = { ...actionBusy }
    }
  }

  async function dispatchOpOnDeployments(deps, op, reason) {
    const results = []
    for (const d of deps) {
      try {
        const path = `/api/v1/agents/${encodeURIComponent(d.agent_id)}`
          + `/deployments/${encodeURIComponent(d.deployment_id)}/ops/${encodeURIComponent(op)}`
        const r = await api.authedFetch(path, {
          method: 'POST',
          body: JSON.stringify({ reason }),
        })
        if (!r.ok) {
          const text = await r.text().catch(() => '')
          results.push({ ok: false, err: `${r.status} ${text}`, dep: d })
        } else {
          results.push({ ok: true, dep: d })
        }
      } catch (e) {
        results.push({ ok: false, err: e?.message || String(e), dep: d })
      }
    }
    return results
  }

  async function dispatchAction(v, op, verb) {
    if (!v.deployments?.length) return
    if (actionBusy[v.key]) return
    actionBusy[v.key] = true
    actionBusy = { ...actionBusy }
    actionStatus[v.key] = { phase: 'pending', text: `${verb} ${v.deployments.length}…` }
    actionStatus = { ...actionStatus }
    const res = await dispatchOpOnDeployments(
      v.deployments, op, `violations panel: ${v.category} ${v.metric}`,
    )
    const ok = res.filter((r) => r.ok).length
    actionStatus[v.key] = {
      phase: ok === res.length ? 'ok' : 'warn',
      text: op === 'widen' ? `L1 on ${ok}/${res.length}` : `${verb} ${ok}/${res.length}`,
    }
    actionStatus = { ...actionStatus }
    delete actionBusy[v.key]
    actionBusy = { ...actionBusy }
  }

  const actPause = (v) => dispatchAction(v, 'pause', 'paused')
  const actWidenL1 = (v) => dispatchAction(v, 'widen', 'widening')

  const violations = $derived(aggregateViolations({
    slaRows, fleetRows, reconRows, manipulationRows,
  }))

  // Wave G1 — split into open (render) vs acknowledged (counted
  // at header). Ack is session-only and does NOT touch engine
  // state — it's purely a tidy-the-view affordance.
  const openViolations = $derived(violations.filter((v) => !acknowledged[v.key]))
  const ackedViolations = $derived(violations.filter((v) => acknowledged[v.key]))
  const counts = $derived.by(() => {
    const c = { high: 0, med: 0, low: 0 }
    for (const v of openViolations) c[v.severity]++
    return c
  })
</script>

<div class="panel">
  <header class="head">
    <div class="head-meta">
      <span class="label">Compliance violations</span>
      <span class="hint">rollup across SLA · kill · recon · manipulation</span>
    </div>
    <div class="head-right">
      {#if counts.high > 0}<span class="sev-chip sev-high">{counts.high} high</span>{/if}
      {#if counts.med > 0}<span class="sev-chip sev-med">{counts.med} med</span>{/if}
      {#if counts.low > 0}<span class="sev-chip sev-low">{counts.low} low</span>{/if}
      {#if lastFetch && openViolations.length === 0}
        <span class="all-clear">all clear · {lastFetch.toLocaleTimeString()}</span>
      {/if}
      {#if ackedViolations.length > 0}
        <span class="ack-chip" title="Acknowledged rows hidden from the list">
          {ackedViolations.length} acked
        </span>
      {/if}
    </div>
  </header>

  {#if error}
    <div class="error">error: {error}</div>
  {/if}

  {#if openViolations.length === 0}
    <div class="empty">
      <Icon name="check" size={14} />
      <span>
        {ackedViolations.length > 0
          ? `No open violations · ${ackedViolations.length} acknowledged (hidden)`
          : 'No open violations across the fleet.'}
      </span>
    </div>
  {:else}
    <table class="vio-table">
      <thead>
        <tr>
          <th>severity</th>
          <th>category</th>
          <th>target</th>
          <th>metric</th>
          <th>detail</th>
          <th class="actions-col">actions</th>
        </tr>
      </thead>
      <tbody>
        {#each openViolations as v (v.key)}
          <ViolationRow
            violation={v}
            busy={!!actionBusy[v.key]}
            status={actionStatus[v.key]}
            onPause={actPause}
            onWidenL1={actWidenL1}
            onOpenIncident={openIncident}
            onHide={hideViolation}
          />
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .panel {
    display: flex; flex-direction: column; gap: var(--s-2);
    padding: var(--s-3); background: var(--bg-raised);
    border: 1px solid var(--border-subtle); border-radius: var(--r-md);
  }
  .head { display: flex; align-items: center; justify-content: space-between; gap: var(--s-3); flex-wrap: wrap; }
  .head-meta { display: flex; flex-direction: column; gap: 2px; }
  .label { font-size: var(--fs-sm); font-weight: 600; color: var(--fg-primary); }
  .hint { font-size: 10px; color: var(--fg-muted); }
  .head-right { display: flex; gap: var(--s-2); align-items: center; }
  .all-clear { font-size: 10px; color: var(--ok); font-family: var(--font-mono); }

  .sev-chip {
    padding: 2px 8px; font-size: 10px; font-family: var(--font-mono);
    border-radius: var(--r-sm); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
  }
  .sev-chip.sev-high { background: color-mix(in srgb, var(--danger) 25%, transparent); color: var(--danger); font-weight: 600; }
  .sev-chip.sev-med  { background: color-mix(in srgb, var(--warn) 20%, transparent); color: var(--warn); }
  .sev-chip.sev-low  { background: color-mix(in srgb, var(--accent) 18%, transparent); color: var(--accent); }

  .error { color: var(--neg); font-size: var(--fs-xs); }
  .empty {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-3); font-size: var(--fs-sm);
    background: color-mix(in srgb, var(--ok) 10%, transparent);
    color: var(--ok); border-radius: var(--r-sm);
  }

  .vio-table { width: 100%; border-collapse: collapse; }
  .vio-table th {
    padding: var(--s-2); font-size: var(--fs-xs); text-align: left;
    border-bottom: 1px solid var(--border-subtle);
    color: var(--fg-muted); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-size: 10px;
  }
  .actions-col { text-align: right; }
  .ack-chip {
    font-size: 10px; font-family: var(--font-mono);
    padding: 2px 6px; border-radius: var(--r-sm);
    background: var(--bg-raised); color: var(--fg-muted);
  }
</style>
