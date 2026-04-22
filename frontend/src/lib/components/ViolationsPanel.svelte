<script>
  /*
   * Wave D5 — compliance violations panel.
   *
   * Aggregates four signal streams into a single watch list:
   *   - SLA uptime < 95%           → from /api/v1/sla (fleet-aware)
   *   - Kill ladder escalated      → from fleet rows (kill_level > 0)
   *   - Reconciliation drift       → from /api/v1/reconciliation/fleet
   *   - Manipulation score > 0.8   → from /api/v1/surveillance/fleet
   *
   * Presents a single sorted list of open violations so on-call
   * sees the fleet's state of compliance at a glance rather than
   * visiting four pages. Rows include (severity · category ·
   * agent/symbol · metric · detail) with an optional deep-link
   * back to the source page.
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

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
  // operator clicked "Ack" on. They stay hidden locally until
  // the metric recovers or the page reloads.
  let acknowledged = $state({})
  let actionBusy = $state({})
  let actionStatus = $state({})  // key → { phase, text }

  // Wave G3 — per-category auto-action hint. We fetch current
  // tunables and show a banner when operator has auto-widen on
  // for this category, so the row's "Widen L1" button stays a
  // manual override while the loop handles recurrence.
  let tunables = $state(null)
  async function refreshTunables() {
    try {
      tunables = await api.getJson('/api/v1/tunables').catch(() => null)
    } catch { /* ignore */ }
  }
  $effect(() => { refreshTunables() })

  async function ackViolation(key) {
    acknowledged[key] = Date.now()
    acknowledged = { ...acknowledged }
    actionStatus[key] = { phase: 'ok', text: 'acknowledged' }
    actionStatus = { ...actionStatus }
  }

  // Wave G2 — open a persistent incident from this violation
  // so on-call has a paper trail + workflow (ack → resolve).
  // Local Ack (above) just hides the row; opening an incident
  // records it on the controller.
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
      actionStatus = { ...actionStatus }
    } catch (e) {
      actionStatus[v.key] = { phase: 'err', text: e?.message || String(e) }
      actionStatus = { ...actionStatus }
    } finally {
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

  async function actPause(v) {
    if (!v.deployments?.length) return
    if (actionBusy[v.key]) return
    actionBusy[v.key] = true
    actionBusy = { ...actionBusy }
    actionStatus[v.key] = { phase: 'pending', text: `pausing ${v.deployments.length}…` }
    actionStatus = { ...actionStatus }
    const res = await dispatchOpOnDeployments(
      v.deployments,
      'pause',
      `violations panel: ${v.category} ${v.metric}`,
    )
    const ok = res.filter(r => r.ok).length
    actionStatus[v.key] = {
      phase: ok === res.length ? 'ok' : 'warn',
      text: `paused ${ok}/${res.length}`,
    }
    actionStatus = { ...actionStatus }
    delete actionBusy[v.key]
    actionBusy = { ...actionBusy }
  }

  async function actWidenL1(v) {
    if (!v.deployments?.length) return
    if (actionBusy[v.key]) return
    actionBusy[v.key] = true
    actionBusy = { ...actionBusy }
    actionStatus[v.key] = { phase: 'pending', text: `widening ${v.deployments.length}…` }
    actionStatus = { ...actionStatus }
    const res = await dispatchOpOnDeployments(
      v.deployments,
      'widen',
      `violations panel: ${v.category} ${v.metric}`,
    )
    const ok = res.filter(r => r.ok).length
    actionStatus[v.key] = {
      phase: ok === res.length ? 'ok' : 'warn',
      text: `L1 on ${ok}/${res.length}`,
    }
    actionStatus = { ...actionStatus }
    delete actionBusy[v.key]
    actionBusy = { ...actionBusy }
  }

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

  const violations = $derived.by(() => {
    const out = []
    // SLA breaches.
    // Build symbol → deployments map so SLA/presence rows can
    // target concrete deployments, not just "the symbol".
    const symbolToDeps = new Map()
    for (const a of fleetRows) {
      for (const d of a.deployments || []) {
        if (!symbolToDeps.has(d.symbol)) symbolToDeps.set(d.symbol, [])
        symbolToDeps.get(d.symbol).push({ agent_id: a.agent_id, deployment_id: d.deployment_id })
      }
    }

    for (const s of slaRows) {
      const uptime = Number(s.uptime_pct ?? 0)
      const deps = symbolToDeps.get(s.symbol) || []
      if (uptime > 0 && uptime < 95) {
        out.push({
          key: `sla#${s.symbol}`,
          severity: uptime < 90 ? 'high' : 'med',
          category: 'SLA',
          target: s.symbol,
          metric: `uptime ${uptime.toFixed(2)}%`,
          detail: 'Below 95% presence floor (MiCA obligation).',
          deployments: deps,
        })
      }
      const presence = Number(s.presence_pct_24h ?? 0)
      if (presence > 0 && presence < 95) {
        out.push({
          key: `presence#${s.symbol}`,
          severity: presence < 90 ? 'high' : 'med',
          category: 'presence',
          target: s.symbol,
          metric: `24h presence ${presence.toFixed(2)}%`,
          detail: 'Per-pair two-sided presence breaches the MiCA rolling window.',
          deployments: deps,
        })
      }
    }
    // Kill-ladder escalations.
    for (const a of fleetRows) {
      for (const d of a.deployments || []) {
        if ((d.kill_level || 0) > 0) {
          out.push({
            key: `kill#${a.agent_id}/${d.deployment_id}`,
            severity: d.kill_level >= 4 ? 'high' : d.kill_level >= 2 ? 'med' : 'low',
            category: 'kill',
            target: `${a.agent_id} · ${d.symbol}`,
            metric: `L${d.kill_level}`,
            detail: 'Kill ladder escalated — strategy is not running normally.',
            deployments: [{ agent_id: a.agent_id, deployment_id: d.deployment_id }],
          })
        }
      }
    }
    // Reconciliation drift.
    for (const r of reconRows) {
      if (!r.has_drift) continue
      const bits = []
      if (r.ghost_orders?.length > 0) bits.push(`${r.ghost_orders.length} ghost`)
      if (r.phantom_orders?.length > 0) bits.push(`${r.phantom_orders.length} phantom`)
      if (r.balance_mismatches?.length > 0) bits.push(`${r.balance_mismatches.length} bal Δ`)
      if (r.orders_fetch_failed) bits.push('fetch fail')
      out.push({
        key: `recon#${r.agent_id}/${r.deployment_id}`,
        severity: r.orders_fetch_failed ? 'high' : 'med',
        category: 'recon',
        target: `${r.agent_id} · ${r.symbol}`,
        metric: bits.join(' · '),
        detail: 'Order/balance reconciliation cycle reported drift this tick.',
        deployments: [{ agent_id: r.agent_id, deployment_id: r.deployment_id }],
      })
    }
    // Manipulation detector escalations.
    for (const m of manipulationRows) {
      const score = Number(m.combined || 0)
      if (score >= 0.8) {
        out.push({
          key: `manip#${m.agent_id}/${m.deployment_id}`,
          severity: score >= 0.95 ? 'high' : 'med',
          category: 'manip',
          target: `${m.agent_id} · ${m.symbol}`,
          metric: `combined ${(score * 100).toFixed(0)}%`,
          detail: 'Manipulation detector score breached alert threshold (0.8).',
          deployments: [{ agent_id: m.agent_id, deployment_id: m.deployment_id }],
        })
      }
    }
    // High severity first; ties break by category then target.
    const rank = { high: 0, med: 1, low: 2 }
    out.sort((a, b) => {
      const r = rank[a.severity] - rank[b.severity]
      if (r !== 0) return r
      return a.category.localeCompare(b.category) || a.target.localeCompare(b.target)
    })
    return out
  })

  // Wave G1 — split violations into open (render in table) and
  // acknowledged (counted at header, kept out of main list until
  // reload). Ack is deliberately in-memory / session-only; it
  // does NOT change anything on the engine side, it just tidies
  // the operator's view while they work an incident.
  const openViolations = $derived(violations.filter(v => !acknowledged[v.key]))
  const ackedViolations = $derived(violations.filter(v => acknowledged[v.key]))
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
          {@const busy = !!actionBusy[v.key]}
          {@const status = actionStatus[v.key]}
          {@const hasDeps = (v.deployments || []).length > 0}
          <tr class="sev-{v.severity}">
            <td><span class="sev-chip sev-{v.severity}">{v.severity}</span></td>
            <td class="mono">{v.category}</td>
            <td class="mono">{v.target}</td>
            <td class="mono">{v.metric}</td>
            <td>
              {v.detail}
              {#if hasDeps && v.deployments.length > 1}
                <span class="muted"> · {v.deployments.length} deployments</span>
              {/if}
            </td>
            <td class="actions-cell">
              {#if status}
                <span class="action-status {status.phase}">{status.text}</span>
              {/if}
              {#if hasDeps}
                <button
                  type="button"
                  class="row-btn"
                  disabled={busy}
                  onclick={() => actPause(v)}
                  title="Flip paused=true on every affected deployment"
                >Pause</button>
                <button
                  type="button"
                  class="row-btn"
                  disabled={busy}
                  onclick={() => actWidenL1(v)}
                  title="Escalate to L1 (widen spreads) on every affected deployment"
                >Widen L1</button>
              {/if}
              <button
                type="button"
                class="row-btn"
                disabled={busy}
                onclick={() => openIncident(v)}
                title="Open a tracked incident on the controller — persistent, supports ack/resolve + post-mortem"
              >Open incident</button>
              <button
                type="button"
                class="row-btn ghost"
                onclick={() => ackViolation(v.key)}
                title="Hide this row from the current session — doesn't change anything on the engine"
              >Hide</button>
            </td>
          </tr>
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
  .vio-table th, .vio-table td {
    padding: var(--s-2); font-size: var(--fs-xs); text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .vio-table th {
    color: var(--fg-muted); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-size: 10px;
  }
  .vio-table tr.sev-high td { background: color-mix(in srgb, var(--danger) 8%, transparent); }
  .mono { font-family: var(--font-mono); }

  .actions-col, .actions-cell { text-align: right; }
  .actions-cell {
    display: flex; gap: 4px; justify-content: flex-end;
    flex-wrap: wrap; align-items: center;
  }
  .row-btn {
    padding: 2px 8px; font-size: 10px; font-family: var(--font-mono);
    background: var(--bg-chip); color: var(--fg-secondary);
    border: 1px solid var(--border-subtle); border-radius: var(--r-sm);
    cursor: pointer;
  }
  .row-btn:hover { border-color: var(--warn); color: var(--warn); }
  .row-btn.ghost { background: transparent; }
  .row-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .action-status {
    font-size: 10px; font-family: var(--font-mono);
    padding: 1px 6px; border-radius: var(--r-sm);
    margin-right: 4px;
  }
  .action-status.pending { background: var(--bg-raised); color: var(--fg-muted); }
  .action-status.ok { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .action-status.warn { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .action-status.err { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
  .ack-chip {
    font-size: 10px; font-family: var(--font-mono);
    padding: 2px 6px; border-radius: var(--r-sm);
    background: var(--bg-raised); color: var(--fg-muted);
  }
  .muted { color: var(--fg-muted); }
</style>
