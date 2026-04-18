<script>
  /*
   * Surveillance — live detector score board.
   *
   * Reads GET /api/v1/surveillance/scores every `REFRESH_MS`,
   * groups by pattern, and renders a row per (pattern, symbol)
   * with a horizontal bar that colour-codes on the 0.8 alert
   * threshold. Cumulative alert count per row is the total
   * audit rows fired for that pattern since server start.
   */
  import Card from '../components/Card.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = createApiClient(auth)

  const REFRESH_MS = 3_000
  const ALERT_THRESHOLD = 0.8

  // Patterns in a canonical order so the board doesn't reshuffle
  // when detectors fire in different sequences tick-to-tick.
  const PATTERN_ORDER = [
    'spoofing', 'layering', 'quote_stuffing',
    'wash', 'momentum_ignition', 'fake_liquidity',
    'marking_close', 'cross_market', 'latency_exploit',
    'rebate_abuse', 'imbalance_manipulation',
    'cancel_on_reaction', 'one_sided_quoting',
    'inventory_pushing', 'strategic_non_filling',
  ]

  const PATTERN_LABELS = {
    spoofing: 'Spoofing',
    layering: 'Layering',
    quote_stuffing: 'Quote stuffing',
    wash: 'Wash trading',
    momentum_ignition: 'Momentum ignition',
    fake_liquidity: 'Fake liquidity',
    marking_close: 'Marking the close',
    cross_market: 'Cross-market manipulation',
    latency_exploit: 'Latency exploit',
    rebate_abuse: 'Rebate abuse',
    imbalance_manipulation: 'Imbalance manipulation',
    cancel_on_reaction: 'Cancel on reaction',
    one_sided_quoting: 'One-sided quoting',
    inventory_pushing: 'Inventory pushing',
    strategic_non_filling: 'Strategic non-filling',
  }

  let patterns = $state({})
  let error = $state(null)
  let lastFetch = $state(null)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/surveillance/scores')
      patterns = data?.patterns ?? {}
      error = null
      lastFetch = new Date()
    } catch (e) {
      error = e?.message || String(e)
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(t)
  })

  function barColour(score) {
    if (score >= ALERT_THRESHOLD) return 'var(--danger)'
    if (score >= 0.5) return 'var(--warn)'
    return 'var(--accent)'
  }

  function totalAlerts(patternKey) {
    const rows = patterns[patternKey] || {}
    return Object.values(rows).reduce((a, r) => a + (r.alerts_total ?? 0), 0)
  }

  function maxScore(patternKey) {
    const rows = patterns[patternKey] || {}
    return Object.values(rows).reduce((m, r) => Math.max(m, r.score ?? 0), 0)
  }
</script>

<div class="page scroll">
  <div class="header">
    <div class="title">Surveillance detectors</div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if lastFetch}
        <span class="stale">last refresh {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  <div class="grid">
    {#each PATTERN_ORDER as pattern (pattern)}
      {@const rows = patterns[pattern] || {}}
      {@const symbols = Object.keys(rows).sort()}
      {@const mx = maxScore(pattern)}
      {@const alerts = totalAlerts(pattern)}
      <Card
        title={PATTERN_LABELS[pattern] || pattern}
        subtitle={symbols.length ? `${symbols.length} symbol(s)` : 'no data'}
        span={1}
      >
        {#snippet children()}
          <div class="pattern-body">
            <div class="pattern-summary">
              <span class="stat">
                <span class="stat-label">peak</span>
                <span class="stat-value" style:color={barColour(mx)}>
                  {mx.toFixed(2)}
                </span>
              </span>
              <span class="stat">
                <span class="stat-label">alerts</span>
                <span class="stat-value" class:hot={alerts > 0}>
                  {alerts}
                </span>
              </span>
            </div>
            {#if symbols.length === 0}
              <div class="empty">detector hasn't run yet</div>
            {:else}
              <div class="rows">
                {#each symbols as sym (sym)}
                  {@const row = rows[sym]}
                  {@const pct = Math.min(100, Math.max(0, row.score * 100))}
                  <div class="row" class:alerting={row.score >= ALERT_THRESHOLD}>
                    <span class="sym">{sym}</span>
                    <div class="bar-track">
                      <div class="bar-fill" style:width="{pct}%" style:background={barColour(row.score)}></div>
                      <div class="threshold" style:left="{ALERT_THRESHOLD * 100}%" aria-hidden="true"></div>
                    </div>
                    <span class="score" style:color={barColour(row.score)}>
                      {row.score.toFixed(3)}
                    </span>
                    <span class="alerts" class:hot={row.alerts_total > 0}>
                      {row.alerts_total}
                    </span>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        {/snippet}
      </Card>
    {/each}
  </div>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--s-4);
  }
  .title {
    font-family: var(--font-sans);
    font-size: var(--fs-lg);
    font-weight: 600;
    color: var(--fg-primary);
  }
  .meta { font-size: var(--fs-xs); color: var(--fg-muted); }
  .meta .error { color: var(--danger); }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(380px, 1fr));
    gap: var(--s-4);
  }
  .pattern-body { display: flex; flex-direction: column; gap: var(--s-3); }
  .pattern-summary {
    display: flex;
    gap: var(--s-4);
    font-size: var(--fs-xs);
  }
  .stat { display: flex; flex-direction: column; gap: 2px; }
  .stat-label { color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; font-size: 10px; }
  .stat-value { font-family: var(--font-mono); font-weight: 600; color: var(--fg-primary); }
  .stat-value.hot { color: var(--danger); }
  .empty { color: var(--fg-muted); font-size: var(--fs-xs); padding: var(--s-2) 0; }
  .rows { display: flex; flex-direction: column; gap: var(--s-1); }
  .row {
    display: grid;
    grid-template-columns: 90px 1fr 60px 40px;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-1) 0;
    font-size: var(--fs-xs);
  }
  .row.alerting { font-weight: 600; }
  .sym { font-family: var(--font-mono); color: var(--fg-secondary); }
  .bar-track {
    position: relative;
    height: 8px;
    background: var(--bg-chip);
    border-radius: var(--r-pill);
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    border-radius: var(--r-pill);
    transition: width var(--dur-base) var(--ease-out);
  }
  .threshold {
    position: absolute;
    top: -2px;
    bottom: -2px;
    width: 2px;
    background: var(--danger);
    opacity: 0.5;
  }
  .score { font-family: var(--font-mono); font-variant-numeric: tabular-nums; text-align: right; }
  .alerts { font-family: var(--font-mono); text-align: right; color: var(--fg-muted); font-variant-numeric: tabular-nums; }
  .alerts.hot { color: var(--danger); font-weight: 600; }
</style>
