<script>
  /*
   * Epic H — deploy history footer. Collapsible. Shows every
   * recorded deploy with `{ name, hash, operator, deployed_at,
   * scope }`; clicking a row triggers the parent's `onReload`
   * handler so the operator can roll back or branch from an
   * earlier version.
   *
   * The side-by-side diff modal + its helpers live in
   * ./deploy-history/*.
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'
  import DeployDiffModal from './deploy-history/DeployDiffModal.svelte'
  import { priorFor } from './deploy-history/deploy-diff-utils.js'

  let { auth, onReload, onRollback, onRollbackToDeployment } = $props()
  const api = $derived(createApiClient(auth))

  let entries = $state([])
  let open = $state(false)
  let error = $state('')
  let listed = $state([])
  // Fleet snapshot refreshed on open so each history row can show
  // "running now on N deployment(s)" when any accepted agent has
  // a deployment with matching active_graph hash. Answers the
  // operator's question: is anyone still on the old hash I want
  // to roll back to?
  let fleet = $state([])

  const runningByHash = $derived.by(() => {
    const out = new Map()
    for (const a of fleet || []) {
      for (const d of a.deployments || []) {
        const h = d.active_graph?.hash
        if (!h) continue
        if (!out.has(h)) out.set(h, [])
        out.get(h).push({
          agent_id: a.agent_id,
          deployment_id: d.deployment_id,
          symbol: d.symbol,
        })
      }
    }
    return out
  })

  // UI-5 — diff state. Parent loads both graphs and feeds the
  // shape the DeployDiffModal expects.
  let diff = $state(null)
  let diffError = $state('')
  let diffLoading = $state(false)

  async function refresh() {
    try {
      entries = await api.getJson('/api/v1/strategy/deploys')
      listed = await api.getJson('/api/v1/strategy/graphs')
      fleet = await api.getJson('/api/v1/fleet').catch(() => [])
      error = ''
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => { if (open) refresh() })

  function fmtTs(t) {
    if (!t) return '—'
    return new Date(t).toLocaleString()
  }

  async function openDiff(current) {
    diff = null
    diffError = ''
    diffLoading = true
    try {
      const currentBody = await api.getJson(
        `/api/v1/strategy/graphs/${encodeURIComponent(current.name)}/history/${encodeURIComponent(current.hash)}`,
      )
      const prior = priorFor(current, entries)
      let priorBody = null
      if (prior) {
        priorBody = await api.getJson(
          `/api/v1/strategy/graphs/${encodeURIComponent(prior.name)}/history/${encodeURIComponent(prior.hash)}`,
        )
      }
      diff = {
        current, prior,
        currentJson: JSON.stringify(currentBody, null, 2),
        priorJson: priorBody === null ? '' : JSON.stringify(priorBody, null, 2),
      }
    } catch (e) {
      diffError = String(e)
    } finally {
      diffLoading = false
    }
  }

  function closeDiff() {
    diff = null
    diffError = ''
  }
</script>

<div class="history">
  <button type="button" class="toggle" onclick={() => { open = !open; if (open) refresh() }}>
    <Icon name={open ? 'chevronDown' : 'chevronUp'} size={12} />
    <span>Deploy history</span>
    <span class="count">{entries.length}</span>
  </button>
  {#if open}
    <div class="panel">
      {#if error}
        <div class="error">{error}</div>
      {:else}
        <div class="section">
          <div class="section-title">Saved graphs</div>
          <div class="chips">
            {#each listed as name (name)}
              <button type="button" class="chip" onclick={() => onReload?.(name)}>{name}</button>
            {/each}
            {#if listed.length === 0}
              <span class="muted">none yet</span>
            {/if}
          </div>
        </div>
        <div class="section">
          <div class="section-title">Deploys</div>
          {#if entries.length === 0}
            <span class="muted">no deploys recorded</span>
          {:else}
            <table>
              <thead>
                <tr><th>When</th><th>Name</th><th>Hash</th><th>Operator</th><th>Scope</th><th></th></tr>
              </thead>
              <tbody>
                {#each entries.slice().reverse() as rec (rec.hash + rec.deployed_at)}
                  {@const runningOn = runningByHash.get(rec.hash) || []}
                  <tr>
                    <td class="num">{fmtTs(rec.deployed_at)}</td>
                    <td><code>{rec.name}</code></td>
                    <td class="num">
                      {rec.hash.slice(0, 12)}…
                      {#if runningOn.length > 0}
                        <span class="running-chip" title={runningOn.map((r) => `${r.agent_id}/${r.deployment_id} (${r.symbol})`).join('\n')}>
                          running × {runningOn.length}
                        </span>
                      {/if}
                    </td>
                    <td>{rec.operator}</td>
                    <td><code class="small">{rec.scope}</code></td>
                    <td class="actions">
                      <button
                        type="button"
                        class="rb-btn"
                        onclick={() => openDiff(rec)}
                        title="Side-by-side diff against the previous deploy of this graph"
                      >Diff</button>
                      <button type="button" class="rb-btn" onclick={() => onRollback?.(rec.name, rec.hash)} title="Load this version onto canvas, then pick targets">
                        Load
                      </button>
                      {#if onRollbackToDeployment}
                        <button
                          type="button"
                          class="rb-btn primary"
                          onclick={() => onRollbackToDeployment(rec.name, rec.hash)}
                          title="Load this version and open the deploy modal in one click"
                        >
                          Rollback
                        </button>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </div>
      {/if}
    </div>
  {/if}

  <DeployDiffModal
    state={diff}
    loading={diffLoading}
    error={diffError}
    onClose={closeDiff}
  />
</div>

<style>
  .history { border-top: 1px solid var(--border-subtle); background: var(--bg-raised); }
  .toggle {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-4);
    width: 100%;
    background: transparent; border: none; cursor: pointer; color: var(--fg-primary);
    font-size: var(--fs-xs); text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .toggle:hover { background: var(--bg-chip); }
  .count {
    margin-left: auto;
    font-family: var(--font-mono); font-size: var(--fs-2xs); color: var(--fg-muted);
  }
  .panel { padding: var(--s-3) var(--s-4); display: flex; flex-direction: column; gap: var(--s-3); max-height: 280px; overflow-y: auto; }
  .section { display: flex; flex-direction: column; gap: var(--s-2); }
  .section-title { font-size: var(--fs-2xs); color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .chips { display: flex; flex-wrap: wrap; gap: var(--s-2); }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: var(--s-2); font-size: var(--fs-xs); text-align: left; border-bottom: 1px solid var(--border-subtle); }
  th { color: var(--fg-muted); font-weight: 500; text-transform: uppercase; letter-spacing: var(--tracking-label); font-size: var(--fs-2xs); }
  .num, .small { font-family: var(--font-mono); font-size: var(--fs-2xs); }
  .error { color: var(--neg); font-size: var(--fs-xs); }
  .rb-btn {
    padding: 2px var(--s-2);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-secondary);
    font-size: var(--fs-2xs); cursor: pointer;
  }
  .rb-btn:hover { border-color: var(--warn); color: var(--warn); }
  .rb-btn.primary { border-color: var(--accent); color: var(--accent); }
  .rb-btn.primary:hover { background: color-mix(in srgb, var(--accent) 15%, transparent); }
  .actions { display: flex; gap: var(--s-2); }
  .running-chip {
    display: inline-block;
    padding: 1px 6px; margin-left: 6px;
    font-size: 10px; font-family: var(--font-mono);
    background: color-mix(in srgb, var(--ok) 15%, transparent);
    color: var(--ok);
    border-radius: var(--r-sm);
  }
</style>
