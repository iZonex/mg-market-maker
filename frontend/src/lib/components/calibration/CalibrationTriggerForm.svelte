<script>
  /*
   * Inline hyperopt trigger form — symbol, recording path,
   * trial count, loss fn. Parent handles the POST; this
   * component owns form state + client-side trial clamp.
   */
  import Icon from '../Icon.svelte'
  import { Button } from '../../primitives/index.js'

  let {
    initialSymbol = '',
    initialPath = 'data/recorded/BTCUSDT.jsonl',
    busy = false,
    onSubmit,              // async (payload) => void
    onCancel,
  } = $props()

  let sym = $state(initialSymbol)
  let path = $state(initialPath)
  let trials = $state(100)
  let loss = $state('sharpe')

  async function submit(e) {
    e.preventDefault()
    if (!sym) return
    const clamped = Math.max(10, Math.min(10_000, parseInt(trials, 10) || 100))
    await onSubmit({
      symbol: sym,
      recording_path: path,
      num_trials: clamped,
      loss_fn: loss,
    })
  }
</script>

<form class="trigger" onsubmit={submit}>
  <div class="trigger-head">
    <span class="label">Run hyperopt</span>
    <Button variant="ghost" size="sm" iconOnly onclick={onCancel} aria-label="Close trigger form">
      {#snippet children()}<Icon name="close" size={12} />{/snippet}
    </Button>
  </div>
  <div class="trigger-row">
    <label class="f-label" for="tr-sym">Symbol</label>
    <input id="tr-sym" type="text" class="text-input" bind:value={sym} placeholder="BTCUSDT" disabled={busy} />
  </div>
  <div class="trigger-row">
    <label class="f-label" for="tr-path">Recording path</label>
    <input id="tr-path" type="text" class="text-input" bind:value={path} placeholder="data/recorded/BTCUSDT.jsonl" disabled={busy} />
  </div>
  <div class="trigger-row">
    <label class="f-label" for="tr-trials">Trials</label>
    <input id="tr-trials" type="number" class="text-input" bind:value={trials} min="10" max="10000" step="10" disabled={busy} />
  </div>
  <div class="trigger-row">
    <label class="f-label" for="tr-loss">Loss fn</label>
    <select id="tr-loss" class="select-input" bind:value={loss} disabled={busy}>
      <option value="sharpe">sharpe</option>
      <option value="sortino">sortino</option>
      <option value="calmar">calmar</option>
      <option value="maxdd">maxdd</option>
    </select>
  </div>
  <div class="actions">
    <Button variant="ghost" onclick={onCancel} disabled={busy}>
      {#snippet children()}Cancel{/snippet}
    </Button>
    <Button variant="primary" type="submit" disabled={busy || !sym || !path}>
      {#snippet children()}{#if busy}
        <span class="spinner"></span>
        <span>Queueing…</span>
      {:else}
        <Icon name="bolt" size={14} />
        <span>Queue run</span>
      {/if}{/snippet}
    </Button>
  </div>
</form>

<style>
  .trigger {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    padding: var(--s-4);
    background: var(--bg-base);
    border: 1px dashed var(--border-strong);
    border-radius: var(--r-md);
  }
  .trigger-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--s-1);
  }
  .trigger-row {
    display: grid;
    grid-template-columns: 120px 1fr;
    align-items: center;
    gap: var(--s-3);
  }
  .f-label {
    font-size: var(--fs-xs);
    color: var(--fg-secondary);
    font-weight: 500;
  }
  .text-input,
  .select-input {
    padding: 6px 10px;
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-primary);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
  }
  .text-input:focus,
  .select-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .actions {
    display: flex;
    gap: var(--s-2);
    justify-content: flex-end;
    flex-wrap: wrap;
  }

  .spinner {
    width: 12px; height: 12px;
    border: 2px solid color-mix(in srgb, var(--fg-on-accent) 25%, transparent);
    border-top-color: var(--fg-on-accent);
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
