<script>
  /*
   * Pending calibration card (Epic 33.5).
   *
   * Surfaces hyperopt runs staged in
   * `DashboardState.pending_calibrations`. Each pending entry
   * shows a diff of current vs suggested parameters; the
   * operator reviews and either applies (dispatches matching
   * `ConfigOverride`s through the engine's channel) or discards.
   *
   * Also hosts the "run a new calibration" trigger form
   * (collapsed by default) so the full approval flow lives in
   * one place on the Calibration page.
   *
   * Polls `/api/admin/optimize/pending` every 15 s — calibration
   * runs take minutes, a faster cadence just burns tokens. On
   * apply / discard / trigger the card re-fetches immediately so
   * the list state stays truthful without a refresh.
   *
   * Safety: every endpoint is admin-gated server-side (admin
   * middleware + per-user rate limit). The card still gates on
   * `auth.canControl()` at the call site so viewers never see
   * buttons they cannot use.
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'
  import { Button } from '../primitives/index.js'
  import CalibrationEntry from './calibration/CalibrationEntry.svelte'
  import CalibrationTriggerForm from './calibration/CalibrationTriggerForm.svelte'

  let { data, auth } = $props()
  const api = $derived(createApiClient(auth))

  const s = $derived(data?.state ?? { activeSymbol: '', symbols: [], data: {} })
  const activeSymbol = $derived(s.activeSymbol || s.symbols?.[0] || '')

  let pending = $state([])
  let error = $state('')
  let busy = $state('')           // symbol currently in flight, or '' idle
  let status = $state('')         // last success/failure message
  let showTrigger = $state(false)
  let triggerBusy = $state(false)

  async function refresh() {
    try {
      pending = await api.getJson('/api/admin/optimize/pending')
      error = ''
    } catch (e) {
      error = e.message
    }
  }

  async function apply(sym) {
    if (busy) return
    busy = sym
    status = ''
    try {
      const resp = await api.postJson('/api/admin/optimize/apply', { symbol: sym })
      const applied = resp.applied ?? 0
      const skipped = resp.skipped ?? []
      status = skipped.length
        ? `Applied ${applied} to ${sym}; skipped ${skipped.length}: ${skipped.join(', ')}`
        : `Applied ${applied} override${applied === 1 ? '' : 's'} to ${sym}`
      await refresh()
    } catch (e) {
      status = `Apply failed: ${e.message}`
    } finally {
      busy = ''
    }
  }

  async function discard(sym) {
    if (busy) return
    if (!confirm(`Discard pending calibration for ${sym}?`)) return
    busy = sym
    status = ''
    try {
      await api.postJson('/api/admin/optimize/discard', { symbol: sym })
      status = `Discarded ${sym}`
      await refresh()
    } catch (e) {
      status = `Discard failed: ${e.message}`
    } finally {
      busy = ''
    }
  }

  function openTriggerForm() {
    showTrigger = true
    status = ''
  }

  async function submitTrigger(payload) {
    triggerBusy = true
    status = ''
    try {
      const resp = await api.postJson('/api/admin/optimize/trigger', payload)
      const runTrials = resp.trials ?? payload.num_trials
      status = `Queued hyperopt run for ${payload.symbol} (${runTrials} trials) — pending list will update on completion.`
      showTrigger = false
      await refresh()
    } catch (e) {
      status = `Trigger failed: ${e.message}`
    } finally {
      triggerBusy = false
    }
  }

  const initialPath = $derived(activeSymbol
    ? `data/recorded/${activeSymbol}.jsonl`
    : 'data/recorded/BTCUSDT.jsonl')

  $effect(() => {
    refresh()
    // 15 s — hyperopt is a multi-minute operation, no point
    // polling tighter.
    const id = setInterval(refresh, 15_000)
    return () => clearInterval(id)
  })
</script>

<div class="calib">
  {#if error}
    <div class="empty-state">
      <span class="empty-state-icon" style="color: var(--neg)"><Icon name="alert" size={18} /></span>
      <span class="empty-state-title">Failed to load pending calibrations</span>
      <span class="empty-state-hint">{error}</span>
    </div>
  {:else if pending.length === 0}
    <div class="empty-state">
      <span class="empty-state-icon"><Icon name="calibration" size={18} /></span>
      <span class="empty-state-title">No pending calibrations</span>
      <span class="empty-state-hint">
        Trigger a hyperopt run against a recorded JSONL to stage
        a suggestion for review.
      </span>
      {#if !showTrigger}
        <Button variant="primary" onclick={openTriggerForm}>
          {#snippet children()}<Icon name="bolt" size={14} />
          <span>Run calibration</span>{/snippet}
        </Button>
      {/if}
    </div>
  {:else}
    <header class="head">
      <div class="head-left">
        <span class="label">Pending</span>
        <span class="chip tone-warn">{pending.length}</span>
      </div>
      <Button variant="primary" onclick={openTriggerForm} disabled={showTrigger}>
        {#snippet children()}<Icon name="bolt" size={12} />
        <span>New run</span>{/snippet}
      </Button>
    </header>

    <div class="list">
      {#each pending as p (p.symbol)}
        <CalibrationEntry entry={p} {busy} onApply={apply} onDiscard={discard} />
      {/each}
    </div>
  {/if}

  {#if showTrigger}
    <CalibrationTriggerForm
      initialSymbol={activeSymbol}
      {initialPath}
      busy={triggerBusy}
      onSubmit={submitTrigger}
      onCancel={() => { showTrigger = false }}
    />
  {/if}

  {#if status}
    <div class="status-line">
      <Icon name="info" size={12} />
      <span>{status}</span>
    </div>
  {/if}
</div>

<style>
  .calib {
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
  }
  .head-left {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
  }

  .status-line {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    word-break: break-word;
  }
</style>
