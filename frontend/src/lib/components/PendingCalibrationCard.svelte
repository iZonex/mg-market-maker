<script>
  /*
   * Pending calibration card (Epic 33.5).
   *
   * Surfaces hyperopt runs staged in `DashboardState.pending_
   * calibrations`. Each pending entry shows a diff of
   * current vs suggested parameters; the operator reviews
   * and either applies (dispatches matching `ConfigOverride`s
   * through the engine's channel) or discards.
   *
   * Also hosts the "run a new calibration" trigger form
   * (collapsed by default) so the full approval flow lives in
   * one place on the Calibration page.
   *
   * Polls `/api/admin/optimize/pending` every 15 s — calibration
   * runs take minutes, a faster cadence just burns tokens. On
   * apply / discard / trigger the card re-fetches immediately
   * so the list state stays truthful without a refresh.
   *
   * Safety: every endpoint is admin-gated server-side (admin
   * middleware + per-user rate limit). The card still gates on
   * `auth.canControl()` at the call site so viewers never see
   * buttons they cannot use.
   */

  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'
  import { Button } from '../primitives/index.js'

  let { data, auth } = $props()
  const api = $derived(createApiClient(auth))

  const s = $derived(data?.state ?? { activeSymbol: '', symbols: [], data: {} })
  const activeSymbol = $derived(s.activeSymbol || s.symbols?.[0] || '')

  let pending = $state([])
  let error = $state('')
  let busy = $state('')           // symbol currently in flight, or '' idle
  let status = $state('')         // last success/failure message
  let lastRefreshMs = $state(null)

  // Inline trigger form state.
  let showTrigger = $state(false)
  let triggerSym = $state('')
  let triggerPath = $state('data/recorded/BTCUSDT.jsonl')
  let triggerTrials = $state(100)
  let triggerLoss = $state('sharpe')
  let triggerBusy = $state(false)

  async function refresh() {
    try {
      pending = await api.getJson('/api/admin/optimize/pending')
      error = ''
      lastRefreshMs = Date.now()
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
    triggerSym = activeSymbol
    triggerPath = activeSymbol
      ? `data/recorded/${activeSymbol}.jsonl`
      : triggerPath
    showTrigger = true
    status = ''
  }

  async function submitTrigger() {
    if (!triggerSym) {
      status = 'Trigger requires a symbol.'
      return
    }
    triggerBusy = true
    status = ''
    try {
      const trials = Math.max(10, Math.min(10_000, parseInt(triggerTrials, 10) || 100))
      const resp = await api.postJson('/api/admin/optimize/trigger', {
        symbol: triggerSym,
        recording_path: triggerPath,
        num_trials: trials,
        loss_fn: triggerLoss,
      })
      const runTrials = resp.trials ?? trials
      status = `Queued hyperopt run for ${triggerSym} (${runTrials} trials) — pending list will update on completion.`
      showTrigger = false
      await refresh()
    } catch (e) {
      status = `Trigger failed: ${e.message}`
    } finally {
      triggerBusy = false
    }
  }

  // Merge suggested + current keys so the diff table has a
  // stable row order and renders an em-dash when the key is
  // missing on one side (e.g. kappa / sigma currently skipped
  // by the apply handler).
  function paramRows(p) {
    const keys = new Set([
      ...Object.keys(p.suggested || {}),
      ...Object.keys(p.current || {}),
    ])
    // Deterministic label order — a random HashMap iteration
    // order makes screenshots jump between loads.
    const order = ['gamma', 'kappa', 'sigma', 'min_spread_bps',
      'order_size', 'num_levels', 'max_distance_bps', 'max_inventory']
    const sorted = [...keys].sort((a, b) => {
      const ai = order.indexOf(a)
      const bi = order.indexOf(b)
      if (ai === -1 && bi === -1) return a.localeCompare(b)
      if (ai === -1) return 1
      if (bi === -1) return -1
      return ai - bi
    })
    return sorted.map((k) => ({
      key: k,
      current: p.current?.[k] ?? null,
      suggested: p.suggested?.[k] ?? null,
    }))
  }

  // Percentage delta current → suggested. Returns null when
  // either side is missing or current is zero (no meaningful
  // relative change). String-decimal inputs from the backend
  // are normalised via `Number(…)`.
  function deltaPct(row) {
    const c = Number(row.current)
    const n = Number(row.suggested)
    if (!Number.isFinite(c) || !Number.isFinite(n)) return null
    if (c === 0) return null
    return ((n - c) / Math.abs(c)) * 100
  }

  function fmtNum(v) {
    if (v === null || v === undefined) return '—'
    const n = Number(v)
    if (!Number.isFinite(n)) return String(v)
    // Integer-valued params render clean, fractional get 4 dp.
    if (Number.isInteger(n)) return n.toString()
    return n.toFixed(4).replace(/0+$/, '').replace(/\.$/, '')
  }

  function fmtLoss(v) {
    const n = Number(v)
    if (!Number.isFinite(n)) return '—'
    return n.toFixed(4)
  }

  function relTime(iso) {
    if (!iso) return ''
    const ms = new Date(iso).getTime()
    if (!Number.isFinite(ms)) return ''
    const deltaSec = Math.max(0, Math.floor((Date.now() - ms) / 1000))
    if (deltaSec < 60) return `${deltaSec}s ago`
    if (deltaSec < 3600) return `${Math.floor(deltaSec / 60)}m ago`
    if (deltaSec < 86400) return `${Math.floor(deltaSec / 3600)}h ago`
    return `${Math.floor(deltaSec / 86400)}d ago`
  }

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
        <span class="chip chip-warn">{pending.length}</span>
      </div>
      <Button variant="primary" onclick={openTriggerForm} disabled={showTrigger}>
          {#snippet children()}<Icon name="bolt" size={12} />
        <span>New run</span>{/snippet}
        </Button>
    </header>

    <div class="list">
      {#each pending as p (p.symbol)}
        {@const rows = paramRows(p)}
        <article class="entry">
          <header class="entry-head">
            <div class="entry-id">
              <span class="sym num">{p.symbol}</span>
              <span class="chip chip-info">{p.loss_fn}</span>
              <span class="meta num">{p.trials} trials</span>
              <span class="meta">{relTime(p.created_at)}</span>
            </div>
            <div class="entry-loss" title="Lowest loss across the trial run (lower = better)">
              <span class="meta">best loss</span>
              <span class="num loss">{fmtLoss(p.best_loss)}</span>
            </div>
          </header>

          <table class="diff">
            <thead>
              <tr>
                <th>Param</th>
                <th class="right">Current</th>
                <th class="right">Suggested</th>
                <th class="right">Δ</th>
              </tr>
            </thead>
            <tbody>
              {#each rows as r (r.key)}
                {@const d = deltaPct(r)}
                <tr>
                  <td class="pk">{r.key}</td>
                  <td class="num-cell right">{fmtNum(r.current)}</td>
                  <td class="num-cell right hi">{fmtNum(r.suggested)}</td>
                  <td class="num-cell right delta" class:pos={d !== null && d > 0.5} class:neg={d !== null && d < -0.5}>
                    {#if d === null}
                      —
                    {:else}
                      {d > 0 ? '+' : ''}{d.toFixed(1)}%
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>

          <div class="actions">
            <Button variant="primary" onclick={() => apply(p.symbol)}
 disabled={busy !== ''}>
          {#snippet children()}{#if busy === p.symbol}
                <span class="spinner"></span>
                <span>Applying…</span>
              {:else}
                <Icon name="check" size={14} />
                <span>Apply</span>
              {/if}{/snippet}
        </Button>
            <Button variant="primary" onclick={() => discard(p.symbol)}
 disabled={busy !== ''}>
          {#snippet children()}<Icon name="close" size={14} />
              <span>Discard</span>{/snippet}
        </Button>
          </div>
        </article>
      {/each}
    </div>
  {/if}

  {#if showTrigger}
    <form class="trigger" onsubmit={(e) => { e.preventDefault(); submitTrigger() }}>
      <div class="trigger-head">
        <span class="label">Run hyperopt</span>
        <Button variant="primary" onclick={() => { showTrigger = false }} aria-label="Close trigger form">
          {#snippet children()}<Icon name="close" size={12} />{/snippet}
        </Button>
      </div>
      <div class="trigger-row">
        <label class="f-label" for="tr-sym">Symbol</label>
        <input
          id="tr-sym"
          type="text"
          class="text-input"
          bind:value={triggerSym}
          placeholder="BTCUSDT"
          disabled={triggerBusy}
        />
      </div>
      <div class="trigger-row">
        <label class="f-label" for="tr-path">Recording path</label>
        <input
          id="tr-path"
          type="text"
          class="text-input"
          bind:value={triggerPath}
          placeholder="data/recorded/BTCUSDT.jsonl"
          disabled={triggerBusy}
        />
      </div>
      <div class="trigger-row">
        <label class="f-label" for="tr-trials">Trials</label>
        <input
          id="tr-trials"
          type="number"
          class="num-input"
          bind:value={triggerTrials}
          min="10"
          max="10000"
          step="10"
          disabled={triggerBusy}
        />
      </div>
      <div class="trigger-row">
        <label class="f-label" for="tr-loss">Loss fn</label>
        <select id="tr-loss" class="select-input" bind:value={triggerLoss} disabled={triggerBusy}>
          <option value="sharpe">sharpe</option>
          <option value="sortino">sortino</option>
          <option value="calmar">calmar</option>
          <option value="maxdd">maxdd</option>
        </select>
      </div>
      <div class="actions">
        <Button variant="primary" type="submit" disabled={triggerBusy || !triggerSym || !triggerPath}>
          {#snippet children()}{#if triggerBusy}
            <span class="spinner"></span>
            <span>Queueing…</span>
          {:else}
            <Icon name="bolt" size={14} />
            <span>Queue run</span>
          {/if}{/snippet}
        </Button>
        <Button variant="primary" onclick={() => { showTrigger = false }} disabled={triggerBusy}>
          {#snippet children()}Cancel{/snippet}
        </Button>
      </div>
    </form>
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

  .entry {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .entry-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
    flex-wrap: wrap;
  }
  .entry-id {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    flex-wrap: wrap;
  }
  .sym {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: var(--tracking-tight);
  }
  .meta {
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .entry-loss {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
  }
  .loss {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-primary);
    font-variant-numeric: tabular-nums;
  }

  .diff {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-xs);
  }
  .diff thead th {
    text-align: left;
    padding: var(--s-1) var(--s-2);
    color: var(--fg-muted);
    font-weight: 500;
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    font-size: var(--fs-2xs);
    border-bottom: 1px solid var(--border-subtle);
  }
  .diff tbody td {
    padding: var(--s-1) var(--s-2);
    border-bottom: 1px solid var(--border-faint);
  }
  .diff tbody tr:last-child td { border-bottom: none; }
  .right { text-align: right; }
  .pk {
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    color: var(--fg-secondary);
  }
  .hi { color: var(--accent); font-weight: 600; }
  .delta.pos { color: var(--pos); }
  .delta.neg { color: var(--neg); }

  .actions {
    display: flex;
    gap: var(--s-2);
    flex-wrap: wrap;
  }

  .trigger {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    padding: var(--s-4);
    background: var(--bg-base);
    border: 1px dashed var(--border-strong);
    border-radius: var(--r-md);
  }
  .trigger-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--s-1);
  }
  .trigger-row {
    display: grid;
    grid-template-columns: 120px 1fr;
    align-items: center;
    gap: var(--s-3);
  }
  .f-label {
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    font-weight: 500;
  }
  .text-input,
  .select-input {
    padding: 6px 10px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
  }
  .text-input:focus,
  .select-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid rgba(0, 0, 0, 0.25);
    border-top-color: #001510;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

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
