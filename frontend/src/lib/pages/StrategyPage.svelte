<script>
  /*
   * Epic H — visual strategy builder (Phase 1 frontend).
   *
   * Four sections laid out top-to-bottom: top bar (name, scope,
   * actions), left palette, center canvas (svelte-flow), right
   * selected-node config panel. Deploy history is a collapsible
   * footer.
   *
   * Data flow:
   *   on mount        → GET /api/v1/strategy/catalog → palette
   *   drag from palette → add node at drop position, fresh UUID
   *   deploy button   → POST /api/admin/strategy/graph with full
   *                     graph JSON; backend validates + broadcasts.
   */

  import {
    SvelteFlow,
    Background,
    Controls,
    MiniMap,
    useSvelteFlow,
  } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { createApiClient } from '../api.svelte.js'
  import Icon from '../components/Icon.svelte'
  import StrategyPalette from '../components/StrategyPalette.svelte'
  import StrategyNodeConfig from '../components/StrategyNodeConfig.svelte'
  import StrategyDeployHistory from '../components/StrategyDeployHistory.svelte'
  import StrategyNode from '../components/StrategyNode.svelte'
  import ActivePlans from '../components/ActivePlans.svelte'

  let { auth } = $props()
  const api = createApiClient(auth)

  let nodes = $state.raw([])
  let edges = $state.raw([])
  let catalog = $state([])
  let templates = $state([])
  let graphName = $state('untitled')
  let scopeKind = $state('symbol')
  let scopeValue = $state('BTCUSDT')
  let selected = $state(null)
  let deployStatus = $state('')
  let deployBusy = $state(false)
  let previewResult = $state(null)
  // Epic H Phase 3 — set when the operator loads a historical hash
  // from the deploy-history panel. Passed as `?rollback_from=` on
  // the next deploy so the audit row records intent (rollback vs.
  // accidental hash match). Cleared after a successful deploy.
  let rollbackFrom = $state(null)
  let previewBusy = $state(false)
  // UI-6 — set when the backend returns 412 with a
  // restricted-nodes list. Frontend opens a confirmation
  // modal; operator ticks the box + clicks Acknowledge &
  // Deploy, which re-issues the request with
  // restricted_ack=yes-pentest-mode.
  let restrictedAck = $state(null)

  // Custom / user-authored templates, loaded from disk via
  // /api/v1/strategy/custom_templates. Shown in the same dropdown
  // as bundled templates with a `custom:` prefix in the value.
  let customTemplates = $state([])

  // Live server-side validation snapshot. Debounced on any graph
  // mutation. `valid` drives the Deploy button; `issues` render as
  // red chips in the validation strip.
  let validation = $state({ valid: false, issues: [], node_count: 0, edge_count: 0, sink_count: 0 })

  let fileInput = $state(null)
  let saveDialogOpen = $state(false)
  let saveDialogName = $state('')
  let saveDialogDesc = $state('')
  let saveDialogBusy = $state(false)
  let saveDialogError = $state('')

  // ─── Local draft persistence ──────────────────────────────
  // Canvas work is checkpointed into localStorage on every change so
  // F5 / tab crashes never lose the WIP. Deploy is still the single
  // source of truth for "live" — draft only restores the *editor*
  // state, never activates a graph.
  const DRAFT_KEY = 'mm.strategy.draft.v1'

  function saveDraft() {
    try {
      const draft = {
        graphName, scopeKind, scopeValue,
        nodes: nodes.map((n) => ({
          id: n.id, kind: n.data.kind, config: n.data.config,
          pos: [n.position.x, n.position.y],
        })),
        edges: edges.map((e) => ({
          id: e.id,
          source: e.source, sourceHandle: e.sourceHandle,
          target: e.target, targetHandle: e.targetHandle,
        })),
        savedAt: Date.now(),
      }
      localStorage.setItem(DRAFT_KEY, JSON.stringify(draft))
    } catch {
      // Storage disabled / quota — silently skip.
    }
  }

  function restoreDraft() {
    try {
      const raw = localStorage.getItem(DRAFT_KEY)
      if (!raw) return false
      const d = JSON.parse(raw)
      if (!Array.isArray(d.nodes) || d.nodes.length === 0) return false
      graphName = d.graphName ?? 'untitled'
      scopeKind = d.scopeKind ?? 'symbol'
      scopeValue = d.scopeValue ?? ''
      nodes = d.nodes.map((n) => ({
        id: n.id,
        type: 'graphNode',
        position: { x: n.pos?.[0] ?? 0, y: n.pos?.[1] ?? 0 },
        data: nodeData(n.kind, n.config),
      }))
      edges = (d.edges ?? []).map((e, i) => ({
        id: e.id ?? `e${i}`,
        source: e.source, sourceHandle: e.sourceHandle,
        target: e.target, targetHandle: e.targetHandle,
      }))
      return true
    } catch {
      return false
    }
  }

  // Autosave: fires on any mutation of the observed state.
  // Debounced via a micro-task so a 10-node load triggers one save
  // at the end, not ten.
  let saveScheduled = false
  $effect(() => {
    // Subscribe to the reactive state.
    nodes.length; edges.length; graphName; scopeKind; scopeValue
    if (saveScheduled) return
    saveScheduled = true
    queueMicrotask(() => { saveScheduled = false; saveDraft() })
  })

  // ─── Live validation ──────────────────────────────────────
  //
  // Server-side is the single source of truth (same validator
  // Deploy uses) so client-side rules never silently drift.
  // Debounced 300 ms — fast enough that operator feedback is
  // immediate, slow enough that dragging a node doesn't spam
  // the endpoint.
  let validateTimer = null
  function scheduleValidate() {
    if (validateTimer) clearTimeout(validateTimer)
    validateTimer = setTimeout(runValidate, 300)
  }
  async function runValidate() {
    if (nodes.length === 0) {
      validation = { valid: false, issues: [], node_count: 0, edge_count: 0, sink_count: 0 }
      return
    }
    try {
      const body = { graph: toBackendGraph() }
      validation = await api.postJson('/api/v1/strategy/validate', body)
    } catch (e) {
      // Non-fatal — keep last state, surface as a single issue so
      // the operator at least sees something.
      validation = { ...validation, valid: false, issues: [`validator unreachable: ${e}`] }
    }
  }
  $effect(() => {
    // Re-validate on any canvas mutation.
    nodes.length; edges.length; graphName; scopeKind; scopeValue
    scheduleValidate()
  })

  async function loadCustomTemplates() {
    try {
      customTemplates = await api.getJson('/api/v1/strategy/custom_templates')
    } catch {
      customTemplates = []
    }
  }

  // ─── Export / Import ──────────────────────────────────────

  function exportGraph() {
    const blob = new Blob(
      [JSON.stringify(toBackendGraph(), null, 2)],
      { type: 'application/json' },
    )
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `${graphName || 'strategy-graph'}.json`
    a.click()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  }

  async function importGraph(ev) {
    const file = ev.target.files?.[0]
    if (!file) return
    try {
      const text = await file.text()
      const g = JSON.parse(text)
      graphName = g.name ?? 'untitled'
      scopeKind = g.scope?.kind ?? 'symbol'
      scopeValue = g.scope?.value ?? ''
      nodes = (g.nodes ?? []).map((n) => ({
        id: n.id,
        type: 'graphNode',
        position: { x: n.pos?.[0] ?? 0, y: n.pos?.[1] ?? 0 },
        data: nodeData(n.kind, n.config),
      }))
      edges = (g.edges ?? []).map((e, i) => ({
        id: `e${i}`,
        source: e.from.node, sourceHandle: e.from.port,
        target: e.to.node, targetHandle: e.to.port,
      }))
      deployStatus = `imported ${nodes.length} nodes from ${file.name}`
    } catch (e) {
      deployStatus = `import failed: ${e}`
    } finally {
      ev.target.value = ''
    }
  }

  // ─── Save as custom template ─────────────────────────────

  function openSaveDialog() {
    saveDialogName = graphName && graphName !== 'untitled' ? graphName : ''
    saveDialogDesc = ''
    saveDialogError = ''
    saveDialogOpen = true
  }

  async function confirmSaveTemplate() {
    saveDialogBusy = true
    saveDialogError = ''
    try {
      await api.postJson('/api/v1/strategy/custom_templates', {
        name: saveDialogName.trim(),
        description: saveDialogDesc.trim(),
        graph: toBackendGraph(),
      })
      saveDialogOpen = false
      await loadCustomTemplates()
      deployStatus = `saved template '${saveDialogName.trim()}'`
    } catch (e) {
      saveDialogError = String(e)
    } finally {
      saveDialogBusy = false
    }
  }

  async function loadCatalog() {
    try {
      catalog = await api.getJson('/api/v1/strategy/catalog')
    } catch (e) {
      deployStatus = `catalog fetch failed: ${e}`
    }
  }
  async function loadTemplates() {
    try {
      templates = await api.getJson('/api/v1/strategy/templates')
    } catch (e) {
      // Non-fatal — operator can still compose from scratch.
      templates = []
    }
  }
  let restored = $state(false)
  $effect(() => {
    // Catalog/templates/draft all load once on mount. Draft wakes
    // after catalog so node metadata (label/summary) resolves
    // correctly during hydrate.
    loadCatalog().then(() => {
      if (!restored) {
        restored = true
        if (restoreDraft()) {
          deployStatus = 'restored local draft'
        }
      }
    })
    loadTemplates()
    loadCustomTemplates()
  })

  async function loadTemplate(name) {
    if (!name) return
    // `custom:<name>` → user-saved template on disk; hydrates from
    // the full record (graph + metadata) rather than the bundled
    // endpoint.
    const isCustom = name.startsWith('custom:')
    const realName = isCustom ? name.slice('custom:'.length) : name
    try {
      let g
      if (isCustom) {
        const rec = await api.getJson(
          `/api/v1/strategy/custom_templates/${encodeURIComponent(realName)}`
        )
        g = rec.graph
      } else {
        g = await api.getJson(
          `/api/v1/strategy/templates/${encodeURIComponent(realName)}`
        )
      }
      graphName = g.name
      scopeKind = g.scope.kind
      scopeValue = g.scope.value ?? ''
      nodes = g.nodes.map((n) => ({
        id: n.id,
        type: 'graphNode',
        position: { x: n.pos?.[0] ?? 0, y: n.pos?.[1] ?? 0 },
        data: nodeData(n.kind, n.config),
      }))
      edges = g.edges.map((e, i) => ({
        id: `e${i}`,
        source: e.from.node,
        sourceHandle: e.from.port,
        target: e.to.node,
        targetHandle: e.to.port,
      }))
      deployStatus = `loaded template '${realName}'`
    } catch (e) {
      deployStatus = `template load failed: ${e}`
    }
  }

  function uuid() {
    // Simple v4 UUID without adding a dep.
    const h = 'abcdef0123456789'
    let s = ''
    for (let i = 0; i < 32; i++) {
      if (i === 8 || i === 12 || i === 16 || i === 20) s += '-'
      if (i === 12) s += '4'
      else if (i === 16) s += h[(Math.random() * 4) | 0 | 8]
      else s += h[(Math.random() * 16) | 0]
    }
    return s
  }

  // Build the `data` blob svelte-flow hands to StrategyNode.svelte.
  // Centralised so every path that materialises a node (drag/click,
  // template load, backend reload, rollback) attaches the same
  // label/group/summary fields and doesn't drift.
  function nodeData(kind, config) {
    const entry = catalog.find((c) => c.kind === kind)
    // Prefill the config with every schema default so the form is
    // never empty — operators see what they can tweak even before
    // they open the right-pane.
    const defaults = {}
    for (const f of entry?.config_schema ?? []) {
      defaults[f.name] = f.default
    }
    return {
      kind,
      label: entry?.label ?? kind,
      summary: entry?.summary ?? '',
      group: entry?.group ?? kind.split('.')[0],
      config: { ...defaults, ...(config ?? {}) },
      configSchema: entry?.config_schema ?? [],
      inputs: entry?.inputs ?? [],
      outputs: entry?.outputs ?? [],
      restricted: entry?.restricted ?? false,
    }
  }

  function addNode(kind, pos) {
    const entry = catalog.find((c) => c.kind === kind)
    if (!entry) return
    nodes = [
      ...nodes,
      {
        id: uuid(),
        type: 'graphNode',
        position: pos,
        data: nodeData(kind, defaultConfigFor(kind)),
      },
    ]
  }

  function defaultConfigFor(kind) {
    if (kind === 'Stats.EWMA') return { alpha: '0.1' }
    if (kind === 'Cast.ToBool') return { threshold: '0', cmp: 'ge' }
    if (kind === 'Math.Const') return { value: '1.0' }
    if (kind === 'Cast.StrategyEq') return { target: 'AvellanedaStoikov' }
    if (kind === 'Cast.PairClassEq') return { target: 'MajorSpot' }
    if (kind === 'Risk.ToxicityWiden') return { scale: '2' }
    if (kind === 'Risk.InventoryUrgency') return { cap: '1', exponent: '2' }
    if (kind === 'Risk.CircuitBreaker') return { wide_bps: '100' }
    if (kind === 'Indicator.SMA' || kind === 'Indicator.EMA' || kind === 'Indicator.HMA' || kind === 'Indicator.RSI' || kind === 'Indicator.ATR') return { period: 14 }
    if (kind === 'Indicator.Bollinger') return { period: 20, k_stddev: '2' }
    if (kind === 'Exec.TwapConfig') return { duration_secs: 120, slice_count: 5 }
    if (kind === 'Exec.VwapConfig') return { duration_secs: 300 }
    if (kind === 'Exec.PovConfig') return { target_pct: 10 }
    if (kind === 'Exec.IcebergConfig') return { display_qty: '0.1' }
    return {}
  }

  function onDrop(e) {
    e.preventDefault()
    const kind = e.dataTransfer?.getData('mm/strategy-kind')
    if (!kind) return
    const rect = e.currentTarget.getBoundingClientRect()
    addNode(kind, {
      x: e.clientX - rect.left - 80,
      y: e.clientY - rect.top - 30,
    })
  }
  function onDragOver(e) { e.preventDefault() }

  function toBackendGraph() {
    const backendNodes = nodes.map(n => ({
      id: n.id,
      kind: n.data.kind,
      config: n.data.config ?? null,
      pos: [n.position.x, n.position.y],
    }))
    const backendEdges = edges.map(e => ({
      from: { node: e.source, port: e.sourceHandle ?? '' },
      to: { node: e.target, port: e.targetHandle ?? '' },
    }))
    return {
      version: 1,
      name: graphName,
      scope: { kind: scopeKind, value: scopeKind === 'global' ? null : scopeValue },
      nodes: backendNodes,
      edges: backendEdges,
      stale_hold_ms: 30000,
    }
  }

  async function deploy(ackToken = null) {
    deployBusy = true
    deployStatus = ''
    try {
      const body = toBackendGraph()
      const params = []
      if (rollbackFrom) params.push(`rollback_from=${encodeURIComponent(rollbackFrom)}`)
      if (ackToken) params.push(`restricted_ack=${encodeURIComponent(ackToken)}`)
      const path = params.length
        ? `/api/admin/strategy/graph?${params.join('&')}`
        : '/api/admin/strategy/graph'
      const resp = await api.authedFetch(path, {
        method: 'POST',
        body: JSON.stringify(body),
      })
      if (resp.status === 412) {
        // UI-6 — restricted deploy awaiting operator ack. Open
        // the modal; resubmit only if the operator confirms.
        const payload = await resp.json().catch(() => ({}))
        restrictedAck = {
          nodes: payload?.restricted_nodes ?? [],
          acknowledged: false,
          busy: false,
          error: '',
        }
        deployStatus = 'restricted deploy — operator ack required'
        return
      }
      if (!resp.ok) {
        const text = await resp.text().catch(() => '')
        throw new Error(`${resp.status} ${text}`)
      }
      const r = await resp.json().catch(() => ({}))
      deployStatus = rollbackFrom
        ? `rolled back · hash ${r.hash?.slice(0, 12)}… · ${r.recipients} engines`
        : `deployed · hash ${r.hash?.slice(0, 12)}… · ${r.recipients} engines`
      rollbackFrom = null
      restrictedAck = null
    } catch (e) {
      deployStatus = `deploy failed: ${e}`
    } finally {
      deployBusy = false
    }
  }

  async function confirmRestrictedDeploy() {
    if (!restrictedAck?.acknowledged) return
    restrictedAck = { ...restrictedAck, busy: true, error: '' }
    await deploy('yes-pentest-mode')
    if (restrictedAck) {
      // deploy() cleared it on success; if we're here, leave
      // the modal open so the operator can read the error.
      restrictedAck = { ...restrictedAck, busy: false, error: deployStatus }
    }
  }

  async function simulate() {
    previewBusy = true
    previewResult = null
    try {
      const body = {
        graph: toBackendGraph(),
        source_inputs: {},
      }
      previewResult = await api.postJson('/api/v1/strategy/preview', body)
      decorateEdges()
    } catch (e) {
      previewResult = { errors: [String(e)], edges: {}, sinks: [] }
    } finally {
      previewBusy = false
    }
  }

  // Decorate canvas edges with live values from the preview trace.
  // svelte-flow supports `label` + `labelStyle` per edge, so we
  // materialise the value the upstream node produced on the edge's
  // `sourceHandle`. Edges without a trace hit stay unlabelled.
  function decorateEdges() {
    const lookup = previewResult?.edges ?? null
    edges = edges.map((e) => {
      if (!lookup) return { ...e, label: undefined }
      const key = `${e.source}:${e.sourceHandle}`
      const label = lookup[key]
      return label !== undefined
        ? {
            ...e,
            label,
            labelStyle: 'font-family: var(--font-mono); font-size: 10px; fill: var(--fg-primary);',
            labelBgStyle: 'fill: var(--bg-raised); stroke: var(--accent); stroke-width: 1;',
            labelBgPadding: [4, 2],
            labelBgBorderRadius: 4,
          }
        : { ...e, label: undefined }
    })
  }

  async function loadGraph(name) {
    try {
      const g = await api.getJson(`/api/v1/strategy/graphs/${encodeURIComponent(name)}`)
      graphName = g.name
      scopeKind = g.scope.kind
      scopeValue = g.scope.value ?? ''
      nodes = g.nodes.map((n) => ({
        id: n.id,
        type: 'graphNode',
        position: { x: n.pos?.[0] ?? 0, y: n.pos?.[1] ?? 0 },
        data: nodeData(n.kind, n.config),
      }))
      edges = g.edges.map((e, i) => ({
        id: `e${i}`,
        source: e.from.node,
        sourceHandle: e.from.port,
        target: e.to.node,
        targetHandle: e.to.port,
      }))
      deployStatus = `loaded '${name}'`
    } catch (e) {
      deployStatus = `load failed: ${e}`
    }
  }

  function onSelectionChange(evt) {
    selected = evt.nodes?.[0] ?? null
  }
  function updateSelectedConfig(cfg) {
    if (!selected) return
    const id = selected.id
    nodes = nodes.map(n =>
      n.id === id ? { ...n, data: { ...n.data, config: cfg } } : n
    )
    // Keep the local reference in sync so the panel re-renders.
    selected = nodes.find(n => n.id === id) ?? null
  }

  function deleteSelected() {
    if (!selected) return
    const id = selected.id
    nodes = nodes.filter(n => n.id !== id)
    edges = edges.filter(e => e.source !== id && e.target !== id)
    selected = null
  }

  const nodeTypes = { graphNode: StrategyNode }
</script>

<div class="page">
  <div class="top">
    <div class="left-chunk">
      <label class="field">
        <span class="field-label">Name</span>
        <input type="text" bind:value={graphName} />
      </label>
      <label class="field">
        <span class="field-label">Scope</span>
        <select bind:value={scopeKind}>
          <option value="symbol">Symbol</option>
          <option value="asset_class">Asset class</option>
          <option value="client">Client</option>
          <option value="global">Global</option>
        </select>
      </label>
      <label class="field" class:field-hidden={scopeKind === 'global'}>
        <span class="field-label">Value</span>
        <input
          type="text"
          bind:value={scopeValue}
          disabled={scopeKind === 'global'}
          placeholder={scopeKind === 'symbol' ? 'BTCUSDT' : ''}
        />
      </label>
    </div>
    <div class="right-chunk">
      {#if templates.length + customTemplates.length > 0}
        <label class="field">
          <span class="field-label">Template</span>
          <select onchange={(e) => { loadTemplate(e.currentTarget.value); e.currentTarget.value = '' }}>
            <option value="">—</option>
            {#if templates.length > 0}
              <optgroup label="Built-in">
                {#each templates as t (t.name)}
                  <option value={t.name} title={t.description}>{t.name}</option>
                {/each}
              </optgroup>
            {/if}
            {#if customTemplates.length > 0}
              <optgroup label="Saved">
                {#each customTemplates as t (t.name)}
                  <option value="custom:{t.name}" title={t.description}>{t.name}</option>
                {/each}
              </optgroup>
            {/if}
          </select>
        </label>
      {/if}
      <button type="button" class="btn ghost" onclick={exportGraph} disabled={nodes.length === 0} title="Download graph as JSON">
        <Icon name="download" size={14} />
      </button>
      <button type="button" class="btn ghost" onclick={() => fileInput?.click()} title="Import graph from JSON file">
        <Icon name="upload" size={14} />
      </button>
      <input
        type="file" accept="application/json" bind:this={fileInput}
        onchange={importGraph} style="display: none"
      />
      <button type="button" class="btn ghost" onclick={openSaveDialog} disabled={nodes.length === 0} title="Save as reusable template">
        <Icon name="save" size={14} />
      </button>
      <button type="button" class="btn ghost" onclick={simulate} disabled={previewBusy || nodes.length === 0} title="Evaluate graph without deploying">
        <Icon name="pulse" size={14} />
        <span>{previewBusy ? 'Simulating…' : 'Simulate'}</span>
      </button>
      <button type="button" class="btn" onclick={deploy} disabled={deployBusy || nodes.length === 0 || !validation.valid}>
        <Icon name="bolt" size={14} />
        <span>{deployBusy ? 'Deploying…' : 'Deploy'}</span>
      </button>
    </div>
  </div>

  <!-- Validation strip: live counters + issue list, server-side
       authoritative. Green pill = Evaluator::build succeeded +
       no dangling edges; red pill lists every blocker so the
       operator fixes them before noticing the disabled Deploy. -->
  <div class="validate-bar" class:valid={validation.valid} class:invalid={!validation.valid && nodes.length > 0}>
    {#if nodes.length === 0}
      <span class="v-pill muted"><span class="dot"></span> empty</span>
      <span class="v-hint">drag a node from the palette to start</span>
    {:else if validation.valid}
      <span class="v-pill ok"><span class="dot"></span> ready</span>
      <span class="v-stats">
        {validation.node_count} nodes · {validation.edge_count} edges · {validation.sink_count} sinks
      </span>
      <span class="v-status">{deployStatus}</span>
    {:else}
      <span class="v-pill bad"><span class="dot"></span> {validation.issues.length} issue{validation.issues.length === 1 ? '' : 's'}</span>
      <span class="v-stats">
        {validation.node_count} nodes · {validation.edge_count} edges · {validation.sink_count} sinks
      </span>
      <span class="v-issues">
        {#each validation.issues as iss}
          <code class="v-issue">{iss}</code>
        {/each}
      </span>
    {/if}
  </div>

  {#if saveDialogOpen}
    <div class="modal-backdrop" onclick={() => (saveDialogOpen = false)}>
      <div class="modal" onclick={(e) => e.stopPropagation()} role="dialog" aria-label="Save as template">
        <h3>Save as template</h3>
        <label class="field stacked">
          <span class="field-label">Name</span>
          <input type="text" bind:value={saveDialogName} placeholder="my-cool-setup" />
        </label>
        <label class="field stacked">
          <span class="field-label">Description</span>
          <input type="text" bind:value={saveDialogDesc} placeholder="What does this do?" />
        </label>
        {#if saveDialogError}
          <div class="modal-err">{saveDialogError}</div>
        {/if}
        <div class="modal-actions">
          <button type="button" class="btn ghost" onclick={() => (saveDialogOpen = false)}>Cancel</button>
          <button type="button" class="btn" onclick={confirmSaveTemplate} disabled={saveDialogBusy || !saveDialogName.trim()}>
            {saveDialogBusy ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>
    </div>
  {/if}

  {#if previewResult}
    <div class="preview-bar" class:has-error={previewResult.errors?.length > 0}>
      {#if previewResult.errors?.length > 0}
        <span class="preview-label">Simulate · errors</span>
        {#each previewResult.errors as err}
          <code class="preview-err">{err}</code>
        {/each}
      {:else}
        <span class="preview-label">Simulate · {previewResult.sinks?.length ?? 0} sink{previewResult.sinks?.length === 1 ? '' : 's'}</span>
        {#each previewResult.sinks ?? [] as sink}
          <code class="preview-sink">{sink}</code>
        {/each}
        {#if (previewResult.sinks?.length ?? 0) === 0}
          <span class="muted">no sinks fired — values shown on edges</span>
        {/if}
      {/if}
      <button type="button" class="preview-close" onclick={() => { previewResult = null; decorateEdges() }}>×</button>
    </div>
  {/if}

  <div class="body">
    <aside class="palette">
      <StrategyPalette {catalog} onAdd={(k) => addNode(k, { x: 120, y: 120 })} />
    </aside>

    <section
      class="canvas"
      ondrop={onDrop}
      ondragover={onDragOver}
      role="region"
      aria-label="Strategy graph canvas"
    >
      {#if nodes.length === 0}
        <div class="empty">
          Drag a node from the palette to start. Every graph needs exactly one
          <code>Out.SpreadMult</code> sink.
        </div>
      {/if}
      <SvelteFlow
        bind:nodes
        bind:edges
        {nodeTypes}
        fitView
        fitViewOptions={{ padding: 0.3, maxZoom: 1, minZoom: 0.85 }}
        minZoom={0.85}
        maxZoom={1.5}
        colorMode="dark"
        onselectionchange={onSelectionChange}
        proOptions={{ hideAttribution: true }}
      >
        <Background />
        <Controls />
        <MiniMap />
      </SvelteFlow>
    </section>

    <aside class="config">
      <StrategyNodeConfig
        node={selected}
        onUpdate={updateSelectedConfig}
        onDelete={deleteSelected}
      />
    </aside>
  </div>

  <StrategyDeployHistory
    {auth}
    onReload={(n) => loadGraph(n)}
    onRollback={async (name, hash) => {
      try {
        const g = await api.getJson(
          `/api/v1/strategy/graphs/${encodeURIComponent(name)}/history/${encodeURIComponent(hash)}`
        )
        graphName = g.name
        scopeKind = g.scope.kind
        scopeValue = g.scope.value ?? ''
        nodes = g.nodes.map((n) => ({
          id: n.id,
          type: 'graphNode',
          position: { x: n.pos?.[0] ?? 0, y: n.pos?.[1] ?? 0 },
          data: nodeData(n.kind, n.config),
        }))
        edges = g.edges.map((e, i) => ({
          id: `e${i}`,
          source: e.from.node,
          sourceHandle: e.from.port,
          target: e.to.node,
          targetHandle: e.to.port,
        }))
        rollbackFrom = hash
        deployStatus = `loaded hash ${hash.slice(0, 8)}… — click Deploy to roll back`
      } catch (e) {
        deployStatus = `rollback fetch failed: ${e}`
      }
    }}
  />

  <div class="plans-footer">
    <ActivePlans {auth} />
  </div>

  {#if restrictedAck}
    <div class="ack-backdrop">
      <div class="ack-card">
        <div class="ack-title">⚠ Restricted deploy</div>
        <div class="ack-body">
          <p class="ack-lead">
            This graph references {restrictedAck.nodes.length} pentest-only
            node{restrictedAck.nodes.length === 1 ? '' : 's'}. Deployment
            places market-manipulating patterns on the engine pool; make
            sure the run is authorised before continuing.
          </p>
          <ul class="ack-nodes">
            {#each restrictedAck.nodes as n (n)}
              <li><code>{n}</code></li>
            {/each}
          </ul>
          <label class="ack-check">
            <input
              type="checkbox"
              bind:checked={restrictedAck.acknowledged}
              disabled={restrictedAck.busy}
            />
            <span>I acknowledge the restricted node list above and authorise this deploy.</span>
          </label>
          {#if restrictedAck.error}
            <div class="ack-error">{restrictedAck.error}</div>
          {/if}
        </div>
        <div class="ack-actions">
          <button
            type="button"
            class="btn ghost"
            onclick={() => { restrictedAck = null }}
            disabled={restrictedAck.busy}
          >
            Cancel
          </button>
          <button
            type="button"
            class="btn"
            onclick={confirmRestrictedDeploy}
            disabled={!restrictedAck.acknowledged || restrictedAck.busy}
          >
            {restrictedAck.busy ? 'Deploying…' : 'Acknowledge & Deploy'}
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .page {
    display: flex;
    flex-direction: column;
    height: calc(100vh - 57px);
  }
  .plans-footer {
    padding: var(--s-3) var(--s-4);
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-raised);
  }

  /* ─── Top bar: one row, one height, one typography scale.
   *    Every control (input/select/button) is exactly 30px tall,
   *    every label is 10px monospace uppercase, every gap is s-2.
   *    That's why nothing "jumps" between fields anymore. ────── */
  .top {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--s-2) var(--s-4);
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-raised);
    gap: var(--s-3);
    height: 48px;
    flex-shrink: 0;
  }
  .left-chunk, .right-chunk {
    display: flex; gap: var(--s-2); align-items: center; flex: 0 0 auto;
  }
  .divider {
    width: 1px; height: 22px;
    background: var(--border-subtle); margin: 0 var(--s-2);
  }
  .field {
    display: flex; align-items: center; gap: var(--s-2);
    height: 30px;
  }
  .field-label {
    font-family: var(--font-mono);
    font-size: 10px; line-height: 1;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  .field input, .field select {
    height: 30px;
    padding: 0 var(--s-3);
    background: var(--bg-base); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary);
    font-family: var(--font-mono); font-size: 12px; line-height: 30px;
  }
  .field input { width: 140px; }
  .field select { min-width: 120px; }
  .field input:focus, .field select:focus {
    outline: none; border-color: var(--accent);
  }
  .status {
    height: 30px; display: flex; align-items: center;
    padding: 0 var(--s-3);
    font-family: var(--font-mono); font-size: 11px;
    color: var(--fg-muted);
    max-width: 260px;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .btn {
    display: inline-flex; align-items: center; justify-content: center;
    gap: var(--s-2);
    height: 30px;
    padding: 0 var(--s-3);
    background: var(--accent-dim); color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: var(--r-sm); cursor: pointer;
    font-family: var(--font-sans); font-size: 12px; font-weight: 500;
    line-height: 1;
  }
  .btn.ghost {
    background: transparent; color: var(--fg-secondary);
    border-color: var(--border-strong);
  }
  .btn.ghost:hover:not(:disabled) {
    color: var(--accent); border-color: var(--accent);
  }
  .btn:hover:not(:disabled) { background: var(--accent); color: var(--bg-base); }
  .btn:disabled { opacity: 0.4; cursor: not-allowed; }

  /* Hide a top-bar field without removing it from the flex layout
   * so the bar keeps its width on scope toggles. */
  .field-hidden { visibility: hidden; pointer-events: none; }

  /* UI-6 — restricted-deploy ack modal. Full-screen backdrop +
   * centred card over the editor. Checkbox-gated Deploy button
   * so the operator can't tab-enter their way into a pentest
   * deploy. */
  .ack-backdrop {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.65);
    display: flex; align-items: center; justify-content: center;
    z-index: 20;
  }
  .ack-card {
    width: min(520px, 92vw);
    background: var(--bg-raised);
    border: 1px solid var(--danger);
    border-radius: var(--r-md);
    display: flex; flex-direction: column;
    gap: var(--s-3);
    padding: var(--s-4);
  }
  .ack-title {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--danger);
    letter-spacing: var(--tracking-tight);
  }
  .ack-body { display: flex; flex-direction: column; gap: var(--s-3); font-size: var(--fs-sm); }
  .ack-lead { color: var(--fg-primary); }
  .ack-nodes {
    list-style: disc;
    padding-left: var(--s-4);
    color: var(--fg-secondary);
    font-family: var(--font-mono); font-size: var(--fs-xs);
  }
  .ack-check {
    display: flex; align-items: center; gap: var(--s-2);
    color: var(--fg-primary); font-size: var(--fs-xs);
  }
  .ack-error { color: var(--danger); font-size: var(--fs-xs); }
  .ack-actions {
    display: flex; justify-content: flex-end; gap: var(--s-2);
  }

  /* Validation strip — sits between the top bar and the canvas,
   * always present (even when empty) so the bar's height doesn't
   * jitter when issues come or go. */
  .validate-bar {
    display: flex; align-items: center; gap: var(--s-3);
    padding: 4px var(--s-4);
    background: var(--bg-raised);
    border-bottom: 1px solid var(--border-subtle);
    font-size: 11px;
    min-height: 26px; flex-shrink: 0;
  }
  .validate-bar.valid   { border-bottom-color: color-mix(in srgb, var(--pos) 40%, var(--border-subtle)); }
  .validate-bar.invalid { border-bottom-color: color-mix(in srgb, var(--neg) 40%, var(--border-subtle)); }

  .v-pill {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 2px 8px;
    border: 1px solid var(--border-subtle); border-radius: var(--r-pill);
    font-family: var(--font-mono); font-size: 10px;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
  }
  .v-pill .dot {
    width: 6px; height: 6px; border-radius: 50%;
    background: currentColor;
  }
  .v-pill.ok    { color: var(--pos); border-color: color-mix(in srgb, var(--pos) 60%, transparent); }
  .v-pill.bad   { color: var(--neg); border-color: color-mix(in srgb, var(--neg) 60%, transparent); }
  .v-pill.muted { color: var(--fg-muted); }
  .v-stats { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); }
  .v-status { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); margin-left: auto; max-width: 320px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .v-hint { color: var(--fg-muted); font-size: 11px; }
  .v-issues { display: flex; gap: 4px; flex-wrap: wrap; flex: 1; min-width: 0; }
  .v-issue {
    padding: 2px 6px;
    background: color-mix(in srgb, var(--neg) 10%, var(--bg-chip));
    border: 1px solid color-mix(in srgb, var(--neg) 40%, var(--border-subtle));
    border-radius: var(--r-sm);
    color: color-mix(in srgb, var(--neg) 80%, var(--fg-primary));
    font-family: var(--font-mono); font-size: 10px;
    max-width: 380px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }

  /* Save-as-template modal. */
  .modal-backdrop {
    position: fixed; inset: 0; z-index: 50;
    background: rgba(0, 0, 0, 0.55);
    display: flex; align-items: center; justify-content: center;
  }
  .modal {
    min-width: 360px; max-width: 480px;
    padding: var(--s-4);
    background: var(--bg-raised);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    box-shadow: 0 20px 40px rgba(0, 0, 0, 0.5);
    display: flex; flex-direction: column; gap: var(--s-3);
  }
  .modal h3 {
    margin: 0; font-size: 14px; font-weight: 600;
    color: var(--fg-primary);
  }
  .modal .field.stacked {
    display: flex; flex-direction: column; gap: 4px; height: auto;
  }
  .modal-err {
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--neg) 15%, var(--bg-base));
    border: 1px solid var(--neg); border-radius: var(--r-sm);
    color: var(--neg); font-family: var(--font-mono); font-size: 11px;
  }
  .modal-actions { display: flex; justify-content: flex-end; gap: var(--s-2); }

  .preview-bar {
    display: flex; align-items: center; flex-wrap: wrap; gap: var(--s-2);
    padding: var(--s-2) var(--s-4);
    background: var(--bg-raised);
    border-bottom: 1px solid var(--accent);
    font-size: var(--fs-xs);
  }
  .preview-bar.has-error { border-bottom-color: var(--neg); }
  .preview-label {
    font-family: var(--font-mono); font-size: var(--fs-2xs);
    color: var(--accent); text-transform: uppercase; letter-spacing: var(--tracking-label);
  }
  .preview-bar.has-error .preview-label { color: var(--neg); }
  .preview-sink, .preview-err {
    padding: 2px var(--s-2);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); font-family: var(--font-mono); font-size: var(--fs-2xs);
    color: var(--fg-primary);
  }
  .preview-err { color: var(--neg); border-color: var(--neg); }
  .preview-close {
    margin-left: auto;
    background: transparent; border: none; color: var(--fg-muted);
    font-size: var(--fs-md); cursor: pointer; padding: 0 var(--s-2);
  }
  .preview-close:hover { color: var(--fg-primary); }

  .body {
    display: grid;
    grid-template-columns: 220px 1fr 280px;
    flex: 1;
    min-height: 0;
  }
  .palette {
    border-right: 1px solid var(--border-subtle);
    background: var(--bg-raised);
    overflow-y: auto;
  }
  .canvas {
    position: relative;
    background: var(--bg-base);
    min-height: 0;
    overflow: hidden;
  }
  .canvas :global(.svelte-flow) { width: 100%; height: 100%; }
  .empty {
    position: absolute; inset: var(--s-6);
    color: var(--fg-muted); font-size: var(--fs-sm); line-height: 1.6;
    pointer-events: none;
    z-index: 10;
  }
  .empty code { font-family: var(--font-mono); background: var(--bg-chip); padding: 2px 6px; border-radius: var(--r-sm); }
  .config {
    border-left: 1px solid var(--border-subtle);
    background: var(--bg-raised);
    overflow-y: auto;
  }
</style>
