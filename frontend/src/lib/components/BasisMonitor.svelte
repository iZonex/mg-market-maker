<script>
  /*
   * 23-UX-5 — basis monitor.
   *
   * For every base asset with ≥ 2 L1 books on the bus, shows
   * the pairwise basis in bps — spot-vs-perp within a venue,
   * spot-vs-spot across venues. Critical for MM decisions
   * around funding arb + cross-venue routing: when basis flips
   * sign (Binance perp drops below Binance spot) that's the
   * signal to flatten the long-perp / short-spot carry trade.
   */
  import { createApiClient } from '../api.svelte.js'
  import { fmtBps, fmtPrice, fmtRelative } from '../format.js'
  import Icon from './Icon.svelte'

  let { auth } = $props()
  const api = $derived(createApiClient(auth))

  let bases = $state([])
  let error = $state('')
  let lastOk = $state(0)

  async function refresh() {
    try {
      bases = await api.getJson('/api/v1/basis')
      error = ''
      lastOk = Date.now()
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => {
    refresh()
    const id = setInterval(refresh, 3000)
    return () => clearInterval(id)
  })

  function basisSeverity(bps) {
    const abs = Math.abs(parseFloat(bps || 0))
    if (abs < 5) return 'muted'   // basically zero
    if (abs < 25) return 'info'    // typical
    if (abs < 100) return 'warn'   // wide — opportunity or stress
    return 'neg'                    // > 100 bps — alarm
  }
</script>

<div class="basis">
  {#if error}
    <div class="alert-bar">
      <Icon name="alert" size={14} />
      <span>{error}</span>
    </div>
  {:else if bases.length === 0}
    <div class="empty-state">
      <span class="empty-state-title">Need ≥ 2 legs per base asset</span>
      <span class="empty-state-hint">
        Basis shows up when at least two L1 books for the same
        base currency (across venues or products) have been
        published. Spot-vs-perp or BTCUSDT-vs-BTCUSDC both count.
      </span>
    </div>
  {:else}
    {#each bases as b (b.base_asset)}
      <div class="base-block">
        <div class="base-head">
          <span class="base-asset">{b.base_asset}</span>
          <span class="ref-hint">
            ref: <code>{b.reference_venue}:{b.reference_symbol}:{b.reference_product}</code>
            mid <span class="num">{fmtPrice(b.reference_mid, 4)}</span>
          </span>
        </div>
        <table class="grid">
          <thead>
            <tr>
              <th>Venue</th>
              <th>Symbol</th>
              <th>Product</th>
              <th class="right">Mid</th>
              <th class="right">Basis bps</th>
            </tr>
          </thead>
          <tbody>
            {#each b.legs as l (l.venue + ':' + l.symbol + ':' + l.product)}
              <tr>
                <td class="venue">{l.venue}</td>
                <td class="sym num">{l.symbol}</td>
                <td><span class="chip">{l.product}</span></td>
                <td class="num right">{fmtPrice(l.mid, 4)}</td>
                <td class="num right">
                  <span data-sev={basisSeverity(l.basis_bps)}
                        class:pos={parseFloat(l.basis_bps) > 0}
                        class:neg={parseFloat(l.basis_bps) < 0}>
                    {fmtBps(l.basis_bps)}
                  </span>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/each}
  {/if}
  {#if lastOk > 0}
    <div class="foot muted small">last poll {fmtRelative(lastOk)}</div>
  {/if}
</div>

<style>
  .basis { display: flex; flex-direction: column; gap: var(--s-3); }
  .base-block { display: flex; flex-direction: column; gap: var(--s-1); }
  .base-head {
    display: flex; align-items: baseline; gap: var(--s-3);
    padding: var(--s-1) var(--s-2);
  }
  .base-asset {
    font-size: var(--fs-md); font-weight: 600;
    color: var(--fg-primary);
    font-family: var(--font-mono);
  }
  .ref-hint { color: var(--fg-muted); font-size: var(--fs-2xs); }
  .ref-hint code {
    font-family: var(--font-mono);
    background: var(--bg-chip);
    padding: 1px 4px; border-radius: 3px;
    color: var(--accent);
    font-size: var(--fs-2xs);
  }
  table { width: 100%; border-collapse: collapse; }
  th {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    text-align: left;
    padding: var(--s-1) var(--s-2);
    border-bottom: 1px solid var(--border-subtle);
    font-weight: 500;
  }
  th.right, td.right { text-align: right; }
  td {
    padding: var(--s-1) var(--s-2);
    border-bottom: 1px solid var(--border-subtle);
    color: var(--fg-primary);
  }
  td.num { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .venue { font-family: var(--font-mono); font-size: var(--fs-xs); color: var(--accent); }
  .sym { font-size: var(--fs-xs); }
  .chip {
    display: inline-block;
    padding: 1px var(--s-2);
    border-radius: var(--r-sm);
    font-size: var(--fs-2xs);
    background: var(--bg-chip);
    color: var(--fg-secondary);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
  }
  [data-sev='warn'] { color: var(--warn); font-weight: 600; }
  [data-sev='neg']  { color: var(--neg); font-weight: 600; }
  [data-sev='info'] { color: var(--info); }
  [data-sev='muted']{ color: var(--fg-muted); }
  .pos { color: var(--pos); }
  .neg { color: var(--neg); }
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
