/*
 * Helpers for the strategy-deploy diff view.
 */

// Find the newest deploy record for `current.name` strictly older
// than `current.deployed_at`. Returns null if this is the first
// deploy of that name.
export function priorFor(current, entries) {
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

// Per-line diff markers zipped by index. Not a true LCS diff —
// good enough for a quick visual scan and keeps the frontend
// free of a diff library dependency.
export function diffMarkers(a, b) {
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
