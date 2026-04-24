<script>
  /*
   * DeployDialog — modal that pushes a strategy to an agent.
   *
   * Sources:
   *   - `/api/v1/templates` — catalog of known strategy templates
   *     with a variables-hint starter JSON.
   *   - `/api/v1/agents/{id}/credentials` — credentials this
   *     specific agent is authorised to receive (filtered by
   *     each credential's `allowed_agents` whitelist).
   *
   * POSTs to `/api/v1/agents/{id}/deployments`. Controller
   * pre-validates every referenced credential_id against the
   * store + authz whitelist and rejects with 412 on mismatch;
   * we surface that error inline so the operator fixes the bind.
   */
  import Icon from './Icon.svelte'
  import { createApiClient } from '../api.svelte.js'
  import { Button, Modal } from '../primitives/index.js'

  let { auth, agent, agents = null, onClose = () => {}, onDeployed = () => {} } = $props()
  const api = $derived(createApiClient(auth))

  // Wave C3 — batch mode. `agents` (optional, non-empty) replaces
  // `agent`: the same template + variables fan out to every
  // listed agent serially. Credentials dropdown sources from the
  // first agent; operator is expected to pick IDs that exist on
  // every target (shared infra) or to run batch only for
  // same-tenant agents with mirrored vaults.
  const batchMode = $derived(agents != null && Array.isArray(agents) && agents.length > 1)
  const targetAgents = $derived(batchMode ? agents : [agent])
  const primaryAgent = $derived(batchMode ? agents[0] : agent)
  let batchResults = $state([])  // [{ agent_id, phase: 'pending'|'ok'|'err', detail }]

  let templates = $state([])
  let creds = $state([])
  let loading = $state(true)
  let error = $state(null)
  let submitting = $state(false)

  // Deploy mode — "template" uses the catalog row's variables
  // starter JSON; "graph" embeds an operator-authored strategy
  // graph into `variables.strategy_graph`, letting the agent
  // fire `StrategyGraphSwap` on engine start. Both flows hit
  // the same POST — just different variable shape.
  let mode = $state('template')  // 'template' | 'graph'

  let form = $state({
    deployment_id: '',
    template: '',
    symbol: '',
    selected_credentials: new Set(),
    variables_text: '{}',
    graph_text: '',
    graph_filename: '',
  })

  $effect(() => {
    ;(async () => {
      try {
        const [tpls, cs] = await Promise.all([
          api.getJson('/api/v1/templates'),
          api.getJson(`/api/v1/agents/${encodeURIComponent(primaryAgent.agent_id)}/credentials`),
        ])
        templates = Array.isArray(tpls) ? tpls : []
        creds = Array.isArray(cs) ? cs : []
      } catch (e) {
        error = e?.message || String(e)
      } finally {
        loading = false
      }
    })()
  })

  // When the operator picks a template, pre-fill the variables
  // editor with the catalog's starter JSON — gives them the
  // right shape to tweak.
  function pickTemplate(name) {
    form.template = name
    const t = templates.find(x => x.name === name)
    if (t) {
      form.variables_text = JSON.stringify(t.variables_hint, null, 2)
    }
  }

  function toggleCred(id) {
    const next = new Set(form.selected_credentials)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    form.selected_credentials = next
  }

  function parseVariables() {
    const txt = form.variables_text.trim()
    if (!txt) return {}
    return JSON.parse(txt)
  }

  async function deploy() {
    error = null
    if (!form.deployment_id.trim()) {
      error = 'Deployment id is required'
      return
    }
    if (mode === 'template' && !form.template) {
      error = 'Pick a template'
      return
    }
    if (!form.symbol.trim()) {
      error = 'Symbol is required'
      return
    }
    if (form.selected_credentials.size === 0) {
      error = 'At least one credential must be selected'
      return
    }
    if (tenantBlock) {
      error = `Tenant isolation: ${tenantBlock}`
      return
    }
    let variables
    if (mode === 'template') {
      try {
        variables = parseVariables()
      } catch (e) {
        error = `variables JSON: ${e.message}`
        return
      }
    } else {
      // Graph mode — validate the JSON body shape, then embed
      // as a string in `variables.strategy_graph`. Agent's
      // config-override plumbing picks up the key and swaps
      // the engine onto the custom graph on start.
      const raw = form.graph_text.trim()
      if (!raw) {
        error = 'Paste a strategy graph JSON or load one from file'
        return
      }
      let parsed
      try {
        parsed = JSON.parse(raw)
      } catch (e) {
        error = `graph JSON: ${e.message}`
        return
      }
      if (!parsed || typeof parsed !== 'object' || !Array.isArray(parsed.nodes)) {
        error = 'graph JSON must be an object with a `nodes` array'
        return
      }
      variables = { strategy_graph: raw }
    }
    submitting = true
    const newStrategy = {
      deployment_id: form.deployment_id.trim(),
      // In graph mode the template is a thin runner (any
      // `-via-graph` row works); we pick the first one as
      // a carrier so the backend has a valid catalog name.
      template: mode === 'template' ? form.template : (form.template || 'avellaneda-via-graph'),
      symbol: form.symbol.trim().toUpperCase(),
      credentials: Array.from(form.selected_credentials),
      variables,
    }
    // UI-DEPLOY-1 (2026-04-22) — `SetDesiredStrategies` is
    // replace-by-set on the agent. Sending just the new
    // strategy would stop every sibling deployment silently.
    // Fetch the current desired set, replace the matching
    // deployment_id (operator might be editing), append when
    // it's a fresh add, then POST the union. DeploymentStateRow
    // echoes `credentials` back specifically so this merge
    // path works. In batch mode each agent's existing set is
    // fetched + merged independently.
    async function mergedBodyFor(agentId) {
      let existing = []
      try {
        existing = await api.getJson(
          `/api/v1/agents/${encodeURIComponent(agentId)}/deployments`,
        )
        if (!Array.isArray(existing)) existing = []
      } catch (_) {
        // Fall back to "replace all" if GET fails; this is
        // the legacy behaviour, a regression vs pre-fix is
        // better than a confusing deploy refusal.
        existing = []
      }
      const merged = []
      for (const d of existing) {
        if (d.deployment_id === newStrategy.deployment_id) continue
        merged.push({
          deployment_id: d.deployment_id,
          template: d.template || '',
          symbol: d.symbol,
          credentials: Array.isArray(d.credentials) ? d.credentials : [],
          variables: d.variables || {},
        })
      }
      merged.push(newStrategy)
      return { strategies: merged }
    }
    if (batchMode) {
      batchResults = targetAgents.map(a => ({ agent_id: a.agent_id, phase: 'pending', detail: '' }))
      const settled = await Promise.all(targetAgents.map(async (a) => {
        try {
          const body = await mergedBodyFor(a.agent_id)
          const r = await api.authedFetch(`/api/v1/agents/${encodeURIComponent(a.agent_id)}/deployments`, {
            method: 'POST',
            body: JSON.stringify(body),
          })
          if (!r.ok) {
            const t = await r.text().catch(() => '')
            return { agent_id: a.agent_id, phase: 'err', detail: t || r.statusText }
          }
          return { agent_id: a.agent_id, phase: 'ok', detail: '' }
        } catch (e) {
          return { agent_id: a.agent_id, phase: 'err', detail: e.message || String(e) }
        }
      }))
      batchResults = settled
      submitting = false
      const okCount = settled.filter(s => s.phase === 'ok').length
      if (okCount > 0) onDeployed()
      // Keep dialog open so operator sees per-agent results
      // and can adjust + retry the failures.
      return
    }
    try {
      const body = await mergedBodyFor(primaryAgent.agent_id)
      const r = await api.authedFetch(`/api/v1/agents/${encodeURIComponent(primaryAgent.agent_id)}/deployments`, {
        method: 'POST',
        body: JSON.stringify(body),
      })
      if (!r.ok) {
        const t = await r.text().catch(() => '')
        throw new Error(t || r.statusText)
      }
      onDeployed()
      onClose()
    } catch (e) {
      error = e.message || 'Deploy failed'
    } finally {
      submitting = false
    }
  }

  async function loadGraphFromFile(event) {
    const file = event.target.files?.[0]
    if (!file) return
    const text = await file.text()
    try {
      JSON.parse(text)
    } catch (e) {
      error = `graph file is not valid JSON: ${e.message}`
      return
    }
    form.graph_text = text
    form.graph_filename = file.name
    error = null
  }

  function handleBackdrop(e) {
    if (e.target === e.currentTarget) onClose()
  }

  // Group templates by category for a slightly saner dropdown.
  const grouped = $derived.by(() => {
    const by = new Map()
    for (const t of templates) {
      const k = t.category || 'other'
      if (!by.has(k)) by.set(k, [])
      by.get(k).push(t)
    }
    return Array.from(by.entries()).map(([cat, items]) => ({ category: cat, items }))
  })

  const currentTemplate = $derived(templates.find(t => t.name === form.template))

  // Wave 2b — agent's tenant, surfaced from the FleetPage's
  // stitched row. null / empty = untagged agent ("shared infra");
  // a concrete client_id means every credential the operator
  // picks must either be tenant-less (shared credential) or
  // match this string exactly.
  const agentTenant = $derived(agent?.profile?.client_id || '')

  /**
   * Pre-flight tenant check over the currently-selected credentials.
   * Mirrors the controller's `pre_validate_deploy` gate so the
   * operator sees the conflict inline instead of via a 412
   * after clicking Deploy.
   *
   * Returns `null` when selection is fine, otherwise a string
   * describing the specific conflict.
   */
  const tenantBlock = $derived.by(() => {
    const selected = creds.filter(c => form.selected_credentials.has(c.id))
    const tenanted = selected
      .map(c => c.client_id)
      .filter(t => t && t.length > 0)
    if (tenanted.length === 0) return null
    // Cross-tenant mix within the same deployment.
    const first = tenanted[0]
    const mismatch = tenanted.find(t => t !== first)
    if (mismatch) {
      return `selected credentials mix two tenants (${first} and ${mismatch})`
    }
    // Credential tenant ≠ agent tenant.
    if (agentTenant && first !== agentTenant) {
      return `credential tenant '${first}' does not match agent tenant '${agentTenant}'`
    }
    if (!agentTenant && first) {
      return `credential tenant '${first}' picked, but agent has no tenant (profile.client_id is empty — set it in Fleet → Edit)`
    }
    return null
  })
</script>

<Modal
  open={true}
  ariaLabel="Deploy strategy"
  maxWidth="720px"
  {onClose}
>
  {#snippet children()}
    <header class="modal-head">
      <div class="head-text">
        <div class="title">
          {batchMode ? `Deploy to ${targetAgents.length} agents` : 'Deploy strategy'}
        </div>
        {#if batchMode}
          <div class="sub">
            batch: {targetAgents.map(a => a.agent_id).join(', ')}
          </div>
          <div class="sub batch-warn">
            Credentials dropdown sourced from <code class="mono">{primaryAgent.agent_id}</code> —
            pick IDs that are shared across every target (same-tenant + mirrored vault).
          </div>
        {:else}
          <div class="sub">on agent <code class="mono">{primaryAgent.agent_id}</code> · fingerprint <code class="mono">{primaryAgent.fingerprint}</code></div>
        {/if}
      </div>
      <Button variant="ghost" size="sm" iconOnly onclick={onClose} aria-label="Close">
        {#snippet children()}<Icon name="close" size={14} />{/snippet}
      </Button>
    </header>

    <div class="body">
      {#if loading}
        <div class="muted">Loading templates and credentials…</div>
      {:else}
        <div class="section">
          <div class="section-head">Identity</div>
          <div class="grid-2">
            <div class="field">
              <label for="dep-id">Deployment ID</label>
              <input id="dep-id" type="text" bind:value={form.deployment_id} disabled={submitting} placeholder="btc-maker-eu" />
            </div>
            <div class="field">
              <label for="dep-symbol">Symbol</label>
              <input id="dep-symbol" type="text" bind:value={form.symbol} disabled={submitting} placeholder="BTCUSDT" />
            </div>
          </div>
        </div>

        <div class="section">
          <div class="mode-tabs" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={mode === 'template'}
              class="mode-tab"
              class:active={mode === 'template'}
              onclick={() => (mode = 'template')}
              disabled={submitting}
            >Template</button>
            <button
              type="button"
              role="tab"
              aria-selected={mode === 'graph'}
              class="mode-tab"
              class:active={mode === 'graph'}
              onclick={() => (mode = 'graph')}
              disabled={submitting}
            >Custom graph</button>
          </div>
          {#if mode === 'template'}
            <div class="section-head">Template</div>
            <div class="templates">
              {#each grouped as g (g.category)}
                <div class="template-group">
                  <div class="group-label">{g.category}</div>
                  <div class="group-items">
                    {#each g.items as t (t.name)}
                      <button
                        type="button"
                        class="template-card"
                        class:selected={form.template === t.name}
                        onclick={() => pickTemplate(t.name)}
                        disabled={submitting}
                      >
                        <div class="t-head">
                          <span class="t-name mono">{t.name}</span>
                          {#if t.risk_band}
                            <span class="risk-chip risk-{t.risk_band}">{t.risk_band}</span>
                          {/if}
                        </div>
                        <span class="t-desc">{t.description}</span>
                        {#if t.recommended_for}
                          <div class="t-tip">
                            <span class="tip-k">for</span>
                            <span class="tip-v">{t.recommended_for}</span>
                          </div>
                        {/if}
                        {#if t.caveats}
                          <div class="t-tip caveat">
                            <span class="tip-k">⚠</span>
                            <span class="tip-v">{t.caveats}</span>
                          </div>
                        {/if}
                      </button>
                    {/each}
                  </div>
                </div>
              {/each}
            </div>
          {:else}
            <div class="section-head">
              Custom graph
              <span class="hint">paste a graph JSON or load from file — applied via <code>variables.strategy_graph</code></span>
            </div>
            <div class="graph-actions">
              <label class="file-btn">
                <input type="file" accept=".json,application/json" onchange={loadGraphFromFile} disabled={submitting} />
                Load JSON file…
              </label>
              {#if form.graph_filename}
                <span class="graph-file mono">{form.graph_filename}</span>
              {/if}
            </div>
            <textarea
              class="vars graph-text"
              rows="12"
              spellcheck="false"
              bind:value={form.graph_text}
              placeholder={`{\n  "nodes": [ ... ],\n  "edges": [ ... ]\n}`}
              disabled={submitting}
            ></textarea>
            <div class="tpl-note">
              <Icon name="info" size={12} />
              <span>Author the graph visually on Strategy page, use the Export button, then paste here (or load the exported file).</span>
            </div>
          {/if}
        </div>

        <div class="section">
          <div class="section-head">
            Credentials
            <span class="hint">pick every credential this deployment may touch</span>
          </div>
          {#if creds.length === 0}
            <div class="warn-banner">
              <Icon name="alert" size={12} />
              <span>No credentials authorised for this agent. Add one in <strong>Admin → Credentials</strong> (or widen <code>allowed_agents</code> on an existing one) before deploying.</span>
            </div>
          {:else}
            <div class="tenant-line">
              {#if agentTenant}
                Agent tenant: <code class="mono">{agentTenant}</code> · credentials must match (or be shared).
              {:else}
                Agent is untagged — only shared credentials (no <code>client_id</code>) will pass the tenant gate.
              {/if}
            </div>
            <div class="cred-picker">
              {#each creds as c (c.id)}
                {@const tenantBad = (agentTenant && c.client_id && c.client_id !== agentTenant)
                  || (!agentTenant && c.client_id)}
                <label
                  class="cred-option"
                  class:selected={form.selected_credentials.has(c.id)}
                  class:tenant-bad={tenantBad}
                  title={tenantBad ? `tenant '${c.client_id}' does not match agent tenant '${agentTenant || '(none)'}'` : ''}
                >
                  <input
                    type="checkbox"
                    checked={form.selected_credentials.has(c.id)}
                    disabled={submitting}
                    onchange={() => toggleCred(c.id)}
                  />
                  <span class="c-id mono">{c.id}</span>
                  <span class="c-meta">
                    <span class="c-venue">{c.exchange} · {c.product}</span>
                    {#if c.client_id}
                      <span class="c-tenant" class:c-tenant-bad={tenantBad}>{c.client_id}</span>
                    {:else}
                      <span class="c-tenant c-tenant-shared">shared</span>
                    {/if}
                  </span>
                </label>
              {/each}
            </div>
            {#if tenantBlock}
              <div class="warn-banner" role="alert">
                <Icon name="alert" size={12} />
                <span>Tenant isolation: {tenantBlock}</span>
              </div>
            {/if}
          {/if}
        </div>

        {#if mode === 'template'}
          <div class="section">
            <div class="section-head">
              Variables (JSON)
              <span class="hint">template-specific · pre-filled from the template hint</span>
            </div>
            <textarea
              class="vars"
              rows="10"
              spellcheck="false"
              bind:value={form.variables_text}
              disabled={submitting}
            ></textarea>
            {#if currentTemplate}
              <div class="tpl-note">
                <Icon name="info" size={12} />
                <span>{currentTemplate.description}</span>
              </div>
            {/if}
          </div>
        {/if}

        {#if batchResults.length > 0}
          <div class="section">
            <div class="section-head">Batch dispatch results</div>
            <div class="batch-results">
              {#each batchResults as res (res.agent_id)}
                <div class="batch-row">
                  <span class="mono">{res.agent_id}</span>
                  {#if res.phase === 'pending'}
                    <span class="res-pending">dispatching…</span>
                  {:else if res.phase === 'ok'}
                    <span class="res-ok">✓ deployed</span>
                  {:else}
                    <span class="res-err">✗ {res.detail}</span>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
        {/if}

        {#if error}
          <div class="error">
            <Icon name="alert" size={12} />
            <span>{error}</span>
          </div>
        {/if}
      {/if}
    </div>

  {/snippet}
  {#snippet actions()}
    <Button variant="ghost" onclick={onClose} disabled={submitting}>
      {#snippet children()}Cancel{/snippet}
    </Button>
    <Button variant="primary" onclick={deploy} loading={submitting} disabled={loading || !!tenantBlock}>
      {#snippet children()}Deploy strategy{/snippet}
    </Button>
  {/snippet}
</Modal>

<style>
  /* `.backdrop`, `.modal`, `.close` moved to primitives/Modal.svelte
     + Button.svelte — design system v1. */
  .modal-head {
    display: flex; align-items: flex-start; justify-content: space-between;
    padding-bottom: var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
  }
  .head-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .title {
    font-size: var(--fs-lg); font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: var(--tracking-tight);
  }
  .sub { font-size: var(--fs-xs); color: var(--fg-muted); }

  .body {
    display: flex; flex-direction: column; gap: var(--s-5);
    padding-top: var(--s-3);
  }
  .section { display: flex; flex-direction: column; gap: var(--s-3); }
  .section-head {
    display: flex; align-items: baseline; gap: var(--s-2);
    font-size: 11px; font-weight: 600;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .hint { font-weight: 400; font-size: 10px; color: var(--fg-faint); text-transform: none; letter-spacing: normal; }

  .grid-2 { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-3); }
  @media (max-width: 520px) { .grid-2 { grid-template-columns: 1fr; } }
  .field { display: flex; flex-direction: column; gap: 6px; }
  .field label {
    font-size: 11px; color: var(--fg-muted); letter-spacing: 0.02em;
  }
  .field input, .vars {
    padding: 9px 12px;
    background: rgba(10, 14, 20, 0.5);
    color: var(--fg-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--r-md);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    outline: none;
  }
  .field input:focus, .vars:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-ring);
  }
  .vars { width: 100%; resize: vertical; font-size: var(--fs-xs); line-height: 1.5; }
  .graph-text { font-family: var(--font-mono); min-height: 220px; }

  .mode-tabs {
    display: inline-flex; gap: 0;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    padding: 2px;
    margin-bottom: var(--s-3);
  }
  .mode-tab {
    padding: 4px 12px;
    font-size: var(--fs-xs); font-family: inherit;
    background: transparent; border: 0;
    color: var(--fg-muted); cursor: pointer;
    border-radius: var(--r-sm);
  }
  .mode-tab.active {
    background: var(--bg-raised); color: var(--fg-primary);
    box-shadow: 0 0 0 1px var(--border-subtle);
  }
  .mode-tab:disabled { opacity: 0.5; cursor: not-allowed; }

  .graph-actions { display: flex; align-items: center; gap: var(--s-3); margin-bottom: var(--s-2); }
  /* Ghost-button-styled file picker. We don't use <Button> here
     because file-input needs a `<label>` parent for accessibility. */
  .file-btn {
    display: inline-flex; align-items: center; gap: var(--s-2);
    padding: 4px 10px;
    font-size: var(--fs-xs);
    border-radius: var(--r-sm);
    border: 1px solid var(--border-subtle);
    color: var(--fg-primary);
    background: transparent;
    cursor: pointer;
    position: relative;
    overflow: hidden;
  }
  .file-btn:hover { background: var(--bg-chip-hover); border-color: var(--border-default); }
  .file-btn input[type="file"] {
    position: absolute; inset: 0; opacity: 0; cursor: pointer;
  }
  .graph-file { font-size: var(--fs-xs); color: var(--fg-secondary); }
  .batch-warn {
    margin-top: 4px; padding: var(--s-2);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    border-radius: var(--r-sm);
    color: var(--warn); font-size: 10px;
  }
  .batch-results { display: flex; flex-direction: column; gap: 4px; }
  .batch-row {
    display: flex; gap: var(--s-3); align-items: center;
    padding: var(--s-2); background: var(--bg-chip);
    border-radius: var(--r-sm); font-size: var(--fs-xs);
  }
  .res-pending { color: var(--fg-muted); }
  .res-ok { color: var(--ok); }
  .res-err { color: var(--danger); font-family: var(--font-mono); font-size: 10px; }

  .templates {
    display: flex; flex-direction: column; gap: var(--s-3);
  }
  .template-group { display: flex; flex-direction: column; gap: 6px; }
  .group-label {
    font-size: 10px; color: var(--fg-faint);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .group-items {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: var(--s-2);
  }
  .template-card {
    display: flex; flex-direction: column; align-items: flex-start; gap: 4px;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    cursor: pointer;
    text-align: left;
    transition: border-color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
  }
  .template-card:hover { border-color: var(--border-default); background: var(--bg-chip-hover, var(--bg-raised)); }
  .template-card.selected { border-color: var(--accent); background: rgba(0, 209, 178, 0.08); }
  .template-card:disabled { opacity: 0.5; cursor: not-allowed; }
  .t-head { display: flex; align-items: center; gap: var(--s-2); width: 100%; }
  .t-name { font-size: var(--fs-sm); font-weight: 600; color: var(--fg-primary); flex: 1; }
  .t-desc { font-size: 11px; color: var(--fg-muted); line-height: 1.4; }
  .risk-chip {
    padding: 1px 6px; font-size: 9px;
    text-transform: uppercase; letter-spacing: var(--tracking-label); font-weight: 600;
    border-radius: var(--r-sm); font-family: var(--font-mono);
  }
  .risk-chip.risk-low    { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .risk-chip.risk-medium { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .risk-chip.risk-high   { background: color-mix(in srgb, var(--danger) 22%, transparent); color: var(--danger); }
  .t-tip {
    display: flex; gap: 6px;
    margin-top: 4px; padding: 4px 8px;
    background: var(--bg-chip); border-radius: var(--r-sm);
    font-size: 10px; line-height: 1.45;
  }
  .t-tip .tip-k { color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); font-weight: 600; flex-shrink: 0; }
  .t-tip .tip-v { color: var(--fg-secondary); }
  .t-tip.caveat { background: color-mix(in srgb, var(--warn) 10%, transparent); }
  .t-tip.caveat .tip-k { color: var(--warn); }

  .cred-picker {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: var(--s-2);
  }
  .cred-option {
    display: grid;
    grid-template-columns: 18px 1fr auto;
    gap: var(--s-2);
    align-items: center;
    padding: 8px 12px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    cursor: pointer;
    font-size: var(--fs-sm);
    transition: border-color var(--dur-fast) var(--ease-out);
  }
  .cred-option.selected {
    border-color: var(--accent);
    background: rgba(0, 209, 178, 0.08);
  }
  .cred-option.tenant-bad {
    border-color: rgba(239, 68, 68, 0.45);
  }
  .cred-option.tenant-bad.selected {
    background: rgba(239, 68, 68, 0.08);
  }
  .cred-option input { margin: 0; }
  .c-id { font-weight: 500; color: var(--fg-primary); }
  .c-meta { display: inline-flex; gap: 6px; align-items: center; }
  .c-venue { font-family: var(--font-mono); font-size: 10px; color: var(--fg-muted); text-transform: uppercase; }
  .c-tenant {
    font-family: var(--font-mono); font-size: 9px;
    padding: 1px 6px; border-radius: var(--r-sm);
    background: var(--bg-base); color: var(--fg-secondary);
    border: 1px solid var(--border-subtle);
    text-transform: uppercase; letter-spacing: var(--tracking-label);
  }
  .c-tenant.c-tenant-shared { color: var(--fg-muted); }
  .c-tenant.c-tenant-bad {
    color: var(--danger);
    border-color: rgba(239, 68, 68, 0.45);
    background: rgba(239, 68, 68, 0.08);
  }
  .tenant-line {
    padding: 4px 8px;
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
  }
  .tenant-line code {
    font-family: var(--font-mono); background: var(--bg-chip);
    padding: 0 4px; border-radius: 3px; color: var(--fg-primary);
  }

  .tpl-note {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 6px 10px;
    font-size: var(--fs-xs);
    color: var(--fg-muted);
  }

  .warn-banner {
    display: flex; gap: var(--s-2); align-items: flex-start;
    padding: var(--s-2) var(--s-3);
    background: rgba(245, 158, 11, 0.08);
    border: 1px solid rgba(245, 158, 11, 0.25);
    border-radius: var(--r-sm);
    color: var(--warn);
    font-size: var(--fs-xs);
    line-height: 1.5;
  }
  .warn-banner code { font-family: var(--font-mono); background: var(--bg-chip); padding: 0 4px; border-radius: 3px; }

  .error {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 8px 12px;
    background: rgba(239, 68, 68, 0.08);
    border: 1px solid rgba(239, 68, 68, 0.25);
    border-radius: var(--r-sm);
    color: var(--danger);
    font-size: var(--fs-xs);
  }

  /* `.foot`, `.btn*`, `.spinner` moved to primitives/Modal.svelte +
     Button.svelte (built-in loading spinner) — design system v1. */

  code.mono { font-family: var(--font-mono); font-size: 11px; background: var(--bg-chip); padding: 0 4px; border-radius: 3px; color: var(--fg-primary); }
</style>
