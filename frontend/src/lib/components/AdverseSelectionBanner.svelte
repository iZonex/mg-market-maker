<script>
  /*
   * Overview-level adverse-selection banner.
   *
   * MM operator loop: "spread widens, am I being adversely
   * selected?" The full AdverseSelection panel lives on Admin,
   * but the critical signal — "some symbol is toxic RIGHT now" —
   * needs to be on the primary cockpit. This banner polls the
   * same /api/v1/adverse-selection endpoint and shows ONLY rows
   * that cross the warn threshold. Quiet by design: zero height
   * when nothing's toxic.
   *
   * Thresholds mirror the full panel's colour logic:
   *   |adverse_bps| ≥ 5  → warning (the 2-bps lane is still
   *                        info-tier, not a primary-screen alert)
   *   as_prob_bid / ask drifting past 0.55 / 0.45 → also banner
   */
  import { createApiClient } from '../api.svelte.js'

  let { auth, route } = $props()
  const api = $derived(createApiClient(auth))

  const REFRESH_MS = 6_000

  let rows = $state([])
  let error = $state(null)

  async function refresh() {
    try {
      const fleet = await api.getJson('/api/v1/fleet')
      const fetches = []
      for (const a of Array.isArray(fleet) ? fleet : []) {
        for (const d of a.deployments || []) {
          if (!d.running) continue
          const path = `/api/v1/agents/${encodeURIComponent(a.agent_id)}`
            + `/deployments/${encodeURIComponent(d.deployment_id)}`
            + `/details/adverse_selection`
          fetches.push(
            api.getJson(path)
              .then(resp => resp.payload?.row ? [resp.payload.row] : [])
              .catch(() => []),
          )
        }
      }
      const all = (await Promise.all(fetches)).flat()
      const bySymbol = new Map()
      for (const r of all) {
        const prev = bySymbol.get(r.symbol)
        if (!prev || Number(r.adverse_bps) > Number(prev.adverse_bps)) {
          bySymbol.set(r.symbol, r)
        }
      }
      rows = Array.from(bySymbol.values())
      error = null
    } catch (e) {
      error = e?.message || String(e)
    }
  }

  $effect(() => {
    refresh()
    const t = setInterval(refresh, REFRESH_MS)
    return () => clearInterval(t)
  })

  // Only surface rows that meet the "primary-screen worthy"
  // threshold. Everything else the operator sees on the Admin
  // detail panel — banner must stay quiet when nothing urgent
  // is happening.
  const alerting = $derived.by(() => {
    return rows.filter(r => {
      const bps = Math.abs(Number(r.adverse_bps ?? 0))
      const pbid = Number(r.as_prob_bid ?? 0.5)
      const pask = Number(r.as_prob_ask ?? 0.5)
      return bps >= 5 || pbid >= 0.55 || pbid <= 0.45 || pask >= 0.55 || pask <= 0.45
    })
  })

  function onClick() {
    // Jump to Venues for the full adverse-selection drilldown.
    if (route) route('venues')
  }
</script>

{#if alerting.length > 0}
  <div class="as-banner" role="status">
    <div class="icon" aria-hidden="true">⚠</div>
    <div class="body">
      <div class="title">Adverse-flow detected — {alerting.length} symbol{alerting.length === 1 ? '' : 's'}</div>
      <div class="rows">
        {#each alerting as r (r.symbol)}
          <span class="pill">
            <span class="sym">{r.symbol}</span>
            <span class="sep">·</span>
            <span class="metric">adv {Number(r.adverse_bps).toFixed(1)} bps</span>
            {#if Number(r.as_prob_bid ?? 0.5) >= 0.55 || Number(r.as_prob_bid ?? 0.5) <= 0.45}
              <span class="sep">·</span>
              <span class="metric">ρbid {Number(r.as_prob_bid).toFixed(2)}</span>
            {/if}
            {#if Number(r.as_prob_ask ?? 0.5) >= 0.55 || Number(r.as_prob_ask ?? 0.5) <= 0.45}
              <span class="sep">·</span>
              <span class="metric">ρask {Number(r.as_prob_ask).toFixed(2)}</span>
            {/if}
          </span>
        {/each}
      </div>
    </div>
    <button type="button" class="drill" onclick={onClick} title="Open full panel">
      Details →
    </button>
  </div>
{/if}

<style>
  .as-banner {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    background: rgba(245, 158, 11, 0.08);
    border: 1px solid rgba(245, 158, 11, 0.3);
    border-radius: var(--r-md);
    margin-bottom: var(--s-3);
  }
  .icon {
    font-size: 16px;
    line-height: 1;
    color: var(--warn);
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .title {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--warn);
    letter-spacing: var(--tracking-tight);
  }
  .rows {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-1);
  }  .sym { font-weight: 700; color: var(--fg-primary); }
  .metric { color: var(--fg-secondary); font-variant-numeric: tabular-nums; }
  .sep { color: var(--fg-faint); }
  .drill {
    flex-shrink: 0;
    padding: 4px var(--s-2);
    background: transparent;
    border: 1px solid rgba(245, 158, 11, 0.4);
    border-radius: var(--r-sm);
    color: var(--warn);
    font-size: var(--fs-xs);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .drill:hover {
    background: rgba(245, 158, 11, 0.15);
  }
</style>
