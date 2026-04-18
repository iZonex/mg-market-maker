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
  $effect(() => { loadCatalog(); loadTemplates() })

  async function loadTemplate(name) {
    if (!name) return
    try {
      const g = await api.getJson(
        `/api/v1/strategy/templates/${encodeURIComponent(name)}`
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
      deployStatus = `loaded template '${name}'`
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
    return {
      kind,
      label: entry?.label ?? kind,
      summary: entry?.summary ?? '',
      group: entry?.group ?? kind.split('.')[0],
      config: config ?? {},
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

  async function deploy() {
    deployBusy = true
    deployStatus = ''
    try {
      const body = toBackendGraph()
      const path = rollbackFrom
        ? `/api/admin/strategy/graph?rollback_from=${encodeURIComponent(rollbackFrom)}`
        : '/api/admin/strategy/graph'
      const r = await api.postJson(path, body)
      deployStatus = rollbackFrom
        ? `rolled back · hash ${r.hash?.slice(0, 12)}… · ${r.recipients} engines`
        : `deployed · hash ${r.hash?.slice(0, 12)}… · ${r.recipients} engines`
      rollbackFrom = null
    } catch (e) {
      deployStatus = `deploy failed: ${e}`
    } finally {
      deployBusy = false
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
      {#if scopeKind !== 'global'}
        <label class="field">
          <span class="field-label">Value</span>
          <input type="text" bind:value={scopeValue} />
        </label>
      {/if}
    </div>
    <div class="right-chunk">
      {#if templates.length > 0}
        <label class="field inline">
          <span class="field-label">Template</span>
          <select onchange={(e) => { loadTemplate(e.currentTarget.value); e.currentTarget.value = '' }}>
            <option value="">—</option>
            {#each templates as t (t.name)}
              <option value={t.name} title={t.description}>{t.name}</option>
            {/each}
          </select>
        </label>
      {/if}
      <span class="status">{deployStatus}</span>
      <button type="button" class="btn ghost" onclick={simulate} disabled={previewBusy || nodes.length === 0} title="Evaluate graph without deploying">
        <Icon name="pulse" size={14} />
        <span>{previewBusy ? 'Simulating…' : 'Simulate'}</span>
      </button>
      <button type="button" class="btn" onclick={deploy} disabled={deployBusy || nodes.length === 0}>
        <Icon name="bolt" size={14} />
        <span>{deployBusy ? 'Deploying…' : 'Deploy'}</span>
      </button>
    </div>
  </div>

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
</div>

<style>
  .page {
    display: flex;
    flex-direction: column;
    height: calc(100vh - 57px);
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
