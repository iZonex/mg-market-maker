<script>
  import HeroKpis from '../components/HeroKpis.svelte'
  import Card from '../components/Card.svelte'
  import PnlChart from '../components/PnlChart.svelte'
  import SpreadChart from '../components/SpreadChart.svelte'
  import OrderBook from '../components/OrderBook.svelte'
  import SignalsPanel from '../components/SignalsPanel.svelte'
  import AdaptivePanel from '../components/AdaptivePanel.svelte'
  import InventoryPanel from '../components/InventoryPanel.svelte'
  import InventoryChart from '../components/InventoryChart.svelte'
  import CrossVenuePortfolio from '../components/CrossVenuePortfolio.svelte'
  import FundingPanel from '../components/FundingPanel.svelte'
  import PerLegInventoryChart from '../components/PerLegInventoryChart.svelte'
  import PerLegPnl from '../components/PerLegPnl.svelte'

  let { ws, auth } = $props()

  const sym = $derived(ws.state.activeSymbol || ws.state.symbols[0] || '')
  const symData = $derived(ws.state.data[sym] || {})
</script>

<div class="overview scroll">
  <HeroKpis data={symData} />

  <!-- Row 1: PnL chart (2) · Orderbook (1) -->
  <div class="row row-2-1">
    <Card title="PnL" subtitle="session $">
      {#snippet children()}<PnlChart data={ws} />{/snippet}
    </Card>
    <Card title="Orderbook" subtitle={sym || ''}>
      {#snippet children()}<OrderBook data={ws} />{/snippet}
    </Card>
  </div>

  <!-- Row 2: Spread chart (2) · Signals (1) -->
  <div class="row row-2-1">
    <Card title="Spread" subtitle="bps">
      {#snippet children()}<SpreadChart data={ws} />{/snippet}
    </Card>
    <Card title="Signals" subtitle="microstructure">
      {#snippet children()}<SignalsPanel data={ws} {auth} />{/snippet}
    </Card>
  </div>

  <!-- Row 2.5: Cross-venue portfolio (full width) -->
  <div class="row row-1">
    <Card title="Cross-venue portfolio" subtitle="net delta per base asset">
      {#snippet children()}<CrossVenuePortfolio {auth} />{/snippet}
    </Card>
  </div>

  <!-- Row 2.6: Per-leg inventory history (full width) -->
  <div class="row row-1">
    <Card title="Inventory by leg" subtitle="per-venue history, 4h window">
      {#snippet children()}<PerLegInventoryChart {auth} />{/snippet}
    </Card>
  </div>

  <!-- Row 2.7: Per-leg PnL attribution (full width) -->
  <div class="row row-1">
    <Card title="PnL attribution by leg" subtitle="which leg sources the PnL">
      {#snippet children()}<PerLegPnl {auth} />{/snippet}
    </Card>
  </div>

  <!-- Row 2.75: Funding state per perp leg (full width) -->
  <div class="row row-1">
    <Card title="Funding state" subtitle="per-leg rate + settlement countdown">
      {#snippet children()}<FundingPanel {auth} />{/snippet}
    </Card>
  </div>

  <!-- Row 3: Inventory (1) · Adaptive (1) · Market quality (1) -->
  <div class="row row-3">
    <Card title="Inventory" subtitle="position">
      {#snippet children()}<InventoryPanel data={ws} />{/snippet}
    </Card>
    <Card title="Adaptive tuner" subtitle="γ feedback">
      {#snippet children()}<AdaptivePanel data={ws} />{/snippet}
    </Card>
    <Card title="Market quality" subtitle="regime">
      {#snippet children()}
        <div class="mq">
          <div class="mq-row">
            <span class="label">Regime</span>
            <span class="chip"
                  class:chip-info={symData.regime === 'Quiet'}
                  class:chip-warn={symData.regime === 'Volatile'}
                  class:chip-pos={symData.regime === 'Trending'}
                  class:chip-neg={symData.regime === 'MeanReverting'}>
              {symData.regime || '—'}
            </span>
          </div>
          <div class="mq-row">
            <span class="label">Venue</span>
            <span class="mq-val num">{symData.venue || '—'} · {symData.product || '—'}</span>
          </div>
          <div class="mq-row">
            <span class="label">Strategy</span>
            <span class="mq-val">
              {symData.strategy || '—'}
              {#if symData.active_graph}
                <span class="graph-tag" title="Deployed {new Date(symData.active_graph.deployed_at_ms).toLocaleString()} · hash {symData.active_graph.hash?.slice(0, 8)} · {symData.active_graph.node_count} nodes">
                  graph: {symData.active_graph.name}
                </span>
              {/if}
            </span>
          </div>
          <div class="mq-row">
            <span class="label">Mode</span>
            <span class="chip chip-{symData.mode === 'live' ? 'neg' : symData.mode === 'smoke' ? 'warn' : 'pos'}">
              {(symData.mode || 'paper').toUpperCase()}
            </span>
          </div>
          <div class="mq-row">
            <span class="label">Live orders</span>
            <span class="mq-val num">{symData.live_orders || 0}</span>
          </div>
          <div class="mq-row">
            <span class="label">Fills</span>
            <span class="mq-val num">{symData.total_fills || 0}</span>
          </div>
        </div>
      {/snippet}
    </Card>
  </div>

  <!-- Row 4: Inventory history (full-width) -->
  <div class="row row-1">
    <Card title="Inventory history" subtitle="position over time">
      {#snippet children()}<InventoryChart data={ws} />{/snippet}
    </Card>
  </div>
</div>

<style>
  .overview {
    padding: var(--s-6);
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
    height: calc(100vh - 57px);
    overflow-y: auto;
  }
  .row { display: grid; gap: var(--s-4); }
  .row-2-1 { grid-template-columns: 2fr 1fr; }
  .row-3   { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .row-1   { grid-template-columns: 1fr; }

  @media (max-width: 1100px) {
    .row-2-1, .row-3 { grid-template-columns: 1fr; }
  }

  .mq {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  .mq-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
    padding: var(--s-2) 0;
    border-bottom: 1px solid var(--border-subtle);
  }
  .mq-row:last-child { border-bottom: none; }
  .mq-row .label {
    font-size: var(--fs-xs);
    color: var(--fg-muted);
  }
  .mq-val {
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--fg-primary);
  }
  .graph-tag {
    display: inline-block;
    margin-left: var(--s-2);
    padding: 1px 6px;
    border-radius: var(--r-pill);
    background: var(--accent-dim);
    color: var(--accent);
    font-family: var(--font-mono);
    font-size: 9px;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    cursor: help;
  }
</style>
