/*
 * Formatting helpers shared across the per-client drilldown cards.
 */

export function fmtDec(v, digits = 2) {
  if (v === null || v === undefined || v === '') return '—'
  const n = Number(v)
  if (!Number.isFinite(n)) return String(v)
  return n.toLocaleString(undefined, { maximumFractionDigits: digits })
}

// Tone the SLA percentage chip. ≥99% ok · 95-99% warn · <95% bad.
export function slaTone(pct) {
  const n = Number(pct || 0)
  if (n >= 99) return 'ok'
  if (n >= 95) return 'warn'
  return 'bad'
}

export const SLA_LEGEND = '≥99% compliant · 95–99% warning · <95% breach'

export function fmtTime(t) {
  if (!t) return '—'
  try { return new Date(t).toLocaleTimeString() } catch { return '—' }
}
