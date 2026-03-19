<script>
  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})

  const inv = $derived(parseFloat(d.inventory || 0))
  const invValue = $derived(parseFloat(d.inventory_value || 0))
  const vpin = $derived(parseFloat(d.vpin || 0))
  const kyle = $derived(parseFloat(d.kyle_lambda || 0))
  const adverse = $derived(parseFloat(d.adverse_bps || 0))
  const vol = $derived(parseFloat(d.volatility || 0))
</script>

<div>
  <h3>Inventory & Signals</h3>

  <div class="inv-display">
    <div class="big-number" class:positive={inv > 0} class:negative={inv < 0}>
      {inv.toFixed(6)}
    </div>
    <div class="sub">≈ ${invValue.toFixed(2)}</div>
  </div>

  <div class="bar-container">
    <div class="bar-bg">
      <div class="bar-fill" class:long={inv > 0} class:short={inv < 0}
        style="width: {Math.min(Math.abs(inv) * 1000, 100)}%; {inv > 0 ? 'left: 50%' : `right: 50%`}">
      </div>
      <div class="bar-center"></div>
    </div>
  </div>

  <div class="signals">
    <div class="signal-row">
      <span class="label">VPIN</span>
      <span class="value" class:danger={vpin > 0.7}>{vpin.toFixed(3)}</span>
    </div>
    <div class="signal-row">
      <span class="label">Kyle's λ</span>
      <span class="value">{kyle.toFixed(6)}</span>
    </div>
    <div class="signal-row">
      <span class="label">Adverse Sel.</span>
      <span class="value" class:danger={adverse > 5}>{adverse.toFixed(2)} bps</span>
    </div>
    <div class="signal-row">
      <span class="label">Volatility</span>
      <span class="value">{(vol * 100).toFixed(2)}%</span>
    </div>
  </div>
</div>

<style>
  h3 { font-size: 12px; color: #8b949e; margin-bottom: 12px; text-transform: uppercase; letter-spacing: 0.5px; }
  .inv-display { text-align: center; margin-bottom: 12px; }
  .big-number { font-size: 24px; font-weight: 700; }
  .sub { font-size: 12px; color: #8b949e; }
  .positive { color: #3fb950; }
  .negative { color: #f85149; }
  .bar-container { margin: 12px 0; }
  .bar-bg { position: relative; height: 8px; background: #21262d; border-radius: 4px; }
  .bar-fill { position: absolute; top: 0; height: 100%; border-radius: 4px; }
  .bar-fill.long { background: #3fb950; }
  .bar-fill.short { background: #f85149; }
  .bar-center { position: absolute; left: 50%; top: -2px; width: 2px; height: 12px; background: #484f58; }
  .signals { display: flex; flex-direction: column; gap: 6px; margin-top: 12px; }
  .signal-row { display: flex; justify-content: space-between; }
  .label { color: #8b949e; font-size: 12px; }
  .value { font-weight: 600; font-size: 12px; }
  .danger { color: #f85149; }
</style>
