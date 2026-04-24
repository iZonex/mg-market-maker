/*
 * Pure helpers for the StrategyPage graph builder.
 *
 * Kept out of the Svelte file so template loading, rollback, and
 * drag/drop all materialise nodes through the same path without
 * drift. No Svelte reactivity here — callers own state.
 */

// Simple v4 UUID without adding a dep.
export function uuid() {
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

// Build the `data` blob svelte-flow hands to StrategyNode. Every
// path that materialises a node (drag/click, template load,
// backend reload, rollback) attaches the same label/group/summary
// fields and doesn't drift.
export function nodeData(catalog, kind, config) {
  const entry = catalog.find((c) => c.kind === kind)
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

// Per-kind non-trivial default config. Schema defaults cover the
// common path; these override when a sensible starting value
// can't be expressed as a schema default alone.
export function defaultConfigFor(kind) {
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

// Serialise the canvas state into the shape the backend expects.
// Symmetric with the shape returned by GET /api/v1/strategy/graphs
// so load → edit → deploy roundtrips cleanly.
export function toBackendGraph({ nodes, edges, name, scopeKind, scopeValue }) {
  return {
    version: 1,
    name,
    scope: { kind: scopeKind, value: scopeKind === 'global' ? null : scopeValue },
    nodes: nodes.map((n) => ({
      id: n.id,
      kind: n.data.kind,
      config: n.data.config ?? null,
      pos: [n.position.x, n.position.y],
    })),
    edges: edges.map((e) => ({
      from: { node: e.source, port: e.sourceHandle ?? '' },
      to: { node: e.target, port: e.targetHandle ?? '' },
    })),
    stale_hold_ms: 30000,
  }
}

// Backend graph → svelte-flow canvas shape. Used by load-graph,
// rollback-to-deployment, and version-load paths.
export function fromBackendGraph(g, catalog) {
  return {
    name: g.name,
    scopeKind: g.scope.kind,
    scopeValue: g.scope.value ?? '',
    nodes: g.nodes.map((n) => ({
      id: n.id,
      type: 'graphNode',
      position: { x: n.pos?.[0] ?? 0, y: n.pos?.[1] ?? 0 },
      data: nodeData(catalog, n.kind, n.config),
    })),
    edges: g.edges.map((e, i) => ({
      id: `e${i}`,
      source: e.from.node,
      sourceHandle: e.from.port,
      target: e.to.node,
      targetHandle: e.to.port,
    })),
  }
}
