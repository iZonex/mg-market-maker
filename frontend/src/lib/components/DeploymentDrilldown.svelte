<script>
  /*
   * Per-deployment drilldown.
   *
   * Opened from FleetPage — takes the agent + deployment the
   * operator clicked on, then owns:
   *   - polling the fleet endpoint to refresh this deployment's
   *     snapshot (features / variables / regime / kill_level / γ …)
   *   - the `onPatch` adaptor that hits
   *     `PATCH /api/v1/agents/{agent_id}/deployments/{deployment_id}/variables`
   *     and optimistically merges the patch into the local row
   *     so the UI doesn't flicker while the next telemetry
   *     sample catches up.
   *   - flatten-preview (L4) and file-incident side modals.
   *
   * The actual panels (FeatureStatusPanel, AdaptivePanel,
   * ConfigViewer, ParamTuner) take a `{ row, onPatch, canControl }`
   * contract; the ops ladder, funding-arb section, and flatten
   * modal live in ./drilldown/*.
   */

  import { untrack } from 'svelte'
  import { createApiClient } from '../api.svelte.js'
  import FeatureStatusPanel from './FeatureStatusPanel.svelte'
  import AdaptivePanel from './AdaptivePanel.svelte'
  import ConfigViewer from './ConfigViewer.svelte'
  import ParamTuner from './ParamTuner.svelte'
  import Icon from './Icon.svelte'
  import { Button } from '../primitives/index.js'
  import OpsLadder from './drilldown/OpsLadder.svelte'
  import FundingArbPanel from './drilldown/FundingArbPanel.svelte'
  import FlattenPreviewModal from './drilldown/FlattenPreviewModal.svelte'

  let {
    auth,
    agent,
    deployment,
    onClose,
    onOpenGraphLive = null,
    onNavigate = () => {},
  } = $props()
  const api = $derived(createApiClient(auth))

  const POLL_MS = 2_000

  let row = $state(untrack(() => deployment))
  let error = $state(null)
  let lastFetch = $state(null)
  let patchStatus = $state(null)

  let fundingEvents = $state([])
  let fundingEventsError = $state(null)
  let fundingEventsLoading = $state(false)

  const canControl = $derived(auth?.canControl?.() ?? false)

  async function refresh() {
    try {
      const fleetData = await api.getJson('/api/v1/fleet')
      const agentRow = Array.isArray(fleetData)
        ? fleetData.find((a) => a.agent_id === agent.agent_id)
        : null
      const depRow = agentRow?.deployments?.find(
        (d) => d.deployment_id === deployment.deployment_id,
      )
      if (depRow) {
        row = depRow
        error = null
      } else {
        // Deployment vanished (stopped / reconcile dropped it).
        // Keep last snapshot visible but surface the divergence.
        error = 'deployment no longer in fleet snapshot'
      }
      lastFetch = new Date()
    } catch (e) {
      error = e?.message || String(e)
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, POLL_MS)
    return () => clearInterval(t)
  })

  async function loadFundingEvents() {
    fundingEventsLoading = true
    fundingEventsError = null
    try {
      const path = `/api/v1/agents/${encodeURIComponent(agent.agent_id)}`
        + `/deployments/${encodeURIComponent(deployment.deployment_id)}`
        + `/details/funding_arb_recent_events`
      const body = await api.getJson(path)
      if (body.error) {
        fundingEventsError = body.error
        fundingEvents = []
      } else {
        fundingEvents = body.payload?.events || []
      }
    } catch (e) {
      fundingEventsError = e?.message || String(e)
      fundingEvents = []
    } finally {
      fundingEventsLoading = false
    }
  }

  // Auto-load the funding-arb events drawer once any driver
  // counter is non-zero OR the driver is currently engaged.
  let fundingEventsEverLoaded = $state(false)
  $effect(() => {
    const hasActivity = (row?.funding_arb_active)
      || (row?.funding_arb_entered ?? 0) > 0
      || (row?.funding_arb_exited ?? 0) > 0
      || (row?.funding_arb_pair_break ?? 0) > 0
      || (row?.funding_arb_pair_break_uncompensated ?? 0) > 0
    if (hasActivity && !fundingEventsEverLoaded) {
      fundingEventsEverLoaded = true
      loadFundingEvents()
    }
  })

  async function onOp(op, body = {}) {
    patchStatus = { phase: 'sending', keys: [op] }
    try {
      const path = `/api/v1/agents/${encodeURIComponent(agent.agent_id)}`
        + `/deployments/${encodeURIComponent(deployment.deployment_id)}/ops/${encodeURIComponent(op)}`
      const r = await api.authedFetch(path, {
        method: 'POST',
        body: JSON.stringify(body),
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      patchStatus = { phase: 'ok', applied: [op] }
      refresh()
    } catch (e) {
      patchStatus = { phase: 'err', error: e?.message || String(e) }
    }
  }

  async function onKillOp(op, verb) {
    // Wave C8 — flatten needs an explicit preview before we
    // dispatch. Kicks the modal with qty/side/depth estimate
    // instead of the standard reason prompt.
    if (op === 'flatten') {
      await openFlattenPreview()
      return
    }
    const reason = prompt(
      `Reason for ${verb} on ${deployment.deployment_id}?`,
      'dashboard operator',
    )
    if (reason === null) return
    await onOp(op, { reason })
  }

  let flattenPreview = $state(null)

  async function openFlattenPreview() {
    flattenPreview = { phase: 'loading', data: null, reason: '' }
    try {
      const path = `/api/v1/agents/${encodeURIComponent(agent.agent_id)}`
        + `/deployments/${encodeURIComponent(deployment.deployment_id)}/details/flatten_preview`
      const r = await api.getJson(path)
      flattenPreview = { phase: 'confirm', data: r?.payload || null, reason: 'operator flatten (L4)' }
    } catch (e) {
      flattenPreview = { phase: 'err', data: null, error: e?.message || String(e), reason: '' }
    }
  }

  async function confirmFlatten() {
    if (!flattenPreview) return
    const reason = flattenPreview.reason.trim() || 'operator flatten (L4)'
    flattenPreview = { ...flattenPreview, phase: 'dispatching' }
    await onOp('flatten', { reason })
    flattenPreview = null
  }

  function closeFlattenPreview() { flattenPreview = null }
  function updateFlattenReason(v) {
    if (flattenPreview) flattenPreview = { ...flattenPreview, reason: v }
  }

  // M4-4 GOBS — file an incident stamped with the deployment's
  // latest graph tick. Reads `graph_trace_recent?limit=1` so the
  // incident row later opens the live canvas at the exact frame.
  let incidentBusy = $state(false)
  async function fileIncident() {
    if (incidentBusy) return
    incidentBusy = true
    try {
      let tickNum = null
      try {
        const traceResp = await api.getJson(
          `/api/v1/agents/${encodeURIComponent(agent.agent_id)}` +
            `/deployments/${encodeURIComponent(deployment.deployment_id)}` +
            `/details/graph_trace_recent?limit=1`,
        )
        tickNum = traceResp?.payload?.traces?.[0]?.tick_num ?? null
      } catch { /* leave null — engine may not have ticked yet */ }

      const severity = (row?.kill_level ?? 0) >= 3 ? 'critical' : 'warning'
      const detail =
        (row?.kill_level ?? 0) > 0
          ? `kill_level=L${row.kill_level}`
          : `graph ${row?.active_graph?.name ?? 'unknown'} @ ${row?.active_graph?.hash?.slice(0, 12) ?? '?'}`
      const body = {
        violation_key: `graph#${agent.agent_id}/${deployment.deployment_id}`,
        severity,
        category: 'strategy-graph',
        target: `${agent.agent_id}/${deployment.deployment_id}`,
        metric: row?.active_graph?.name ?? 'graph',
        detail,
        graph_agent_id: agent.agent_id,
        graph_deployment_id: deployment.deployment_id,
        graph_tick_num: tickNum,
      }
      await api.postJson('/api/v1/incidents', body)
      onClose()
      onNavigate('incidents')
    } catch (e) {
      patchStatus = { phase: 'err', error: `file-incident failed: ${e}` }
    } finally {
      incidentBusy = false
    }
  }

  /**
   * Ship a patch map (`{ gamma: "0.02", momentum_enabled: false }`)
   * at the agent. The controller validates + forwards; the agent's
   * registry merges into the variables snapshot and — when the
   * deployment supports hot-reload — translates recognised keys
   * into ConfigOverride variants.
   *
   * Optimistically merges into the local `row.variables`.
   */
  async function onPatch(patch) {
    if (!patch || Object.keys(patch).length === 0) return
    patchStatus = { phase: 'sending', keys: Object.keys(patch) }
    try {
      const path = `/api/v1/agents/${encodeURIComponent(agent.agent_id)}`
        + `/deployments/${encodeURIComponent(deployment.deployment_id)}/variables`
      const r = await api.authedFetch(path, {
        method: 'PATCH',
        body: JSON.stringify(patch),
      })
      if (!r.ok) {
        const text = await r.text().catch(() => '')
        throw new Error(`${r.status} ${text}`)
      }
      const body = await r.json().catch(() => ({}))
      if (row) {
        row = { ...row, variables: { ...(row.variables || {}), ...patch } }
      }
      patchStatus = { phase: 'ok', applied: body.patched_fields ?? Object.keys(patch) }
    } catch (e) {
      patchStatus = { phase: 'err', error: e?.message || String(e) }
    }
  }

  function formatMs(date) {
    if (!date) return '—'
    return date.toLocaleTimeString()
  }

  const title = $derived(
    `${row?.template || 'deployment'} · ${row?.symbol || deployment.symbol}`,
  )
  const subtitle = $derived(
    [row?.venue, row?.product, row?.mode].filter(Boolean).join(' · ')
      || deployment.deployment_id,
  )

  const showFundingArb = $derived(
    row?.funding_arb_active
      || row?.funding_arb_entered
      || row?.funding_arb_exited
      || row?.funding_arb_taker_rejected
      || row?.funding_arb_pair_break
      || row?.funding_arb_pair_break_uncompensated,
  )
</script>

<div class="backdrop" role="presentation" onclick={onClose}></div>

<div class="panel" role="dialog" tabindex="-1" aria-label="Deployment drilldown">
  <header class="head">
    <div class="head-text">
      <span class="title">{title}</span>
      <span class="subtitle mono">{subtitle}</span>
      <span class="subtitle mono faint">{deployment.deployment_id}</span>
    </div>
    <div class="head-actions">
      {#if onOpenGraphLive && row?.active_graph?.name}
        <Button variant="ghost" size="sm" onclick={() => onOpenGraphLive(agent.agent_id, deployment.deployment_id)}
          title="Open this deployment's strategy graph in Live mode">
          {#snippet children()}<Icon name="pulse" size={12} />
          <span>Open graph (live)</span>{/snippet}
        </Button>
      {/if}
      {#if row?.active_graph?.name}
        <Button variant="ghost" size="sm" onclick={fileIncident}
          disabled={incidentBusy}
          title="File an incident stamped with this deployment's latest tick so a post-mortem can jump straight to the live graph at that frame">
          {#snippet children()}<Icon name="alert" size={12} />
          <span>{incidentBusy ? 'Filing…' : 'File incident'}</span>{/snippet}
        </Button>
      {/if}
      <Button variant="ghost" size="sm" iconOnly onclick={onClose} aria-label="Close drilldown">
        {#snippet children()}<Icon name="close" size={16} />{/snippet}
      </Button>
    </div>
  </header>

  <div class="meta">
    <span class="chip tone-{row?.running ? 'ok' : 'muted'}">
      {row?.running ? 'RUNNING' : 'STOPPED'}
    </span>
    {#if row?.kill_level > 0}
      <span class="chip tone-danger">KILL L{row.kill_level}</span>
    {/if}
    {#if row?.template}
      <span class="meta-item">
        <span class="k">template</span>
        <span class="v mono">{row.template}</span>
      </span>
    {/if}
    {#if row?.active_graph?.hash}
      <span class="meta-item" title={`${row.active_graph.name || ''} · deployed ${new Date(row.active_graph.deployed_at_ms).toLocaleString()} · ${row.active_graph.node_count} nodes`}>
        <span class="k">graph</span>
        <span class="v mono">{row.active_graph.name || '—'}
          <span class="faint">@{row.active_graph.hash.slice(0, 8)}</span>
        </span>
      </span>
    {/if}
    <span class="meta-item">
      <span class="k">inventory</span>
      <span class="v mono">{row?.inventory || '—'}</span>
    </span>
    <span class="meta-item">
      <span class="k">PnL</span>
      <span class="v mono">{row?.unrealized_pnl_quote || '—'}</span>
    </span>
    <span class="meta-item">
      <span class="k">live orders</span>
      <span class="v mono">{row?.live_orders ?? 0}</span>
    </span>
    <span class="meta-item stale">
      <span class="k">refreshed</span>
      <span class="v mono">{formatMs(lastFetch)}</span>
    </span>
  </div>

  {#if error}
    <div class="banner err">
      <Icon name="info" size={12} />
      <span>{error}</span>
    </div>
  {/if}

  {#if patchStatus}
    <div class="banner {patchStatus.phase === 'err' ? 'err' : 'ok'}">
      <Icon name="info" size={12} />
      {#if patchStatus.phase === 'sending'}
        <span>Sending patch ({patchStatus.keys.join(', ')})…</span>
      {:else if patchStatus.phase === 'ok'}
        <span>Patch applied: {(patchStatus.applied || []).join(', ')}</span>
      {:else}
        <span>Patch failed: {patchStatus.error}</span>
      {/if}
    </div>
  {/if}

  <div class="grid">
    <section class="section">
      <h3 class="section-title">Adaptive state</h3>
      <AdaptivePanel {row} />
    </section>

    <section class="section span-2">
      <h3 class="section-title">Ops · kill ladder + control</h3>
      <OpsLadder {canControl} {onKillOp} {onOp} />
    </section>

    <section class="section">
      <h3 class="section-title">Execution</h3>
      <div class="exec-grid">
        <div class="exec-cell">
          <span class="exec-k">SOR filled (last)</span>
          <span class="exec-v mono">{row?.sor_filled_qty || '—'}</span>
        </div>
        <div class="exec-cell">
          <span class="exec-k">SOR dispatch OK</span>
          <span class="exec-v mono">{row?.sor_dispatch_success ?? 0}</span>
        </div>
        <div class="exec-cell">
          <span class="exec-k">Atomic bundles · inflight</span>
          <span class="exec-v mono">{row?.atomic_bundles_inflight ?? 0}</span>
        </div>
        <div class="exec-cell">
          <span class="exec-k">Atomic bundles · done</span>
          <span class="exec-v mono">{row?.atomic_bundles_completed ?? 0}</span>
        </div>
      </div>
    </section>

    <section class="section">
      <h3 class="section-title">Calibration (GLFT)</h3>
      {#if !row?.calibration_samples && !row?.calibration_a}
        <p class="exec-hint">
          No samples yet — strategy either doesn't calibrate
          (Avellaneda / Grid / Basis) or the fill window hasn't
          warmed up.
        </p>
      {:else}
        <div class="exec-grid">
          <div class="exec-cell">
            <span class="exec-k">a (arrival)</span>
            <span class="exec-v mono">{row?.calibration_a || '—'}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">k (depth sens.)</span>
            <span class="exec-v mono">{row?.calibration_k || '—'}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">Samples</span>
            <span class="exec-v mono">{row?.calibration_samples ?? 0}</span>
          </div>
        </div>
      {/if}
    </section>

    <section class="section">
      <h3 class="section-title">Manipulation detectors</h3>
      {#if !row?.manipulation_combined}
        <p class="exec-hint">
          Detectors warming up — need a book + trade history to
          produce a score.
        </p>
      {:else}
        <div class="exec-grid">
          <div class="exec-cell">
            <span class="exec-k">Combined</span>
            <span class="exec-v mono">{row?.manipulation_combined || '—'}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">Pump-dump</span>
            <span class="exec-v mono">{row?.manipulation_pump_dump || '—'}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">Wash</span>
            <span class="exec-v mono">{row?.manipulation_wash || '—'}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">Thin book</span>
            <span class="exec-v mono">{row?.manipulation_thin_book || '—'}</span>
          </div>
        </div>
      {/if}
    </section>

    {#if showFundingArb}
      <section class="section span-2">
        <h3 class="section-title">Funding-arb driver</h3>
        <FundingArbPanel
          {row}
          events={fundingEvents}
          loading={fundingEventsLoading}
          error={fundingEventsError}
          onRefresh={loadFundingEvents}
        />
      </section>
    {/if}

    <section class="section">
      <h3 class="section-title">Feature flags</h3>
      <FeatureStatusPanel {row} {onPatch} {canControl} />
    </section>

    <section class="section span-2">
      <h3 class="section-title">Live tuning</h3>
      <ParamTuner {row} {onPatch} {canControl} />
    </section>

    <section class="section span-2">
      <h3 class="section-title">Variables snapshot</h3>
      <ConfigViewer {row} />
    </section>
  </div>
</div>

{#if flattenPreview}
  <FlattenPreviewModal
    state={flattenPreview}
    onConfirm={confirmFlatten}
    onClose={closeFlattenPreview}
    onReasonChange={updateFlattenReason}
  />
{/if}

<style>
  .backdrop {
    position: fixed; inset: 0;
    background: var(--bg-overlay);
    z-index: 40;
  }
  .panel {
    position: fixed; top: 0; right: 0; bottom: 0;
    width: min(860px, 95vw);
    z-index: 41;
    background: var(--bg-base);
    border-left: 1px solid var(--border-strong);
    display: flex; flex-direction: column;
    overflow: hidden;
  }
  .head {
    display: flex; justify-content: space-between; align-items: flex-start;
    padding: var(--s-4);
    border-bottom: 1px solid var(--border-subtle);
    gap: var(--s-3);
  }
  .head-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .title { font-size: var(--fs-md); font-weight: 600; color: var(--fg-primary); }
  .subtitle { font-size: var(--fs-xs); color: var(--fg-secondary); }
  .subtitle.faint { color: var(--fg-muted); font-size: 10px; }
  .head-actions { display: inline-flex; align-items: center; gap: var(--s-2); }

  .meta {
    display: flex; flex-wrap: wrap; gap: var(--s-3);
    padding: var(--s-3) var(--s-4);
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-chip);
    font-size: var(--fs-xs);
  }
  .meta-item { display: inline-flex; gap: 4px; align-items: baseline; }
  .meta-item.stale { margin-left: auto; color: var(--fg-muted); }
  .k { color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); font-size: 10px; }
  .v { color: var(--fg-primary); }

  .banner {
    display: flex; align-items: center; gap: var(--s-2);
    margin: var(--s-2) var(--s-4);
    padding: var(--s-2) var(--s-3);
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .banner.err { color: var(--neg); background: color-mix(in srgb, var(--neg) 8%, transparent); }
  .banner.ok  { color: var(--pos); background: color-mix(in srgb, var(--pos) 8%, transparent); }

  .grid {
    flex: 1; overflow-y: auto;
    padding: var(--s-4);
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--s-4);
    align-content: start;
  }
  .section {
    display: flex; flex-direction: column; gap: var(--s-2);
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    min-width: 0;
  }
  .section.span-2 { grid-column: span 2; }
  .section-title {
    margin: 0;
    font-size: var(--fs-xs); font-weight: 600;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
  }

  .exec-grid { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-2); }
  .exec-cell {
    display: flex; flex-direction: column; gap: 2px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
  }
  .exec-k {
    font-size: 10px;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .exec-v { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 600; }
  .exec-hint {
    margin: 0;
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    line-height: var(--lh-snug);
  }
</style>
