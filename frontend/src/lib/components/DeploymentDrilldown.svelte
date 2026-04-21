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
   *     and optimistically merges the patch into the local row so
   *     the UI doesn't flicker while the next telemetry sample
   *     catches up.
   *
   * The drilldown itself is just the container. The four panels
   * (FeatureStatusPanel, AdaptivePanel, ConfigViewer, ParamTuner)
   * each take a thin `{ row, onPatch, canControl }` contract and
   * render a slice of the row.
   */

  import { createApiClient } from '../api.svelte.js'
  import FeatureStatusPanel from './FeatureStatusPanel.svelte'
  import AdaptivePanel from './AdaptivePanel.svelte'
  import ConfigViewer from './ConfigViewer.svelte'
  import ParamTuner from './ParamTuner.svelte'
  import Icon from './Icon.svelte'

  let { auth, agent, deployment, onClose } = $props()
  const api = createApiClient(auth)

  const POLL_MS = 2_000

  let row = $state(deployment)
  let error = $state(null)
  let lastFetch = $state(null)
  let patchStatus = $state(null)

  // Funding-arb recent-event ring buffer, fetched on-demand
  // from the controller's details endpoint when the
  // funding-arb section is visible.
  let fundingEvents = $state([])
  let fundingEventsError = $state(null)
  let fundingEventsLoading = $state(false)

  const canControl = $derived(auth?.canControl?.() ?? false)

  async function refresh() {
    try {
      const fleetData = await api.getJson('/api/v1/fleet')
      const agentRow = Array.isArray(fleetData)
        ? fleetData.find(a => a.agent_id === agent.agent_id)
        : null
      const depRow = agentRow?.deployments?.find(
        d => d.deployment_id === deployment.deployment_id
      )
      if (depRow) {
        row = depRow
        error = null
      } else {
        // The deployment vanished on the agent side (stopped /
        // reconcile dropped it). Keep last snapshot visible but
        // surface the divergence so the operator knows the
        // drilldown is stale.
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
  // Cheaper than polling — operator can click refresh to pull
  // a fresher page.
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

  /**
   * Fire an operational command at this specific deployment.
   * `op` is the path segment of the canonical per-deployment
   * ops endpoint (widen / stop / cancel-all / flatten /
   * disconnect / reset / pause / resume / emulator-register /
   * emulator-cancel / dca-start / dca-cancel / graph-swap).
   * `body` carries optional `{reason, spec, id, graph}`. We
   * surface the result in the same status banner as PATCH.
   */
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
      // Kick a refresh so the new state (e.g. kill_level) shows
      // up immediately rather than waiting for the next poll.
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

  // Wave C8 — flatten-preview modal state. `phase` drives
  // render; `data` is the parsed details reply.
  let flattenPreview = $state(null)

  async function openFlattenPreview() {
    flattenPreview = { phase: 'loading', data: null, reason: '' }
    try {
      const path = `/api/v1/agents/${encodeURIComponent(agent.agent_id)}`
        + `/deployments/${encodeURIComponent(deployment.deployment_id)}/details/flatten_preview`
      const r = await api.getJson(path)
      flattenPreview = { phase: 'confirm', data: r?.payload || null, reason: 'operator flatten (L4)' }
    } catch (e) {
      flattenPreview = { phase: 'err', data: null, error: e?.message || String(e) }
    }
  }

  async function confirmFlatten() {
    if (!flattenPreview) return
    const reason = flattenPreview.reason.trim() || 'operator flatten (L4)'
    flattenPreview = { ...flattenPreview, phase: 'dispatching' }
    await onOp('flatten', { reason })
    flattenPreview = null
  }

  function closeFlattenPreview() {
    flattenPreview = null
  }

  /**
   * Ship a patch map (`{ gamma: "0.02", momentum_enabled: false }`)
   * at the agent. The controller validates admission + forwards
   * to the agent; the agent's registry merges into the variables
   * snapshot and — when the deployment supports hot-reload —
   * translates recognised keys into ConfigOverride variants.
   *
   * Optimistically merges into the local `row.variables` so the
   * UI reflects the change before the next telemetry tick.
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
      // Optimistic merge — next `refresh()` will overwrite with
      // the authoritative agent snapshot if translation landed.
      if (row) {
        row = {
          ...row,
          variables: { ...(row.variables || {}), ...patch },
        }
      }
      patchStatus = {
        phase: 'ok',
        applied: body.patched_fields ?? Object.keys(patch),
      }
    } catch (e) {
      patchStatus = { phase: 'err', error: e?.message || String(e) }
    }
  }

  function formatMs(date) {
    if (!date) return '—'
    return date.toLocaleTimeString()
  }

  const title = $derived(
    `${row?.template || 'deployment'} · ${row?.symbol || deployment.symbol}`
  )
  const subtitle = $derived(
    [row?.venue, row?.product, row?.mode]
      .filter(Boolean)
      .join(' · ') || deployment.deployment_id
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
    <button type="button" class="close" onclick={onClose} aria-label="Close drilldown">
      <Icon name="close" size={16} />
    </button>
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
      <div class="ops-row">
        <button type="button" class="op-btn kl-L1" disabled={!canControl}
          onclick={() => onKillOp('widen', 'WIDEN')}>
          <span class="kl-tag">L1</span><span>Widen</span>
        </button>
        <button type="button" class="op-btn kl-L2" disabled={!canControl}
          onclick={() => onKillOp('stop', 'STOP NEW')}>
          <span class="kl-tag">L2</span><span>Stop</span>
        </button>
        <button type="button" class="op-btn kl-L3" disabled={!canControl}
          onclick={() => onKillOp('cancel-all', 'CANCEL ALL')}>
          <span class="kl-tag">L3</span><span>Cancel</span>
        </button>
        <button type="button" class="op-btn kl-L4" disabled={!canControl}
          onclick={() => onKillOp('flatten', 'FLATTEN')}>
          <span class="kl-tag">L4</span><span>Flatten</span>
        </button>
        <button type="button" class="op-btn kl-L5" disabled={!canControl}
          onclick={() => onKillOp('disconnect', 'DISCONNECT')}>
          <span class="kl-tag">L5</span><span>Disconnect</span>
        </button>
      </div>
      <div class="ops-row ops-row-aux">
        <button type="button" class="op-btn aux" disabled={!canControl}
          onclick={() => onKillOp('reset', 'RESET')}>
          Reset kill switch
        </button>
        <button type="button" class="op-btn aux" disabled={!canControl}
          onclick={() => onOp('pause')}>
          Pause quoting
        </button>
        <button type="button" class="op-btn aux" disabled={!canControl}
          onclick={() => onOp('resume')}>
          Resume quoting
        </button>
        <button type="button" class="op-btn aux" disabled={!canControl}
          onclick={() => onOp('dca-cancel')}>
          Cancel DCA
        </button>
      </div>
      {#if !canControl}
        <p class="ops-hint">
          Read-only — operator role required.
        </p>
      {:else}
        <p class="ops-hint">
          Ladder actions prompt for a reason; reset clears the
          manual escalation recorded in the audit trail.
          Pause/Resume flip the `paused` variable live.
        </p>
      {/if}
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

    {#if row?.funding_arb_active || row?.funding_arb_entered || row?.funding_arb_exited || row?.funding_arb_taker_rejected || row?.funding_arb_pair_break || row?.funding_arb_pair_break_uncompensated}
      <section class="section span-2">
        <h3 class="section-title">Funding-arb driver</h3>
        <div class="exec-grid">
          <div class="exec-cell">
            <span class="exec-k">State</span>
            {#if row.funding_arb_active}
              <span class="exec-v mono" style="color: var(--pos)">ENGAGED</span>
            {:else}
              <span class="exec-v mono">IDLE</span>
            {/if}
          </div>
          <div class="exec-cell">
            <span class="exec-k">Entered</span>
            <span class="exec-v mono">{row.funding_arb_entered ?? 0}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">Exited</span>
            <span class="exec-v mono">{row.funding_arb_exited ?? 0}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">Taker rejected</span>
            <span class="exec-v mono">{row.funding_arb_taker_rejected ?? 0}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">Pair break</span>
            <span class="exec-v mono">{row.funding_arb_pair_break ?? 0}</span>
          </div>
          <div class="exec-cell">
            <span class="exec-k">Pair break (uncomp.)</span>
            {#if (row.funding_arb_pair_break_uncompensated ?? 0) > 0}
              <span class="exec-v mono" style="color: var(--neg)">{row.funding_arb_pair_break_uncompensated}</span>
            {:else}
              <span class="exec-v mono">0</span>
            {/if}
          </div>
        </div>

        <div class="events-head">
          <span class="events-title">Recent events</span>
          <button
            type="button"
            class="events-refresh"
            disabled={fundingEventsLoading}
            onclick={loadFundingEvents}
          >
            {fundingEventsLoading ? 'Loading…' : 'Refresh'}
          </button>
        </div>
        {#if fundingEventsError}
          <p class="exec-hint err">Details fetch failed: {fundingEventsError}</p>
        {:else if fundingEvents.length === 0 && !fundingEventsLoading}
          <p class="exec-hint">No recent events in the agent's ring buffer yet.</p>
        {:else}
          <table class="events-table">
            <thead>
              <tr>
                <th>when</th>
                <th>outcome</th>
                <th>reason</th>
              </tr>
            </thead>
            <tbody>
              {#each fundingEvents as ev, i (i)}
                <tr>
                  <td class="mono">{new Date(ev.at_ms).toLocaleTimeString()}</td>
                  <td class="mono">{ev.outcome}</td>
                  <td class="mono">{ev.reason || '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
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
  <div class="flatten-backdrop" onclick={closeFlattenPreview}>
    <div class="flatten-card" onclick={(e) => e.stopPropagation()}>
      <div class="flatten-title">Flatten preview · L4 kill</div>
      {#if flattenPreview.phase === 'loading'}
        <div class="flatten-body muted">fetching current position + book…</div>
      {:else if flattenPreview.phase === 'err'}
        <div class="flatten-body">
          <div class="banner err">
            <Icon name="info" size={12} />
            <span>Preview failed: {flattenPreview.error}</span>
          </div>
        </div>
      {:else if !flattenPreview.data || flattenPreview.data.side === 'flat'}
        <div class="flatten-body">
          <div class="banner ok">Position is flat — nothing to unwind.</div>
        </div>
      {:else}
        <div class="flatten-body">
          <div class="flatten-kv">
            <div class="fk-cell">
              <span class="fk-k">side</span>
              <span class="fk-v mono">{flattenPreview.data.side}</span>
            </div>
            <div class="fk-cell">
              <span class="fk-k">quantity</span>
              <span class="fk-v mono">{flattenPreview.data.quantity}</span>
            </div>
            <div class="fk-cell">
              <span class="fk-k">mid</span>
              <span class="fk-v mono">{flattenPreview.data.mid_price}</span>
            </div>
            <div class="fk-cell">
              <span class="fk-k">notional</span>
              <span class="fk-v mono">{flattenPreview.data.inventory_value_quote}</span>
            </div>
          </div>
          {#if flattenPreview.data.book_depth_covers_position}
            <div class="banner ok">
              Book depth covers the position within
              <code>{flattenPreview.data.estimated_slippage_pct ?? '—'}%</code>
              from mid.
            </div>
          {:else}
            <div class="banner err">
              Book depth does NOT fully cover the position at the visible levels —
              expect slippage beyond
              <code>{flattenPreview.data.estimated_slippage_pct ?? '—'}%</code>
              from mid. A market sweep may pause partway.
            </div>
          {/if}
          {#if flattenPreview.data.book_levels?.length > 0}
            <table class="lvl-table">
              <thead>
                <tr>
                  <th>pct from mid</th>
                  <th class="num">bid depth (quote)</th>
                  <th class="num">ask depth (quote)</th>
                </tr>
              </thead>
              <tbody>
                {#each flattenPreview.data.book_levels as l (l.pct_from_mid)}
                  <tr>
                    <td class="mono">{l.pct_from_mid}</td>
                    <td class="num mono">{l.bid_depth_quote}</td>
                    <td class="num mono">{l.ask_depth_quote}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
          <label class="flatten-reason">
            <span class="fk-k">Reason</span>
            <input type="text" bind:value={flattenPreview.reason} placeholder="operator flatten (L4)" />
          </label>
        </div>
      {/if}
      <div class="flatten-actions">
        <button type="button" class="btn ghost" onclick={closeFlattenPreview}>Cancel</button>
        {#if flattenPreview.phase === 'confirm' && flattenPreview.data?.side !== 'flat'}
          <button type="button" class="btn danger" onclick={confirmFlatten}>
            Confirm flatten
          </button>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.45);
    z-index: 40;
  }
  .flatten-backdrop {
    position: fixed; inset: 0; z-index: 50;
    background: rgba(0, 0, 0, 0.55);
    display: flex; align-items: center; justify-content: center;
    padding: var(--s-5);
  }
  .flatten-card {
    width: 640px; max-width: 100%;
    background: var(--bg-raised); border: 1px solid var(--border-strong);
    border-radius: var(--r-lg); padding: var(--s-4);
    display: flex; flex-direction: column; gap: var(--s-3);
    max-height: 92vh; overflow-y: auto;
  }
  .flatten-title { font-size: var(--fs-lg); font-weight: 600; color: var(--fg-primary); }
  .flatten-body { display: flex; flex-direction: column; gap: var(--s-2); }
  .flatten-kv {
    display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: var(--s-2);
  }
  .fk-cell { display: flex; flex-direction: column; gap: 2px; padding: var(--s-2); background: var(--bg-chip); border-radius: var(--r-sm); }
  .fk-k { font-size: 10px; color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .fk-v { font-size: var(--fs-sm); color: var(--fg-primary); }
  .flatten-reason { display: flex; flex-direction: column; gap: 4px; }
  .flatten-reason input {
    padding: var(--s-2); background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary); font-family: var(--font-mono); font-size: var(--fs-xs);
  }
  .flatten-actions { display: flex; gap: var(--s-2); justify-content: flex-end; }
  .lvl-table { width: 100%; border-collapse: collapse; font-size: var(--fs-xs); }
  .lvl-table th, .lvl-table td {
    padding: 4px var(--s-2); border-bottom: 1px solid var(--border-subtle); text-align: left;
  }
  .lvl-table th { color: var(--fg-muted); text-transform: uppercase; font-size: 10px; letter-spacing: var(--tracking-label); }
  .lvl-table .num { text-align: right; }
  .muted { color: var(--fg-muted); }
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
  .close {
    background: transparent; border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-muted);
    width: 28px; height: 28px; cursor: pointer;
    display: inline-flex; align-items: center; justify-content: center;
  }
  .close:hover { color: var(--fg-primary); background: var(--bg-chip); }

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
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }

  .chip {
    font-family: var(--font-mono); font-size: 10px;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-weight: 600; padding: 2px 6px; border-radius: var(--r-sm);
    border: 1px solid currentColor;
  }
  .tone-ok { color: var(--pos); }
  .tone-danger { color: var(--neg); }
  .tone-muted { color: var(--fg-muted); }

  .banner {
    display: flex; align-items: center; gap: var(--s-2);
    margin: var(--s-2) var(--s-4);
    padding: var(--s-2) var(--s-3);
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .banner.err { color: var(--neg); background: rgba(239, 68, 68, 0.08); }
  .banner.ok  { color: var(--pos); background: rgba(16, 185, 129, 0.08); }

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

  .exec-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s-2);
  }
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
  .exec-v {
    font-size: var(--fs-sm);
    color: var(--fg-primary);
    font-weight: 600;
  }
  .exec-hint {
    margin: 0;
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    line-height: var(--lh-snug);
  }
  .exec-hint.err { color: var(--neg); }

  .events-head {
    display: flex; justify-content: space-between; align-items: center;
    margin-top: var(--s-2);
  }
  .events-title {
    font-size: 10px; color: var(--fg-muted);
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-weight: 600;
  }
  .events-refresh {
    padding: 2px 8px;
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
    font-size: var(--fs-2xs);
    cursor: pointer;
  }
  .events-refresh:disabled { opacity: 0.5; cursor: not-allowed; }
  .events-refresh:hover:not(:disabled) {
    color: var(--fg-primary); background: var(--bg-base);
  }

  .events-table {
    width: 100%; border-collapse: collapse;
    font-size: var(--fs-2xs);
    margin-top: var(--s-1);
  }
  .events-table th, .events-table td {
    padding: 4px var(--s-2);
    text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .events-table th {
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-size: 10px;
    font-weight: 600;
  }
  .events-table td.mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; color: var(--fg-primary); }

  .ops-row {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: var(--s-2);
  }
  .ops-row-aux {
    grid-template-columns: repeat(4, 1fr);
    margin-top: var(--s-2);
  }
  .op-btn {
    display: inline-flex; align-items: center; justify-content: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
    font-family: var(--font-sans);
    font-size: var(--fs-xs);
    font-weight: 600;
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
  }
  .op-btn:hover:not(:disabled) {
    border-color: var(--border-strong); color: var(--fg-primary);
  }
  .op-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .op-btn .kl-tag {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--r-sm);
    font-weight: 700;
  }
  .op-btn.kl-L1 .kl-tag { background: rgba(245, 158, 11, 0.22); color: var(--warn); }
  .op-btn.kl-L1:hover:not(:disabled) { border-color: rgba(245, 158, 11, 0.45); }
  .op-btn.kl-L2 .kl-tag { background: rgba(245, 158, 11, 0.28); color: #fbbf24; }
  .op-btn.kl-L2:hover:not(:disabled) { border-color: rgba(245, 158, 11, 0.55); }
  .op-btn.kl-L3 .kl-tag { background: rgba(239, 68, 68, 0.2);  color: var(--neg); }
  .op-btn.kl-L3:hover:not(:disabled) { border-color: rgba(239, 68, 68, 0.5); }
  .op-btn.kl-L4 .kl-tag { background: rgba(239, 68, 68, 0.4);  color: #fff; }
  .op-btn.kl-L4:hover:not(:disabled) { border-color: rgba(239, 68, 68, 0.75); color: var(--neg); }
  .op-btn.kl-L5 .kl-tag { background: rgba(127, 29, 29, 0.85); color: #fff; }
  .op-btn.kl-L5:hover:not(:disabled) { border-color: rgba(127, 29, 29, 0.9); color: var(--neg); }
  .op-btn.aux { font-size: var(--fs-2xs); }
  .ops-hint {
    margin: var(--s-2) 0 0;
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    line-height: var(--lh-snug);
  }
</style>
