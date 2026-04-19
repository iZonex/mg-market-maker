<script>
  let { data } = $props()
  const s = $derived(data.state)
  const sym = $derived(s.activeSymbol || s.symbols[0] || '')
  const d = $derived(s.data[sym] || {})

  const inv = $derived(parseFloat(d.inventory || 0))
  const invValue = $derived(parseFloat(d.inventory_value || 0))
  const vpin = $derived(parseFloat(d.vpin || 0))
  const kyle = $derived(parseFloat(d.kyle_lambda || 0))
  const adverse = $derived(parseFloat(d.adverse_bps || 0))
  const vol = $derived(parseFloat(d.volatility || 0))

  // 23-UX-8 — shared formatters keep inventory / bps / pct readings
  // identical to every other panel that shows the same metric.
  import { fmtSigned, fmtBps, fmtPct, fmtFixed } from '../format.js'

  // Display helpers — Kyle lambda is shown as magnitude; sign
  // at low sample sizes is usually just noise, not alpha. VPIN
  // warms up from 0 so we flag the pre-warmup state explicitly.
  const kyleAbs = $derived(Math.abs(kyle))
  const kyleDir = $derived(kyle > 1e-9 ? 'up' : kyle < -1e-9 ? 'down' : 'flat')
  const vpinWarming = $derived(vpin < 1e-9)

  const signals = $derived([
    {
      label: 'VPIN',
      value: vpinWarming ? 'warming' : vpin.toFixed(3),
      valueStyle: vpinWarming ? 'muted' : 'num',
      hint: 'Volume-synchronised probability of informed trading (0–1). Needs N bucketised volume bars before it reports.',
      severity: vpinWarming ? 'muted' : vpin > 0.7 ? 'neg' : vpin > 0.4 ? 'warn' : 'ok',
    },
    {
      label: 'Kyle λ',
      // Format as fixed small decimal — operators find 0.000104 far
      // easier to read at-a-glance than 1.04e-4. Drop to dash when
      // the window has no signal.
      value: kyleAbs === 0 ? '—' : kyleAbs < 1e-6 ? '< 0.000001' : kyleAbs.toFixed(6),
      valueStyle: 'num',
      dir: kyleAbs === 0 ? null : kyleDir,
      hint: 'Kyle λ — price impact per unit of signed order flow. Higher = toxic flow (someone informed is pushing through). Typical range 1e-5 .. 1e-3. Magnitude matters; sign at short windows is noise.',
      severity: kyleAbs > 1e-3 ? 'warn' : 'ok',
    },
    {
      label: 'Adverse',
      value: `${fmtBps(adverse)} bps`,
      valueStyle: 'num',
      hint: 'Post-fill price drift against our side, bps. >5 bps = getting picked off.',
      severity: adverse > 5 ? 'neg' : adverse > 2 ? 'warn' : 'ok',
    },
    {
      label: 'Volatility',
      value: fmtPct(vol * 100, 2),
      valueStyle: 'num',
      hint: 'Realised, annualised (EWMA on mid-returns).',
      severity: vol > 0.10 ? 'warn' : 'ok',
    },
  ])
</script>

<div class="inventory">
  <!-- Hero inventory number -->
  <div class="inv-hero">
    <span class="label">Inventory</span>
    <div class="inv-row">
      <span class="inv-big num" class:pos={inv > 0} class:neg={inv < 0}>
        {fmtSigned(inv, 6)}
      </span>
      <span class="inv-sub num">≈ ${fmtFixed(Math.abs(invValue), 2)}</span>
    </div>

    <div class="bar-track" aria-hidden="true">
      <div class="bar-center"></div>
      <div
        class="bar-fill"
        class:pos={inv > 0}
        class:neg={inv < 0}
        style="
          width: {Math.min(Math.abs(inv) * 1000, 50)}%;
          {inv > 0 ? 'left: 50%' : 'right: 50%'};
        "></div>
    </div>
    <div class="bar-labels">
      <span>short</span>
      <span>flat</span>
      <span>long</span>
    </div>
  </div>

  <!-- Signals grid -->
  <div class="sig-grid">
    {#each signals as sig}
      <div class="sig-cell" title={sig.hint}>
        <span class="label">{sig.label}</span>
        <span class="sig-row">
          <span class="sig-val" class:num={sig.valueStyle === 'num'} data-sev={sig.severity}>{sig.value}</span>
          {#if sig.dir === 'up'}<span class="arrow pos">↑</span>
          {:else if sig.dir === 'down'}<span class="arrow neg">↓</span>{/if}
        </span>
      </div>
    {/each}
  </div>
</div>

<style>
  .inventory {
    display: flex;
    flex-direction: column;
    gap: var(--s-5);
  }

  /* ── Hero inventory ─────────────────────────────────────── */
  .inv-hero {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  .inv-row {
    display: flex;
    align-items: baseline;
    gap: var(--s-3);
  }
  .inv-big {
    font-size: var(--fs-3xl);
    font-weight: 600;
    color: var(--fg-primary);
    letter-spacing: -0.01em;
    line-height: 1;
  }
  .inv-big.pos { color: var(--pos); }
  .inv-big.neg { color: var(--neg); }
  .inv-sub {
    font-size: var(--fs-sm);
    color: var(--fg-muted);
  }

  .bar-track {
    position: relative;
    height: 6px;
    background: var(--bg-chip);
    border-radius: var(--r-pill);
    margin-top: var(--s-2);
  }
  .bar-center {
    position: absolute;
    left: 50%;
    top: -3px; bottom: -3px;
    width: 2px;
    background: var(--border-strong);
    border-radius: 1px;
  }
  .bar-fill {
    position: absolute;
    top: 0;
    height: 100%;
    border-radius: var(--r-pill);
    transition: width 200ms var(--ease-out), background 200ms var(--ease-out);
  }
  .bar-fill.pos { background: var(--pos); }
  .bar-fill.neg { background: var(--neg); }
  .bar-labels {
    display: flex;
    justify-content: space-between;
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-faint);
  }

  /* ── Signals grid ───────────────────────────────────────── */
  .sig-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: var(--s-2);
    padding: var(--s-3);
    background: var(--bg-chip);
    border-radius: var(--r-lg);
  }
  .sig-cell {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .sig-row {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
  }
  .sig-val {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .sig-val[data-sev='warn']  { color: var(--warn); }
  .sig-val[data-sev='neg']   { color: var(--neg); }
  .sig-val[data-sev='muted'] { color: var(--fg-muted); font-weight: 500; font-style: italic; }
  .arrow {
    font-size: var(--fs-xs);
    font-weight: 600;
    line-height: 1;
  }

  .pos { color: var(--pos); }
  .neg { color: var(--neg); }
</style>
