/*
 * Pure helpers for the pending-calibration UI — diff ordering,
 * delta percent, number formatting. Kept out of the Svelte file
 * so entry + parent can share the same formatting.
 */

// Merge suggested + current keys so the diff table has a stable
// row order and renders an em-dash when the key is missing on
// one side.
export function paramRows(p) {
  const keys = new Set([
    ...Object.keys(p.suggested || {}),
    ...Object.keys(p.current || {}),
  ])
  // Deterministic label order — a random HashMap iteration
  // order makes screenshots jump between loads.
  const order = ['gamma', 'kappa', 'sigma', 'min_spread_bps',
    'order_size', 'num_levels', 'max_distance_bps', 'max_inventory']
  const sorted = [...keys].sort((a, b) => {
    const ai = order.indexOf(a)
    const bi = order.indexOf(b)
    if (ai === -1 && bi === -1) return a.localeCompare(b)
    if (ai === -1) return 1
    if (bi === -1) return -1
    return ai - bi
  })
  return sorted.map((k) => ({
    key: k,
    current: p.current?.[k] ?? null,
    suggested: p.suggested?.[k] ?? null,
  }))
}

// Percentage delta current → suggested. Returns null when either
// side is missing or current is zero (no meaningful relative
// change). String-decimal inputs from the backend are normalised
// via Number(…).
export function deltaPct(row) {
  const c = Number(row.current)
  const n = Number(row.suggested)
  if (!Number.isFinite(c) || !Number.isFinite(n)) return null
  if (c === 0) return null
  return ((n - c) / Math.abs(c)) * 100
}

export function fmtNum(v) {
  if (v === null || v === undefined) return '—'
  const n = Number(v)
  if (!Number.isFinite(n)) return String(v)
  if (Number.isInteger(n)) return n.toString()
  return n.toFixed(4).replace(/0+$/, '').replace(/\.$/, '')
}

export function fmtLoss(v) {
  const n = Number(v)
  if (!Number.isFinite(n)) return '—'
  return n.toFixed(4)
}

export function relTimeIso(iso) {
  if (!iso) return ''
  const ms = new Date(iso).getTime()
  if (!Number.isFinite(ms)) return ''
  const deltaSec = Math.max(0, Math.floor((Date.now() - ms) / 1000))
  if (deltaSec < 60) return `${deltaSec}s ago`
  if (deltaSec < 3600) return `${Math.floor(deltaSec / 60)}m ago`
  if (deltaSec < 86400) return `${Math.floor(deltaSec / 3600)}h ago`
  return `${Math.floor(deltaSec / 86400)}d ago`
}
