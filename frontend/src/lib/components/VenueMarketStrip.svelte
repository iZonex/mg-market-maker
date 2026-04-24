<script>
  /*
   * Per-venue market-state strip.
   *
   * Polls /api/v1/venues/book_state and renders one row per
   * (venue, product, symbol) with mid / spread / feed age. Purpose
   * — from the MM-expert audit's "UI is unreadable when 3 venues
   * are live": the operator needs to see what price each venue is
   * publishing at a glance, BEFORE drilling into the basis /
   * cross-venue / funding panels.
   *
   * Complements the single-regime chip at the top of Overview
   * (which is primary-venue-only today): if mid on Binance perp
   * diverges from spot, the divergence shows up here first.
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 2_000

  let rows = $state([])
  let error = $state(null)
  let loading = $state(true)
  let lastFetch = $state(null)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/venues/book_state')
      rows = Array.isArray(data) ? data : []
      error = null
      lastFetch = new Date()
      loading = false
    } catch (e) {
      error = e?.message || String(e)
      loading = false
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(t)
  })

  function fmtMid(v) {
    if (v === null || v === undefined) return '—'
    const n = parseFloat(v)
    if (!Number.isFinite(n)) return '—'
    // Dynamic precision: keep 5 sig figs, no trailing zero spam.
    if (n >= 1000) return n.toLocaleString(undefined, { maximumFractionDigits: 2 })
    if (n >= 1)    return n.toFixed(3)
    return n.toPrecision(5)
  }

  function fmtSpread(v) {
    if (v === null || v === undefined) return '—'
    const n = parseFloat(v)
    if (!Number.isFinite(n)) return '—'
    return n.toFixed(2)
  }

  function ageLabel(ms) {
    if (ms === null || ms === undefined) return { text: '—', sev: 'muted' }
    if (ms < 1_500)   return { text: 'live',           sev: 'ok' }
    if (ms < 5_000)   return { text: `${(ms / 1000).toFixed(1)}s`, sev: 'ok' }
    if (ms < 15_000)  return { text: `${Math.round(ms / 1000)}s`, sev: 'info' }
    if (ms < 60_000)  return { text: `${Math.round(ms / 1000)}s`, sev: 'warn' }
    return { text: `${Math.round(ms / 1000)}s`, sev: 'bad' }
  }

  function productChip(p) {
    switch ((p || '').toLowerCase()) {
      case 'spot':        return { text: 'SPOT', cls: 'chip-info' }
      case 'linear_perp':
      case 'linearperp':  return { text: 'PERP', cls: 'chip-pos' }
      case 'inverse_perp':
      case 'inverseperp': return { text: 'INV-PERP', cls: 'chip-warn' }
      default:            return { text: (p || '—').toUpperCase(), cls: '' }
    }
  }

  // UX-VENUE-2 — per-venue regime chip. Colour-codes match the
  // Overview's primary market-quality card so operators read
  // "spot Quiet vs perp Volatile" at a glance without context-
  // switching between panels.
  function regimeChip(label, ageMs) {
    if (!label) return null
    const stale = ageMs != null && ageMs > 10_000
    const key = String(label).toLowerCase()
    let cls = 'regime-quiet'
    let text = label
    switch (key) {
      case 'quiet':         cls = 'regime-quiet';    text = 'QUIET';    break
      case 'trending':      cls = 'regime-trending'; text = 'TREND';    break
      case 'volatile':      cls = 'regime-volatile'; text = 'VOL';      break
      case 'meanreverting': cls = 'regime-mr';       text = 'MR';       break
      default:              cls = '';                text = label.toUpperCase()
    }
    return { text, cls, stale }
  }
</script>

<div class="strip">
  <div class="head">
    <span class="title">Per-venue market state</span>
    <span class="meta">
      {#if error}
        <span class="err">error: {error}</span>
      {:else if loading}
        <span class="muted">loading…</span>
      {:else}
        <span class="muted">
          {rows.length} feed{rows.length === 1 ? '' : 's'}
          {#if lastFetch}· {lastFetch.toLocaleTimeString()}{/if}
        </span>
      {/if}
    </span>
  </div>

  {#if !loading && rows.length === 0}
    <div class="empty">no L1 feed data yet — venues still connecting</div>
  {:else}
    <div class="rows">
      {#each rows as r (r.venue + '|' + r.product + '|' + r.symbol)}
        {@const age = ageLabel(r.age_ms)}
        {@const p = productChip(r.product)}
        {@const rg = regimeChip(r.regime, r.regime_age_ms)}
        <div class="row">
          <span class="venue">{r.venue}</span>
          <span class="chip {p.cls}">{p.text}</span>
          <span class="sym">{r.symbol}</span>
          <span class="mid mono">{fmtMid(r.mid)}</span>
          <span class="spread mono">
            <span class="lbl">spr</span>
            <span>{fmtSpread(r.spread_bps)}<span class="unit">bps</span></span>
          </span>
          {#if rg}
            <span class="regime {rg.cls}" class:stale={rg.stale}
                  title="Regime classifier for this venue's mid stream">
              {rg.text}
            </span>
          {:else}
            <span class="regime muted" title="Regime classifier warming up">—</span>
          {/if}
          <span class="age" data-sev={age.sev}>{age.text}</span>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .strip {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-elev-1, var(--bg-chip));
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: var(--fs-xs);
  }
  .title { font-weight: 600; color: var(--fg-primary); }
  .meta  { color: var(--fg-muted); }
  .meta .err { color: var(--danger); }
  .empty {
    color: var(--fg-muted);
    font-size: var(--fs-xs);
    padding: var(--s-3);
    text-align: center;
  }
  .rows { display: flex; flex-direction: column; gap: 2px; }
  .row {
    display: grid;
    grid-template-columns: 90px 70px 1fr auto auto auto auto;
    gap: var(--s-3);
    align-items: baseline;
    padding: var(--s-1) var(--s-2);
    font-size: var(--fs-xs);
    border-radius: var(--r-sm);
  }
  .row:hover { background: var(--bg-chip); }
  .venue { font-family: var(--font-mono); color: var(--fg-secondary); font-weight: 500; }
  .sym   { font-family: var(--font-mono); color: var(--fg-muted); }
  .mid   { font-weight: 600; color: var(--fg-primary); font-variant-numeric: tabular-nums; }
  .spread {
    display: inline-flex;
    gap: 4px;
    align-items: baseline;
    color: var(--fg-secondary);
  }
  .spread .lbl { color: var(--fg-muted); font-size: 10px; }
  .spread .unit { color: var(--fg-muted); font-size: 10px; margin-left: 2px; }
  .age {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 6px;
    border-radius: var(--r-pill);
    background: var(--bg-chip);
    color: var(--fg-secondary);
  }
  .age[data-sev='ok']   { background: var(--pos-bg, rgba(16, 185, 129, 0.10)); color: var(--pos); }
  .age[data-sev='info'] { background: var(--bg-chip); color: var(--fg-secondary); }
  .age[data-sev='warn'] { background: var(--warn-bg); color: var(--warn); font-weight: 600; }
  .age[data-sev='bad']  { background: var(--danger-bg, rgba(239, 68, 68, 0.12)); color: var(--danger); font-weight: 600; }  .regime {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 6px;
    border-radius: var(--r-pill);
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    background: var(--bg-chip);
    color: var(--fg-secondary);
    text-align: center;
    min-width: 44px;
  }
  .regime.muted       { color: var(--fg-muted); }
  .regime.regime-quiet    { background: var(--pos-bg, rgba(16, 185, 129, 0.10)); color: var(--pos); }
  .regime.regime-trending { background: var(--info-bg, rgba(59, 130, 246, 0.14)); color: var(--info, #60a5fa); }
  .regime.regime-volatile { background: var(--danger-bg, rgba(239, 68, 68, 0.12)); color: var(--danger); font-weight: 600; }
  .regime.regime-mr       { background: var(--warn-bg); color: var(--warn); }
  .regime.stale { opacity: 0.55; }
</style>
