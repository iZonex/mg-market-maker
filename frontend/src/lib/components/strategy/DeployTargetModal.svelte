<script>
  /*
   * Wave A3 — unified deploy modal. Operator clicks Deploy once;
   * this modal opens pre-filled with the fleet, multi-select
   * targets, Confirm fires save + parallel graph-swap to every
   * selected (agent, deployment).
   *
   * Parent owns `state` shape:
   *   { rows: [{ key, agent, deployment, current_hash }],
   *     selected: { [key]: true },
   *     phase: 'select' | 'dispatching' | 'done',
   *     results: [{ key, phase: 'pending'|'ok'|'err', detail }],
   *     status: string }
   */
  import { Button, Modal } from '../../primitives/index.js'

  let {
    state,
    deployBusy = false,
    onToggleTarget,
    onConfirm,
    onClose,
  } = $props()

  const open = $derived(!!state)
</script>

<Modal {open} ariaLabel="Deploy graph — pick targets" maxWidth="720px" {onClose}>
  {#snippet children()}
    {#if state}
      {@const phase = state.phase}
      <div class="ack-title">
        {#if phase === 'select'}Deploy graph — pick target(s)
        {:else if phase === 'dispatching'}Dispatching graph…
        {:else}Deploy result
        {/if}
      </div>
      <div class="ack-body">
        {#if phase === 'select'}
          <p class="ack-lead">
            Check every (agent · deployment) that should run this graph.
            Save + swap fire in a single action; each target is dispatched
            in parallel. Already-running graph hashes are shown for comparison.
          </p>
        {:else}
          <p class="ack-lead">{state.status}</p>
        {/if}
        {#if state.rows.length === 0}
          <div class="ack-error">No running deployments on any accepted agent. Launch one via Fleet → Deploy strategy first.</div>
        {:else}
          <div class="deploy-rows">
            {#each state.rows as row (row.key)}
              {@const res = state.results.find((x) => x.key === row.key)}
              <label class="deploy-row-label" class:disabled={phase !== 'select'}>
                <input
                  type="checkbox"
                  checked={!!state.selected[row.key]}
                  onchange={() => onToggleTarget(row.key)}
                  disabled={phase !== 'select'}
                />
                <span class="deploy-row-inner">
                  <span class="deploy-title mono">{row.deployment.template || 'deployment'} · {row.deployment.symbol}</span>
                  <span class="deploy-sub mono">
                    {row.agent.agent_id}
                    {#if row.deployment.venue}· {row.deployment.venue}{/if}
                    {#if row.deployment.product}· {row.deployment.product}{/if}
                    {#if row.current_hash}· current @{row.current_hash.slice(0, 8)}{/if}
                    · <span class="faint">{row.deployment.deployment_id}</span>
                  </span>
                  {#if res}
                    <span class="deploy-res res-{res.phase}">
                      {#if res.phase === 'pending'}dispatching…
                      {:else if res.phase === 'ok'}✓ applied
                      {:else}✗ {res.detail}
                      {/if}
                    </span>
                  {/if}
                </span>
              </label>
            {/each}
          </div>
        {/if}
        {#if state.status && phase === 'select' && state.rows.length > 0}
          <div class="ack-hint">{state.status}</div>
        {/if}
      </div>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if state}
      {@const targets = state.rows.filter((r) => state.selected[r.key])}
      {@const phase = state.phase}
      <Button variant="ghost" onclick={onClose}>
        {#snippet children()}{phase === 'done' ? 'Close' : 'Cancel'}{/snippet}
      </Button>
      {#if phase === 'select'}
        <Button variant="ok" disabled={targets.length === 0 || deployBusy} onclick={onConfirm}>
          {#snippet children()}Deploy to {targets.length} target{targets.length === 1 ? '' : 's'}{/snippet}
        </Button>
      {/if}
    {/if}
  {/snippet}
</Modal>

<style>
  .ack-title {
    font-size: var(--fs-lg); font-weight: 600;
    color: var(--fg-primary); letter-spacing: var(--tracking-tight);
    margin: 0 0 var(--s-3);
  }
  .ack-body { display: flex; flex-direction: column; gap: var(--s-3); }
  .ack-lead { margin: 0; font-size: var(--fs-sm); color: var(--fg-secondary); line-height: 1.5; }
  .ack-hint { font-size: var(--fs-xs); color: var(--fg-muted); }
  .ack-error {
    padding: var(--s-2); font-size: var(--fs-xs);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    color: var(--danger); border-radius: var(--r-sm);
  }
  .deploy-rows { display: flex; flex-direction: column; gap: 4px; max-height: 420px; overflow-y: auto; }
  .deploy-row-label {
    display: flex; gap: var(--s-2); align-items: flex-start;
    padding: var(--s-2) var(--s-3);
    background: var(--bg-chip);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
  }
  .deploy-row-label:hover:not(.disabled) { border-color: var(--accent); background: var(--bg-raised); }
  .deploy-row-label.disabled { cursor: default; opacity: 0.85; }
  .deploy-row-label input { margin-top: 2px; }
  .deploy-row-inner { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
  .deploy-title { font-size: var(--fs-sm); color: var(--fg-primary); font-weight: 500; }
  .deploy-sub { font-size: 10px; color: var(--fg-muted); }
  .deploy-sub .faint { color: var(--fg-faint); }
  .deploy-res { font-size: 10px; margin-top: 2px; }
  .res-pending { color: var(--fg-muted); }
  .res-ok { color: var(--ok); }
  .res-err { color: var(--danger); }
</style>
