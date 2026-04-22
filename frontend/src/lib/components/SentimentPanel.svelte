<script>
  /*
   * Epic G — live sentiment / social-risk panel.
   *
   * Reads `/api/v1/sentiment/snapshot` on a 15-second cadence
   * (orchestrator ticks default at 60 s, but the operator UI
   * should feel responsive — polling faster is cheap and the
   * endpoint returns at most a few dozen rows). Renders one
   * row per monitored asset with:
   *
   *   - Asset ticker (normalised — BTC, ETH, SPX, …)
   *   - `mentions_rate` with colour ramp:
   *       < 2.0 → muted (chatter floor)
   *       2.0–5.0 → warn (ramp zone)
   *       5.0–10.0 → critical (engine is widening)
   *       ≥ 10.0 → danger (potential kill trigger on vol
   *                 confirmation)
   *   - Sentiment score (-1..+1) as a bar
   *   - Sentiment delta (current - previous) as a sign arrow
   *   - Last-seen timestamp (relative)
   *
   * No controls — this is read-only surveillance. Operator
   * toggles for thresholds live under Settings → Config
   * snapshot (future: editable).
   */
  import { createApiClient } from '../api.svelte.js'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let rows = $state([])
  let histories = $state({}) // asset → Array<{ ts, mentions_rate }>
  let error = $state('')
  let loading = $state(true)
  let lastRefresh = $state(null)

  async function refresh() {
    try {
      const data = await api.getJson('/api/v1/sentiment/snapshot')
      rows = Array.isArray(data) ? data : []
      // Kick off history fetches in parallel for each visible
      // asset. Per-asset cap at 60 points so the sparkline
      // stays readable at ~200 px wide.
      const nextHistories = {}
      await Promise.all(
        rows.map(async (r) => {
          try {
            const h = await api.getJson(
              `/api/v1/sentiment/history?asset=${encodeURIComponent(r.asset)}&limit=60`
            )
            nextHistories[r.asset] = Array.isArray(h) ? h : []
          } catch (_) {
            nextHistories[r.asset] = histories[r.asset] || []
          }
        })
      )
      histories = nextHistories
      lastRefresh = Date.now()
      error = ''
    } catch (e) {
      error = e?.message || String(e)
    } finally {
      loading = false
    }
  }

  // Build an SVG polyline path for the mentions_rate series.
  // Uses a fixed y-range [0, 12] so bars across assets are
  // visually comparable (kill threshold sits at 10).
  function sparklinePath(history, w = 140, h = 28) {
    if (!history || history.length < 2) return ''
    const yMax = 12
    const step = w / (history.length - 1)
    return history
      .map((p, i) => {
        const y = h - (Math.min(yMax, Math.max(0, parseFloat(p.mentions_rate ?? '0'))) / yMax) * h
        return `${i === 0 ? 'M' : 'L'}${(i * step).toFixed(1)},${y.toFixed(1)}`
      })
      .join(' ')
  }
  function sparkFillColour(rate) {
    const r = parseFloat(rate || '0')
    if (r >= 10) return 'var(--neg)'
    if (r >= 5) return 'var(--warn)'
    if (r >= 2) return 'var(--accent)'
    return 'var(--fg-muted)'
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 15000)
    return () => clearInterval(id)
  })

  function rateSeverity(rate) {
    const r = parseFloat(rate || '0')
    if (r >= 10) return 'danger'
    if (r >= 5) return 'critical'
    if (r >= 2) return 'warn'
    return 'muted'
  }

  function fmtNum(s, dp = 2) {
    const n = parseFloat(s ?? '0')
    if (!Number.isFinite(n)) return '—'
    return n.toFixed(dp)
  }

  function fmtScore(s) {
    const n = parseFloat(s ?? '0')
    if (!Number.isFinite(n)) return '—'
    const sign = n > 0 ? '+' : ''
    return `${sign}${n.toFixed(2)}`
  }

  function fmtRelative(ts) {
    if (!ts) return '—'
    const then = new Date(ts).getTime()
    const dt = Math.max(0, Math.round((Date.now() - then) / 1000))
    if (dt < 60) return `${dt}s ago`
    if (dt < 3600) return `${Math.round(dt / 60)}m ago`
    return `${Math.round(dt / 3600)}h ago`
  }

  // Signed bar width (0..100). Centred at 50 so positive grows
  // right, negative grows left.
  function scoreBarStyle(score) {
    const n = Math.max(-1, Math.min(1, parseFloat(score || '0')))
    if (n === 0) return 'width: 2px; left: 50%;'
    if (n > 0) return `width: ${n * 48}%; left: 50%;`
    return `width: ${-n * 48}%; right: 50%;`
  }
</script>

<div class="panel">
  <div class="top">
    <div class="header">
      <span class="label">Social risk</span>
      <span class="hint">mention rate × sentiment × kill signal</span>
    </div>
    <div class="actions">
      {#if lastRefresh}
        <span class="refresh-at">updated {fmtRelative(lastRefresh)}</span>
      {/if}
      <button type="button" class="btn ghost" onclick={refresh} disabled={loading}>
        <Icon name="refresh" size={14} />
        <span>{loading ? 'Loading…' : 'Reload'}</span>
      </button>
    </div>
  </div>

  {#if error}
    <div class="error">{error}</div>
  {:else if rows.length === 0 && !loading}
    <div class="muted">
      No sentiment data yet — configure <code>[sentiment]</code> in <code>config.toml</code> with
      at least one collector (RSS / CryptoPanic / Twitter) and a monitored asset list.
    </div>
  {:else}
    <table class="rows">
      <thead>
        <tr>
          <th>Asset</th>
          <th class="right">Rate</th>
          <th class="trend-col">Trend (1h)</th>
          <th class="right">5min</th>
          <th class="right">1h</th>
          <th class="score-col">Sentiment</th>
          <th class="right">Δ</th>
          <th>Updated</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as r (r.asset)}
          {@const sev = rateSeverity(r.mentions_rate)}
          {@const hist = histories[r.asset] || []}
          <tr>
            <td class="asset">{r.asset}</td>
            <td class="right num" data-sev={sev}>{fmtNum(r.mentions_rate, 2)}×</td>
            <td class="trend-col">
              {#if hist.length >= 2}
                <svg class="spark" viewBox="0 0 140 28" preserveAspectRatio="none">
                  <line x1="0" y1="{28 - (10 / 12) * 28}" x2="140" y2="{28 - (10 / 12) * 28}" class="spark-kill"></line>
                  <path d={sparklinePath(hist)} style="stroke: {sparkFillColour(r.mentions_rate)};"></path>
                </svg>
              {:else}
                <span class="muted spark-empty">warming up…</span>
              {/if}
            </td>
            <td class="right num muted">{r.mentions_5min ?? 0}</td>
            <td class="right num muted">{r.mentions_1h ?? 0}</td>
            <td class="score-col">
              <div class="score-bar">
                <div class="score-track"></div>
                <div class="score-fill" style={scoreBarStyle(r.sentiment_score_5min)} data-dir={parseFloat(r.sentiment_score_5min ?? '0') >= 0 ? 'pos' : 'neg'}></div>
              </div>
              <span class="score-num num" data-dir={parseFloat(r.sentiment_score_5min ?? '0') >= 0 ? 'pos' : 'neg'}>
                {fmtScore(r.sentiment_score_5min)}
              </span>
            </td>
            <td class="right num" data-dir={parseFloat(r.sentiment_delta ?? '0') >= 0 ? 'pos' : 'neg'}>
              {fmtScore(r.sentiment_delta)}
            </td>
            <td class="muted ts">{fmtRelative(r.ts)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .panel { display: flex; flex-direction: column; gap: var(--s-4); }
  .top { display: flex; justify-content: space-between; align-items: center; gap: var(--s-3); }
  .header { display: flex; flex-direction: column; gap: 2px; }
  .label { font-size: var(--fs-sm); font-weight: 600; color: var(--fg-primary); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .hint { font-size: var(--fs-xs); color: var(--fg-muted); }
  .actions { display: flex; align-items: center; gap: var(--s-3); }
  .refresh-at { font-size: var(--fs-xs); color: var(--fg-muted); }
  .btn {
    display: inline-flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-md); color: var(--fg-primary);
    font-size: var(--fs-xs); cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .btn.ghost { background: transparent; }
  .btn:hover:not(:disabled) { background: var(--bg-raised); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }

  .error { padding: var(--s-3); background: var(--danger-bg); color: var(--danger); border-radius: var(--r-md); font-size: var(--fs-sm); }
  .muted { color: var(--fg-muted); font-size: var(--fs-sm); }
  .muted code { font-family: var(--font-mono); background: var(--bg-chip); padding: 1px 4px; border-radius: var(--r-sm); }

  table.rows { width: 100%; border-collapse: collapse; }
  table.rows th { text-align: left; font-size: var(--fs-xs); color: var(--fg-muted); font-weight: 500; padding: var(--s-2) var(--s-3); border-bottom: 1px solid var(--border-subtle); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  table.rows th.right { text-align: right; }
  table.rows td { padding: var(--s-2) var(--s-3); font-size: var(--fs-sm); color: var(--fg-primary); border-bottom: 1px solid var(--border-subtle); }
  table.rows td.right { text-align: right; }
  table.rows tbody tr:last-child td { border-bottom: none; }
  .num { font-family: var(--font-mono); font-size: var(--fs-xs); }
  .asset { font-weight: 600; font-family: var(--font-mono); }
  .ts { font-size: var(--fs-xs); }
  td.num[data-sev='muted'] { color: var(--fg-muted); }
  td.num[data-sev='warn'] { color: var(--warn); }
  td.num[data-sev='critical'] { color: var(--neg); font-weight: 600; }
  td.num[data-sev='danger'] { color: var(--neg); font-weight: 700; text-shadow: 0 0 4px rgba(239, 68, 68, 0.4); }
  td.num[data-dir='pos'] { color: var(--pos); }
  td.num[data-dir='neg'] { color: var(--neg); }

  .trend-col { min-width: 150px; }
  .spark {
    width: 140px; height: 28px; display: block;
  }
  .spark path {
    fill: none;
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .spark .spark-kill {
    stroke: rgba(239, 68, 68, 0.25);
    stroke-width: 1;
    stroke-dasharray: 2 3;
  }
  .spark-empty {
    font-size: var(--fs-xs);
    font-style: italic;
  }

  .score-col { position: relative; min-width: 180px; }
  .score-bar { position: relative; height: 8px; }
  .score-track { position: absolute; inset: 0; background: var(--bg-chip); border-radius: var(--r-pill); }
  .score-fill { position: absolute; top: 0; bottom: 0; border-radius: var(--r-pill); }
  .score-fill[data-dir='pos'] { background: var(--pos); }
  .score-fill[data-dir='neg'] { background: var(--neg); }
  .score-num { display: inline-block; margin-left: var(--s-2); }
  .score-num[data-dir='pos'] { color: var(--pos); }
  .score-num[data-dir='neg'] { color: var(--neg); }
</style>
