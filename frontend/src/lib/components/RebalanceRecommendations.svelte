<script>
  /*
   * S5.1 — Rebalance recommendations panel.
   *
   * Polls /api/v1/rebalance/recommendations. Dashboard groups
   * VenueBalanceSnapshot rows by (venue, asset), runs the
   * rebalancer against the configured thresholds, and returns
   * advisory transfer rows. Empty result means everything is
   * balanced OR the [rebalancer] config section is absent.
   */
  import { createApiClient } from '../api.svelte.js'
  import { Modal, Button } from '../primitives/index.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 15_000

  let recs = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)

  async function refresh() {
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const fetches = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if (!d.running) continue
          const path = `/api/v1/agents/${encodeURIComponent(a.agent_id)}`
            + `/deployments/${encodeURIComponent(d.deployment_id)}`
            + `/details/rebalance_recommendations`
          fetches.push(
            api.getJson(path)
              .then(resp => resp.payload?.recommendations || [])
              .catch(() => []),
          )
        }
      }
      const all = (await Promise.all(fetches)).flat()
      // Rebalance advisories are portfolio-wide so multiple
      // deployments may emit the same advisory. Dedup by
      // (from, to, asset) and keep the largest qty — operator
      // sees the biggest recommended transfer once.
      const byKey = new Map()
      for (const r of all) {
        const key = `${r.from_venue}>${r.to_venue}:${r.asset}`
        const prev = byKey.get(key)
        if (!prev || Number(r.qty) > Number(prev.qty)) {
          byKey.set(key, r)
        }
      }
      recs = Array.from(byKey.values())
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

  // S6.4 — Execute flow: modal confirm → POST /api/v1/rebalance/execute.
  let pending = $state(null)     // the rec currently awaiting confirm
  let submitting = $state(false)
  let execResult = $state(null)  // { status, venue_tx_id?, error? }

  function openExecute(rec) {
    pending = rec
    execResult = null
  }
  function cancelExecute() {
    if (submitting) return
    pending = null
  }
  async function confirmExecute() {
    if (!pending || submitting) return
    submitting = true
    try {
      const body = {
        from_venue: pending.from_venue,
        to_venue: pending.to_venue,
        asset: pending.asset,
        qty: pending.qty.toString(),
        reason: pending.reason,
      }
      const data = await api.postJson('/api/v1/rebalance/execute', body)
      execResult = data
    } catch (e) {
      execResult = { status: 'error', error: e?.message || String(e) }
    } finally {
      submitting = false
      await refresh()
    }
  }
</script>

<div class="rebal">
  <div class="toolbar">
    <div class="title">Rebalance recommendations</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{recs.length} advisory · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && recs.length === 0}
    <div class="empty">balances within thresholds — no transfers recommended</div>
  {:else}
    <div class="rows">
      {#each recs as r, i (i)}
        <div class="rec">
          <div class="head">
            <span class="asset mono">{r.asset}</span>
            <span class="qty mono">{r.qty}</span>
            <span class="route mono">{r.from_venue}<span class="arrow"> → </span>{r.to_venue}</span>
            <button class="exec" onclick={() => openExecute(r)}>Execute</button>
          </div>
          <div class="reason">{r.reason}</div>
        </div>
      {/each}
    </div>
  {/if}

  <Modal
    open={!!pending}
    ariaLabel="Confirm transfer"
    maxWidth="480px"
    onClose={cancelExecute}
  >
    {#snippet children()}
      {#if pending}
        <h3>Confirm transfer</h3>
        <div class="kv">
          <span class="k">Asset</span><span class="v mono">{pending.asset}</span>
          <span class="k">Qty</span><span class="v mono">{pending.qty}</span>
          <span class="k">From</span><span class="v mono">{pending.from_venue}</span>
          <span class="k">To</span><span class="v mono">{pending.to_venue}</span>
          <span class="k">Reason</span><span class="v">{pending.reason}</span>
        </div>
        {#if pending.from_venue !== pending.to_venue}
          <div class="warning">
            Cross-venue transfer — the decision will be logged but NOT dispatched. Complete the move manually on the venue UI.
          </div>
        {/if}
        {#if execResult}
          <div class="result" class:ok={execResult.status === 'executed' || execResult.status === 'accepted'}
               class:err={execResult.status === 'failed' || execResult.status === 'rejected_kill_switch' || execResult.status === 'error'}>
            <div class="status mono">status: {execResult.status}</div>
            {#if execResult.venue_tx_id}
              <div class="tx mono">venue tx: {execResult.venue_tx_id}</div>
            {/if}
            {#if execResult.error}
              <div class="err-text">{execResult.error}</div>
            {/if}
          </div>
        {/if}
      {/if}
    {/snippet}
    {#snippet actions()}
      <Button variant="ghost" onclick={cancelExecute} disabled={submitting}>
        {#snippet children()}{execResult ? 'Close' : 'Cancel'}{/snippet}
      </Button>
      {#if !execResult}
        <Button variant="primary" onclick={confirmExecute} loading={submitting}>
          {#snippet children()}Confirm{/snippet}
        </Button>
      {/if}
    {/snippet}
  </Modal>
</div>

<style>
  .rebal { display: flex; flex-direction: column; gap: var(--s-3); }
  .toolbar {
    display: flex; align-items: center; justify-content: space-between;
    padding: 0 var(--s-2); font-size: var(--fs-xs);
  }
  .title { font-weight: 600; color: var(--fg-primary); }
  .meta { color: var(--fg-muted); display: flex; align-items: center; gap: var(--s-2); }
  .meta .error { color: var(--danger); }
  .empty {
    color: var(--fg-muted); font-size: var(--fs-xs);
    padding: var(--s-4); text-align: center;
  }
  .spinner {
    display: inline-block; width: 10px; height: 10px;
    border: 2px solid var(--border-subtle);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    margin-right: var(--s-1);
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .rows { display: flex; flex-direction: column; gap: var(--s-2); }
  .rec {
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
    display: flex; flex-direction: column; gap: var(--s-1);
  }
  .head {
    display: grid;
    grid-template-columns: 70px 1fr 1.5fr 70px;
    gap: var(--s-2);
    align-items: center;
    font-size: var(--fs-xs);
  }
  .exec {
    justify-self: end;
    padding: 3px 10px;
    font-size: 10px;
    background: var(--accent-dim);
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: var(--r-md);
    cursor: pointer;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    transition: background var(--dur-fast) var(--ease-out);
  }
  .exec:hover { background: var(--accent); color: var(--bg-primary); }

  /* `.modal-backdrop` + `.modal` moved to primitives/Modal.svelte —
     design system v1. */
  h3 { margin: 0; color: var(--fg-primary); font-size: var(--fs-md); }
  .kv {
    display: grid;
    grid-template-columns: 70px 1fr;
    gap: var(--s-2);
    font-size: var(--fs-xs);
  }
  .k { color: var(--fg-muted); }
  .v { color: var(--fg-primary); }
  .warning {
    padding: var(--s-2);
    background: var(--warn-dim, rgba(255,180,0,0.1));
    color: var(--warn);
    font-size: var(--fs-xs);
    border-radius: var(--r-md);
  }
  .result {
    padding: var(--s-2);
    border-radius: var(--r-md);
    font-size: var(--fs-xs);
    display: flex; flex-direction: column; gap: 2px;
  }
  .result.ok { background: var(--accent-dim); color: var(--accent); }
  .result.err { background: var(--danger-dim, rgba(255,80,80,0.1)); color: var(--danger); }
  .status { font-weight: 700; text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .tx, .err-text { font-size: 10px; }
  /* `.actions` button styling moved to primitives/Button.svelte. */
  .asset { color: var(--fg-primary); font-weight: 600; text-transform: uppercase; }
  .qty { color: var(--fg-secondary); text-align: right; font-variant-numeric: tabular-nums; }
  .route { color: var(--fg-primary); text-transform: uppercase; text-align: right; }
  .arrow { color: var(--accent); font-weight: 700; }
  .reason { color: var(--fg-muted); font-size: var(--fs-2xs); }
  .mono { font-family: var(--font-mono); }
</style>
