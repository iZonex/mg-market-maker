<script>
  import { createApiClient } from '../api.svelte.js'

  let { data, auth } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})

  // Event-driven + regulatory + alpha signals added by the
  // ISAC / VisualHFT / mm-toolbox cherry-picks.
  const mr = $derived(parseFloat(d.market_resilience || '1'))
  const otr = $derived(parseFloat(d.order_to_trade_ratio || '0'))
  const hma = $derived(d.hma_value == null ? null : parseFloat(d.hma_value))
  const mid = $derived(parseFloat(d.mid_price || '0'))

  // UI-3 — tiered + dual-timeline OTR fetched from
  // /api/v1/otr/tiered every 4 s.
  const api = $derived(auth ? createApiClient(auth) : null)
  let tiered = $state({})
  $effect(() => {
    const client = api
    if (!client) return
    async function poll() {
      try {
        const data = await client.getJson('/api/v1/otr/tiered')
        tiered = data?.symbols ?? {}
      } catch { /* silent — the other signals still render */ }
    }
    poll()
    const t = setInterval(poll, 4_000)
    return () => clearInterval(t)
  })
  const tRow = $derived(tiered[sym] || null)
  function otrColour(v) {
    if (v === null || v === undefined || Number.isNaN(v)) return 'var(--fg-muted)'
    if (v > 15) return 'var(--danger)'
    if (v > 5) return 'var(--warn)'
    return 'var(--fg-primary)'
  }

  // Colour thresholds match AutoTuner::effective_spread_mult +
  // KillSwitch::update_market_resilience: < 0.3 trips L1 after 3 s.
  const mrDanger = $derived(mr < 0.3)
  const mrWarn   = $derived(mr >= 0.3 && mr < 0.7)

  // OTR thresholds — spoofing/layering surveillance band.
  const otrWarn   = $derived(otr > 5)
  const otrDanger = $derived(otr > 15)

  // HMA slope relative to mid, bps. Rough at-a-glance direction.
  const hmaDelta = $derived(hma == null || mid === 0 ? 0 : ((hma - mid) / mid) * 10000)
</script>

<div class="signals">
  <!-- Market Resilience — progress-bar widget -->
  <div class="block">
    <div class="row-head">
      <span class="label">Market resilience</span>
      <span class="val num" class:warn={mrWarn} class:neg={mrDanger}>{mr.toFixed(3)}</span>
    </div>
    <div class="bar-track" aria-hidden="true">
      <div
        class="bar-fill"
        class:warn={mrWarn}
        class:neg={mrDanger}
        style="width: {Math.max(0, Math.min(1, mr)) * 100}%"
      ></div>
      <div class="bar-threshold" style="left: 30%" title="L1 kill trigger"></div>
    </div>
    <div class="sub">
      {#if mrDanger}shock — kill arming
      {:else if mrWarn}recovering from shock
      {:else}steady state{/if}
    </div>
  </div>

  <!-- OTR (legacy scalar) -->
  <div class="block">
    <div class="row-head">
      <span class="label">Order-to-trade ratio</span>
      <span class="val num" class:warn={otrWarn} class:neg={otrDanger}>{otr.toFixed(2)}</span>
    </div>
    <div class="sub">
      {#if otrDanger}surveillance alert — layering band
      {:else if otrWarn}elevated — investigate
      {:else}normal market quality{/if}
    </div>
  </div>

  <!-- UI-3 — Tiered + dual-timeline OTR (2×2 grid) -->
  <div class="block">
    <div class="row-head">
      <span class="label">OTR tiered (TOB · Top20 / cum · 5m)</span>
    </div>
    {#if tRow}
      <div class="otr-grid">
        <div class="otr-cell">
          <span class="otr-tag">TOB cum</span>
          <span class="otr-num" style:color={otrColour(tRow.tob_cumulative)}>
            {tRow.tob_cumulative.toFixed(2)}
          </span>
        </div>
        <div class="otr-cell">
          <span class="otr-tag">TOB 5m</span>
          <span class="otr-num" style:color={otrColour(tRow.tob_rolling_5min)}>
            {tRow.tob_rolling_5min.toFixed(2)}
          </span>
        </div>
        <div class="otr-cell">
          <span class="otr-tag">Top20 cum</span>
          <span class="otr-num" style:color={otrColour(tRow.top20_cumulative)}>
            {tRow.top20_cumulative.toFixed(2)}
          </span>
        </div>
        <div class="otr-cell">
          <span class="otr-tag">Top20 5m</span>
          <span class="otr-num" style:color={otrColour(tRow.top20_rolling_5min)}>
            {tRow.top20_rolling_5min.toFixed(2)}
          </span>
        </div>
      </div>
    {:else}
      <div class="sub">no tiered data yet — waiting for first snapshot</div>
    {/if}
  </div>

  <!-- HMA vs mid -->
  <div class="block">
    <div class="row-head">
      <span class="label">HMA vs mid</span>
      {#if hma == null}
        <span class="val muted">warming up…</span>
      {:else}
        <span class="val num" class:pos={hmaDelta > 0} class:neg={hmaDelta < 0}>
          {hmaDelta > 0 ? '+' : ''}{hmaDelta.toFixed(2)} bps
        </span>
      {/if}
    </div>
    {#if hma != null}
      <div class="sub num">
        <span>{hma.toFixed(2)}</span>
        <span class="arrow">→</span>
        <span>{mid.toFixed(2)}</span>
      </div>
    {/if}
  </div>
</div>

<style>
  .signals {
    display: flex;
    flex-direction: column;
    gap: var(--s-5);
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
  }
  .row-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--s-3);
  }
  .label {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
  }
  .val {
    font-size: var(--fs-xl);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: var(--tracking-tight);
  }
  .val.muted { color: var(--fg-muted); font-weight: 500; font-size: var(--fs-md); font-style: normal; }
  .val.pos { color: var(--pos); }
  .val.neg { color: var(--neg); }
  .val.warn { color: var(--warn); }
  .sub {
    font-size: var(--fs-xs);
    color: var(--fg-muted);
    display: flex;
    align-items: center;
    gap: var(--s-1);
  }
  .sub.num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .arrow { color: var(--fg-faint); }

  .bar-track {
    position: relative;
    height: 6px;
    background: var(--bg-chip);
    border-radius: var(--r-pill);
    overflow: visible;
    margin-top: var(--s-2);
    margin-bottom: var(--s-2);
  }
  .bar-fill {
    position: absolute;
    top: 0; left: 0;
    height: 100%;
    background: var(--accent);
    border-radius: var(--r-pill);
    transition: width 320ms var(--ease-out), background-color 200ms var(--ease-out);
  }
  .bar-fill.warn { background: var(--warn); }
  .bar-fill.neg  { background: var(--neg); }
  .bar-threshold {
    position: absolute;
    top: -3px; bottom: -3px;
    width: 2px;
    background: var(--neg);
    border-radius: 1px;
    opacity: 0.6;
  }

  .otr-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s-2);
  }
  .otr-cell {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    padding: var(--s-1) var(--s-2);
    background: var(--bg-chip);
    border-radius: var(--r-sm);
  }
  .otr-tag {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    font-family: var(--font-mono);
  }
  .otr-num {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
</style>
