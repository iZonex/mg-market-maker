<script>
  /*
   * Restricted-deploy acknowledgement. Fires when the operator
   * deploys a graph that references pentest-only nodes. Demands
   * an explicit checkbox before the danger-tinted Confirm button
   * becomes enabled.
   *
   * Parent owns `state` shape:
   *   { nodes: string[], acknowledged: boolean, busy: boolean, error?: string }
   */
  import { Button, Modal } from '../../primitives/index.js'

  let {
    state,
    onAckChange,
    onConfirm,
    onClose,
  } = $props()

  const open = $derived(!!state)
</script>

<Modal
  {open}
  ariaLabel="Restricted deploy acknowledgement"
  maxWidth="560px"
  onClose={() => { if (!state?.busy) onClose() }}
>
  {#snippet children()}
    {#if state}
      <div class="ack-title">⚠ Restricted deploy</div>
      <div class="ack-body">
        <p class="ack-lead">
          This graph references {state.nodes.length} pentest-only
          node{state.nodes.length === 1 ? '' : 's'}. Deployment places
          market-manipulating patterns on the engine pool; make sure
          the run is authorised before continuing.
        </p>
        <ul class="ack-nodes">
          {#each state.nodes as n (n)}
            <li><code>{n}</code></li>
          {/each}
        </ul>
        <label class="ack-check">
          <input
            type="checkbox"
            checked={state.acknowledged}
            disabled={state.busy}
            onchange={(e) => onAckChange(e.currentTarget.checked)}
          />
          <span>I acknowledge the restricted node list above and authorise this deploy.</span>
        </label>
        {#if state.error}
          <div class="ack-error">{state.error}</div>
        {/if}
      </div>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if state}
      <Button variant="ghost" onclick={onClose} disabled={state.busy}>
        {#snippet children()}Cancel{/snippet}
      </Button>
      <Button
        variant="danger"
        onclick={onConfirm}
        loading={state.busy}
        disabled={!state.acknowledged}
      >
        {#snippet children()}Acknowledge & Deploy{/snippet}
      </Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .ack-title {
    font-size: var(--fs-lg); font-weight: 600;
    color: var(--danger); letter-spacing: var(--tracking-tight);
    margin: 0 0 var(--s-3);
  }
  .ack-body { display: flex; flex-direction: column; gap: var(--s-3); }
  .ack-lead { margin: 0; font-size: var(--fs-sm); color: var(--fg-secondary); line-height: 1.5; }
  .ack-nodes {
    margin: 0; padding: var(--s-2) var(--s-3);
    background: color-mix(in srgb, var(--danger) 6%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: var(--r-sm);
    list-style: none;
    display: flex; flex-direction: column; gap: 4px;
  }
  .ack-nodes li code {
    font-family: var(--font-mono); font-size: var(--fs-xs);
    color: var(--fg-primary);
  }
  .ack-check {
    display: flex; gap: var(--s-2); align-items: flex-start;
    font-size: var(--fs-xs); color: var(--fg-secondary);
    cursor: pointer;
  }
  .ack-check input { margin-top: 2px; }
  .ack-error {
    padding: var(--s-2); font-size: var(--fs-xs);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    color: var(--danger); border-radius: var(--r-sm);
  }
</style>
