<script>
  let { auth } = $props()
  let report = $state(null)
  let loading = $state(false)
  let error = $state('')

  async function loadReport() {
    loading = true
    error = ''
    try {
      const resp = await fetch('/api/v1/client/report/daily', {
        headers: { 'Authorization': `Bearer ${auth.state.token}` }
      })
      if (resp.ok) {
        report = await resp.json()
      } else {
        error = 'Failed to load report'
      }
    } catch (e) {
      error = e.message
    }
    loading = false
  }

  // Auto-load on mount.
  $effect(() => {
    if (auth.state.loggedIn) loadReport()
  })
</script>

{#if loading}
  <div class="loading">Loading report...</div>
{:else if error}
  <div class="error">{error}</div>
{:else if report}
  <div class="report">
    <div class="report-header">
      <h2>Daily Performance Report</h2>
      <div class="meta">
        <span>{report.date}</span>
        <span class="generated">Generated: {new Date(report.generated_at).toLocaleString()}</span>
      </div>
    </div>

    <!-- Totals -->
    <div class="totals">
      <div class="total-card">
        <div class="total-label">Volume (24h)</div>
        <div class="total-value">${parseFloat(report.summary?.totals?.total_volume_24h || 0).toLocaleString()}</div>
      </div>
      <div class="total-card">
        <div class="total-label">Spread Compliance</div>
        <div class="total-value" class:good={parseFloat(report.summary?.totals?.avg_spread_compliance_pct || 0) >= 95}>
          {parseFloat(report.summary?.totals?.avg_spread_compliance_pct || 0).toFixed(1)}%
        </div>
      </div>
      <div class="total-card">
        <div class="total-label">Uptime</div>
        <div class="total-value" class:good={parseFloat(report.summary?.totals?.avg_uptime_pct || 0) >= 95}>
          {parseFloat(report.summary?.totals?.avg_uptime_pct || 0).toFixed(1)}%
        </div>
      </div>
    </div>

    <!-- Per-symbol breakdown -->
    {#if report.summary?.symbols}
      <h3>Per-Symbol Performance</h3>
      <table>
        <thead>
          <tr>
            <th>Symbol</th>
            <th>Exchange</th>
            <th>Avg Spread</th>
            <th>Compliance</th>
            <th>Uptime</th>
            <th>Volume</th>
            <th>Price</th>
          </tr>
        </thead>
        <tbody>
          {#each report.summary.symbols as sym}
            <tr>
              <td class="symbol">{sym.symbol}</td>
              <td>{sym.exchange}</td>
              <td>{parseFloat(sym.avg_spread_bps).toFixed(1)} bps</td>
              <td class:good={parseFloat(sym.spread_compliance_pct) >= 95}>
                {parseFloat(sym.spread_compliance_pct).toFixed(1)}%
              </td>
              <td class:good={parseFloat(sym.uptime_pct) >= 95}>
                {parseFloat(sym.uptime_pct).toFixed(1)}%
              </td>
              <td>${parseFloat(sym.volume_24h).toLocaleString()}</td>
              <td>${parseFloat(sym.mid_price).toLocaleString()}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <!-- Spread Quality -->
    {#if report.spread_quality?.length > 0}
      <h3>Spread Quality</h3>
      <table>
        <thead>
          <tr>
            <th>Symbol</th>
            <th>Within Target</th>
            <th>TWAS</th>
            <th>VWAS</th>
            <th>Normal</th>
            <th>High Vol</th>
            <th>Current</th>
            <th>Target</th>
          </tr>
        </thead>
        <tbody>
          {#each report.spread_quality as sq}
            <tr>
              <td class="symbol">{sq.symbol}</td>
              <td class:good={parseFloat(sq.within_target_pct) >= 95}>
                {parseFloat(sq.within_target_pct).toFixed(1)}%
              </td>
              <td>{parseFloat(sq.time_weighted_avg_bps).toFixed(1)} bps</td>
              <td>{parseFloat(sq.volume_weighted_avg_bps).toFixed(1)} bps</td>
              <td>{parseFloat(sq.normal_avg_bps).toFixed(1)} bps</td>
              <td>{parseFloat(sq.high_vol_avg_bps).toFixed(1)} bps</td>
              <td>{parseFloat(sq.current_bps).toFixed(1)} bps</td>
              <td>{parseFloat(sq.target_bps).toFixed(0)} bps</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <button class="refresh" onclick={loadReport}>Refresh Report</button>
  </div>
{/if}

<style>
  .report { padding: 4px; }
  .report-header { margin-bottom: 20px; }
  h2 { font-size: 18px; color: #58a6ff; margin-bottom: 4px; }
  h3 { font-size: 13px; color: #8b949e; margin: 20px 0 8px; text-transform: uppercase; letter-spacing: 0.5px; }
  .meta { font-size: 11px; color: #484f58; }
  .generated { margin-left: 12px; }
  .totals {
    display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 12px;
    margin-bottom: 20px;
  }
  .total-card {
    background: #0d1117; border: 1px solid #21262d; border-radius: 8px;
    padding: 16px; text-align: center;
  }
  .total-label { font-size: 11px; color: #8b949e; text-transform: uppercase; margin-bottom: 4px; }
  .total-value { font-size: 24px; font-weight: 700; color: #e1e4e8; }
  .total-value.good { color: #3fb950; }
  table { width: 100%; border-collapse: collapse; margin-bottom: 12px; }
  th { font-size: 10px; color: #484f58; text-align: left; padding: 6px; border-bottom: 1px solid #21262d; }
  td { font-size: 12px; padding: 6px; border-bottom: 1px solid #161b22; }
  .symbol { color: #58a6ff; font-weight: 600; }
  .good { color: #3fb950; }
  .loading { text-align: center; padding: 40px; color: #8b949e; }
  .error { color: #f85149; text-align: center; padding: 20px; }
  .refresh {
    margin-top: 12px; padding: 8px 16px; background: #21262d; border: 1px solid #30363d;
    color: #e1e4e8; border-radius: 4px; cursor: pointer; font-family: inherit; font-size: 12px;
  }
  .refresh:hover { background: #30363d; }
</style>
