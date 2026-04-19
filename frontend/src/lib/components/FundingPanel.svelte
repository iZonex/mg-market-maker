<script>
  /*
   * 23-UX-4 — funding-state panel.
   *
   * MM-desk critical ask (per the domain expert review): operators
   * running a perp leg need to see the current funding rate AND
   * the countdown to the next settlement window. Without it, the
   * decision "hold through settlement or close early" has no
   * surface on the dashboard.
   *
   * Polls /api/v1/venues/funding_state every 5 s — cheap read on
   * a data-bus snapshot already populated by the engine's
   * refresh_funding_rate loop (~30 s cadence per venue).
   */

  import { createApiClient } from '../api.svelte.js'
  import { fmtBps, fmtRelative } from '../format.js'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  const api = createApiClient(auth)

  let rows = $state([])
  let error = $state('')
  let lastOk = $state(0)
  // Client-side clock tick so the countdown updates every second
  // even if the poll cadence is 5 s. The actual data source is
  // only refreshed on the poll.
  let now = $state(Date.now())

  async function refresh() {
    try {
      rows = await api.getJson('/api/v1/venues/funding_state')
      error = ''
      lastOk = Date.now()
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => {
    refresh()
    const pollId = setInterval(refresh, 5000)
    const tickId = setInterval(() => { now = Date.now() }, 1000)
    return () => {
      clearInterval(pollId)
      clearInterval(tickId)
    }
  })

  /**
   * Convert hourly funding rate fraction (e.g. 0.0001 = 1 bps/h)
   * to an annualised bps reading. The engine stores rates as
   * per-hour fractions; operators read APR in bps.
   */
  function annualBps(rate) {
    const r = parseFloat(rate ?? 0)
    if (!Number.isFinite(r) || r === 0) return null
    // rate is per-funding-period. Assume 3 × 8h settlements per
    // 24h — 365 × 3 = 1095 periods per year.
    return r * 1095 * 10_000
  }

  function fmtCountdown(ms) {
    if (!ms) return '—'
    const delta = ms - now
    if (delta <= 0) return 'now'
    const h = Math.floor(delta / 3_600_000)
    const m = Math.floor((delta % 3_600_000) / 60_000)
    const s = Math.floor((delta % 60_000) / 1000)
    if (h > 0) return `${h}h ${m}m`
    if (m > 0) return `${m}m ${s}s`
    return `${s}s`
  }

  function severity(ms) {
    if (!ms) return 'muted'
    const delta = ms - now
    if (delta < 60_000) return 'warn' // < 1 min — decide NOW
    if (delta < 300_000) return 'warn' // < 5 min
    return 'ok'
  }
</script>

<div class="funding">
  {#if error}
    <div class="alert-bar">
      <Icon name="alert" size={14} />
      <span>{error}</span>
    </div>
  {:else if rows.length === 0}
    <div class="empty-state">
      <span class="empty-state-icon"><Icon name="clock" size={18} /></span>
      <span class="empty-state-title">No perp legs yet</span>
      <span class="empty-state-hint">
        Funding data appears once the engine has polled any
        perp-enabled venue (usually within 30 s of boot).
      </span>
    </div>
  {:else}
    <table class="grid">
      <thead>
        <tr>
          <th>Venue</th>
          <th>Symbol</th>
          <th>Product</th>
          <th class="right">Rate</th>
          <th class="right">APR bps</th>
          <th class="right">Next settlement</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as r (r.venue + r.symbol + r.product)}
          <tr>
            <td class="venue">{r.venue}</td>
            <td class="sym num">{r.symbol}</td>
            <td><span class="chip">{r.product}</span></td>
            <td class="num right">
              {#if r.rate != null}
                <span class={parseFloat(r.rate) >= 0 ? 'pos' : 'neg'}>
                  {fmtBps(parseFloat(r.rate) * 10_000, 4)}
                </span>
              {:else}
                —
              {/if}
            </td>
            <td class="num right">
              {#if annualBps(r.rate) != null}
                {fmtBps(annualBps(r.rate), 0)}
              {:else}
                —
              {/if}
            </td>
            <td class="num right">
              <span data-sev={severity(r.next_funding_ts)}>
                {fmtCountdown(r.next_funding_ts)}
              </span>
              {#if r.next_funding_ts}
                <span class="sub">{fmtRelative(r.next_funding_ts)}</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  {#if lastOk > 0}
    <div class="foot muted small">last poll {fmtRelative(lastOk)}</div>
  {/if}
</div>

<style>
  .funding { display: flex; flex-direction: column; gap: var(--s-2); }
  table { width: 100%; border-collapse: collapse; font-family: var(--font-sans); }
  th {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    text-align: left;
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    font-weight: 500;
  }
  th.right, td.right { text-align: right; }
  td {
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    color: var(--fg-primary);
  }
  td.num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .venue { font-family: var(--font-mono); font-size: var(--fs-xs); color: var(--accent); }
  .sym { font-size: var(--fs-xs); }
  .chip {
    display: inline-block;
    padding: 2px var(--s-2);
    border-radius: var(--r-sm);
    font-size: var(--fs-2xs);
    background: var(--bg-chip);
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  [data-sev='warn'] { color: var(--warn); font-weight: 600; }
  [data-sev='ok']   { color: var(--fg-primary); }
  [data-sev='muted']{ color: var(--fg-muted); }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }
  .sub { color: var(--fg-muted); font-size: var(--fs-2xs); margin-left: 4px; }
  .muted { color: var(--fg-muted); }
  .small { font-size: var(--fs-2xs); }
  .foot { text-align: right; padding: 0 var(--s-2); }
  .alert-bar {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--neg-bg);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: var(--r-md);
    color: var(--neg);
    font-size: var(--fs-xs);
  }
</style>
