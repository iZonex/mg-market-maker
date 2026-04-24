<script>
  /*
   * Epic H — deploy history footer. Collapsible. Shows every
   * recorded deploy with `{ name, hash, operator, deployed_at,
   * scope }`; clicking a row triggers the parent's `onReload`
   * handler so the operator can roll back or branch from an
   * earlier version.
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { auth, onReload, onRollback, onRollbackToDeployment } = $props()
  const api = $derived(createApiClient(auth))

  let entries = $state([])
  let open = $state(false)
  let error = $state('')
  let listed = $state([])
  // Fleet snapshot — refreshed on open so each history row can
  // show a chip "running now on N deployment(s)" when any
  // accepted agent has a deployment with matching active_graph
  // hash. Gives the operator immediate answer to "is anyone
  // still on the old hash I want to roll back to?".
  let fleet = $state([])

  // Map hash → [{agent_id, deployment_id, symbol}, …] derived
  // from fleet rows. Used to annotate history rows.
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
  // UI-5 — when the operator clicks Diff on a history row
  // we fetch both that deploy's graph body and the previous
  // deploy of the SAME name and render a side-by-side view.
  // `null` means no diff is open.
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
    const d = new Date(t)
    return d.toLocaleString()
  }

  // UI-5 — prior deploy for the same `name` strictly older
  // than `current`. Returns the DeployRecord or null if this
  // is the first deploy of that name.
  function priorFor(current) {
    let newest = null
    for (const rec of entries) {
      if (rec.name !== current.name) continue
      if (rec.hash === current.hash && rec.deployed_at === current.deployed_at) continue
      if (new Date(rec.deployed_at) >= new Date(current.deployed_at)) continue
      if (!newest || new Date(rec.deployed_at) > new Date(newest.deployed_at)) {
        newest = rec
      }
    }
    return newest
  }

  async function openDiff(current) {
    diff = null
    diffError = ''
    diffLoading = true
    try {
      const currentBody = await api.getJson(
        `/api/v1/strategy/graphs/${encodeURIComponent(current.name)}/history/${encodeURIComponent(current.hash)}`,
      )
      const prior = priorFor(current)
      let priorBody = null
      if (prior) {
        priorBody = await api.getJson(
          `/api/v1/strategy/graphs/${encodeURIComponent(prior.name)}/history/${encodeURIComponent(prior.hash)}`,
        )
      }
      diff = {
        current,
        prior,
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

  // Simple per-line diff markers: `=` identical, `+` added,
  // `-` removed, `~` changed. Zipped by index so output stays
  // aligned with the side-by-side layout the operator reads
  // left-to-right. Not a true LCS diff — good enough for a
  // quick visual scan and keeps the frontend free of a diff
  // library dependency.
  function diffMarkers(a, b) {
    const la = a.split('\n')
    const lb = b.split('\n')
    const n = Math.max(la.length, lb.length)
    const rows = []
    for (let i = 0; i < n; i++) {
      const left = la[i] ?? ''
      const right = lb[i] ?? ''
      let tag = 'eq'
      if (left && !right) tag = 'del'
      else if (!left && right) tag = 'add'
      else if (left !== right) tag = 'chg'
      rows.push({ tag, left, right })
    }
    return rows
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
                        <span class="running-chip" title={runningOn.map(r => `${r.agent_id}/${r.deployment_id} (${r.symbol})`).join('\n')}>
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
                      >
                        Diff
                      </button>
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

  {#if diffLoading}
    <div class="diff-backdrop">
      <div class="diff-card"><div class="diff-title">loading diff…</div></div>
    </div>
  {:else if diff}
    <div
      class="diff-backdrop"
      role="button"
      tabindex="-1"
      aria-label="Close diff"
      onclick={closeDiff}
      onkeydown={(e) => { if (e.key === 'Escape') closeDiff() }}
    >
      <div
        class="diff-card"
        role="dialog"
        aria-modal="true"
        aria-label="Graph diff"
        tabindex="-1"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
      >
        <div class="diff-head">
          <div class="diff-title">
            <code>{diff.current.name}</code>
            <span class="muted">·</span>
            <span class="small">diff</span>
          </div>
          <button type="button" class="rb-btn" onclick={closeDiff}>Close</button>
        </div>
        {#if diffError}
          <div class="error">{diffError}</div>
        {:else if !diff.prior}
          <div class="muted small">First deploy of this graph — nothing to diff against.</div>
          <pre class="diff-one">{diff.currentJson}</pre>
        {:else}
          <div class="diff-meta">
            <div class="diff-col muted small">
              prev · {diff.prior.hash.slice(0, 12)}…
              <span class="muted">· {fmtTs(diff.prior.deployed_at)}</span>
            </div>
            <div class="diff-col muted small">
              this · {diff.current.hash.slice(0, 12)}…
              <span class="muted">· {fmtTs(diff.current.deployed_at)}</span>
            </div>
          </div>
          <div class="diff-rows">
            {#each diffMarkers(diff.priorJson, diff.currentJson) as row}
              <div class="diff-row diff-{row.tag}">
                <div class="diff-cell"><code>{row.left}</code></div>
                <div class="diff-cell"><code>{row.right}</code></div>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  {/if}
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
  .chips { display: flex; flex-wrap: wrap; gap: var(--s-2); }  table { width: 100%; border-collapse: collapse; }
  th, td { padding: var(--s-2); font-size: var(--fs-xs); text-align: left; border-bottom: 1px solid var(--border-subtle); }
  th { color: var(--fg-muted); font-weight: 500; text-transform: uppercase; letter-spacing: var(--tracking-label); font-size: var(--fs-2xs); }
  .num, .small { font-family: var(--font-mono); font-size: var(--fs-2xs); }  .error { color: var(--neg); font-size: var(--fs-xs); }
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

  .diff-backdrop {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex; align-items: center; justify-content: center;
    z-index: 10;
  }
  .diff-card {
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    width: min(1100px, 92vw);
    max-height: 82vh;
    display: flex; flex-direction: column;
    padding: var(--s-3);
    gap: var(--s-2);
  }
  .diff-head {
    display: flex; justify-content: space-between; align-items: center;
  }
  .diff-title { display: flex; align-items: center; gap: var(--s-2); font-size: var(--fs-xs); }
  .diff-meta { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-2); }
  .diff-col { font-family: var(--font-mono); font-size: var(--fs-2xs); }
  .diff-rows {
    display: flex; flex-direction: column;
    max-height: 60vh; overflow: auto;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    background: var(--bg-chip);
  }
  .diff-row {
    display: grid; grid-template-columns: 1fr 1fr;
    gap: 1px; background: var(--border-subtle);
    font-family: var(--font-mono); font-size: var(--fs-2xs);
  }
  .diff-cell {
    padding: 2px var(--s-2);
    background: var(--bg-raised);
    white-space: pre;
  }
  .diff-row.diff-eq .diff-cell { opacity: 0.6; }
  .diff-row.diff-add .diff-cell:last-child { background: rgba(52, 211, 153, 0.18); }
  .diff-row.diff-del .diff-cell:first-child { background: rgba(248, 113, 113, 0.2); }
  .diff-row.diff-chg .diff-cell { background: rgba(251, 191, 36, 0.15); }
  .diff-one {
    max-height: 60vh; overflow: auto;
    padding: var(--s-2);
    background: var(--bg-chip);
    border-radius: var(--r-sm);
    font-family: var(--font-mono); font-size: var(--fs-2xs);
  }
  .small { font-size: var(--fs-2xs); color: var(--fg-muted); }
</style>
