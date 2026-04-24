/**
 * Pure graph-diff utility for the save-preview modal.
 *
 * Takes two backend-shape graphs (same format POSTed to
 * `/api/v1/strategy/validate`) and returns a structured diff:
 *
 *   {
 *     addedNodes:    [{id, kind, config}],
 *     removedNodes:  [{id, kind, config}],
 *     modifiedNodes: [{id, kind, oldConfig, newConfig, kindChanged: bool}],
 *     addedEdges:    [{from, to}],
 *     removedEdges:  [{from, to}],
 *     totalChanges:  number,
 *   }
 *
 * Matching:
 *   - Nodes by `id` (UUID). A kind change on the same id counts
 *     as modified, not removed+added.
 *   - Edges by the canonical key `"${from.node}:${from.port} -> ${to.node}:${to.port}"`.
 *
 * `config` comparison uses a stable stringify so key order
 * doesn't matter. An identical graph with different canvas
 * positions reports 0 changes — positions aren't part of the
 * engine's semantic view.
 */

function stableStringify(v) {
  if (v === null || v === undefined) return 'null'
  if (typeof v !== 'object') return JSON.stringify(v)
  if (Array.isArray(v)) return '[' + v.map(stableStringify).join(',') + ']'
  const keys = Object.keys(v).sort()
  return '{' + keys.map((k) => JSON.stringify(k) + ':' + stableStringify(v[k])).join(',') + '}'
}

function edgeKey(e) {
  return `${e.from?.node}:${e.from?.port} -> ${e.to?.node}:${e.to?.port}`
}

export function computeGraphDiff(oldGraph, newGraph) {
  const oldNodes = new Map((oldGraph?.nodes ?? []).map((n) => [n.id, n]))
  const newNodes = new Map((newGraph?.nodes ?? []).map((n) => [n.id, n]))

  const addedNodes = []
  const removedNodes = []
  const modifiedNodes = []
  for (const [id, nNew] of newNodes) {
    const nOld = oldNodes.get(id)
    if (!nOld) {
      addedNodes.push({ id, kind: nNew.kind, config: nNew.config })
      continue
    }
    const kindChanged = nOld.kind !== nNew.kind
    const configChanged = stableStringify(nOld.config) !== stableStringify(nNew.config)
    if (kindChanged || configChanged) {
      modifiedNodes.push({
        id,
        kind: nNew.kind,
        oldKind: nOld.kind,
        oldConfig: nOld.config,
        newConfig: nNew.config,
        kindChanged,
        configChanged,
      })
    }
  }
  for (const [id, nOld] of oldNodes) {
    if (!newNodes.has(id)) {
      removedNodes.push({ id, kind: nOld.kind, config: nOld.config })
    }
  }

  const oldEdgeKeys = new Map((oldGraph?.edges ?? []).map((e) => [edgeKey(e), e]))
  const newEdgeKeys = new Map((newGraph?.edges ?? []).map((e) => [edgeKey(e), e]))
  const addedEdges = []
  const removedEdges = []
  for (const [k, e] of newEdgeKeys) {
    if (!oldEdgeKeys.has(k)) addedEdges.push(e)
  }
  for (const [k, e] of oldEdgeKeys) {
    if (!newEdgeKeys.has(k)) removedEdges.push(e)
  }

  return {
    addedNodes,
    removedNodes,
    modifiedNodes,
    addedEdges,
    removedEdges,
    totalChanges:
      addedNodes.length +
      removedNodes.length +
      modifiedNodes.length +
      addedEdges.length +
      removedEdges.length,
  }
}
