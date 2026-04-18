<script>
  let { data } = $props()
  const fills = $derived(data.state.fills || [])
  const visible = $derived(fills.slice(0, 20))

  function fmtTime(t) {
    try { return new Date(t || Date.now()).toLocaleTimeString() }
    catch { return '—' }
  }
</script>

{#if visible.length === 0}
  <div class="empty-state">
    <span class="empty-state-icon">
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <path d="M3 12a9 9 0 1 0 9-9"/>
        <polyline points="3 4 3 12 11 12"/>
      </svg>
    </span>
    <span class="empty-state-title">No fills yet</span>
    <span class="empty-state-hint">Paper-mode fills synthesise from public trades that cross our quotes.</span>
  </div>
{:else}
  <div class="fills scroll">
    <table>
      <thead>
        <tr>
          <th>Time</th>
          <th>Side</th>
          <th class="right">Price</th>
          <th class="right">Qty</th>
          <th>Role</th>
        </tr>
      </thead>
      <tbody>
        {#each visible as fill}
          <tr>
            <td class="time">{fmtTime(fill.timestamp)}</td>
            <td>
              <span class="side" data-side={fill.side?.toLowerCase()}>{fill.side?.toUpperCase()}</span>
            </td>
            <td class="num right">{fill.price}</td>
            <td class="num right">{fill.qty}</td>
            <td>
              <span class="role" class:maker={fill.is_maker} class:taker={!fill.is_maker}>
                {fill.is_maker ? 'MAKER' : 'TAKER'}
              </span>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .fills {
    max-height: 260px;
    overflow-y: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-family: var(--font-sans);
  }
  th {
    font-size: var(--fs-2xs);
    text-transform: uppercase;
    letter-spacing: var(--tracking-label);
    color: var(--fg-muted);
    text-align: left;
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    position: sticky;
    top: 0;
    background: var(--bg-raised);
    font-weight: 500;
  }
  th.right { text-align: right; }
  td {
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--fs-sm);
    color: var(--fg-primary);
  }
  td.num {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  td.right { text-align: right; }
  .time {
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--fs-xs);
  }
  .side {
    display: inline-flex;
    align-items: center;
    padding: 2px var(--s-2);
    border-radius: var(--r-sm);
    font-size: var(--fs-2xs);
    font-weight: 700;
    letter-spacing: var(--tracking-label);
    font-family: var(--font-mono);
  }
  .side[data-side='buy']  { background: var(--pos-bg); color: var(--pos); }
  .side[data-side='sell'] { background: var(--neg-bg); color: var(--neg); }
  .role {
    display: inline-flex;
    align-items: center;
    padding: 1px var(--s-2);
    border-radius: var(--r-sm);
    font-size: var(--fs-2xs);
    font-weight: 600;
    letter-spacing: var(--tracking-label);
    font-family: var(--font-mono);
  }
  .role.maker { background: var(--info-bg); color: var(--info); }
  .role.taker { background: var(--bg-chip); color: var(--fg-muted); }
</style>
