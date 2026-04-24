<script>
  /*
   * 23-UX-7 — per-leg drill-down modal.
   *
   * Opens when an operator clicks a row in CrossVenuePortfolio.
   * Aggregates cross-panel data for the selected (venue,
   * symbol) leg into one compact view:
   *   - inventory + notional (from the clicked row)
   *   - funding rate + settlement countdown (perp legs only)
   *   - per-leg PnL attribution
   *   - basis vs spot reference
   *
   * Data pulled from endpoints already wired by 23-UX-3/4/5
   * so no new backend routes. Closes on Esc / backdrop click.
   */
  import { createApiClient } from '../api.svelte.js'
  import { fmtPnl, fmtPrice, fmtBps, fmtRelative, fmtFixed } from '../format.js'
  import Icon from './Icon.svelte'
  import { Modal, Button } from '../primitives/index.js'

  let {
    open = false,
    venue = '',
    symbol = '',
    inventory = 0,
    auth,
    onClose = () => {},
  } = $props()

  const api = $derived(createApiClient(auth))

  let funding = $state(null)
  let pnlRow = $state(null)
  let basisRow = $state(null)
  let now = $state(Date.now())

  async function refresh() {
    if (!open) return
    try {
      // Pull all three in parallel.
      const [f, p, b] = await Promise.all([
        api.getJson('/api/v1/venues/funding_state'),
        api.getJson('/api/v1/pnl/per_leg'),
        api.getJson('/api/v1/basis'),
      ])
      funding = f.find((r) => r.venue === venue && r.symbol === symbol) || null
      pnlRow = p.find((r) => r.venue === venue && r.symbol === symbol) || null
      // Find the basis leg matching our (venue, symbol).
      basisRow = null
      for (const bas of b) {
        const leg = (bas.legs || []).find((l) => l.venue === venue && l.symbol === symbol)
        if (leg) {
          basisRow = { ...leg, reference: bas }
          break
        }
      }
    } catch (_) { /* best-effort */ }
  }

  $effect(() => {
    if (open) {
      refresh()
      const id = setInterval(refresh, 4000)
      const tick = setInterval(() => { now = Date.now() }, 1000)
      const onKey = (e) => { if (e.key === 'Escape') onClose() }
      window.addEventListener('keydown', onKey)
      return () => {
        clearInterval(id)
        clearInterval(tick)
        window.removeEventListener('keydown', onKey)
      }
    }
  })

  function fmtCountdown(ms) {
    if (!ms) return '—'
    const d = ms - now
    if (d <= 0) return 'now'
    const h = Math.floor(d / 3_600_000)
    const m = Math.floor((d % 3_600_000) / 60_000)
    const s = Math.floor((d % 60_000) / 1000)
    if (h > 0) return `${h}h ${m}m`
    if (m > 0) return `${m}m ${s}s`
    return `${s}s`
  }
</script>

<Modal
  {open}
  ariaLabel="Leg details"
  maxWidth="720px"
  {onClose}
>
  {#snippet children()}
    <div class="head">
      <div class="head-meta">
        <span class="venue">{venue}</span>
        <span class="sep">/</span>
        <span class="symbol num">{symbol}</span>
      </div>
      <Button variant="ghost" size="sm" iconOnly onclick={onClose} aria-label="Close">
        {#snippet children()}<Icon name="close" size={14} />{/snippet}
      </Button>
    </div>

    <div class="grid">
      <section class="card-inner">
        <div class="section-title">Inventory</div>
        <div class="big-num" class:pos={inventory > 0} class:neg={inventory < 0}>
          {inventory > 0 ? '+' : ''}{fmtFixed(inventory, 6)}
        </div>
      </section>

      {#if funding}
        <section class="card-inner">
          <div class="section-title">Funding</div>
          <div class="row">
            <span class="lbl">Rate</span>
            <span class="num">{funding.rate != null ? `${fmtBps(parseFloat(funding.rate) * 10_000, 4)} bps` : '—'}</span>
          </div>
          <div class="row">
            <span class="lbl">Next settlement</span>
            <span class="num">{fmtCountdown(funding.next_funding_ts)}</span>
          </div>
          <div class="row">
            <span class="lbl">ETA</span>
            <span class="num small muted">{funding.next_funding_ts ? fmtRelative(funding.next_funding_ts) : '—'}</span>
          </div>
        </section>
      {/if}

      {#if pnlRow}
        <section class="card-inner wide">
          <div class="section-title">PnL attribution</div>
          <div class="pnl-grid">
            <div><span class="lbl">Total</span><span class="num pnl" class:pos={parseFloat(pnlRow.total) > 0} class:neg={parseFloat(pnlRow.total) < 0}>{fmtPnl(pnlRow.total)}</span></div>
            <div><span class="lbl">Spread</span><span class="num pos">{fmtPnl(pnlRow.spread_capture)}</span></div>
            <div><span class="lbl">Inv</span><span class="num" class:pos={parseFloat(pnlRow.inventory_pnl) > 0} class:neg={parseFloat(pnlRow.inventory_pnl) < 0}>{fmtPnl(pnlRow.inventory_pnl)}</span></div>
            <div><span class="lbl">Rebates</span><span class="num pos">{fmtFixed(pnlRow.rebate_income, 2)}</span></div>
            <div><span class="lbl">Fees</span><span class="num neg">−{fmtFixed(pnlRow.fees_paid, 2)}</span></div>
            <div><span class="lbl">Volume</span><span class="num">${fmtFixed(pnlRow.volume, 0)}</span></div>
            <div><span class="lbl">Fills</span><span class="num">{pnlRow.fills}</span></div>
            <div><span class="lbl">Eff bps</span><span class="num">{fmtBps(pnlRow.efficiency_bps)}</span></div>
          </div>
        </section>
      {/if}

      {#if basisRow}
        <section class="card-inner wide">
          <div class="section-title">Basis vs reference</div>
          <div class="row">
            <span class="lbl">Ref</span>
            <span class="num small">{basisRow.reference.reference_venue}:{basisRow.reference.reference_symbol}:{basisRow.reference.reference_product}</span>
          </div>
          <div class="row">
            <span class="lbl">Ref mid</span>
            <span class="num">{fmtPrice(basisRow.reference.reference_mid, 4)}</span>
          </div>
          <div class="row">
            <span class="lbl">Leg mid</span>
            <span class="num">{fmtPrice(basisRow.mid, 4)}</span>
          </div>
          <div class="row">
            <span class="lbl">Basis</span>
            <span class="num" class:pos={parseFloat(basisRow.basis_bps) > 0} class:neg={parseFloat(basisRow.basis_bps) < 0}>{fmtBps(basisRow.basis_bps)} bps</span>
          </div>
        </section>
      {/if}
    </div>
  {/snippet}
</Modal>

<style>
  /* `.backdrop` + `.modal` moved to primitives/Modal.svelte —
     design system v1. */
  .head {
    display: flex; align-items: center; justify-content: space-between;
    padding-bottom: var(--s-2);
    border-bottom: 1px solid var(--border-subtle);
  }
  .head-meta { display: flex; gap: var(--s-2); align-items: baseline; }
  .venue { color: var(--accent); font-family: var(--font-mono); font-weight: 600; }
  .sep { color: var(--fg-faint); }
  .symbol { font-family: var(--font-mono); font-weight: 600; font-size: var(--fs-md); }
  /* `.close` button styling moved to Button primitive (variant=ghost size=sm iconOnly). */
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s-3);
  }
  .card-inner {
    background: var(--bg-chip);
    border-radius: var(--r-md);
    padding: var(--s-3);
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  .card-inner.wide { grid-column: 1 / -1; }
  .section-title {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    font-weight: 600;
  }
  .big-num {
    font-family: var(--font-mono);
    font-size: var(--fs-xl);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .row {
    display: flex; justify-content: space-between;
    font-size: var(--fs-sm);
  }
  .lbl { color: var(--fg-muted); }
  .num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; color: var(--fg-primary); }
  .num.small { font-size: var(--fs-xs); }
  .muted { color: var(--fg-muted); }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }
  .pnl-grid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: var(--s-2);
  }
  .pnl-grid > div {
    display: flex; flex-direction: column; gap: 2px;
  }
  .pnl-grid .lbl { font-size: var(--fs-2xs); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .pnl-grid .pnl { font-size: var(--fs-md); font-weight: 600; }
</style>
