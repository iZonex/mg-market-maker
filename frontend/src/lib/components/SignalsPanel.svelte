<script>
  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})

  // Event-driven + regulatory + alpha signals added by the
  // ISAC / VisualHFT / mm-toolbox cherry-picks. Each row reads
  // a single field from `SymbolState`. Missing fields fall
  // back to sensible placeholders so the panel stays readable
  // during the warmup window.
  const mr = $derived(parseFloat(d.market_resilience || '1'))
  const otr = $derived(parseFloat(d.order_to_trade_ratio || '0'))
  const hma = $derived(d.hma_value == null ? null : parseFloat(d.hma_value))
  const mid = $derived(parseFloat(d.mid_price || '0'))

  // Colour thresholds match the logic wired into
  // AutoTuner::effective_spread_mult and
  // KillSwitch::update_market_resilience: below 0.3 the
  // kill switch will trip L1 WidenSpreads after 3 s.
  const mrDanger = $derived(mr < 0.3)
  const mrWarn = $derived(mr >= 0.3 && mr < 0.7)

  // OTR above ~5 is a surveillance-worthy read for a liquid
  // market — spoofing / layering bands usually sit in [5, 20].
  const otrWarn = $derived(otr > 5)
  const otrDanger = $derived(otr > 15)

  // HMA slope relative to mid, as a rough visual of direction.
  // Not alpha itself — just an at-a-glance indicator.
  const hmaDelta = $derived(hma == null || mid === 0 ? 0 : ((hma - mid) / mid) * 10000)
</script>

<div>
  <h3>Event Signals</h3>

  <div class="signal-block">
    <div class="signal-label">Market Resilience</div>
    <div class="mr-bar-bg">
      <div
        class="mr-bar-fill"
        class:danger={mrDanger}
        class:warn={mrWarn}
        style="width: {Math.max(0, Math.min(1, mr)) * 100}%"
      ></div>
      <div class="mr-threshold" style="left: 30%"></div>
    </div>
    <div class="mr-value" class:danger={mrDanger} class:warn={mrWarn}>
      {mr.toFixed(3)}
    </div>
    <div class="sub">
      {#if mrDanger}
        shock — kill switch arming
      {:else if mrWarn}
        recovering from shock
      {:else}
        steady state
      {/if}
    </div>
  </div>

  <div class="signal-block">
    <div class="signal-label">Order-to-Trade Ratio</div>
    <div class="otr-value" class:danger={otrDanger} class:warn={otrWarn}>
      {otr.toFixed(2)}
    </div>
    <div class="sub">
      {#if otrDanger}
        surveillance alert — layering band
      {:else if otrWarn}
        elevated — investigate
      {:else}
        normal market quality
      {/if}
    </div>
  </div>

  <div class="signal-block">
    <div class="signal-label">HMA vs Mid</div>
    {#if hma == null}
      <div class="hma-warmup">warming up…</div>
    {:else}
      <div class="hma-value" class:positive={hmaDelta > 0} class:negative={hmaDelta < 0}>
        {hmaDelta > 0 ? '+' : ''}{hmaDelta.toFixed(2)} bps
      </div>
      <div class="sub">{hma.toFixed(2)} vs {mid.toFixed(2)}</div>
    {/if}
  </div>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 12px; text-transform: uppercase; letter-spacing: 0.5px; }
  .signal-block { margin-bottom: 14px; }
  .signal-label { font-size: 11px; color: #8b949e; margin-bottom: 4px; }
  .mr-bar-bg {
    position: relative; height: 10px; background: #21262d;
    border-radius: 4px; margin-bottom: 4px;
  }
  .mr-bar-fill {
    position: absolute; top: 0; left: 0; height: 100%;
    background: #3fb950; border-radius: 4px;
    transition: width 0.3s ease;
  }
  .mr-bar-fill.warn { background: #d29922; }
  .mr-bar-fill.danger { background: #f85149; }
  .mr-threshold {
    position: absolute; top: -2px; width: 2px; height: 14px; background: #f85149;
  }
  .mr-value, .otr-value, .hma-value {
    font-size: 18px; font-weight: 700;
  }
  .mr-value.warn, .otr-value.warn { color: #d29922; }
  .mr-value.danger, .otr-value.danger { color: #f85149; }
  .hma-value.positive { color: #3fb950; }
  .hma-value.negative { color: #f85149; }
  .hma-warmup { color: #8b949e; font-size: 13px; font-style: italic; }
  .sub { font-size: 11px; color: #8b949e; margin-top: 2px; }
</style>
