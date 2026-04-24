<script>
  /*
   * Surveillance — admin-only fleet-wide detector roster.
   *
   * UX-SURV-1 demoted this from the Compliance group (where
   * operators kept hitting a "no data" wall until a deployment
   * quoted) to the Admin group. Operator-facing per-symbol
   * scores now live in DeploymentDrilldown's "Manipulation
   * detectors" section — this page is the raw roster for
   * admins scanning the whole fleet at once.
   *
   * Reads GET /api/v1/surveillance/fleet every `REFRESH_MS` —
   * the controller joins every live DeploymentStateRow's
   * manipulation_* scalars into one array, sorted by the
   * combined score (highest first). Engine emits the underlying
   * Prometheus gauges on each refresh tick; the agent scrapes +
   * forwards via telemetry.
   *
   * The earlier 16-pattern detector board was speculative UI
   * ahead of the risk layer — the actual aggregator surfaces
   * only four categories (pump_dump, wash, thin_book, combined)
   * and this page now matches what the detector really reports.
   */
  import Card from '../components/Card.svelte'
  import { createApiClient } from '../api.svelte.js'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 3_000
  const ALERT_THRESHOLD = 0.8
  const WATCH_THRESHOLD = 0.5

  // Category order mirrors the `ManipulationScoreSnapshot` enum
  // order in crates/risk/src/manipulation.rs so the board reads
  // the same way every tick.
  const CATEGORIES = [
    { key: 'combined',  label: 'Combined',    hint: 'Aggregator over sub-detectors' },
    { key: 'pump_dump', label: 'Pump-dump',   hint: 'Abnormal up-move followed by dump' },
    { key: 'wash',      label: 'Wash',        hint: 'Self-trading / matched counterparty' },
    { key: 'thin_book', label: 'Thin book',   hint: 'Depth collapse relative to recent baseline' },
  ]

  let rows = $state([])
  let error = $state(null)
  let lastFetch = $state(null)
  let loading = $state(true)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/surveillance/fleet')
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

  function score(row, key) {
    const n = parseFloat(row[key])
    return Number.isFinite(n) ? n : 0
  }

  function tone(s) {
    if (s >= ALERT_THRESHOLD) return 'danger'
    if (s >= WATCH_THRESHOLD) return 'warn'
    return 'ok'
  }

  function barColour(s) {
    if (s >= ALERT_THRESHOLD) return 'var(--danger)'
    if (s >= WATCH_THRESHOLD) return 'var(--warn)'
    return 'var(--accent)'
  }

  // Counter roll-up per category — how many deployments are
  // above the alert / watch thresholds right now. Gives the
  // operator a single-glance pulse of the whole fleet.
  const rollup = $derived.by(() => {
    const out = {}
    for (const cat of CATEGORIES) {
      let alert = 0, watch = 0
      let peak = 0
      for (const r of rows) {
        const s = score(r, cat.key)
        if (s >= ALERT_THRESHOLD) alert += 1
        else if (s >= WATCH_THRESHOLD) watch += 1
        if (s > peak) peak = s
      }
      out[cat.key] = { alert, watch, peak }
    }
    return out
  })

  function fmtScore(n) {
    if (!Number.isFinite(n)) return '—'
    return n.toFixed(3)
  }
</script>

<div class="page scroll">
  <div class="header">
    <div class="head-text">
      <div class="title">Surveillance · fleet roster <span class="admin-pill">admin</span></div>
      <div class="subtitle">
        Raw manipulation detector scores across every live
        deployment, sorted by combined risk. Scores above
        {WATCH_THRESHOLD} go amber, above {ALERT_THRESHOLD} red.
        <strong>For contextual scores on a specific symbol</strong>,
        open Fleet → deployment drilldown → “Manipulation detectors”
        instead — this page is the all-fleet admin pulse.
      </div>
    </div>
    <div class="meta">
      {#if error}
        <span class="error">error: {error}</span>
      {:else if loading}
        <span class="stale"><span class="spinner" aria-hidden="true"></span>loading…</span>
      {:else if lastFetch}
        <span class="stale">{rows.length} deployment(s) · {lastFetch.toLocaleTimeString()}</span>
      {/if}
    </div>
  </div>

  <div class="rollup-row">
    {#each CATEGORIES as cat (cat.key)}
      {@const r = rollup[cat.key]}
      <div class="rollup-cell">
        <div class="rollup-label">{cat.label}</div>
        <div class="rollup-stats">
          <span class="rollup-peak" style:color={barColour(r?.peak ?? 0)}>
            peak {fmtScore(r?.peak ?? 0)}
          </span>
          {#if (r?.alert ?? 0) > 0}
            <span class="chip tone-danger">{r.alert} alert</span>
          {/if}
          {#if (r?.watch ?? 0) > 0}
            <span class="chip tone-warn">{r.watch} watch</span>
          {/if}
          {#if (r?.alert ?? 0) === 0 && (r?.watch ?? 0) === 0}
            <span class="chip tone-muted">quiet</span>
          {/if}
        </div>
      </div>
    {/each}
  </div>

  <Card title="Deployments" subtitle={`${rows.length} with data`} span={3}>
    {#snippet children()}
      {#if loading}
        <div class="empty">loading fleet surveillance…</div>
      {:else if rows.length === 0}
        <div class="empty">
          No deployments have emitted a manipulation score yet.
          Detectors warm up after a few minutes of book + trade
          history — this board populates once the first sample
          lands.
        </div>
      {:else}
        <table class="board">
          <thead>
            <tr>
              <th>agent</th>
              <th>deployment</th>
              <th>symbol</th>
              {#each CATEGORIES as cat (cat.key)}
                <th title={cat.hint}>{cat.label}</th>
              {/each}
              <th class="num">kill</th>
            </tr>
          </thead>
          <tbody>
            {#each rows as row (`${row.agent_id}/${row.deployment_id}`)}
              {@const combined = score(row, 'combined')}
              <tr class:alerting={combined >= ALERT_THRESHOLD} class:watching={combined >= WATCH_THRESHOLD && combined < ALERT_THRESHOLD}>
                <td class="mono">{row.agent_id}</td>
                <td class="mono">{row.deployment_id}</td>
                <td class="mono">{row.symbol}</td>
                {#each CATEGORIES as cat (cat.key)}
                  {@const s = score(row, cat.key)}
                  {@const pct = Math.min(100, Math.max(0, s * 100))}
                  <td class="score-cell">
                    <div class="bar-track">
                      <div class="bar-fill" style:width="{pct}%" style:background={barColour(s)}></div>
                      <div class="threshold" style:left="{ALERT_THRESHOLD * 100}%" aria-hidden="true"></div>
                    </div>
                    <span class="score-value mono" style:color={barColour(s)}>{fmtScore(s)}</span>
                  </td>
                {/each}
                <td class="num">
                  {#if row.kill_level > 0}
                    <span class="chip tone-danger">L{row.kill_level}</span>
                  {:else}
                    <span class="faint">—</span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {/snippet}
  </Card>
</div>

<style>
  .page { padding: var(--s-6); height: calc(100vh - 57px); overflow-y: auto; }
  .header {
    display: flex; align-items: flex-start; justify-content: space-between;
    margin-bottom: var(--s-4); gap: var(--s-3);
  }
  .head-text { display: flex; flex-direction: column; gap: 2px; max-width: 680px; }
  .title { font-size: var(--fs-lg); font-weight: 600; color: var(--fg-primary); display: inline-flex; align-items: center; gap: var(--s-2); }
  .admin-pill {
    font-family: var(--font-mono); font-size: 10px; font-weight: 700;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    padding: 2px 6px; border-radius: var(--r-pill);
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
  }
  .subtitle { font-size: var(--fs-xs); color: var(--fg-muted); line-height: 1.5; }
  .subtitle strong { color: var(--fg-secondary); font-weight: 600; }
  .meta { font-size: var(--fs-xs); color: var(--fg-muted); display: flex; align-items: center; gap: var(--s-2); }
  .meta .error { color: var(--danger); }
  .spinner {
    display: inline-block;
    width: 10px; height: 10px;
    border: 2px solid var(--border-subtle);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    margin-right: var(--s-2);
    vertical-align: middle;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .rollup-row {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--s-3);
    margin-bottom: var(--s-4);
  }
  .rollup-cell {
    display: flex; flex-direction: column; gap: 4px;
    padding: var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .rollup-label {
    font-size: 10px; color: var(--fg-muted);
    text-transform: uppercase; letter-spacing: var(--tracking-label);
    font-weight: 600;
  }
  .rollup-stats { display: flex; gap: var(--s-2); align-items: center; flex-wrap: wrap; }
  .rollup-peak {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    font-weight: 600;
  }
  .chip {
    font-family: var(--font-mono); font-size: 10px;
    padding: 2px 6px; border-radius: var(--r-sm);
    border: 1px solid currentColor;
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-weight: 600;
  }
  .chip.tone-danger { color: var(--danger); }
  .chip.tone-warn { color: var(--warn); }
  .chip.tone-muted { color: var(--fg-muted); }

  .empty {
    padding: var(--s-4);
    color: var(--fg-muted);
    font-size: var(--fs-sm);
    text-align: center;
    line-height: 1.6;
  }

  .board {
    width: 100%; border-collapse: collapse;
    font-size: var(--fs-xs);
  }
  .board th, .board td {
    padding: var(--s-2);
    text-align: left;
    border-bottom: 1px solid var(--border-subtle);
    vertical-align: middle;
  }
  .board th {
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    font-size: 10px;
    font-weight: 600;
  }
  .board .num { text-align: right; }
  .board tr.alerting { background: rgba(239, 68, 68, 0.06); }
  .board tr.watching { background: rgba(245, 158, 11, 0.04); }

  .score-cell {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    min-width: 140px;
  }
  .bar-track {
    position: relative;
    flex: 1;
    height: 6px;
    background: var(--bg-base);
    border-radius: var(--r-pill);
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    transition: width var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
  }
  .threshold {
    position: absolute; top: -2px; bottom: -2px;
    width: 2px; background: var(--border-strong);
  }
  .score-value {
    font-size: 10px;
    min-width: 40px;
    text-align: right;
  }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .faint { color: var(--fg-muted); }
</style>
