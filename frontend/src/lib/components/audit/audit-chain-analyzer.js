/*
 * Wave D1 — client-side hash-chain inspection of the audit tail.
 *
 * The audit JSONL carries `prev_hash` on every row
 * (risk::audit::AuditEvent). For each (agent, deployment) source
 * we walk the row sequence oldest-first:
 *
 *   - a non-first row with `prev_hash: null` → truncation
 *   - two consecutive rows sharing the same `prev_hash`
 *     → a row was deleted between them
 *
 * We annotate events with `_chain_broken` so the UI can render
 * them in a distinct band. The real SHA-256 verification runs
 * server-side (/api/v1/audit/verify); this is the cheap visual
 * pre-check that makes drift immediately visible.
 */

export function analyzeChain(events) {
  const bySource = new Map()
  for (const ev of events) {
    const key = `${ev._source_agent}#${ev._source_deployment}`
    if (!bySource.has(key)) bySource.set(key, [])
    bySource.get(key).push(ev)
  }
  const brokenKeys = new Set()
  let totalSources = 0
  let brokenSources = 0
  for (const [, list] of bySource) {
    totalSources += 1
    list.sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0))
    let thisBroken = false
    let prevPrev
    for (let i = 0; i < list.length; i++) {
      const curr = list[i]
      const k = `${curr._source_agent}#${curr._source_deployment}#${curr.seq}`
      if (i > 0 && (curr.prev_hash === null || curr.prev_hash === undefined)) {
        brokenKeys.add(k); thisBroken = true
      }
      if (i > 0 && prevPrev === curr.prev_hash) {
        brokenKeys.add(k); thisBroken = true
      }
      prevPrev = curr.prev_hash
    }
    if (thisBroken) brokenSources += 1
  }
  return {
    events: events.map((ev) => ({
      ...ev,
      _chain_broken: brokenKeys.has(
        `${ev._source_agent}#${ev._source_deployment}#${ev.seq}`,
      ),
    })),
    totalSources,
    brokenSources,
    brokenRowCount: brokenKeys.size,
  }
}

// Audit event → severity bucket for the colored dot. Pattern-
// matches against the event_type string so new event kinds don't
// need a code change — just map the word in.
export function severityFor(evtType) {
  const t = (evtType || '').toLowerCase()
  if (t.includes('kill') || t.includes('breaker') || t.includes('halt') || t.includes('fail')) return 'neg'
  if (t.includes('drift') || t.includes('resync') || t.includes('violation') || t.includes('delist') || t.includes('disconnect') || t.includes('shutdown')) return 'warn'
  if (t.includes('login') || t.includes('logout') || t.includes('escalated') || t.includes('reset')) return 'info'
  return 'muted'
}
