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
   *
   * Wave C3 — batch mode: `agents` (optional, non-empty) replaces
   * `agent`; the same template + variables fan out to every
   * listed agent serially.
   *
   * Presentational pieces live in ./deploy/ — this file is the
   * coordinator (fetch, form state, submit + merge logic).
   */
  import Icon from './Icon.svelte'
  import { createApiClient } from '../api.svelte.js'
  import { Button, Modal } from '../primitives/index.js'
  import TemplatePicker from './deploy/TemplatePicker.svelte'
  import CustomGraphEditor from './deploy/CustomGraphEditor.svelte'
  import CredentialPicker from './deploy/CredentialPicker.svelte'

  let { auth, agent, agents = null, onClose = () => {}, onDeployed = () => {} } = $props()
  const api = $derived(createApiClient(auth))

  const batchMode = $derived(agents != null && Array.isArray(agents) && agents.length > 1)
  const targetAgents = $derived(batchMode ? agents : [agent])
  const primaryAgent = $derived(batchMode ? agents[0] : agent)
  let batchResults = $state([])

  let templates = $state([])
  let creds = $state([])
  let loading = $state(true)
  let error = $state(null)
  let submitting = $state(false)

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

  function pickTemplate(name) {
    form.template = name
    const t = templates.find((x) => x.name === name)
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
    if (!form.deployment_id.trim()) { error = 'Deployment id is required'; return }
    if (mode === 'template' && !form.template) { error = 'Pick a template'; return }
    if (!form.symbol.trim()) { error = 'Symbol is required'; return }
    if (form.selected_credentials.size === 0) { error = 'At least one credential must be selected'; return }
    if (tenantBlock) { error = `Tenant isolation: ${tenantBlock}`; return }

    let variables
    if (mode === 'template') {
      try {
        variables = parseVariables()
      } catch (e) {
        error = `variables JSON: ${e.message}`
        return
      }
    } else {
      const raw = form.graph_text.trim()
      if (!raw) { error = 'Paste a strategy graph JSON or load one from file'; return }
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
      // `-via-graph` row works); we pick the first one as a
      // carrier so the backend has a valid catalog name.
      template: mode === 'template' ? form.template : (form.template || 'avellaneda-via-graph'),
      symbol: form.symbol.trim().toUpperCase(),
      credentials: Array.from(form.selected_credentials),
      variables,
    }

    // UI-DEPLOY-1 (2026-04-22) — `SetDesiredStrategies` is
    // replace-by-set on the agent. Fetch the current desired
    // set, replace the matching deployment_id (editing case),
    // append when fresh, then POST the union.
    async function mergedBodyFor(agentId) {
      let existing = []
      try {
        existing = await api.getJson(`/api/v1/agents/${encodeURIComponent(agentId)}/deployments`)
        if (!Array.isArray(existing)) existing = []
      } catch (_) {
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
      batchResults = targetAgents.map((a) => ({ agent_id: a.agent_id, phase: 'pending', detail: '' }))
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
      const okCount = settled.filter((s) => s.phase === 'ok').length
      if (okCount > 0) onDeployed()
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

  const currentTemplate = $derived(templates.find((t) => t.name === form.template))
  const agentTenant = $derived(agent?.profile?.client_id || '')

  // Pre-flight tenant check over the currently-selected credentials.
  // Mirrors the controller's `pre_validate_deploy` gate so the
  // operator sees the conflict inline instead of via a 412 after
  // clicking Deploy. Returns null when fine, otherwise a string.
  const tenantBlock = $derived.by(() => {
    const selected = creds.filter((c) => form.selected_credentials.has(c.id))
    const tenanted = selected.map((c) => c.client_id).filter((t) => t && t.length > 0)
    if (tenanted.length === 0) return null
    const first = tenanted[0]
    const mismatch = tenanted.find((t) => t !== first)
    if (mismatch) return `selected credentials mix two tenants (${first} and ${mismatch})`
    if (agentTenant && first !== agentTenant) {
      return `credential tenant '${first}' does not match agent tenant '${agentTenant}'`
    }
    if (!agentTenant && first) {
      return `credential tenant '${first}' picked, but agent has no tenant (profile.client_id is empty — set it in Fleet → Edit)`
    }
    return null
  })
</script>

<Modal open={true} ariaLabel="Deploy strategy" maxWidth="720px" {onClose}>
  {#snippet children()}
    <header class="modal-head">
      <div class="head-text">
        <div class="title">
          {batchMode ? `Deploy to ${targetAgents.length} agents` : 'Deploy strategy'}
        </div>
        {#if batchMode}
          <div class="sub">
            batch: {targetAgents.map((a) => a.agent_id).join(', ')}
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
            <button type="button" role="tab" aria-selected={mode === 'template'} class="mode-tab" class:active={mode === 'template'} onclick={() => (mode = 'template')} disabled={submitting}>Template</button>
            <button type="button" role="tab" aria-selected={mode === 'graph'} class="mode-tab" class:active={mode === 'graph'} onclick={() => (mode = 'graph')} disabled={submitting}>Custom graph</button>
          </div>
          {#if mode === 'template'}
            <div class="section-head">Template</div>
            <TemplatePicker
              {templates}
              selected={form.template}
              disabled={submitting}
              onPick={pickTemplate}
            />
          {:else}
            <div class="section-head">
              Custom graph
              <span class="hint">paste a graph JSON or load from file — applied via <code>variables.strategy_graph</code></span>
            </div>
            <CustomGraphEditor
              bind:graphText={form.graph_text}
              bind:filename={form.graph_filename}
              disabled={submitting}
              onError={(msg) => { error = msg }}
            />
          {/if}
        </div>

        <div class="section">
          <div class="section-head">
            Credentials
            <span class="hint">pick every credential this deployment may touch</span>
          </div>
          <CredentialPicker
            {creds}
            selected={form.selected_credentials}
            {agentTenant}
            disabled={submitting}
            onToggle={toggleCred}
          />
          {#if tenantBlock}
            <div class="warn-banner" role="alert">
              <Icon name="alert" size={12} />
              <span>Tenant isolation: {tenantBlock}</span>
            </div>
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
  .field label { font-size: 11px; color: var(--fg-muted); letter-spacing: 0.02em; }
  .field input, .vars {
    padding: 9px 12px;
    background: color-mix(in srgb, var(--bg-raised) 50%, transparent);
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

  .tpl-note {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 6px 10px;
    font-size: var(--fs-xs);
    color: var(--fg-muted);
  }

  .warn-banner {
    display: flex; gap: var(--s-2); align-items: flex-start;
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 25%, transparent);
    border-radius: var(--r-sm);
    color: var(--warn);
    font-size: var(--fs-xs);
    line-height: 1.5;
  }

  .error {
    display: flex; gap: var(--s-2); align-items: center;
    padding: 8px 12px;
    background: color-mix(in srgb, var(--danger) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: var(--r-sm);
    color: var(--danger);
    font-size: var(--fs-xs);
  }

  code.mono { font-family: var(--font-mono); font-size: 11px; background: var(--bg-chip); padding: 0 4px; border-radius: 3px; color: var(--fg-primary); }
</style>
