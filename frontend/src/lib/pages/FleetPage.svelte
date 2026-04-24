<script>
  /*
   * Fleet page — controller-side view of every connected agent +
   * the admission-control surface (approve / reject / revoke) plus
   * operator-editable profile metadata.
   *
   * Two data sources:
   *   - /api/v1/fleet       — live sessions (who's connected right now)
   *   - /api/v1/approvals   — admission records (who's ever registered,
   *                           persisted across restarts, with state +
   *                           profile)
   *
   * Joined by `pubkey_fingerprint`. Fingerprints in approvals but
   * NOT in fleet are "known but offline" — still shown so operator
   * can reject stale records.
   *
   * Heavy UI pieces live in components/fleet/*; this file is the
   * coordinator (fetch loops, state machines for revoke/batch/
   * pre-approve/retire, child dispatch).
   */
  import Card from '../components/Card.svelte'
  import DeployDialog from '../components/DeployDialog.svelte'
  import DeploymentDrilldown from '../components/DeploymentDrilldown.svelte'
  import EmptyStateGuide from '../components/EmptyStateGuide.svelte'
  import FleetRollupCard from '../components/fleet/FleetRollupCard.svelte'
  import PendingAgentsCard from '../components/fleet/PendingAgentsCard.svelte'
  import AgentCard from '../components/fleet/AgentCard.svelte'
  import PreApproveModal from '../components/fleet/PreApproveModal.svelte'
  import RevokeAgentModal from '../components/fleet/RevokeAgentModal.svelte'
  import { Button } from '../primitives/index.js'
  import { createApiClient } from '../api.svelte.js'

  let { auth, onNavigate = () => {}, onOpenGraphLive = () => {} } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 2_000

  let fleet = $state([])
  let approvals = $state([])
  let agentCreds = $state({})         // agent_id → [{id, exchange, product}]
  let deployTarget = $state(null)     // agent row passed to DeployDialog
  let batchSelection = $state({})
  let batchDeployAgents = $state(null)
  let preApproveOpen = $state(false)
  let preApproveBusy = $state(false)
  let preApproveError = $state(null)
  let drilldownTarget = $state(null)
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)
  let busyFp = $state({})
  let fleetOpBusy = $state(false)
  let fleetOpStatus = $state(null)
  let skippedFp = $state({})
  let revokeModal = $state(null)

  async function refresh() {
    try {
      const [fleetData, approvalsData] = await Promise.all([
        api.getJson('/api/v1/fleet'),
        api.getJson('/api/v1/approvals').catch(() => []),
      ])
      fleet = Array.isArray(fleetData) ? fleetData : []
      approvals = Array.isArray(approvalsData) ? approvalsData : []
      const credResults = await Promise.all(
        fleet.map((a) =>
          api.getJson(`/api/v1/agents/${encodeURIComponent(a.agent_id)}/credentials`).catch(() => []),
        ),
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

  // Merge fleet (live sessions) with approvals (persistent records)
  // into one row per fingerprint. Fleet wins for live fields;
  // approvals wins for state + profile + first_seen.
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
    // Pending first, then Accepted/Connected, then offline known,
    // then rejected.
    const statePriority = {
      pending: 0, accepted: 1, unsigned: 2, unknown: 3, revoked: 4, rejected: 5,
    }
    return Array.from(byFp.values()).sort((a, b) => {
      const sa = statePriority[a.state] ?? 9
      const sb = statePriority[b.state] ?? 9
      if (sa !== sb) return sa - sb
      if (a.connected !== b.connected) return a.connected ? -1 : 1
      return (a.fingerprint || '').localeCompare(b.fingerprint || '')
    })
  })

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

  function reject(fp) {
    const reason = prompt('Reject reason? (optional)') || 'operator reject'
    doAction(fp, 'reject', reason)
  }

  function skip(fp) {
    skippedFp[fp] = true
    skippedFp = { ...skippedFp }
  }

  // UI-STOP-1 (2026-04-22) — retire a single deployment. Same
  // union-fetch pattern as the deploy dialog.
  async function retireDeployment(agentId, deploymentId, symbol) {
    if (!confirm(
      `Retire deployment "${deploymentId}" (${symbol}) from agent "${agentId}"?\n\n` +
      'Engine stops, orders cancelled, deployment removed from the desired set. ' +
      'Other deployments on this agent stay running.',
    )) return
    try {
      let existing = await api.getJson(
        `/api/v1/agents/${encodeURIComponent(agentId)}/deployments`,
      )
      if (!Array.isArray(existing)) existing = []
      const merged = existing
        .filter((d) => d.deployment_id !== deploymentId)
        .map((d) => ({
          deployment_id: d.deployment_id,
          template: d.template || '',
          symbol: d.symbol,
          credentials: Array.isArray(d.credentials) ? d.credentials : [],
          variables: d.variables || {},
        }))
      const resp = await api.authedFetch(
        `/api/v1/agents/${encodeURIComponent(agentId)}/deployments`,
        { method: 'POST', body: JSON.stringify({ strategies: merged }) },
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

  function revoke(fp) {
    const row = rows.find((r) => r.fingerprint === fp)
    const liveDeps = (row?.live?.deployments || []).filter((d) => d.running)
    if (liveDeps.length === 0) {
      const reason = prompt('Revoke reason?') || 'operator revoke'
      doAction(fp, 'revoke', reason)
      return
    }
    revokeModal = { row, liveDeps, reason: '', phase: 'confirm', results: [] }
  }

  async function revokeWithStop() {
    if (!revokeModal) return
    const { row, liveDeps, reason } = revokeModal
    const reasonTxt = reason.trim() || 'operator revoke + stop all'
    revokeModal = {
      ...revokeModal,
      phase: 'stopping',
      results: liveDeps.map((d) => ({ deployment_id: d.deployment_id, phase: 'pending', detail: '' })),
    }
    const settled = await Promise.all(liveDeps.map(async (d) => {
      try {
        const r = await api.authedFetch(
          `/api/v1/agents/${encodeURIComponent(row.agent_id)}/deployments/${encodeURIComponent(d.deployment_id)}/ops/cancel-all`,
          { method: 'POST', body: JSON.stringify({ reason: reasonTxt }) },
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
    await new Promise((res) => setTimeout(res, 500))
    try {
      await api.authedFetch(
        `/api/v1/approvals/${encodeURIComponent(row.fingerprint)}/revoke`,
        { method: 'POST', body: JSON.stringify({ reason: reasonTxt }) },
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

  function toggleBatch(fp) {
    const next = { ...batchSelection }
    if (next[fp]) delete next[fp]
    else next[fp] = true
    batchSelection = next
  }

  function clearBatch() { batchSelection = {} }

  const batchCount = $derived(Object.keys(batchSelection).length)

  function openBatchDeploy() {
    const picks = rows.filter((r) => batchSelection[r.fingerprint] && r.state === 'accepted' && r.connected)
    if (picks.length === 0) return
    batchDeployAgents = picks.map((r) => r.live).filter(Boolean)
  }

  async function submitPreApprove(payload) {
    preApproveError = null
    preApproveBusy = true
    try {
      const r = await api.authedFetch('/api/v1/approvals/pre-approve', {
        method: 'POST',
        body: JSON.stringify(payload),
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

  async function fleetOp(op) {
    const verb = op === 'pause' ? 'Pause' : 'Resume'
    const ok = confirm(
      `${verb} every running deployment across every accepted agent?`
      + `\n\nThis applies immediately — engines stop quoting `
      + (op === 'pause' ? 'until you click Resume.' : 'from paused state.'),
    )
    if (!ok) return
    fleetOpBusy = true
    fleetOpStatus = { phase: 'pending', text: `${verb.toLowerCase()}ing fleet…` }
    try {
      const r = await api.authedFetch('/api/v1/ops/fleet/' + op, {
        method: 'POST',
        body: JSON.stringify({ reason: `operator global ${op}` }),
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

  async function saveProfile(fp, profile) {
    busyFp[fp] = true
    try {
      await api.authedFetch(`/api/v1/agents/${encodeURIComponent(fp)}/profile`, {
        method: 'PUT',
        body: JSON.stringify(profile),
      })
      await refresh()
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      delete busyFp[fp]
      busyFp = { ...busyFp }
    }
  }
</script>

<div class="page scroll">
  <div class="grid">
    <PendingAgentsCard
      {rows}
      {busyFp}
      {skippedFp}
      {nowMs}
      onAccept={accept}
      onReject={reject}
      onSkip={skip}
    />

    {#if rows.length > 0}
      <FleetRollupCard {rows} />
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
            <Button variant="ghost" size="sm" onclick={() => (preApproveOpen = true)} title="Pre-approve a fingerprint before the agent connects">
              {#snippet children()}Pre-approve fingerprint{/snippet}
            </Button>
            <Button variant="warn" size="sm" disabled={fleetOpBusy} onclick={() => fleetOp('pause')} title="Flip paused = true on every running deployment across every accepted agent">
              {#snippet children()}Pause fleet{/snippet}
            </Button>
            <Button variant="ok" size="sm" disabled={fleetOpBusy} onclick={() => fleetOp('resume')} title="Flip paused = false on every running deployment across every accepted agent">
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
              <AgentCard
                row={r}
                credentials={agentCreds[r.agent_id] || []}
                {nowMs}
                busy={busyFp[r.fingerprint]}
                batchSelected={!!batchSelection[r.fingerprint]}
                onToggleBatch={toggleBatch}
                onDeploy={(row) => (deployTarget = row)}
                onAccept={accept}
                onRevoke={revoke}
                onEditSave={(profile) => saveProfile(r.fingerprint, profile)}
                onRetireDeployment={retireDeployment}
                onOpenDrilldown={(agent, deployment) => (drilldownTarget = { agent, deployment })}
              />
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

<RevokeAgentModal
  state={revokeModal}
  onReasonChange={(v) => { if (revokeModal) revokeModal = { ...revokeModal, reason: v } }}
  onCancelOrdersAndRevoke={revokeWithStop}
  onRevokeSkipStop={revokeSkipStop}
  onClose={() => (revokeModal = null)}
/>

<PreApproveModal
  open={preApproveOpen}
  busy={preApproveBusy}
  error={preApproveError}
  onSubmit={submitPreApprove}
  onClose={() => { preApproveOpen = false; preApproveError = null }}
/>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4); }

  .toolbar { display: flex; justify-content: space-between; align-items: center; gap: var(--s-3); margin-bottom: var(--s-2); flex-wrap: wrap; }
  .meta { color: var(--fg-muted); font-size: var(--fs-xs); }
  .meta .error { color: var(--danger); }
  .fleet-ops { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; }
  .fleet-op-status {
    font-size: var(--fs-xs); font-family: var(--font-mono);
    padding: 2px 8px; border-radius: var(--r-sm);
  }
  .fleet-op-status.pending { background: var(--bg-raised); color: var(--fg-muted); }
  .fleet-op-status.ok      { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .fleet-op-status.warn    { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .fleet-op-status.err     { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }

  .rows { display: flex; flex-direction: column; gap: var(--s-3); }
</style>
