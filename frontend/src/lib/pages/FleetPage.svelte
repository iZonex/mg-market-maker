<script>
  /*
   * Fleet page — controller-side view of every connected agent +
   * the admission-control surface (approve / reject / revoke)
   * plus operator-editable profile metadata (description,
   * client, region, environment, purpose, owner_contact, notes,
   * labels).
   *
   * Two data sources:
   *   - /api/v1/fleet       — live sessions (who's connected right now)
   *   - /api/v1/approvals   — admission records (who's ever registered,
   *                           persisted across restarts, with state +
   *                           profile)
   *
   * The UI joins them by `pubkey_fingerprint` so each connected
   * agent row surfaces both runtime state (lease / deployments)
   * and operator state (approval / description). Fingerprints
   * that appear in approvals but NOT in fleet are "known but
   * offline" — still shown so operator can reject stale records.
   */
  import Card from '../components/Card.svelte'
  import DeployDialog from '../components/DeployDialog.svelte'
  import DeploymentDrilldown from '../components/DeploymentDrilldown.svelte'
  import EmptyStateGuide from '../components/EmptyStateGuide.svelte'
  import { Button, Modal } from '../primitives/index.js'
  import { createApiClient } from '../api.svelte.js'

  let { auth, onNavigate = () => {}, onOpenGraphLive = () => {} } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 2_000

  let fleet = $state([])
  let approvals = $state([])
  let agentCreds = $state({})         // agent_id → [{id, exchange, product}]
  let deployTarget = $state(null)     // agent row passed to DeployDialog
  // Wave C3 — batch deploy: set of fingerprints currently
  // selected. DeployDialog opens with array of agents.
  let batchSelection = $state({})
  let batchDeployAgents = $state(null)
  // Wave F2 — pre-approve modal state.
  let preApproveOpen = $state(false)
  let preApproveForm = $state({ fingerprint: '', notes: '' })
  let preApproveBusy = $state(false)
  let preApproveError = $state(null)
  let drilldownTarget = $state(null)  // { agent, deployment } — selected row
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)
  let busyFp = $state({})
  let editingFp = $state(null)
  // Wave C2 — fleet-wide pause/resume state. `fleetOpStatus`
  // carries the last operation's rollup ({ phase, text }) so
  // the operator sees success/fail counts inline.
  let fleetOpBusy = $state(false)
  let fleetOpStatus = $state(null)
  let editForm = $state({
    description: '',
    client_id: '',
    region: '',
    environment: '',
    purpose: '',
    owner_contact: '',
    notes: '',
    labels: '',
  })

  async function refresh() {
    try {
      const [fleetData, approvalsData] = await Promise.all([
        api.getJson('/api/v1/fleet'),
        api.getJson('/api/v1/approvals').catch(() => []),
      ])
      fleet = Array.isArray(fleetData) ? fleetData : []
      approvals = Array.isArray(approvalsData) ? approvalsData : []
      // Fan out per-agent credential fetches in parallel.
      // Each call returns the credentials that would be pushed
      // to this agent (filtered by allowed_agents whitelist) —
      // operators see at a glance what each box can reach.
      const credResults = await Promise.all(
        fleet.map(a =>
          api.getJson(`/api/v1/agents/${encodeURIComponent(a.agent_id)}/credentials`)
            .catch(() => [])
        )
      )
      const nextCreds = {}
      fleet.forEach((a, i) => { nextCreds[a.agent_id] = credResults[i] || [] })
      agentCreds = nextCreds
      error = null
      lastFetch = new Date()
      loading = false
    } catch (e) {
      error = e?.message || String(e)
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(t)
  })

  let nowMs = $state(Date.now())
  $effect(() => {
    const t = setInterval(() => (nowMs = Date.now()), 500)
    return () => clearInterval(t)
  })

  // Merge fleet (live sessions) with approvals (persistent
  // records) into one row per fingerprint. Fleet wins for live
  // fields; approvals wins for state + profile + first_seen.
  const rows = $derived.by(() => {
    const byFp = new Map()
    for (const a of approvals) {
      byFp.set(a.fingerprint, {
        fingerprint: a.fingerprint,
        agent_id: a.agent_id,
        pubkey_hex: a.pubkey_hex,
        state: a.state,
        first_seen_ms: a.first_seen_ms,
        last_seen_ms: a.last_seen_ms,
        approved_by: a.approved_by,
        approved_at_ms: a.approved_at_ms,
        revoked_by: a.revoked_by,
        revoke_reason: a.revoke_reason,
        profile: a.profile || {},
        connected: false,
        live: null,
      })
    }
    for (const f of fleet) {
      const fp = f.pubkey_fingerprint || `_unsigned_${f.agent_id}`
      const existing = byFp.get(fp) || {
        fingerprint: fp,
        agent_id: f.agent_id,
        pubkey_hex: '',
        state: fp.startsWith('_unsigned_') ? 'unsigned' : 'unknown',
        first_seen_ms: f.last_seen_ms,
        last_seen_ms: f.last_seen_ms,
        profile: {},
      }
      existing.connected = true
      existing.live = f
      existing.agent_id = f.agent_id
      existing.last_seen_ms = f.last_seen_ms
      byFp.set(fp, existing)
    }
    // Sort: Pending first (demands operator attention), then
    // Accepted/Connected, then offline known, then rejected.
    const statePriority = {
      pending: 0,
      accepted: 1,
      unsigned: 2,
      unknown: 3,
      revoked: 4,
      rejected: 5,
    }
    return Array.from(byFp.values()).sort((a, b) => {
      const sa = statePriority[a.state] ?? 9
      const sb = statePriority[b.state] ?? 9
      if (sa !== sb) return sa - sb
      if (a.connected !== b.connected) return a.connected ? -1 : 1
      return (a.fingerprint || '').localeCompare(b.fingerprint || '')
    })
  })

  const pendingCount = $derived(rows.filter(r => r.state === 'pending').length)

  function ageMs(ms) {
    if (!ms) return null
    return nowMs - ms
  }

  function formatAge(ms) {
    if (ms === null || ms === undefined) return '—'
    const s = Math.max(0, Math.round(ms / 1000))
    if (s < 60) return `${s}s`
    const m = Math.floor(s / 60)
    if (m < 60) return `${m}m ${s % 60}s`
    const h = Math.floor(m / 60)
    if (h < 24) return `${h}h ${m % 60}m`
    return `${Math.floor(h / 24)}d ${h % 24}h`
  }

  function leaseTtlMs(lease) {
    if (!lease?.expires_at) return null
    return new Date(lease.expires_at).getTime() - nowMs
  }

  function stateTone(s) {
    switch (s) {
      case 'accepted': return 'ok'
      case 'pending': return 'warn'
      case 'rejected':
      case 'revoked': return 'danger'
      case 'unsigned': return 'danger'
      default: return 'muted'
    }
  }

  function stateLabel(s) {
    return (s || 'unknown').toUpperCase()
  }

  async function doAction(fingerprint, action, reason) {
    busyFp[fingerprint] = true
    try {
      const body = reason !== undefined ? { reason } : {}
      await api.postJson(`/api/v1/approvals/${encodeURIComponent(fingerprint)}/${action}`, body)
      await refresh()
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      delete busyFp[fingerprint]
      busyFp = { ...busyFp }
    }
  }

  function accept(fp) { doAction(fp, 'accept') }

  // UI-STOP-1 (2026-04-22) — retire a single deployment. Before
  // this there was no UI action to take a single strategy off an
  // agent: DeployDialog only added; DeploymentDrilldown's kill
  // ladder at L5 (Disconnect) halts the engine but leaves the
  // row in the desired set, so reconcile re-spawns it. Genuine
  // removal needs SetDesiredStrategies without that deployment_id.
  // Same union-fetch pattern as the deploy dialog.
  async function retireDeployment(agentId, deploymentId, symbol) {
    if (
      !confirm(
        `Retire deployment "${deploymentId}" (${symbol}) from agent "${agentId}"?\n\n` +
          'Engine stops, orders cancelled, deployment removed from the desired set. ' +
          'Other deployments on this agent stay running.',
      )
    ) {
      return
    }
    try {
      let existing = await api.getJson(
        `/api/v1/agents/${encodeURIComponent(agentId)}/deployments`,
      )
      if (!Array.isArray(existing)) existing = []
      const merged = existing
        .filter(d => d.deployment_id !== deploymentId)
        .map(d => ({
          deployment_id: d.deployment_id,
          template: d.template || '',
          symbol: d.symbol,
          credentials: Array.isArray(d.credentials) ? d.credentials : [],
          variables: d.variables || {},
        }))
      const resp = await api.authedFetch(
        `/api/v1/agents/${encodeURIComponent(agentId)}/deployments`,
        {
          method: 'POST',
          body: JSON.stringify({ strategies: merged }),
        },
      )
      if (!resp.ok) {
        const t = await resp.text().catch(() => '')
        throw new Error(t || resp.statusText)
      }
      await refresh()
    } catch (e) {
      error = `retire failed: ${e.message || e}`
    }
  }
  function reject(fp) {
    const reason = prompt('Reject reason? (optional)') || 'operator reject'
    doAction(fp, 'reject', reason)
  }
  // Wave C4 — revoke-flow safety. If the agent has live
  // deployments the modal lists them and forces a choice
  // between "stop all first then revoke" and cancel.
  // Revoking an agent while orders are live leaves them on
  // the venue until the agent reconnects (which it won't —
  // it's revoked) so operators need explicit consent.
  let revokeModal = $state(null)  // { row, reason, phase, progress, results }

  function revoke(fp) {
    const row = rows.find(r => r.fingerprint === fp)
    const liveDeps = (row?.live?.deployments || []).filter(d => d.running)
    if (liveDeps.length === 0) {
      const reason = prompt('Revoke reason?') || 'operator revoke'
      doAction(fp, 'revoke', reason)
      return
    }
    revokeModal = {
      row,
      liveDeps,
      reason: '',
      phase: 'confirm',  // 'confirm' | 'stopping' | 'revoking' | 'done'
      results: [],
    }
  }

  // Fire kill ladder L5 (disconnect) to every live deployment
  // on this agent, then revoke the agent itself. Orders are
  // cancelled venue-side before the agent loses authority.
  async function revokeWithStop() {
    if (!revokeModal) return
    const { row, liveDeps, reason } = revokeModal
    const reasonTxt = reason.trim() || 'operator revoke + stop all'
    revokeModal = { ...revokeModal, phase: 'stopping',
      results: liveDeps.map(d => ({ deployment_id: d.deployment_id, phase: 'pending', detail: '' })) }
    const settled = await Promise.all(liveDeps.map(async (d) => {
      try {
        const r = await api.authedFetch(
          `/api/v1/agents/${encodeURIComponent(row.agent_id)}/deployments/${encodeURIComponent(d.deployment_id)}/ops/cancel-all`,
          { method: 'POST', body: JSON.stringify({ reason: reasonTxt }) }
        )
        if (!r.ok) {
          const text = await r.text().catch(() => '')
          return { deployment_id: d.deployment_id, phase: 'err', detail: `${r.status} ${text}` }
        }
        return { deployment_id: d.deployment_id, phase: 'ok', detail: 'cancel-all dispatched' }
      } catch (e) {
        return { deployment_id: d.deployment_id, phase: 'err', detail: e?.message || String(e) }
      }
    }))
    revokeModal = { ...revokeModal, phase: 'revoking', results: settled }
    // Brief pause so the cancel messages land before the agent
    // drops — the agent will still ACK cancels it already sent.
    await new Promise(res => setTimeout(res, 500))
    try {
      await api.authedFetch(
        `/api/v1/approvals/${encodeURIComponent(row.fingerprint)}/revoke`,
        { method: 'POST', body: JSON.stringify({ reason: reasonTxt }) }
      )
      revokeModal = { ...revokeModal, phase: 'done' }
    } catch (e) {
      revokeModal = { ...revokeModal, phase: 'done', error: e?.message || String(e) }
    }
    await refresh()
  }

  function revokeSkipStop() {
    if (!revokeModal) return
    const { row, reason } = revokeModal
    const reasonTxt = reason.trim() || 'operator revoke (orders left live)'
    doAction(row.fingerprint, 'revoke', reasonTxt)
    revokeModal = null
  }

  function closeRevokeModal() {
    revokeModal = null
  }
  // Skip is a pure UI action — just hide this row from the
  // pending section locally. Reverts on next refresh cycle;
  // operator revisits when they're ready to decide.
  let skippedFp = $state({})
  function skip(fp) {
    skippedFp[fp] = true
    skippedFp = { ...skippedFp }
  }

  // Wave F2 — pre-approve flow. Admin pastes the fingerprint
  // the agent logged on boot (before it ever connects);
  // controller creates an Accepted record with empty pubkey.
  // When the agent does connect, its pubkey binds silently
  // and the handshake clears without a Pending step.
  function openPreApprove() {
    preApproveForm = { fingerprint: '', notes: '' }
    preApproveError = null
    preApproveOpen = true
  }
  function closePreApprove() {
    preApproveOpen = false
  }
  async function submitPreApprove() {
    preApproveError = null
    const fp = preApproveForm.fingerprint.trim()
    if (!fp) { preApproveError = 'Fingerprint required'; return }
    preApproveBusy = true
    try {
      const r = await api.authedFetch('/api/v1/approvals/pre-approve', {
        method: 'POST',
        body: JSON.stringify({
          fingerprint: fp,
          notes: preApproveForm.notes.trim() || null,
        }),
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      preApproveOpen = false
      await refresh()
    } catch (e) {
      preApproveError = e?.message || String(e)
    } finally {
      preApproveBusy = false
    }
  }

  // Wave C2 — fleet-wide pause / resume. POSTs /api/v1/ops/fleet/{op}
  // and surfaces a per-target rollup. Operator confirms before the
  // op goes out — pause fleet touches every running deployment.
  // Wave C3 — batch helpers.
  function toggleBatch(fp) {
    const next = { ...batchSelection }
    if (next[fp]) delete next[fp]
    else next[fp] = true
    batchSelection = next
  }

  function clearBatch() {
    batchSelection = {}
  }

  const batchCount = $derived(Object.keys(batchSelection).length)

  function openBatchDeploy() {
    const picks = rows.filter(r => batchSelection[r.fingerprint] && r.state === 'accepted' && r.connected)
    if (picks.length === 0) return
    batchDeployAgents = picks.map(r => r.live).filter(Boolean)
  }

  async function fleetOp(op, reason) {
    const verb = op === 'pause' ? 'Pause' : 'Resume'
    const ok = confirm(
      `${verb} every running deployment across every accepted agent?`
      + `\n\nThis applies immediately — engines stop quoting ` +
      (op === 'pause' ? 'until you click Resume.' : 'from paused state.')
    )
    if (!ok) return
    fleetOpBusy = true
    fleetOpStatus = { phase: 'pending', text: `${verb.toLowerCase()}ing fleet…` }
    try {
      const r = await api.authedFetch('/api/v1/ops/fleet/' + op, {
        method: 'POST',
        body: JSON.stringify({ reason }),
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      const body = await r.json()
      fleetOpStatus = {
        phase: body.failed > 0 ? 'warn' : 'ok',
        text: `${op}: ${body.succeeded}/${body.attempted} applied${body.failed > 0 ? ` · ${body.failed} failed` : ''}`,
      }
    } catch (e) {
      fleetOpStatus = { phase: 'err', text: `${op} failed: ${e?.message || e}` }
    } finally {
      fleetOpBusy = false
      await refresh()
    }
  }

  function startEdit(row) {
    editingFp = row.fingerprint
    const p = row.profile || {}
    editForm = {
      description: p.description || '',
      client_id: p.client_id || '',
      region: p.region || '',
      environment: p.environment || '',
      purpose: p.purpose || '',
      owner_contact: p.owner_contact || '',
      notes: p.notes || '',
      labels: Object.entries(p.labels || {}).map(([k, v]) => `${k}=${v}`).join(', '),
    }
  }

  function cancelEdit() {
    editingFp = null
  }

  async function saveProfile() {
    if (!editingFp) return
    const labels = {}
    for (const pair of (editForm.labels || '').split(',').map(s => s.trim()).filter(Boolean)) {
      const eq = pair.indexOf('=')
      if (eq > 0) labels[pair.slice(0, eq).trim()] = pair.slice(eq + 1).trim()
    }
    busyFp[editingFp] = true
    try {
      await api.authedFetch(`/api/v1/agents/${encodeURIComponent(editingFp)}/profile`, {
        method: 'PUT',
        body: JSON.stringify({
          description: editForm.description,
          client_id: editForm.client_id,
          region: editForm.region,
          environment: editForm.environment,
          purpose: editForm.purpose,
          owner_contact: editForm.owner_contact,
          notes: editForm.notes,
          labels,
        }),
      })
      editingFp = null
      await refresh()
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      delete busyFp[editingFp]
      busyFp = { ...busyFp }
    }
  }

  function fmtDecimal(s, digits = 4) {
    if (s === null || s === undefined || s === '') return '—'
    const n = Number(s)
    if (!Number.isFinite(n)) return s
    return n.toFixed(digits)
  }
</script>

<div class="page scroll">
  <div class="grid">
    {#if pendingCount > 0}
      <Card title="Pending approvals" subtitle="new agents awaiting admission" span={3}>
        {#snippet children()}
          <div class="pending-list">
            {#each rows.filter(r => r.state === 'pending' && !skippedFp[r.fingerprint]) as r (r.fingerprint)}
              <div class="pending-row">
                <div class="pending-info">
                  <div class="row-line">
                    <span class="fp mono">{r.fingerprint}</span>
                    <span class="chip tone-warn">PENDING</span>
                    {#if r.connected}<span class="chip tone-ok">CONNECTED</span>{/if}
                  </div>
                  <div class="row-meta">
                    advertised id <span class="mono">{r.agent_id}</span>
                    · first seen {formatAge(ageMs(r.first_seen_ms))} ago
                  </div>
                </div>
                <div class="actions">
                  <Button variant="ok" disabled={busyFp[r.fingerprint]} onclick={() => accept(r.fingerprint)}>
          {#snippet children()}Accept{/snippet}
        </Button>
                  <Button variant="danger" disabled={busyFp[r.fingerprint]} onclick={() => reject(r.fingerprint)}>
          {#snippet children()}Reject{/snippet}
        </Button>
                  <Button variant="ghost" onclick={() => skip(r.fingerprint)}>
          {#snippet children()}Skip{/snippet}
        </Button>
                </div>
              </div>
            {/each}
          </div>
        {/snippet}
      </Card>
    {/if}

    {#if rows.length > 0}
      {@const accepted = rows.filter(r => r.state === 'accepted').length}
      {@const online = rows.filter(r => r.connected).length}
      {@const allDeps = rows.flatMap(r => r.live?.deployments || [])}
      {@const runningDeps = allDeps.filter(d => d.running).length}
      {@const liveOrders = allDeps.reduce((s, d) => s + Number(d.live_orders || 0), 0)}
      {@const totalPnl = allDeps.reduce((s, d) => s + Number(d.unrealized_pnl_quote || 0), 0)}
      {@const killed = allDeps.filter(d => (d.kill_level || 0) > 0).length}
      {@const nowMs = Date.now()}
      {@const oldestTick = allDeps
        .filter(d => d.running)
        .reduce((m, d) => {
          const age = nowMs - Number(d.last_tick_ms || 0)
          return Number.isFinite(age) && age < m ? m : (age >= 0 && age > m ? age : m)
        }, 0)}
      <!-- C7 GOBS — fleet-wide rollup card. Every KPI is a
           pure-client derivation from the `fleet` + `approvals`
           snapshots the page already polls, so no new endpoint
           needed. Renders above the per-agent list as the
           operator's "is the fleet healthy" glance. -->
      <Card title="Fleet rollup" subtitle="live totals across every accepted agent" span={3}>
        {#snippet children()}
          <div class="rollup-grid">
            <div class="rollup-cell">
              <span class="rollup-k">agents</span>
              <span class="rollup-v mono">{online}/{accepted}</span>
              <span class="rollup-sub">online/accepted</span>
            </div>
            <div class="rollup-cell">
              <span class="rollup-k">deployments</span>
              <span class="rollup-v mono">{runningDeps}/{allDeps.length}</span>
              <span class="rollup-sub">running/total</span>
            </div>
            <div class="rollup-cell">
              <span class="rollup-k">live orders</span>
              <span class="rollup-v mono">{liveOrders}</span>
            </div>
            <div class="rollup-cell" class:pos={totalPnl > 0} class:neg={totalPnl < 0}>
              <span class="rollup-k">total PnL</span>
              <span class="rollup-v mono">{totalPnl !== 0 ? totalPnl.toFixed(2) : '—'}</span>
              <span class="rollup-sub">unrealized · quote</span>
            </div>
            <div class="rollup-cell" class:alert={killed > 0}>
              <span class="rollup-k">kill-escalated</span>
              <span class="rollup-v mono">{killed}</span>
            </div>
            <div class="rollup-cell">
              <span class="rollup-k">oldest tick</span>
              <span class="rollup-v mono">
                {#if runningDeps === 0}—
                {:else if oldestTick < 1000}&lt;1s
                {:else if oldestTick < 60_000}{Math.round(oldestTick / 1000)}s
                {:else}{Math.round(oldestTick / 60_000)}m
                {/if}
              </span>
              <span class="rollup-sub">across running deployments</span>
            </div>
          </div>
        {/snippet}
      </Card>
    {/if}

    <Card title="Fleet" subtitle="every known agent · approved + offline + rejected" span={3}>
      {#snippet children()}
        <div class="toolbar">
          <div class="meta">
            {#if error}<span class="error">error: {error}</span>
            {:else if loading}<span class="stale">loading…</span>
            {:else if lastFetch}<span class="stale">{rows.length} agent(s) · {lastFetch.toLocaleTimeString()}</span>
            {/if}
          </div>
          <div class="fleet-ops">
            {#if fleetOpStatus}
              <span class="fleet-op-status {fleetOpStatus.phase}">{fleetOpStatus.text}</span>
            {/if}
            {#if batchCount > 0}
              <Button variant="ok" size="sm" onclick={openBatchDeploy}>
          {#snippet children()}Deploy to {batchCount} selected{/snippet}
        </Button>
              <Button variant="ghost" size="sm" onclick={clearBatch}>
          {#snippet children()}Clear selection{/snippet}
        </Button>
            {/if}
            <Button variant="ghost" size="sm" onclick={openPreApprove} title="Pre-approve a fingerprint before the agent connects">
          {#snippet children()}Pre-approve fingerprint{/snippet}
        </Button>
            <Button variant="warn" size="sm" disabled={fleetOpBusy} onclick={() => fleetOp('pause', 'operator global pause')} title="Flip paused = true on every running deployment across every accepted agent">
          {#snippet children()}Pause fleet{/snippet}
        </Button>
            <Button variant="ok" size="sm" disabled={fleetOpBusy} onclick={() => fleetOp('resume', 'operator global resume')} title="Flip paused = false on every running deployment across every accepted agent">
          {#snippet children()}Resume fleet{/snippet}
        </Button>
          </div>
        </div>

        {#if !loading && rows.length === 0}
          <EmptyStateGuide
            title="No agents connected yet"
            message="The controller is healthy — nothing's wrong. You just haven't booted an mm-agent process that points at this server. Follow the steps below to connect your first box."
            steps={[
              {
                title: 'Install a credential in the vault',
                description: 'The agent needs exchange API keys before it can post orders. Paper / testnet deploys still need any credential — the engine won\'t spawn without a resolvable primary.',
                action: { label: 'Open Vault', route: 'vault' },
              },
              {
                title: 'Start an mm-agent process',
                description: 'On the trading box, run MM_BRAIN_WS_ADDR=ws://<this-controller>:9091 mm-agent. It generates an Ed25519 identity on first run and connects as Pending.',
              },
              {
                title: 'Accept the fingerprint',
                description: 'The pending fingerprint shows up here. Verify it matches the one mm-agent logged on the trading box, then click Accept.',
              },
              {
                title: 'Deploy a strategy',
                description: 'With the agent accepted, click Deploy strategy on its card. Start with a paper-mode template on BTCUSDT to verify the pipe.',
              },
            ]}
            {onNavigate}
          />
        {:else}
          <div class="rows">
            {#each rows as r (r.fingerprint)}
              {@const live = r.live}
              {@const ttl = live ? leaseTtlMs(live.current_lease) : null}
              {@const lastSeen = ageMs(r.last_seen_ms)}
              {@const liveOrders = (live?.deployments || []).reduce((n, d) => n + (d.live_orders || 0), 0)}
              {@const nowMs = Date.now()}
              {@const STALE_MS = 30_000}
              {@const freshDeps = (live?.deployments || []).filter(d => d.sampled_at_ms && (nowMs - d.sampled_at_ms) < STALE_MS)}
              {@const staleCount = (live?.deployments || []).length - freshDeps.length}
              {@const totalPnl = freshDeps.reduce((s, d) => s + Number(d.unrealized_pnl_quote || 0), 0)}
              {@const killedCount = (live?.deployments || []).filter(d => (d.kill_level || 0) > 0).length}
              <div class="agent-card">
                <div class="head">
                  <div class="head-left">
                    {#if r.state === 'accepted' && r.connected}
                      <label class="batch-check" title="Include in batch deploy">
                        <input
                          type="checkbox"
                          checked={!!batchSelection[r.fingerprint]}
                          onchange={() => toggleBatch(r.fingerprint)}
                        />
                      </label>
                    {/if}
                    <span class="fp mono">{r.fingerprint}</span>
                    <span class="chip tone-{stateTone(r.state)}">{stateLabel(r.state)}</span>
                    {#if r.connected}<span class="chip tone-ok">CONNECTED</span>
                    {:else}<span class="chip tone-muted">OFFLINE</span>{/if}
                  </div>
                  <div class="head-right">
                    {#if r.state === 'accepted' && r.connected}
                      <Button variant="ok" size="sm" onclick={() => (deployTarget = r)}>
          {#snippet children()}Deploy strategy{/snippet}
        </Button>
                    {/if}
                    {#if r.state === 'accepted'}
                      <Button variant="danger" size="sm" disabled={busyFp[r.fingerprint]} onclick={() => revoke(r.fingerprint)}>
          {#snippet children()}Revoke{/snippet}
        </Button>
                    {:else if r.state === 'rejected' || r.state === 'revoked' || r.state === 'pending'}
                      <Button variant="ok" size="sm" disabled={busyFp[r.fingerprint]} onclick={() => accept(r.fingerprint)}>
          {#snippet children()}Accept{/snippet}
        </Button>
                    {/if}
                    {#if editingFp !== r.fingerprint}
                      <Button variant="ghost" size="sm" onclick={() => startEdit(r)}>
          {#snippet children()}Edit{/snippet}
        </Button>
                    {/if}
                  </div>
                </div>

                <div class="profile-line">
                  {#if r.profile?.description}<span class="desc">{r.profile.description}</span>{/if}
                  <span class="kv"><span class="k">id</span><span class="v mono">{r.agent_id}</span></span>
                  {#if r.profile?.environment}<span class="env-chip env-{r.profile.environment}">{r.profile.environment}</span>{/if}
                  {#if r.profile?.client_id}<span class="kv"><span class="k">client</span><span class="v mono">{r.profile.client_id}</span></span>{/if}
                  {#if r.profile?.region}<span class="kv"><span class="k">region</span><span class="v mono">{r.profile.region}</span></span>{/if}
                  {#if r.profile?.purpose}<span class="kv"><span class="k">purpose</span><span class="v">{r.profile.purpose}</span></span>{/if}
                  {#if r.profile?.owner_contact}<span class="kv"><span class="k">on-call</span><span class="v mono">{r.profile.owner_contact}</span></span>{/if}
                  {#each Object.entries(r.profile?.labels || {}) as [k, v] (k)}
                    <span class="label-chip">{k}={v}</span>
                  {/each}
                </div>

                {#if r.profile?.notes}
                  <div class="notes-line">{r.profile.notes}</div>
                {/if}

                {#if editingFp === r.fingerprint}
                  <div class="edit-form">
                    <label>Description<input type="text" bind:value={editForm.description} placeholder="Frankfurt HFT box #2" /></label>
                    <label>Client<input type="text" bind:value={editForm.client_id} placeholder="alice" /></label>
                    <label>Region<input type="text" bind:value={editForm.region} placeholder="eu-fra" /></label>
                    <label>Environment
                      <select bind:value={editForm.environment}>
                        <option value="">—</option>
                        <option value="production">production</option>
                        <option value="staging">staging</option>
                        <option value="dev">dev</option>
                        <option value="smoke">smoke</option>
                      </select>
                    </label>
                    <label>Purpose<input type="text" bind:value={editForm.purpose} placeholder="primary BTC/ETH market-maker" /></label>
                    <label>On-call contact<input type="text" bind:value={editForm.owner_contact} placeholder="oncall@team or @slack-handle" /></label>
                    <label>Labels<input type="text" bind:value={editForm.labels} placeholder="env=prod, role=hft" /></label>
                    <label class="notes-field">Notes<textarea rows="3" bind:value={editForm.notes} placeholder="Deployment gotchas, runbook links, known quirks"></textarea></label>
                    <div class="edit-actions">
                      <Button variant="ok" size="sm" disabled={busyFp[editingFp]} onclick={saveProfile}>
          {#snippet children()}Save{/snippet}
        </Button>
                      <Button variant="ghost" size="sm" onclick={cancelEdit}>
          {#snippet children()}Cancel{/snippet}
        </Button>
                    </div>
                  </div>
                {/if}

                {#if agentCreds[r.agent_id]?.length}
                  <div class="cred-list">
                    <span class="cred-list-k">Available credentials</span>
                    <div class="cred-chips">
                      {#each agentCreds[r.agent_id] as c (c.id)}
                        <span class="cred-chip" title={`${c.exchange}/${c.product}`}>
                          <span class="cred-id">{c.id}</span>
                          <span class="cred-venue">{c.exchange}/{c.product}</span>
                        </span>
                      {/each}
                    </div>
                  </div>
                {:else if r.state === 'accepted'}
                  <div class="cred-empty">No credentials authorised for this agent — add one in Credentials, or widen an existing credential's <code>allowed_agents</code>.</div>
                {/if}

                <div class="runtime">
                  <div class="kv-col">
                    <span class="k">lease TTL</span>
                    <span class="v mono">{ttl !== null ? formatAge(ttl) : '—'}</span>
                  </div>
                  <div class="kv-col">
                    <span class="k">last seen</span>
                    <span class="v mono" class:stale-txt={lastSeen !== null && lastSeen > 10_000}>
                      {formatAge(lastSeen)} ago
                    </span>
                  </div>
                  <div class="kv-col">
                    <span class="k">protocol</span>
                    <span class="v mono">{live ? `v${live.protocol_version}` : '—'}</span>
                  </div>
                  <div class="kv-col">
                    <span class="k">agent ver</span>
                    <span class="v mono">{live?.agent_version || '—'}</span>
                  </div>
                  <div class="kv-col">
                    <span class="k">applied seq</span>
                    <span class="v mono">{live ? `#${live.last_applied_seq}` : '—'}</span>
                  </div>
                  <div class="kv-col">
                    <span class="k">deployments</span>
                    <span class="v mono">
                      {(live?.deployments || []).filter(d => d.running).length}
                      <span class="faint">/ {live?.deployments?.length ?? 0}</span>
                    </span>
                  </div>
                  <div class="kv-col">
                    <span class="k">live orders</span>
                    <span class="v mono">{liveOrders}</span>
                  </div>
                  <div class="kv-col">
                    <span class="k">
                      unrealised PnL
                      {#if staleCount > 0}
                        <span class="stale-chip" title={`${staleCount} deployment(s) last sampled > 30s ago — excluded from this sum`}>
                          {staleCount} stale
                        </span>
                      {/if}
                    </span>
                    <span class="v mono" class:pos={totalPnl > 0} class:neg={totalPnl < 0}>
                      {totalPnl !== 0 ? totalPnl.toFixed(2) : '—'}
                    </span>
                  </div>
                  {#if killedCount > 0}
                    <div class="kv-col">
                      <span class="k">kill escalated</span>
                      <span class="v mono neg">{killedCount}</span>
                    </div>
                  {/if}
                </div>

                {#if live?.deployments?.length}
                  <table class="dep-table">
                    <thead>
                      <tr>
                        <th>deployment</th>
                        <th>symbol</th>
                        <th>template</th>
                        <th>graph</th>
                        <th>running</th>
                        <th class="num">inventory</th>
                        <th class="num">unrealised PnL</th>
                        <th></th>
                        <th></th>
                      </tr>
                    </thead>
                    <tbody>
                      {#each live.deployments as d (d.deployment_id)}
                        <tr
                          class="dep-row"
                          onclick={() => (drilldownTarget = { agent: live, deployment: d })}
                          title="Open drilldown"
                        >
                          <td class="mono">{d.deployment_id}</td>
                          <td class="mono">{d.symbol}</td>
                          <td class="mono">{d.template || '—'}</td>
                          <td class="mono">
                            {#if d.active_graph?.hash}
                              <span title={`${d.active_graph.name || ''} · ${d.active_graph.hash} · deployed ${new Date(d.active_graph.deployed_at_ms).toLocaleString()}`}>
                                {d.active_graph.hash.slice(0, 8)}
                              </span>
                            {:else}—{/if}
                          </td>
                          <td>
                            <span class="chip tone-{d.running ? 'ok' : 'muted'}">
                              {d.running ? 'RUN' : 'STOP'}
                            </span>
                          </td>
                          <td class="num mono">{fmtDecimal(d.inventory, 6)}</td>
                          <td class="num mono">{fmtDecimal(d.unrealized_pnl_quote, 4)}</td>
                          <td class="dep-actions">
                            <Button variant="ghost" size="xs" title="Retire deployment (remove from desired set)" onclick={(e) => { e.stopPropagation(); retireDeployment(live.agent_id, d.deployment_id, d.symbol) }}>
          {#snippet children()}Retire{/snippet}
        </Button>
                          </td>
                          <td class="chev">›</td>
                        </tr>
                      {/each}
                    </tbody>
                  </table>
                {/if}

                {#if r.state === 'rejected' || r.state === 'revoked'}
                  <div class="denial">
                    <span class="k">{r.state === 'rejected' ? 'rejected' : 'revoked'}</span>
                    {#if r.revoked_by} by <span class="mono">{r.revoked_by}</span>{/if}
                    {#if r.revoke_reason} — "{r.revoke_reason}"{/if}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      {/snippet}
    </Card>

  </div>
</div>

{#if deployTarget}
  <DeployDialog
    {auth}
    agent={deployTarget}
    onClose={() => (deployTarget = null)}
    onDeployed={refresh}
  />
{/if}

{#if batchDeployAgents}
  <DeployDialog
    {auth}
    agent={batchDeployAgents[0]}
    agents={batchDeployAgents}
    onClose={() => (batchDeployAgents = null)}
    onDeployed={() => { refresh(); clearBatch() }}
  />
{/if}

{#if drilldownTarget}
  <DeploymentDrilldown
    {auth}
    agent={drilldownTarget.agent}
    deployment={drilldownTarget.deployment}
    onClose={() => (drilldownTarget = null)}
    onOpenGraphLive={(agentId, depId) => {
      drilldownTarget = null
      onOpenGraphLive(agentId, depId)
    }}
    {onNavigate}
  />
{/if}

<Modal
  open={!!revokeModal}
  ariaLabel="Revoke agent"
  maxWidth="560px"
  onClose={closeRevokeModal}
>
  {#snippet children()}
    {#if revokeModal}
      <div class="revoke-title">
        {#if revokeModal.phase === 'confirm'}Revoke agent with live deployments
        {:else if revokeModal.phase === 'stopping'}Cancelling orders…
        {:else if revokeModal.phase === 'revoking'}Revoking agent…
        {:else}Revoke complete
        {/if}
      </div>
      <div class="revoke-body">
        <div class="revoke-sub">
          <span class="fp mono">{revokeModal.row.fingerprint}</span>
          <span class="muted">· {revokeModal.row.agent_id}</span>
        </div>

        {#if revokeModal.phase === 'confirm'}
          <p class="revoke-warn">
            This agent has <strong>{revokeModal.liveDeps.length}</strong> live
            deployment{revokeModal.liveDeps.length === 1 ? '' : 's'}. Revoking
            drops its authority — orders on the venue will remain open until
            someone cancels them manually.
          </p>
          <div class="dep-list">
            {#each revokeModal.liveDeps as d (d.deployment_id)}
              <div class="dep-item">
                <span class="mono">{d.deployment_id}</span>
                <span class="muted">· {d.symbol}</span>
                {#if (d.live_orders || 0) > 0}
                  <span class="chip tone-warn">{d.live_orders} order{d.live_orders === 1 ? '' : 's'}</span>
                {/if}
              </div>
            {/each}
          </div>
          <label class="reason-field">
            <span class="reason-k">Reason</span>
            <input type="text" bind:value={revokeModal.reason} placeholder="e.g. key compromise, decommission" />
          </label>
        {:else}
          <div class="dep-list">
            {#each revokeModal.results as res (res.deployment_id)}
              <div class="dep-item">
                <span class="mono">{res.deployment_id}</span>
                <span class="res-{res.phase}">
                  {#if res.phase === 'pending'}…
                  {:else if res.phase === 'ok'}✓ {res.detail}
                  {:else}✗ {res.detail}
                  {/if}
                </span>
              </div>
            {/each}
          </div>
          {#if revokeModal.phase === 'done'}
            {#if revokeModal.error}
              <p class="revoke-warn">Agent revoke failed: {revokeModal.error}</p>
            {:else}
              <p class="revoke-ok">Agent revoked. Orders were cancelled on the venue before authority was dropped.</p>
            {/if}
          {/if}
        {/if}
      </div>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if revokeModal}
      {#if revokeModal.phase === 'confirm'}
        <Button variant="ghost" onclick={closeRevokeModal}>
          {#snippet children()}Cancel{/snippet}
        </Button>
        <Button variant="warn" onclick={revokeSkipStop}>
          {#snippet children()}Revoke without cancelling{/snippet}
        </Button>
        <Button variant="danger" onclick={revokeWithStop}>
          {#snippet children()}Cancel orders + revoke{/snippet}
        </Button>
      {:else if revokeModal.phase === 'done'}
        <Button variant="ok" onclick={closeRevokeModal}>
          {#snippet children()}Close{/snippet}
        </Button>
      {/if}
    {/if}
  {/snippet}
</Modal>

<Modal
  open={preApproveOpen}
  ariaLabel="Pre-approve fingerprint"
  maxWidth="520px"
  onClose={closePreApprove}
>
  {#snippet children()}
    <div class="preapprove-title">Pre-approve fingerprint</div>
    <div class="preapprove-body">
      <p class="preapprove-lead">
        Paste the fingerprint the agent logged on its first boot
        — you'll see it in the agent's stdout / systemd journal:
        <code class="mono">mm-agent starting … fingerprint=d5d0bf4df0ad14f5</code>.
        When the agent connects, it'll be auto-accepted without a
        Pending step.
      </p>
      <label class="field">
        <span>Fingerprint</span>
        <input
          type="text"
          bind:value={preApproveForm.fingerprint}
          placeholder="d5d0bf4df0ad14f5"
          disabled={preApproveBusy}
        />
      </label>
      <label class="field">
        <span>Notes (optional)</span>
        <input
          type="text"
          bind:value={preApproveForm.notes}
          placeholder="e.g. eu-01 trading box, ACME tenant"
          disabled={preApproveBusy}
        />
      </label>
      {#if preApproveError}
        <div class="preapprove-err">{preApproveError}</div>
      {/if}
    </div>
  {/snippet}
  {#snippet actions()}
    <Button variant="ghost" onclick={closePreApprove} disabled={preApproveBusy}>
      {#snippet children()}Cancel{/snippet}
    </Button>
    <Button variant="ok" onclick={submitPreApprove} disabled={preApproveBusy}>
      {#snippet children()}{preApproveBusy ? 'Creating…' : 'Pre-approve'}{/snippet}
    </Button>
  {/snippet}
</Modal>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }

  .toolbar { display: flex; justify-content: space-between; align-items: center; gap: var(--s-3); margin-bottom: var(--s-2); flex-wrap: wrap; }
  .meta { color: var(--fg-muted); font-size: var(--fs-xs); }
  .fleet-ops { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; }
  .fleet-op-status {
    font-size: var(--fs-xs); font-family: var(--font-mono);
    padding: 2px 8px; border-radius: var(--r-sm);
  }
  .fleet-op-status.pending { background: var(--bg-raised); color: var(--fg-muted); }
  .fleet-op-status.ok      { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .fleet-op-status.warn    { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .fleet-op-status.err     { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }

  /* `.modal-backdrop` moved to primitives/Modal.svelte — design system v1. */

  .revoke-card {
    max-width: 100%;
    display: flex; flex-direction: column; gap: var(--s-3);
  }
  .revoke-title { font-size: var(--fs-lg); color: var(--fg-primary); font-weight: 600; }
  .revoke-sub { font-size: var(--fs-xs); color: var(--fg-secondary); }
  .revoke-warn {
    padding: var(--s-2); font-size: var(--fs-xs);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    color: var(--danger); border-radius: var(--r-sm);
    border-left: 2px solid var(--danger);
  }
  .revoke-ok {
    padding: var(--s-2); font-size: var(--fs-xs);
    background: color-mix(in srgb, var(--ok) 12%, transparent);
    color: var(--ok); border-radius: var(--r-sm);
  }
  .dep-list { display: flex; flex-direction: column; gap: 4px; max-height: 240px; overflow-y: auto; }
  .dep-item {
    display: flex; gap: var(--s-2); align-items: center;
    padding: var(--s-2) var(--s-3); background: var(--bg-chip);
    border-radius: var(--r-sm); font-size: var(--fs-xs);
  }
  .reason-field { display: flex; flex-direction: column; gap: 4px; }
  .reason-k { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .reason-field input {
    padding: var(--s-2); background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary); font-family: var(--font-mono); font-size: var(--fs-xs);
  }
  .revoke-actions { display: flex; gap: var(--s-2); justify-content: flex-end; }
  .res-pending { color: var(--fg-muted); }
  .res-ok { color: var(--ok); }
  .res-err { color: var(--danger); }
  .batch-check { display: inline-flex; align-items: center; margin-right: 6px; }
  .batch-check input { cursor: pointer; }

  .preapprove-card {
    width: 520px; max-width: 100%;
    background: var(--bg-raised); border: 1px solid var(--border-subtle);
    border-radius: var(--r-lg); padding: var(--s-4);
    display: flex; flex-direction: column; gap: var(--s-3);
  }
  .preapprove-title { font-size: var(--fs-lg); color: var(--fg-primary); font-weight: 600; }
  .preapprove-body { display: flex; flex-direction: column; gap: var(--s-3); }
  .preapprove-lead { margin: 0; font-size: var(--fs-xs); color: var(--fg-secondary); line-height: 1.5; }
  .preapprove-lead code { background: var(--bg-chip); padding: 1px 4px; border-radius: var(--r-sm); font-size: 10px; }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .field span {
    font-size: 10px; color: var(--fg-muted);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .field input {
    padding: var(--s-2); background: var(--bg-chip);
    border: 1px solid var(--border-subtle); border-radius: var(--r-sm);
    color: var(--fg-primary); font-family: var(--font-mono);
    font-size: var(--fs-xs);
  }
  .preapprove-err {
    padding: var(--s-2); background: color-mix(in srgb, var(--danger) 15%, transparent);
    color: var(--danger); border-radius: var(--r-sm); font-size: var(--fs-xs);
  }
  .preapprove-actions { display: flex; gap: var(--s-2); justify-content: flex-end; }
  .meta .error { color: var(--danger); }
  .empty { color: var(--fg-muted); font-size: var(--fs-sm); padding: var(--s-4); text-align: center; line-height: 1.6; }
  .empty-sub { color: var(--fg-muted); font-size: var(--fs-xs); padding: var(--s-3); text-align: center; line-height: 1.6; }
  code {
    font-family: var(--font-mono); background: var(--bg-chip);
    padding: 0 4px; border-radius: var(--r-sm); color: var(--fg-primary);
  }

  .pending-list { display: flex; flex-direction: column; gap: var(--s-2); }
  .pending-row {
    display: flex; align-items: center; justify-content: space-between;
    padding: var(--s-3);
    border: 1px solid var(--warn);
    background: var(--warn-bg, rgba(245,158,11,0.08));
    border-radius: var(--r-md);
    gap: var(--s-3);
  }
  .pending-info { display: flex; flex-direction: column; gap: 4px; flex: 1; min-width: 0; }
  .row-line { display: flex; align-items: center; gap: var(--s-2); }
  .row-meta { font-size: var(--fs-xs); color: var(--fg-muted); }
  .actions { display: flex; gap: var(--s-1); }

  .rows { display: flex; flex-direction: column; gap: var(--s-3); }
  .agent-card {
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
    padding: var(--s-3);
    display: flex; flex-direction: column; gap: var(--s-2);
  }
  .head { display: flex; justify-content: space-between; align-items: center; gap: var(--s-2); }
  .head-left { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; }
  .head-right { display: flex; gap: var(--s-1); }
  .fp { font-weight: 600; color: var(--fg-primary); font-size: var(--fs-sm); }

  .profile-line {
    display: flex; align-items: baseline; gap: var(--s-3);
    flex-wrap: wrap; font-size: var(--fs-xs);
  }
  .desc { color: var(--fg-primary); font-weight: 500; }
  .label-chip {
    font-family: var(--font-mono); font-size: 10px;
    background: var(--bg-raised); color: var(--fg-secondary);
    padding: 2px 6px; border-radius: var(--r-sm);
  }
  .env-chip {
    font-family: var(--font-mono); font-size: 10px; font-weight: 600;
    padding: 2px 6px; border-radius: var(--r-sm);
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    background: var(--bg-chip); color: var(--fg-secondary);
    border: 1px solid var(--border-subtle);
  }
  .env-chip.env-production { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); border-color: color-mix(in srgb, var(--danger) 30%, transparent); }
  .env-chip.env-staging { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); border-color: color-mix(in srgb, var(--warn) 30%, transparent); }
  .env-chip.env-dev { background: color-mix(in srgb, var(--accent) 18%, transparent); color: var(--accent); border-color: color-mix(in srgb, var(--accent) 30%, transparent); }
  .env-chip.env-smoke { background: var(--bg-raised); color: var(--fg-muted); }

  .notes-line {
    font-size: var(--fs-xs); color: var(--fg-secondary);
    padding: var(--s-2); background: var(--bg-raised); border-radius: var(--r-sm);
    white-space: pre-wrap;
  }

  .edit-form {
    display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-2);
    padding: var(--s-2); background: var(--bg-raised); border-radius: var(--r-sm);
  }
  .edit-form label { display: flex; flex-direction: column; gap: 2px; font-size: 10px; color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .edit-form label.notes-field { grid-column: 1 / -1; }
  .edit-form input,
  .edit-form select,
  .edit-form textarea {
    padding: var(--s-2); background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary); font-family: var(--font-mono); font-size: var(--fs-xs);
  }
  .edit-form textarea { font-family: inherit; resize: vertical; }
  .edit-actions { grid-column: 1 / -1; display: flex; gap: var(--s-2); justify-content: flex-end; }

  .runtime {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: var(--s-2);
    padding: var(--s-2);
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .kv { display: inline-flex; gap: 4px; align-items: baseline; }
  .kv-col { display: flex; flex-direction: column; gap: 2px; }
  .k { color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; font-size: 10px; }
  .v { color: var(--fg-primary); }
  .v.stale-txt { color: var(--warn); }
  .v.pos { color: var(--pos); }
  .v.neg { color: var(--neg); }
  .v .faint { color: var(--fg-faint); }
  .v.mono.neg { color: var(--neg); }
  .stale-chip {
    display: inline-block; margin-left: 4px;
    padding: 1px 4px; font-size: 9px;
    background: color-mix(in srgb, var(--warn) 18%, transparent);
    color: var(--warn); border-radius: var(--r-sm);
  }
  /* `.btn*` CSS moved to primitives/Button.svelte — design system v1. */

  .cred-list {
    display: flex; flex-direction: column; gap: 6px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-raised);
    border-radius: var(--r-sm);
  }
  .cred-list-k {
    font-size: 10px; color: var(--fg-muted);
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
  }
  .cred-chips { display: flex; gap: var(--s-2); flex-wrap: wrap; }
  .cred-chip {
    display: inline-flex; flex-direction: column; gap: 1px;
    padding: 4px 8px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    font-size: 10px;
  }
  .cred-id {
    font-family: var(--font-mono);
    color: var(--fg-primary);
    font-weight: 500;
  }
  .cred-venue {
    font-family: var(--font-mono);
    color: var(--fg-muted);
    font-size: 9px;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .cred-empty {
    padding: var(--s-2) var(--s-3);
    background: rgba(245, 158, 11, 0.06);
    border: 1px solid rgba(245, 158, 11, 0.2);
    border-radius: var(--r-sm);
    color: var(--fg-muted);
    font-size: var(--fs-xs);
    line-height: 1.5;
  }
  .cred-empty code {
    font-family: var(--font-mono);
    background: var(--bg-chip);
    padding: 0 4px;
    border-radius: 3px;
    color: var(--fg-primary);
  }

  /* C7 GOBS — fleet-wide rollup card */
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

  .dep-table {
    width: 100%; border-collapse: collapse; font-size: var(--fs-xs);
  }
  .dep-table th, .dep-table td {
    padding: var(--s-2); text-align: left; border-bottom: 1px solid var(--border-subtle);
  }
  .dep-table th {
    color: var(--fg-muted); text-transform: uppercase;
    letter-spacing: var(--tracking-label); font-size: 10px; font-weight: 600;
  }
  .num { text-align: right; }
  .dep-row { cursor: pointer; transition: background var(--dur-fast) var(--ease-out); }
  .dep-row:hover { background: var(--bg-raised); }
  .chev { color: var(--fg-muted); text-align: right; font-size: var(--fs-md); width: 16px; }
  .dep-row:hover .chev { color: var(--accent); }
  .dep-actions { text-align: right; width: 1%; white-space: nowrap; }
  .denial {
    font-size: var(--fs-xs); color: var(--fg-muted);
    border-top: 1px solid var(--border-subtle); padding-top: var(--s-2);
  }
</style>
