<script>
  /*
   * MV-UI-1 — Cross-venue portfolio panel.
   *
   * Polls /api/v1/portfolio/cross_venue every REFRESH_MS. One
   * row per base asset with the aggregated net delta + a
   * collapsible per-venue breakdown so the operator sees
   * "BTC = +0.3 (Binance +0.5 · Bybit -0.2)" at a glance.
   */
  import { createApiClient } from '../api.svelte.js'
  import LegDetailModal from './LegDetailModal.svelte'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  // 23-UX-7 — click-through leg detail modal state.
  let modalOpen = $state(false)
  let modalVenue = $state('')
  let modalSymbol = $state('')
  let modalInv = $state(0)
  function openLeg(leg) {
    modalVenue = leg.venue
    modalSymbol = leg.symbol
    modalInv = leg.inventory
    modalOpen = true
  }

  const REFRESH_MS = 4_000
  const FUNDING_REFRESH_MS = 10_000

  let assets = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)
  // 23-UX-16 — funding state map keyed by `${venue}|${symbol}`.
  // Populated by a parallel poll so perp legs can render the
  // inline settlement countdown without the operator having to
  // open the LegDetailModal just to check "when does funding
  // settle?" — the UX-audit top-three gap.
  let fundingByLeg = $state({})
  let now = $state(Date.now())

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/portfolio/cross_venue')
      assets = (data?.assets ?? []).map(a => ({
        ...a,
        net_delta: Number(a.net_delta),
        legs: (a.legs ?? []).map(l => ({ ...l, inventory: Number(l.inventory) })),
      }))
      error = null
      lastFetch = new Date()
      loading = false
    } catch (e) {
      error = e?.message || String(e)
      loading = false
    }
  }

  async function refreshFunding() {
    try {
      const rows = await api.getJson('/api/v1/venues/funding_state')
      const next = {}
      for (const r of rows ?? []) {
        if (r?.venue && r?.symbol) {
          next[`${r.venue}|${r.symbol}`] = r
        }
      }
      fundingByLeg = next
    } catch (_) { /* best-effort; inline countdown just hides */ }
  }

  $effect(() => {
    refresh()
    refreshFunding()
    const t = setInterval(refresh, REFRESH_MS)
    const f = setInterval(refreshFunding, FUNDING_REFRESH_MS)
    const tick = setInterval(() => { now = Date.now() }, 1000)
    return () => {
      clearInterval(t)
      clearInterval(f)
      clearInterval(tick)
    }
  })

  function deltaColour(v) {
    if (Math.abs(v) < 1e-12) return 'var(--fg-muted)'
    return v > 0 ? 'var(--accent)' : 'var(--danger)'
  }

  function fundingFor(leg) {
    return fundingByLeg[`${leg.venue}|${leg.symbol}`] ?? null
  }

  function fmtCountdown(ms) {
    if (!ms) return null
    const d = ms - now
    if (d <= 0) return 'now'
    const h = Math.floor(d / 3_600_000)
    const m = Math.floor((d % 3_600_000) / 60_000)
    const s = Math.floor((d % 60_000) / 1000)
    if (h > 0) return `${h}h ${m}m`
    if (m > 0) return `${m}m ${s}s`
    return `${s}s`
  }

  function countdownSeverity(ms) {
    if (!ms) return 'muted'
    const d = ms - now
    if (d <= 60_000) return 'warn'   // <1min
    if (d <= 300_000) return 'info'  // <5min
    return 'ok'
  }

  function fmtRateBps(rateStr) {
    if (rateStr === null || rateStr === undefined) return null
    const n = parseFloat(rateStr)
    if (!Number.isFinite(n)) return null
    // rate is fractional — 0.0001 = 1 bps per period
    return (n * 10_000).toFixed(2)
  }
</script>

<div class="cvp">
  <div class="toolbar">
    <div class="title">Cross-venue portfolio</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{assets.length} asset(s) · refreshed {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  {#if !loading && assets.length === 0}
    <div class="empty">no engines registered — start at least one engine</div>
  {:else}
    <div class="rows">
      {#each assets as a (a.base)}
        <div class="asset">
          <div class="asset-head">
            <span class="base">{a.base}</span>
            <span class="net mono" style:color={deltaColour(a.net_delta)}>
              {a.net_delta > 0 ? '+' : ''}{a.net_delta}
            </span>
          </div>
          <div class="legs">
            {#each a.legs as leg}
              {@const f = fundingFor(leg)}
              <!-- 23-UX-7 — clickable leg opens the detail modal.
                   23-UX-16 — inline funding countdown renders on
                   perp legs so the operator doesn't need the modal
                   just to answer "hold through settlement?" -->
              <button type="button" class="leg leg-btn" onclick={() => openLeg(leg)}>
                <span class="venue">{leg.venue}</span>
                <span class="sym">{leg.symbol}</span>
                {#if f}
                  <span class="funding" title="Funding rate + settlement countdown">
                    {#if fmtRateBps(f.rate) !== null}
                      <span class="funding-rate mono" class:neg={parseFloat(f.rate) < 0} class:pos={parseFloat(f.rate) > 0}>
                        {parseFloat(f.rate) > 0 ? '+' : ''}{fmtRateBps(f.rate)}bps
                      </span>
                    {/if}
                    {#if f.next_funding_ts}
                      <span class="funding-eta mono" data-sev={countdownSeverity(f.next_funding_ts)}>
                        {fmtCountdown(f.next_funding_ts)}
                      </span>
                    {/if}
                  </span>
                {/if}
                <span class="leg-val mono" style:color={deltaColour(leg.inventory)}>
                  {leg.inventory > 0 ? '+' : ''}{leg.inventory}
                </span>
              </button>
            {/each}
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<LegDetailModal
  open={modalOpen}
  venue={modalVenue}
  symbol={modalSymbol}
  inventory={modalInv}
  {auth}
  onClose={() => (modalOpen = false)}
/>

<style>
  .cvp { display: flex; flex-direction: column; gap: var(--s-3); }
  .toolbar {
    display: flex; align-items: center; justify-content: space-between;
    padding: 0 var(--s-2); font-size: var(--fs-xs);
  }
  .title { font-weight: 600; color: var(--fg-primary); }
  .meta { color: var(--fg-muted); display: flex; align-items: center; gap: var(--s-2); }
  .meta .error { color: var(--danger); }
  .empty {
    color: var(--fg-muted); font-size: var(--fs-xs);
    padding: var(--s-4); text-align: center;
  }
  .spinner {
    display: inline-block; width: 10px; height: 10px;
    border: 2px solid var(--border-subtle);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    margin-right: var(--s-1);
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .rows { display: flex; flex-direction: column; gap: var(--s-3); }
  .asset {
    padding: var(--s-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-chip);
  }
  .asset-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--s-3);
    margin-bottom: var(--s-2);
  }
  .base {
    font-family: var(--font-mono);
    font-weight: 600;
    font-size: var(--fs-md);
    letter-spacing: var(--tracking-tight);
    color: var(--fg-primary);
  }
  .net {
    font-family: var(--font-mono);
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .legs {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .leg {
    /* 23-UX-16 — 4 columns so the funding strip fits between
       symbol and inventory without cramping. Middle strip is
       flexible for long symbol names; funding is fixed-ish. */
    display: grid;
    grid-template-columns: 90px 1fr auto auto;
    gap: var(--s-2);
    font-size: var(--fs-xs);
    align-items: baseline;
    padding: var(--s-1) var(--s-2);
    border-radius: var(--r-sm);
  }
  .funding {
    display: inline-flex;
    gap: 6px;
    align-items: baseline;
    font-size: 10px;
  }
  .funding-rate { color: var(--fg-secondary); }
  .funding-rate.pos { color: var(--pos); }
  .funding-rate.neg { color: var(--neg); }
  .funding-eta {
    padding: 0 4px;
    border-radius: 3px;
    background: var(--bg-chip);
    color: var(--fg-secondary);
  }
  .funding-eta[data-sev='warn'] {
    background: var(--warn-bg);
    color: var(--warn);
    font-weight: 600;
  }
  .funding-eta[data-sev='info'] {
    background: rgba(245, 158, 11, 0.08);
    color: var(--warn);
  }
  /* 23-UX-7 clickable-leg affordance. */
  .leg-btn {
    background: transparent;
    border: 1px solid transparent;
    cursor: pointer;
    text-align: left;
    transition: background var(--dur-fast) var(--ease-out),
                border-color var(--dur-fast) var(--ease-out);
  }
  .leg-btn:hover {
    background: var(--bg-chip);
    border-color: var(--border-subtle);
  }
  .venue { font-family: var(--font-mono); color: var(--fg-secondary); }
  .sym { font-family: var(--font-mono); color: var(--fg-muted); font-size: 10px; }
  .leg-val { font-family: var(--font-mono); font-variant-numeric: tabular-nums; text-align: right; }
  .mono { font-family: var(--font-mono); }
</style>
