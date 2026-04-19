/**
 * 23-UX-8 — centralised number + time formatting utilities.
 *
 * Before this module, every panel rolled its own `.toFixed(n)` / `new Date(ms).toLocaleTimeString()`
 * with inconsistent precision (4 dp on Controls, 6 dp on InventoryPanel for
 * the same inventory metric) and timestamp formats (Unix epoch vs ISO vs
 * relative). The operator feedback — "looks like admin debug tools vs an
 * operator cockpit" — was earned.
 *
 * Every panel that renders money, quantities, rates, or timestamps MUST
 * import from here. If a new use case needs a new format, add it here
 * rather than inlining `.toFixed(...)` in a component.
 *
 * Conventions
 * -----------
 * - Prices: 2 dp for USDT-quoted majors, 4 dp for shitcoins / altcoins,
 *   caller passes `product` or explicit dp.
 * - Quantities: 4 dp default, 6 dp for crypto sub-unit precision when
 *   caller opts in.
 * - Bps: always 2 dp. Bps is already a small fraction of a percent, more
 *   precision is noise.
 * - PnL: signed, 2 dp, explicit `+` prefix for positive. A flat zero
 *   renders as `0.00`, not `+0.00` (cosmetic — looks cleaner).
 * - Timestamps: three modes — `time` (HH:MM:SS), `datetime` (YYYY-MM-DD
 *   HH:MM:SS), `relative` (2 min ago). ISO is available via
 *   `Date#toISOString` if the caller really needs it.
 *
 * All helpers accept `null` / `undefined` / non-finite inputs and return
 * an em-dash `—` so panels never render `NaN` or `undefined` strings.
 */

/** Em-dash placeholder for missing numeric values. */
export const EMPTY = '—'

/** Parse a value that might be a Decimal string, number, or null. */
function toNumber(x) {
  if (x === null || x === undefined) return NaN
  if (typeof x === 'number') return x
  const n = parseFloat(x)
  return Number.isFinite(n) ? n : NaN
}

/** Format a price. `dp` defaults to 2 — good for USDT-quoted majors. */
export function fmtPrice(val, dp = 2) {
  const n = toNumber(val)
  if (!Number.isFinite(n)) return EMPTY
  return n.toFixed(dp)
}

/**
 * Format a quantity (base asset). Default 4 dp covers most retail venues;
 * bump to 6-8 dp for crypto sub-unit precision when the product's
 * `lot_size` demands it.
 */
export function fmtQty(val, dp = 4) {
  const n = toNumber(val)
  if (!Number.isFinite(n)) return EMPTY
  return n.toFixed(dp)
}

/** Format a basis-points value — always 2 dp, always unsigned for display. */
export function fmtBps(val, dp = 2) {
  const n = toNumber(val)
  if (!Number.isFinite(n)) return EMPTY
  return n.toFixed(dp)
}

/**
 * Format a signed PnL value. Positive prefixed with `+`; flat zero renders
 * as `0.00` (no sign). `dp` defaults to 2 dp which is right for USDT PnL.
 */
export function fmtPnl(val, dp = 2) {
  const n = toNumber(val)
  if (!Number.isFinite(n)) return EMPTY
  if (n === 0) return (0).toFixed(dp)
  const prefix = n > 0 ? '+' : ''
  return prefix + n.toFixed(dp)
}

/**
 * Generic signed number formatter. Used for inventory (can go long or
 * short), exposure deltas, factor weights. Caller picks dp.
 */
export function fmtSigned(val, dp = 4) {
  const n = toNumber(val)
  if (!Number.isFinite(n)) return EMPTY
  if (n === 0) return (0).toFixed(dp)
  const prefix = n > 0 ? '+' : ''
  return prefix + n.toFixed(dp)
}

/** Format a raw unsigned number to `dp` decimal places. */
export function fmtFixed(val, dp = 2) {
  const n = toNumber(val)
  if (!Number.isFinite(n)) return EMPTY
  return n.toFixed(dp)
}

/**
 * Format a percentage (already in percent units, i.e. `50` not `0.5`).
 * Includes the `%` suffix so callers never forget it.
 */
export function fmtPct(val, dp = 1) {
  const n = toNumber(val)
  if (!Number.isFinite(n)) return EMPTY
  return `${n.toFixed(dp)}%`
}

// ── timestamps ────────────────────────────────────────────────

/**
 * Format an epoch-milliseconds timestamp.
 * `style`:
 *   - `'time'`     (default) → `HH:MM:SS` local
 *   - `'datetime'` → `YYYY-MM-DD HH:MM:SS` local
 *   - `'iso'`      → `2026-04-19T14:23:45.123Z`
 *   - `'relative'` → `2 min ago`, `3 h ago`, `just now`
 */
export function fmtTime(ms, style = 'time') {
  if (ms === null || ms === undefined) return EMPTY
  const n = typeof ms === 'number' ? ms : parseInt(ms, 10)
  if (!Number.isFinite(n)) return EMPTY
  const d = new Date(n)
  switch (style) {
    case 'iso':
      return d.toISOString()
    case 'datetime': {
      const pad = (x) => String(x).padStart(2, '0')
      return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
    }
    case 'relative':
      return fmtRelative(n)
    case 'time':
    default: {
      const pad = (x) => String(x).padStart(2, '0')
      return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
    }
  }
}

/**
 * Relative-time formatter — "2 min ago" / "3 h ago" / "just now".
 * Uses discrete buckets rather than exact fractions so the UI doesn't
 * update every second. Forward-relative times (future) show as
 * `in 5 min` — useful for funding-rate countdowns.
 */
export function fmtRelative(ms) {
  const now = Date.now()
  const delta = now - ms
  const abs = Math.abs(delta)
  const future = delta < 0
  const prefix = future ? 'in ' : ''
  const suffix = future ? '' : ' ago'

  if (abs < 5_000) return 'just now'
  if (abs < 60_000) return `${prefix}${Math.round(abs / 1000)}s${suffix}`
  if (abs < 3_600_000) return `${prefix}${Math.round(abs / 60_000)} min${suffix}`
  if (abs < 86_400_000) return `${prefix}${Math.round(abs / 3_600_000)} h${suffix}`
  return `${prefix}${Math.round(abs / 86_400_000)} d${suffix}`
}

/**
 * Tailwind-ish semantic CSS class hint for a signed number. Panels
 * typically wrap a value with `<span class={pnlClass(val)}>` to colour
 * positive values green and negative red. Returns `''` for zero/missing
 * so the default text colour stays.
 */
export function pnlClass(val) {
  const n = toNumber(val)
  if (!Number.isFinite(n)) return ''
  if (n > 0) return 'pos'
  if (n < 0) return 'neg'
  return ''
}
