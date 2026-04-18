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
  let graphName = $state('untitled')
  let scopeKind = $state('symbol')
  let scopeValue = $state('BTCUSDT')
  let selected = $state(null)
  let deployStatus = $state('')
  let deployBusy = $state(false)

  async function loadCatalog() {
    try {
      catalog = await api.getJson('/api/v1/strategy/catalog')
    } catch (e) {
      deployStatus = `catalog fetch failed: ${e}`
    }
  }
  $effect(() => { loadCatalog() })

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

  function addNode(kind, pos) {
    const id = uuid()
    const entry = catalog.find(c => c.kind === kind)
    if (!entry) return
    nodes = [
      ...nodes,
      {
        id,
        type: 'graphNode',
        position: pos,
        data: {
          kind,
          config: defaultConfigFor(kind),
          inputs: entry.inputs,
          outputs: entry.outputs,
          restricted: entry.restricted,
        },
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
      const r = await api.postJson('/api/admin/strategy/graph', body)
      deployStatus = `deployed · hash ${r.hash?.slice(0, 12)}… · ${r.recipients} engines`
    } catch (e) {
      deployStatus = `deploy failed: ${e}`
    } finally {
      deployBusy = false
    }
  }

  async function loadGraph(name) {
    try {
      const g = await api.getJson(`/api/v1/strategy/graphs/${encodeURIComponent(name)}`)
      graphName = g.name
      scopeKind = g.scope.kind
      scopeValue = g.scope.value ?? ''
      nodes = g.nodes.map(n => {
        const entry = catalog.find(c => c.kind === n.kind)
        return {
          id: n.id,
          type: 'graphNode',
          position: { x: n.pos?.[0] ?? 0, y: n.pos?.[1] ?? 0 },
          data: {
            kind: n.kind,
            config: n.config ?? {},
            inputs: entry?.inputs ?? [],
            outputs: entry?.outputs ?? [],
            restricted: entry?.restricted ?? false,
          },
        }
      })
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
      <span class="status">{deployStatus}</span>
      <button type="button" class="btn" onclick={deploy} disabled={deployBusy || nodes.length === 0}>
        <Icon name="bolt" size={14} />
        <span>{deployBusy ? 'Deploying…' : 'Deploy'}</span>
      </button>
    </div>
  </div>

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
        onselectionchange={onSelectionChange}
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

  <StrategyDeployHistory {auth} onReload={(n) => loadGraph(n)} />
</div>

<style>
  .page {
    display: flex;
    flex-direction: column;
    height: calc(100vh - 57px);
  }

  .top {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--s-3) var(--s-4);
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-raised);
    gap: var(--s-4);
  }
  .left-chunk { display: flex; gap: var(--s-3); align-items: flex-end; }
  .right-chunk { display: flex; gap: var(--s-3); align-items: center; }
  .field { display: flex; flex-direction: column; gap: 2px; font-size: var(--fs-xs); color: var(--fg-secondary); }
  .field-label { font-size: var(--fs-xs); color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .field input, .field select {
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md); color: var(--fg-primary);
    font-family: var(--font-sans); font-size: var(--fs-sm);
    min-width: 160px;
  }
  .status { font-size: var(--fs-xs); color: var(--fg-muted); font-family: var(--font-mono); }
  .btn {
    display: inline-flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-4);
    background: var(--accent-dim); color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: var(--r-md); cursor: pointer;
    font-size: var(--fs-sm); font-weight: 500;
  }
  .btn:hover:not(:disabled) { background: var(--accent); color: var(--bg-base); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }

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
