<script>
  /*
   * Wave C8 — flatten preview modal.
   *
   * Before dispatching an L4 flatten, fetch the deployment's
   * current position + visible book depth, render a qty / side /
   * slippage estimate, and require explicit confirm. Parent
   * owns the `state` object shape:
   *
   *   { phase: 'loading' | 'confirm' | 'dispatching' | 'err',
   *     data:  { side, quantity, mid_price, inventory_value_quote,
   *              book_depth_covers_position, estimated_slippage_pct,
   *              book_levels: [{ pct_from_mid, bid_depth_quote, ask_depth_quote }] } | null,
   *     reason: string,
   *     error?: string }
   */
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'

  let {
    state,
    onConfirm,
    onClose,
    onReasonChange,
  } = $props()

  function onBackdropKey(e) {
    if (e.key === 'Escape') onClose()
  }
</script>

<div
  class="flatten-backdrop"
  role="button"
  tabindex="-1"
  aria-label="Close preview"
  onclick={onClose}
  onkeydown={onBackdropKey}
>
  <div
    class="flatten-card"
    role="dialog"
    aria-modal="true"
    aria-label="Flatten preview"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
  >
    <div class="flatten-title">Flatten preview · L4 kill</div>
    {#if state.phase === 'loading'}
      <div class="flatten-body muted">fetching current position + book…</div>
    {:else if state.phase === 'err'}
      <div class="flatten-body">
        <div class="banner err">
          <Icon name="info" size={12} />
          <span>Preview failed: {state.error}</span>
        </div>
      </div>
    {:else if !state.data || state.data.side === 'flat'}
      <div class="flatten-body">
        <div class="banner ok">Position is flat — nothing to unwind.</div>
      </div>
    {:else}
      <div class="flatten-body">
        <div class="flatten-kv">
          <div class="fk-cell">
            <span class="fk-k">side</span>
            <span class="fk-v mono">{state.data.side}</span>
          </div>
          <div class="fk-cell">
            <span class="fk-k">quantity</span>
            <span class="fk-v mono">{state.data.quantity}</span>
          </div>
          <div class="fk-cell">
            <span class="fk-k">mid</span>
            <span class="fk-v mono">{state.data.mid_price}</span>
          </div>
          <div class="fk-cell">
            <span class="fk-k">notional</span>
            <span class="fk-v mono">{state.data.inventory_value_quote}</span>
          </div>
        </div>
        {#if state.data.book_depth_covers_position}
          <div class="banner ok">
            Book depth covers the position within
            <code>{state.data.estimated_slippage_pct ?? '—'}%</code>
            from mid.
          </div>
        {:else}
          <div class="banner err">
            Book depth does NOT fully cover the position at the visible levels —
            expect slippage beyond
            <code>{state.data.estimated_slippage_pct ?? '—'}%</code>
            from mid. A market sweep may pause partway.
          </div>
        {/if}
        {#if state.data.book_levels?.length > 0}
          <table class="lvl-table">
            <thead>
              <tr>
                <th>pct from mid</th>
                <th class="num">bid depth (quote)</th>
                <th class="num">ask depth (quote)</th>
              </tr>
            </thead>
            <tbody>
              {#each state.data.book_levels as l (l.pct_from_mid)}
                <tr>
                  <td class="mono">{l.pct_from_mid}</td>
                  <td class="num mono">{l.bid_depth_quote}</td>
                  <td class="num mono">{l.ask_depth_quote}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
        <label class="flatten-reason">
          <span class="fk-k">Reason</span>
          <input
            type="text"
            value={state.reason}
            oninput={(e) => onReasonChange(e.target.value)}
            placeholder="operator flatten (L4)"
          />
        </label>
      </div>
    {/if}
    <div class="flatten-actions">
      <Button variant="ghost" onclick={onClose}>
        {#snippet children()}Cancel{/snippet}
      </Button>
      {#if state.phase === 'confirm' && state.data?.side !== 'flat'}
        <Button variant="danger" onclick={onConfirm}>
          {#snippet children()}Confirm flatten{/snippet}
        </Button>
      {/if}
    </div>
  </div>
</div>

<style>
  .flatten-backdrop {
    position: fixed; inset: 0; z-index: 50;
    background: var(--bg-overlay);
    display: flex; align-items: center; justify-content: center;
    padding: var(--s-5);
  }
  .flatten-card {
    width: 640px; max-width: 100%;
    background: var(--bg-raised); border: 1px solid var(--border-strong);
    border-radius: var(--r-lg); padding: var(--s-4);
    display: flex; flex-direction: column; gap: var(--s-3);
    max-height: 92vh; overflow-y: auto;
  }
  .flatten-title { font-size: var(--fs-lg); font-weight: 600; color: var(--fg-primary); }
  .flatten-body { display: flex; flex-direction: column; gap: var(--s-2); }
  .flatten-kv {
    display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: var(--s-2);
  }
  .fk-cell { display: flex; flex-direction: column; gap: 2px; padding: var(--s-2); background: var(--bg-chip); border-radius: var(--r-sm); }
  .fk-k { font-size: 10px; color: var(--fg-muted); text-transform: uppercase; letter-spacing: var(--tracking-label); }
  .fk-v { font-size: var(--fs-sm); color: var(--fg-primary); }
  .flatten-reason { display: flex; flex-direction: column; gap: 4px; }
  .flatten-reason input {
    padding: var(--s-2); background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary); font-family: var(--font-mono); font-size: var(--fs-xs);
  }
  .flatten-actions { display: flex; gap: var(--s-2); justify-content: flex-end; }
  .lvl-table { width: 100%; border-collapse: collapse; font-size: var(--fs-xs); }
  .lvl-table th, .lvl-table td {
    padding: 4px var(--s-2); border-bottom: 1px solid var(--border-subtle); text-align: left;
  }
  .lvl-table th { color: var(--fg-muted); text-transform: uppercase; font-size: 10px; letter-spacing: var(--tracking-label); }
  .lvl-table .num { text-align: right; }

  .banner {
    display: flex; align-items: center; gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    border-radius: var(--r-sm);
    font-size: var(--fs-xs);
  }
  .banner.err { color: var(--neg); background: color-mix(in srgb, var(--neg) 8%, transparent); }
  .banner.ok  { color: var(--pos); background: color-mix(in srgb, var(--pos) 8%, transparent); }
  .banner code { font-family: var(--font-mono); background: var(--bg-chip); padding: 0 4px; border-radius: 3px; }
</style>
