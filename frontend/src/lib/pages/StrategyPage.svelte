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
   *
   * Pure helpers + modal chrome live in strategy-graph-utils.js +
   * components/strategy/*. This file is the coordinator (state,
   * API calls, effects, unified layout).
   */

  import {
    SvelteFlow,
    Background,
    Controls,
    MiniMap,
  } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { createApiClient } from '../api.svelte.js'
  import { untrack } from 'svelte'
  import {
    createGraphLiveStore,
    edgeValuesFromTrace,
    nodeStatsFromTraces,
    formatValue,
  } from '../graphLiveStore.svelte.js'
  import Icon from '../components/Icon.svelte'
  import StrategyPalette from '../components/StrategyPalette.svelte'
  import StrategyNodeConfig from '../components/StrategyNodeConfig.svelte'
  import StrategyDeployHistory from '../components/StrategyDeployHistory.svelte'
  import StrategyNode from '../components/StrategyNode.svelte'
  import ActivePlans from '../components/ActivePlans.svelte'
  import GraphInspector from '../components/GraphInspector.svelte'
  import GraphTimeline from '../components/GraphTimeline.svelte'
  import ReplayModal from '../components/ReplayModal.svelte'
  import VersionsModal from '../components/strategy/VersionsModal.svelte'
  import SaveTemplateDialog from '../components/strategy/SaveTemplateDialog.svelte'
  import DeployTargetModal from '../components/strategy/DeployTargetModal.svelte'
  import RestrictedDeployModal from '../components/strategy/RestrictedDeployModal.svelte'
  import { Button } from '../primitives/index.js'
  import { computeGraphDiff } from '../graphDiff.js'
  import {
    uuid,
    nodeData as buildNodeData,
    defaultConfigFor,
    toBackendGraph as serializeGraph,
    fromBackendGraph,
  } from '../strategy-graph-utils.js'

  // `liveAgent` / `liveDeployment` come from App.svelte when the URL
  // carries `?live=<agentId>/<deploymentId>` — operator opened this
  // page from DeploymentDrilldown to observe the deployed graph.
  let {
    auth,
    liveAgent = null,
    liveDeployment = null,
    // M4-GOBS — `?tick=<tick_num>` deep link from Incidents.
    liveTick = null,
  } = $props()
  const api = $derived(createApiClient(auth))

  // M2-GOBS — authoring vs live mode.
  let mode = $state(
    untrack(() => (liveAgent && liveDeployment ? 'live' : 'authoring')),
  )
  let liveStore = $state(null)
  let liveTarget = $state(
    untrack(() =>
      liveAgent && liveDeployment
        ? { agentId: liveAgent, deploymentId: liveDeployment }
        : null,
    ),
  )
  // M4-GOBS — time-travel. When `pinnedTickNum` is non-null the
  // page renders THAT tick's values on the canvas instead of the
  // latest live frame.
  let pinnedTickNum = $state(liveTick != null ? Number(liveTick) : null)
  let pinWarning = $state(null)
  const liveTraces = $derived(liveStore?.state?.traces ?? [])
  const liveTickTrace = $derived(
    pinnedTickNum != null
      ? liveTraces.find((t) => t.tick_num === pinnedTickNum) ?? liveTraces[0] ?? null
      : liveTraces[0] ?? null,
  )
  // M4-long-session guard — when the pinned tick rolls off the
  // ring (operator held a pin longer than 256 ticks at ~2 Hz ≈
  // 2 min), auto-unpin and tell them.
  $effect(() => {
    if (pinnedTickNum == null) return
    if (liveTraces.length === 0) return
    const found = liveTraces.some((t) => t.tick_num === pinnedTickNum)
    if (!found) {
      pinWarning = `tick #${pinnedTickNum} rolled off the ring — released pin`
      pinnedTickNum = null
      setTimeout(() => { pinWarning = null }, 6000)
    }
  })
  let liveEdgeValues = $derived(edgeValuesFromTrace(liveTickTrace))
  let liveNodeStats = $derived(nodeStatsFromTraces(liveTraces))
  const liveTickOutputs = $derived.by(() => {
    const m = new Map()
    if (!liveTickTrace) return m
    for (const n of liveTickTrace.nodes ?? []) {
      const first = n.outputs?.[0]
      if (first) m.set(n.id, first[1])
    }
    return m
  })
  let liveGraphAnalysis = $derived(liveStore?.state?.graphAnalysis ?? null)
  let deadNodeIds = $derived(new Set(liveGraphAnalysis?.dead_nodes ?? []))

  // Canvas state.
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
  // Epic H Phase 3 — set when the operator loads a historical
  // hash from the deploy-history panel. Passed as
  // `?rollback_from=` on the next deploy so the audit row records
  // intent. Cleared after a successful deploy.
  let rollbackFrom = $state(null)
  let previewBusy = $state(false)
  // UI-6 — set when the backend returns 412 with a
  // restricted-nodes list. Operator acks + we re-issue with
  // restricted_ack=yes-pentest-mode.
  let restrictedAck = $state(null)
  let customTemplates = $state([])
  let validation = $state({
    valid: false, issues: [],
    node_count: 0, edge_count: 0, sink_count: 0,
    required_sources: [], dead_nodes: [], unconsumed_outputs: [],
  })
  let fileInput = $state(null)

  // Wrappers so call sites don't have to thread `catalog`/local
  // state through every helper — keeps the backend<->canvas
  // transforms single-line.
  const nodeData = (kind, config) => buildNodeData(catalog, kind, config)
  const toBackendGraph = () => serializeGraph({
    nodes, edges, name: graphName, scopeKind, scopeValue,
  })
  function applyLoadedGraph(g) {
    const r = fromBackendGraph(g, catalog)
    graphName = r.name
    scopeKind = r.scopeKind
    scopeValue = r.scopeValue
    nodes = r.nodes
    edges = r.edges
  }

  // Spin up / tear down the live-poll store as mode flips.
  $effect(() => {
    if (mode === 'live' && liveTarget) {
      liveStore = createGraphLiveStore(auth, liveTarget.agentId, liveTarget.deploymentId)
      return () => { liveStore?.stop?.(); liveStore = null }
    }
    liveStore = null
    return () => {}
  })

  // In Live mode, re-decorate edges whenever a new trace lands.
  $effect(() => {
    if (mode !== 'live') return
    liveStore?.state?.lastFetch
    decorateEdgesLive()
  })

  // When entering Live mode with a known deployment, pull the
  // deployment's graph onto the canvas.
  $effect(() => {
    if (mode !== 'live' || !liveTarget) return
    loadLiveGraph().catch((e) => { deployStatus = `live graph load failed: ${e}` })
  })

  async function loadLiveGraph() {
    const fleet = await api.getJson('/api/v1/fleet')
    if (!Array.isArray(fleet)) return
    const agent = fleet.find((a) => a.agent_id === liveTarget.agentId)
    const dep = agent?.deployments?.find((d) => d.deployment_id === liveTarget.deploymentId)
    const graphName = dep?.active_graph?.name
    if (!graphName) {
      deployStatus = 'live: deployment has no strategy graph attached'
      return
    }
    // In distributed mode `/strategy/graphs/:name` needs a graph
    // store which isn't wired; `/strategy/templates/:name` returns
    // the bundled graph JSON and works for every template the
    // controller knows about.
    try {
      const g = await api.getJson(
        `/api/v1/strategy/templates/${encodeURIComponent(graphName)}`,
      )
      applyLoadedGraph(g)
      // M5.2-GOBS — cache the deployed graph body so the
      // "Replay vs deployed" side-by-side canvas can render
      // the LEFT pane even after edits.
      deployedGraphSnapshot = g
      deployedGraphName = graphName
      deployStatus = `live: watching ${graphName}`
    } catch (e) {
      deployStatus = `live: failed to fetch ${graphName} (${e})`
    }
  }

  // Save dialog state.
  let saveDialog = $state({
    open: false,
    name: '',
    description: '',
    diffPreview: null,
    busy: false,
    checkBusy: false,
    error: '',
  })

  // ─── Local draft persistence ──────────────────────────────
  // Canvas work is checkpointed into localStorage on every change
  // so F5 / tab crashes never lose the WIP. Deploy is still the
  // single source of truth; draft only restores editor state.
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
  let saveScheduled = false
  $effect(() => {
    nodes.length; edges.length; graphName; scopeKind; scopeValue
    if (saveScheduled) return
    saveScheduled = true
    queueMicrotask(() => { saveScheduled = false; saveDraft() })
  })

  // ─── Live validation ──────────────────────────────────────
  //
  // Server-side is the single source of truth (same validator
  // Deploy uses) so client-side rules never silently drift.
  // Debounced 300 ms.
  let validateTimer = null
  function scheduleValidate() {
    if (validateTimer) clearTimeout(validateTimer)
    validateTimer = setTimeout(runValidate, 300)
  }
  async function runValidate() {
    if (nodes.length === 0) {
      validation = { valid: false, issues: [], node_count: 0, edge_count: 0, sink_count: 0,
        required_sources: [], dead_nodes: [], unconsumed_outputs: [] }
      return
    }
    try {
      const body = { graph: toBackendGraph() }
      validation = await api.postJson('/api/v1/strategy/validate', body)
    } catch (e) {
      validation = { ...validation, valid: false, issues: [`validator unreachable: ${e}`] }
    }
  }
  $effect(() => {
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
    // M-SAVE GOBS — stamp the filename with an ISO date so the
    // operator can tell exports apart.
    const date = new Date().toISOString().slice(0, 10)
    const stem = graphName || 'strategy-graph'
    const blob = new Blob(
      [JSON.stringify(toBackendGraph(), null, 2)],
      { type: 'application/json' },
    )
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `${stem}.${date}.json`
    a.click()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  }

  async function importGraph(ev) {
    const file = ev.target.files?.[0]
    if (!file) return
    try {
      const text = await file.text()
      const g = JSON.parse(text)
      applyLoadedGraph(g)
      deployStatus = `imported ${nodes.length} nodes from ${file.name}`
      // M-SAVE GOBS — nudge if the imported name matches a known
      // custom template; the save dialog's diff preview runs the
      // actual version check at commit time.
      const existing = customTemplates.find((t) => t.name === graphName)
      if (existing) {
        deployStatus = `imported · matches existing '${graphName}' — save will create v${(existing.version_count ?? 0) + 2}`
      }
    } catch (e) {
      deployStatus = `import failed: ${e}`
    } finally {
      ev.target.value = ''
    }
  }

  // ─── Save as custom template ─────────────────────────────
  //
  // Two-phase when the target name already has versions on disk:
  //   phase 1: fetch existing latest graph + compute diff
  //   phase 2: operator confirms → actually POST
  // Brand new name goes straight to phase 2.

  function openSaveDialog() {
    saveDialog = {
      open: true,
      name: graphName && graphName !== 'untitled' ? graphName : '',
      description: '',
      diffPreview: null,
      busy: false,
      checkBusy: false,
      error: '',
    }
  }

  async function onSaveClick() {
    if (saveDialog.diffPreview) {
      await commitSaveTemplate()
      return
    }
    saveDialog = { ...saveDialog, checkBusy: true, error: '' }
    try {
      const name = saveDialog.name.trim()
      if (!name) {
        saveDialog = { ...saveDialog, error: 'name is required' }
        return
      }
      let existing = null
      try {
        existing = await api.getJson(
          `/api/v1/strategy/custom_templates/${encodeURIComponent(name)}`,
        )
      } catch {
        // 404 is expected for a brand-new name.
      }
      if (!existing?.graph) {
        await commitSaveTemplate()
        return
      }
      saveDialog = {
        ...saveDialog,
        diffPreview: { existing, diff: computeGraphDiff(existing.graph, toBackendGraph()) },
      }
    } finally {
      saveDialog = { ...saveDialog, checkBusy: false }
    }
  }

  async function commitSaveTemplate() {
    saveDialog = { ...saveDialog, busy: true, error: '' }
    try {
      const resp = await api.postJson('/api/v1/strategy/custom_templates', {
        name: saveDialog.name.trim(),
        description: saveDialog.description.trim(),
        graph: toBackendGraph(),
      })
      const ver = resp?.version
      deployStatus = ver
        ? `saved '${saveDialog.name.trim()}' (v${ver})`
        : `saved template '${saveDialog.name.trim()}'`
      saveDialog = { ...saveDialog, open: false, diffPreview: null }
      await loadCustomTemplates()
    } catch (e) {
      saveDialog = { ...saveDialog, error: String(e) }
    } finally {
      saveDialog = { ...saveDialog, busy: false }
    }
  }

  function closeSaveDialog() {
    saveDialog = { ...saveDialog, open: false, diffPreview: null }
  }

  // ─── Version history ─────────────────────────────────────
  let versionsModal = $state(null)
  let versionsBusy = $state(false)

  const currentIsCustomTemplate = $derived.by(() =>
    customTemplates.some((t) => t.name === graphName),
  )

  async function openVersionsModal() {
    if (!currentIsCustomTemplate) return
    versionsBusy = true
    try {
      const resp = await api.getJson(
        `/api/v1/strategy/custom_templates/${encodeURIComponent(graphName)}`,
      )
      versionsModal = { name: graphName, history: resp?.history ?? [] }
    } catch (e) {
      deployStatus = `history fetch failed: ${e}`
    } finally {
      versionsBusy = false
    }
  }

  async function loadVersion(hash) {
    if (!versionsModal) return
    try {
      const g = await api.getJson(
        `/api/v1/strategy/custom_templates/${encodeURIComponent(versionsModal.name)}/versions/${encodeURIComponent(hash)}`,
      )
      applyLoadedGraph(g)
      versionsModal = null
      deployStatus = `loaded '${graphName}' @ ${hash.slice(0, 8)}`
    } catch (e) {
      deployStatus = `version load failed: ${e}`
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
    } catch {
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
    // `custom:<name>` → user-saved template on disk; hydrates
    // from the full record (graph + metadata) rather than the
    // bundled endpoint.
    const isCustom = name.startsWith('custom:')
    const realName = isCustom ? name.slice('custom:'.length) : name
    try {
      let g
      if (isCustom) {
        const rec = await api.getJson(
          `/api/v1/strategy/custom_templates/${encodeURIComponent(realName)}`,
        )
        g = rec.graph
      } else {
        g = await api.getJson(
          `/api/v1/strategy/templates/${encodeURIComponent(realName)}`,
        )
      }
      applyLoadedGraph(g)
      deployStatus = `loaded template '${realName}'`
    } catch (e) {
      deployStatus = `template load failed: ${e}`
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

  // ─── Unified deploy flow (Wave A3) ───────────────────────
  let deployTargetModal = $state(null)

  async function saveGraph(ackToken = null) {
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
        // UI-6 — restricted deploy awaiting operator ack.
        const payload = await resp.json().catch(() => ({}))
        restrictedAck = {
          nodes: payload?.restricted_nodes ?? [],
          acknowledged: false,
          busy: false,
          error: '',
        }
        deployStatus = 'restricted deploy — operator ack required'
        return null
      }
      if (!resp.ok) {
        const text = await resp.text().catch(() => '')
        throw new Error(`${resp.status} ${text}`)
      }
      const r = await resp.json().catch(() => ({}))
      deployStatus = `saved · hash ${r.hash?.slice(0, 12)}… — pick a deployment to apply`
      rollbackFrom = null
      restrictedAck = null
      return { hash: r.hash, body }
    } catch (e) {
      deployStatus = `save failed: ${e}`
      return null
    } finally {
      deployBusy = false
    }
  }

  // Open the unified deploy modal. Fetches fleet, renders a
  // multi-select target list. "Confirm" saves the graph and
  // fan-outs graph-swap to every selected (agent, deployment)
  // in parallel. One confirmation = one deploy.
  async function deploy() {
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const rows = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        if (a.approval_state && a.approval_state !== 'accepted') continue
        for (const d of a.deployments || []) {
          const key = `${a.agent_id}/${d.deployment_id}`
          rows.push({
            agent: a,
            deployment: d,
            key,
            running: !!d.running,
            current_hash: d.active_graph?.hash || null,
          })
        }
      }
      deployTargetModal = {
        rows,
        selected: {},
        phase: 'select',
        results: [],
        status: rows.length === 0
          ? 'No running deployments on any accepted agent. Launch one via Fleet → Deploy first.'
          : '',
      }
    } catch (e) {
      deployStatus = `fleet fetch failed: ${e.message || e}`
    }
  }

  function toggleTarget(key) {
    if (!deployTargetModal) return
    const next = { ...deployTargetModal.selected }
    if (next[key]) delete next[key]
    else next[key] = true
    deployTargetModal = { ...deployTargetModal, selected: next }
  }

  // Confirm — saves once, then fan-outs graph-swap per selected
  // row. Failures on one row don't block the rest.
  async function confirmDeploy(ackToken = null) {
    if (!deployTargetModal) return
    const targets = deployTargetModal.rows.filter((r) => deployTargetModal.selected[r.key])
    if (targets.length === 0) return
    deployTargetModal = {
      ...deployTargetModal,
      phase: 'dispatching',
      results: targets.map((r) => ({ key: r.key, phase: 'pending', detail: '' })),
      status: 'saving graph…',
    }
    const saved = await saveGraph(ackToken)
    if (!saved) {
      deployTargetModal = { ...deployTargetModal, phase: 'select', status: deployStatus || 'save failed' }
      return
    }
    const hash = saved.hash
    deployTargetModal = {
      ...deployTargetModal,
      status: `graph saved · ${hash?.slice(0, 12)}… · dispatching to ${targets.length} target${targets.length === 1 ? '' : 's'}`,
    }
    const settled = await Promise.all(targets.map(async (row) => {
      try {
        const path = `/api/v1/agents/${encodeURIComponent(row.agent.agent_id)}`
          + `/deployments/${encodeURIComponent(row.deployment.deployment_id)}/ops/graph-swap`
        const r = await api.authedFetch(path, {
          method: 'POST',
          body: JSON.stringify({ graph: saved.body }),
        })
        if (!r.ok) {
          const text = await r.text().catch(() => '')
          return { key: row.key, phase: 'err', detail: `${r.status} ${text}` }
        }
        return { key: row.key, phase: 'ok', detail: '' }
      } catch (e) {
        return { key: row.key, phase: 'err', detail: e?.message || String(e) }
      }
    }))
    const okCount = settled.filter((s) => s.phase === 'ok').length
    deployStatus = `graph ${hash?.slice(0, 12)}… · ${okCount}/${targets.length} target(s) applied`
    deployTargetModal = { ...deployTargetModal, phase: 'done', results: settled, status: deployStatus }
  }

  // Restricted-graph ack path: saveGraph returned 412, flipped
  // `restrictedAck`. Operator confirms → re-run confirmDeploy
  // with the pentest-mode token on the same selected targets.
  async function confirmRestrictedDeploy() {
    if (!restrictedAck?.acknowledged) return
    restrictedAck = { ...restrictedAck, busy: true, error: '' }
    await confirmDeploy('yes-pentest-mode')
    if (restrictedAck) {
      restrictedAck = { ...restrictedAck, busy: false, error: deployStatus }
    }
  }

  async function simulate() {
    previewBusy = true
    previewResult = null
    try {
      const body = { graph: toBackendGraph(), source_inputs: {} }
      previewResult = await api.postJson('/api/v1/strategy/preview', body)
      decorateEdges()
    } catch (e) {
      previewResult = { errors: [String(e)], edges: {}, sinks: [] }
    } finally {
      previewBusy = false
    }
  }

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

  // M2-GOBS — Live-mode edge decoration. Same svelte-flow label
  // path as preview, but values come from the `TickTrace`
  // returned over `graph_trace_recent`. Wrap `edges` read in
  // `untrack` so the self-write doesn't re-enter the effect.
  function decorateEdgesLive() {
    const lookup = liveEdgeValues
    const deadIds = deadNodeIds
    const current = untrack(() => edges)
    edges = current.map((e) => {
      const key = `${e.source}:${e.sourceHandle}`
      const label = lookup[key]
      const isDead = deadIds.has(e.source) || deadIds.has(e.target)
      if (label !== undefined) {
        return {
          ...e,
          label,
          labelStyle: 'font-family: var(--font-mono); font-size: 10px; fill: var(--fg-primary);',
          labelBgStyle: 'fill: var(--bg-raised); stroke: var(--accent); stroke-width: 1;',
          labelBgPadding: [4, 2],
          labelBgBorderRadius: 4,
          style: isDead ? 'stroke: var(--danger); stroke-dasharray: 4 3;' : undefined,
        }
      }
      return {
        ...e,
        label: undefined,
        style: isDead ? 'stroke: var(--danger); stroke-dasharray: 4 3;' : undefined,
      }
    })
  }

  // M2-GOBS — push per-node Live data onto `node.data` so
  // StrategyNode can render the badge / pulse / status chip /
  // dead border without plumbing a separate store through
  // svelte-flow's custom node slot.
  $effect(() => {
    const activeMode = mode
    const stats = liveNodeStats
    const analysis = liveGraphAnalysis
    const tickOutputs = liveTickOutputs
    untrack(() => {
      if (activeMode !== 'live') {
        let needsReset = false
        for (const n of nodes) {
          if (n.data?.live) { needsReset = true; break }
        }
        if (!needsReset) return
        nodes = nodes.map((n) => {
          if (!n.data?.live) return n
          const { live: _drop, ...rest } = n.data
          return { ...n, data: rest }
        })
        return
      }
      const dead = new Set(analysis?.dead_nodes ?? [])
      const required = new Set(analysis?.required_sources ?? [])
      nodes = nodes.map((n) => {
        const row = stats.get(n.id)
        const pinnedOut = tickOutputs.get(n.id) ?? null
        const fallbackOut = row && row.history.length > 0
          ? row.history[row.history.length - 1]
          : null
        const display = pinnedOut ?? fallbackOut
        const live = {
          latest: formatValue(display),
          status: row?.lastStatus ?? null,
          hitRate: row?.hitRate ?? 0,
          dead: dead.has(n.id),
          dormant:
            (n.data?.inputs?.length ?? 0) === 0 &&
            required.size > 0 &&
            !required.has(n.data?.kind),
          tickCount: row?.fired ?? 0,
        }
        return { ...n, data: { ...n.data, live } }
      })
    })
  })

  async function loadGraph(name) {
    try {
      const g = await api.getJson(`/api/v1/strategy/graphs/${encodeURIComponent(name)}`)
      applyLoadedGraph(g)
      deployStatus = `loaded '${name}'`
    } catch (e) {
      deployStatus = `load failed: ${e}`
    }
  }

  // Load an older deployed hash onto the canvas. Used by the
  // deploy-history panel for both straight rollback (operator
  // clicks Deploy to apply) and rollback-to-deployment (fires
  // deploy() immediately).
  async function loadHistoricalHash(name, hash) {
    const g = await api.getJson(
      `/api/v1/strategy/graphs/${encodeURIComponent(name)}/history/${encodeURIComponent(hash)}`,
    )
    applyLoadedGraph(g)
    rollbackFrom = hash
  }

  function onSelectionChange(evt) {
    selected = evt.nodes?.[0] ?? null
  }
  function updateSelectedConfig(cfg) {
    if (!selected) return
    const id = selected.id
    nodes = nodes.map((n) =>
      n.id === id ? { ...n, data: { ...n.data, config: cfg } } : n,
    )
    selected = nodes.find((n) => n.id === id) ?? null
  }

  function deleteSelected() {
    if (!selected) return
    const id = selected.id
    nodes = nodes.filter((n) => n.id !== id)
    edges = edges.filter((e) => e.source !== id && e.target !== id)
    selected = null
  }

  // ─── Replay vs deployed (M5-GOBS) ────────────────────────
  let replayBusy = $state(false)
  let replayResult = $state(null)
  let deployedGraphSnapshot = $state(null)
  let deployedGraphName = $state(null)
  let candidateGraphForReplay = $state(null)
  async function runReplay() {
    if (replayBusy || !liveTarget) return
    replayBusy = true
    replayResult = null
    candidateGraphForReplay = toBackendGraph()
    // Safety-net re-fetch — if the operator never flipped into
    // Live mode, `loadLiveGraph` never ran so the cache is empty.
    if (!deployedGraphSnapshot) {
      try {
        const fleet = await api.getJson('/api/v1/fleet')
        if (Array.isArray(fleet)) {
          const agent = fleet.find((a) => a.agent_id === liveTarget.agentId)
          const dep = agent?.deployments?.find(
            (d) => d.deployment_id === liveTarget.deploymentId,
          )
          const name = dep?.active_graph?.name
          if (name) {
            deployedGraphSnapshot = await api.getJson(
              `/api/v1/strategy/templates/${encodeURIComponent(name)}`,
            )
            deployedGraphName = name
          }
        }
      } catch (_e) {
        // Swallow — side-by-side pane hides its left half.
      }
    }
    try {
      const url =
        `/api/v1/agents/${encodeURIComponent(liveTarget.agentId)}` +
        `/deployments/${encodeURIComponent(liveTarget.deploymentId)}/replay`
      const resp = await api.postJson(url, {
        candidate_graph: candidateGraphForReplay,
        ticks: 20,
      })
      replayResult = resp?.payload ?? {
        summary: 'empty replay response',
        ticks_replayed: 0, divergence_count: 0,
        divergences: [], candidate_issues: [],
      }
    } catch (e) {
      replayResult = {
        summary: `replay failed: ${e}`,
        ticks_replayed: 0, divergence_count: 0,
        divergences: [], candidate_issues: [String(e)],
      }
    } finally {
      replayBusy = false
    }
  }
  function closeReplay() {
    replayResult = null
    candidateGraphForReplay = null
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
      <Button variant="ghost" size="sm" iconOnly onclick={exportGraph} disabled={nodes.length === 0} title="Download graph as JSON">
        {#snippet children()}<Icon name="download" size={14} />{/snippet}
      </Button>
      <Button variant="ghost" size="sm" iconOnly onclick={() => fileInput?.click()} title="Import graph from JSON file">
        {#snippet children()}<Icon name="upload" size={14} />{/snippet}
      </Button>
      <input type="file" accept="application/json" bind:this={fileInput} onchange={importGraph} style="display: none" />
      <Button variant="ghost" size="sm" iconOnly onclick={openSaveDialog} disabled={nodes.length === 0} title="Save as reusable template">
        {#snippet children()}<Icon name="save" size={14} />{/snippet}
      </Button>
      {#if currentIsCustomTemplate}
        <Button variant="ghost" size="sm" onclick={openVersionsModal} disabled={versionsBusy} title={`Browse saved versions of '${graphName}' and load any older revision.`}>
          {#snippet children()}<Icon name="history" size={14} /><span>{versionsBusy ? '…' : 'Versions'}</span>{/snippet}
        </Button>
      {/if}
      <Button variant="ghost" size="sm" onclick={simulate} disabled={previewBusy || nodes.length === 0 || mode === 'live'} title="Evaluate graph without deploying">
        {#snippet children()}<Icon name="pulse" size={14} /><span>{previewBusy ? 'Simulating…' : 'Simulate'}</span>{/snippet}
      </Button>
      {#if liveTarget && mode === 'authoring'}
        <Button variant="ghost" size="sm" onclick={runReplay} disabled={replayBusy || nodes.length === 0} title={`Replay this canvas against the last 20 ticks of ${liveTarget.agentId}/${liveTarget.deploymentId} and count where the sink actions diverge.`}>
          {#snippet children()}<Icon name="history" size={14} /><span>{replayBusy ? 'Replaying…' : 'Replay vs deployed'}</span>{/snippet}
        </Button>
      {/if}
      <Button variant="primary" size="sm" onclick={deploy} disabled={deployBusy || nodes.length === 0 || !validation.valid || mode === 'live'}>
        {#snippet children()}<Icon name="bolt" size={14} /><span>{deployBusy ? 'Deploying…' : 'Deploy'}</span>{/snippet}
      </Button>
      <div class="mode-toggle" role="tablist" aria-label="Editor mode">
        <button type="button" class="mode-btn" class:active={mode === 'authoring'} role="tab" aria-selected={mode === 'authoring'} onclick={() => (mode = 'authoring')}>
          Authoring
        </button>
        <button type="button" class="mode-btn" class:active={mode === 'live'} role="tab" aria-selected={mode === 'live'} disabled={!liveTarget} title={liveTarget ? 'Watch deployed graph live' : 'Open from Fleet → deployment → Open graph (live)'} onclick={() => (mode = 'live')}>
          <span class={mode === 'live' ? 'live-pulse' : ''}></span>
          Live
        </button>
      </div>
    </div>
  </div>

  {#if pinWarning}
    <div class="pin-warning" role="status">
      <Icon name="alert" size={12} />
      <span>{pinWarning}</span>
      <button type="button" class="pin-warning-close" onclick={() => (pinWarning = null)} aria-label="Dismiss">×</button>
    </div>
  {/if}

  <!-- Validation strip: live counters + issue list, server-side
       authoritative. Green pill = Evaluator::build succeeded + no
       dangling edges; red pill lists every blocker so the
       operator fixes them before noticing the disabled Deploy. -->
  <div class="validate-bar" class:valid={validation.valid} class:invalid={!validation.valid && nodes.length > 0}>
    {#if nodes.length === 0}
      <span class="v-pill muted"><span class="dot"></span> empty</span>
      <span class="v-hint">drag a node from the palette to start</span>
    {:else if validation.valid}
      <span class="v-pill ok"><span class="dot"></span> ready</span>
      {#if rollbackFrom}
        <span class="v-pill rollback" title={`Deploy will be recorded as a rollback to ${rollbackFrom}`}>
          <span class="dot"></span> rollback → @{rollbackFrom.slice(0, 8)}
          <button type="button" class="v-pill-clear" onclick={() => { rollbackFrom = null; deployStatus = 'rollback cleared' }} aria-label="Clear rollback">×</button>
        </span>
      {/if}
      <span class="v-stats">
        {validation.node_count} nodes · {validation.edge_count} edges · {validation.sink_count} sinks
      </span>
      {#if validation.dead_nodes.length > 0}
        <span class="v-pill bad" title="nodes with no path to any sink — dead branches">
          <span class="dot"></span> {validation.dead_nodes.length} dead
        </span>
      {/if}
      {#if validation.unconsumed_outputs.length > 0}
        <span class="v-pill warn" title="output ports never consumed — wire them or drop the node">
          <span class="dot"></span> {validation.unconsumed_outputs.length} unconsumed
        </span>
      {/if}
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

  <VersionsModal
    state={versionsModal}
    onLoadVersion={loadVersion}
    onClose={() => (versionsModal = null)}
  />

  <ReplayModal
    result={replayResult}
    deployedGraph={deployedGraphSnapshot}
    {deployedGraphName}
    candidateGraph={candidateGraphForReplay}
    onClose={closeReplay}
  />

  <SaveTemplateDialog
    open={saveDialog.open}
    state={saveDialog}
    onNameChange={(v) => (saveDialog = { ...saveDialog, name: v })}
    onDescriptionChange={(v) => (saveDialog = { ...saveDialog, description: v })}
    onSave={onSaveClick}
    onClose={closeSaveDialog}
  />

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
      <StrategyPalette
        {catalog}
        requiredSources={validation.required_sources}
        onAdd={(k) => addNode(k, { x: 120, y: 120 })}
      />
    </aside>

    <section class="canvas" ondrop={onDrop} ondragover={onDragOver} aria-label="Strategy graph canvas">
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
      {#if mode === 'live'}
        <GraphInspector
          node={selected}
          stats={liveNodeStats}
          graphAnalysis={liveGraphAnalysis}
          traces={liveStore?.state?.traces ?? []}
          lastFetch={liveStore?.state?.lastFetch ?? null}
          error={liveStore?.state?.error ?? null}
          onReturnToAuthoring={() => (mode = 'authoring')}
        />
      {:else}
        <StrategyNodeConfig
          node={selected}
          onUpdate={updateSelectedConfig}
          onDelete={deleteSelected}
        />
      {/if}
    </aside>
  </div>

  {#if mode === 'live'}
    <GraphTimeline
      traces={liveTraces}
      pinnedTickNum={pinnedTickNum}
      onPin={(n) => (pinnedTickNum = n)}
      onUnpin={() => (pinnedTickNum = null)}
    />
  {/if}

  <StrategyDeployHistory
    {auth}
    onReload={(n) => loadGraph(n)}
    onRollbackToDeployment={async (name, hash) => {
      try {
        await loadHistoricalHash(name, hash)
        await deploy()
      } catch (e) {
        deployStatus = `rollback-to-deployment failed: ${e}`
      }
    }}
    onRollback={async (name, hash) => {
      try {
        await loadHistoricalHash(name, hash)
        deployStatus = `loaded hash ${hash.slice(0, 8)}… — click Deploy to roll back`
      } catch (e) {
        deployStatus = `rollback fetch failed: ${e}`
      }
    }}
  />

  <div class="plans-footer">
    <ActivePlans {auth} />
  </div>

  <DeployTargetModal
    state={deployTargetModal}
    {deployBusy}
    onToggleTarget={toggleTarget}
    onConfirm={() => confirmDeploy()}
    onClose={() => (deployTargetModal = null)}
  />

  <RestrictedDeployModal
    state={restrictedAck}
    onAckChange={(v) => { if (restrictedAck) restrictedAck = { ...restrictedAck, acknowledged: v } }}
    onConfirm={confirmRestrictedDeploy}
    onClose={() => (restrictedAck = null)}
  />
</div>

<style>
  .page {
    display: flex; flex-direction: column;
    height: calc(100vh - 57px);
    background: var(--bg-base);
  }

  .top {
    display: flex; align-items: center; justify-content: space-between;
    padding: var(--s-3) var(--s-4);
    background: var(--bg-raised);
    border-bottom: 1px solid var(--border-subtle);
    gap: var(--s-3);
    flex-wrap: wrap;
  }
  .left-chunk, .right-chunk {
    display: flex; align-items: center; gap: var(--s-3);
    flex-wrap: wrap;
  }
  .field { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .field.stacked { width: 100%; }
  .field-label {
    font-size: 10px; color: var(--fg-muted);
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .field input,
  .field select {
    padding: 4px 8px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    font-family: var(--font-mono); font-size: var(--fs-xs);
    outline: none;
    max-width: 180px;
  }
  .field input:focus, .field select:focus { border-color: var(--accent); }
  .field-hidden { visibility: hidden; }

  .mode-toggle {
    display: inline-flex; gap: 0;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    padding: 2px;
    margin-left: var(--s-2);
  }
  .mode-btn {
    padding: 4px 10px;
    font-size: var(--fs-xs);
    background: transparent; border: 0;
    color: var(--fg-muted);
    cursor: pointer;
    border-radius: var(--r-sm);
    display: inline-flex; align-items: center; gap: 6px;
  }
  .mode-btn.active {
    background: var(--bg-raised);
    color: var(--fg-primary);
    box-shadow: 0 0 0 1px var(--border-subtle);
  }
  .mode-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .mode-btn .live-pulse {
    width: 6px; height: 6px;
    border-radius: 50%;
    background: var(--ok);
    animation: livePulse 1.6s ease-in-out infinite;
  }
  @keyframes livePulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.4; transform: scale(0.6); }
  }

  .pin-warning {
    display: flex; gap: var(--s-2); align-items: center;
    padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    border-bottom: 1px solid color-mix(in srgb, var(--warn) 25%, transparent);
    color: var(--warn);
    font-size: var(--fs-xs);
  }
  .pin-warning-close {
    margin-left: auto; background: transparent; border: 0;
    color: inherit; cursor: pointer; font-size: var(--fs-md);
  }

  .validate-bar {
    display: flex; flex-wrap: wrap; gap: var(--s-2); align-items: center;
    padding: var(--s-2) var(--s-4);
    background: var(--bg-raised);
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--fs-xs);
  }
  .v-pill {
    display: inline-flex; gap: 4px; align-items: center;
    padding: 2px 8px;
    border-radius: var(--r-pill);
    font-family: var(--font-mono); font-size: 10px; font-weight: 600;
    letter-spacing: var(--tracking-label); text-transform: uppercase;
  }
  .v-pill .dot { width: 5px; height: 5px; border-radius: 50%; background: currentColor; }
  .v-pill.ok  { color: var(--ok);    background: color-mix(in srgb, var(--ok) 12%, transparent); }
  .v-pill.bad { color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); }
  .v-pill.warn { color: var(--warn); background: color-mix(in srgb, var(--warn) 12%, transparent); }
  .v-pill.rollback { color: var(--warn); background: color-mix(in srgb, var(--warn) 15%, transparent); border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent); }
  .v-pill-clear {
    background: transparent; border: 0; color: inherit;
    cursor: pointer; padding: 0 0 0 4px;
  }
  .v-pill.muted { color: var(--fg-muted); }
  .v-hint, .v-stats, .v-status { color: var(--fg-muted); }
  .v-status { margin-left: auto; font-family: var(--font-mono); }
  .v-issues { display: flex; flex-wrap: wrap; gap: 4px; }
  .v-issue {
    font-family: var(--font-mono); font-size: 10px;
    padding: 1px 6px;
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    color: var(--danger); border-radius: var(--r-sm);
  }

  .preview-bar {
    display: flex; flex-wrap: wrap; gap: var(--s-2); align-items: center;
    padding: var(--s-2) var(--s-4);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    border-bottom: 1px solid color-mix(in srgb, var(--accent) 25%, transparent);
    font-size: var(--fs-xs);
  }
  .preview-bar.has-error {
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    border-bottom-color: color-mix(in srgb, var(--danger) 25%, transparent);
  }
  .preview-label {
    font-family: var(--font-mono); color: var(--fg-primary);
    text-transform: uppercase; font-size: 10px; letter-spacing: var(--tracking-label); font-weight: 600;
  }
  .preview-sink, .preview-err {
    font-family: var(--font-mono); font-size: 10px;
    padding: 1px 6px; border-radius: var(--r-sm);
    background: var(--bg-chip);
  }
  .preview-err { color: var(--danger); }
  .preview-close {
    margin-left: auto; background: transparent; border: 0;
    color: inherit; cursor: pointer; font-size: var(--fs-md);
  }

  .body {
    flex: 1; min-height: 0;
    display: grid;
    grid-template-columns: 260px 1fr 300px;
    gap: 0;
  }
  .palette { border-right: 1px solid var(--border-subtle); overflow-y: auto; }
  .canvas { position: relative; background: var(--bg-base); }
  .canvas .empty {
    position: absolute; top: 50%; left: 50%;
    transform: translate(-50%, -50%);
    color: var(--fg-muted); font-size: var(--fs-sm);
    text-align: center; pointer-events: none; z-index: 1;
  }
  .canvas .empty code {
    font-family: var(--font-mono);
    background: var(--bg-chip);
    padding: 0 4px; border-radius: 3px; color: var(--fg-primary);
  }
  .config { border-left: 1px solid var(--border-subtle); overflow-y: auto; }

  .plans-footer {
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-raised);
  }
</style>
