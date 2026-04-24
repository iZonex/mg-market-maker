<script>
  /*
   * Wave C4 — revoke-flow safety. If the agent has live
   * deployments the modal lists them and forces a choice between
   * "stop all first then revoke" and cancel. Revoking an agent
   * while orders are live leaves them on the venue until the
   * agent reconnects (which it won't — it's revoked), so
   * operators need explicit consent.
   *
   * Parent owns the `state` shape:
   *   { row, liveDeps, reason, phase, results, error? }
   *   phase: 'confirm' | 'stopping' | 'revoking' | 'done'
   */
  import { Button, Modal } from '../../primitives/index.js'

  let {
    state,
    onReasonChange,
    onCancelOrdersAndRevoke,
    onRevokeSkipStop,
    onClose,
  } = $props()

  const open = $derived(!!state)
</script>

<Modal {open} ariaLabel="Revoke agent" maxWidth="560px" {onClose}>
  {#snippet children()}
    {#if state}
      <div class="revoke-title">
        {#if state.phase === 'confirm'}Revoke agent with live deployments
        {:else if state.phase === 'stopping'}Cancelling orders…
        {:else if state.phase === 'revoking'}Revoking agent…
        {:else}Revoke complete
        {/if}
      </div>
      <div class="revoke-body">
        <div class="revoke-sub">
          <span class="fp mono">{state.row.fingerprint}</span>
          <span class="muted">· {state.row.agent_id}</span>
        </div>

        {#if state.phase === 'confirm'}
          <p class="revoke-warn">
            This agent has <strong>{state.liveDeps.length}</strong> live
            deployment{state.liveDeps.length === 1 ? '' : 's'}. Revoking drops
            its authority — orders on the venue will remain open until someone
            cancels them manually.
          </p>
          <div class="dep-list">
            {#each state.liveDeps as d (d.deployment_id)}
              <div class="dep-item">
                <span class="mono">{d.deployment_id}</span>
                <span class="muted">· {d.symbol}</span>
                {#if (d.live_orders || 0) > 0}
                  <span class="chip tone-warn">{d.live_orders} order{d.live_orders === 1 ? '' : 's'}</span>
                {/if}
              </div>
            {/each}
          </div>
          <label class="reason-field">
            <span class="reason-k">Reason</span>
            <input
              type="text"
              value={state.reason}
              oninput={(e) => onReasonChange(e.target.value)}
              placeholder="e.g. key compromise, decommission"
            />
          </label>
        {:else}
          <div class="dep-list">
            {#each state.results as res (res.deployment_id)}
              <div class="dep-item">
                <span class="mono">{res.deployment_id}</span>
                <span class="res-{res.phase}">
                  {#if res.phase === 'pending'}…
                  {:else if res.phase === 'ok'}✓ {res.detail}
                  {:else}✗ {res.detail}
                  {/if}
                </span>
              </div>
            {/each}
          </div>
          {#if state.phase === 'done'}
            {#if state.error}
              <p class="revoke-warn">Agent revoke failed: {state.error}</p>
            {:else}
              <p class="revoke-ok">Agent revoked. Orders were cancelled on the venue before authority was dropped.</p>
            {/if}
          {/if}
        {/if}
      </div>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if state}
      {#if state.phase === 'confirm'}
        <Button variant="ghost" onclick={onClose}>
          {#snippet children()}Cancel{/snippet}
        </Button>
        <Button variant="warn" onclick={onRevokeSkipStop}>
          {#snippet children()}Revoke without cancelling{/snippet}
        </Button>
        <Button variant="danger" onclick={onCancelOrdersAndRevoke}>
          {#snippet children()}Cancel orders + revoke{/snippet}
        </Button>
      {:else if state.phase === 'done'}
        <Button variant="ok" onclick={onClose}>
          {#snippet children()}Close{/snippet}
        </Button>
      {/if}
    {/if}
  {/snippet}
</Modal>

<style>
  .revoke-title { font-size: var(--fs-lg); color: var(--fg-primary); font-weight: 600; }
  .revoke-body { display: flex; flex-direction: column; gap: var(--s-3); }
  .revoke-sub { font-size: var(--fs-xs); color: var(--fg-secondary); }
  .fp { font-weight: 600; color: var(--fg-primary); font-size: var(--fs-sm); }
  .revoke-warn {
    padding: var(--s-2); font-size: var(--fs-xs);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    color: var(--danger); border-radius: var(--r-sm);
    border-left: 2px solid var(--danger);
  }
  .revoke-ok {
    padding: var(--s-2); font-size: var(--fs-xs);
    background: color-mix(in srgb, var(--ok) 12%, transparent);
    color: var(--ok); border-radius: var(--r-sm);
  }
  .dep-list { display: flex; flex-direction: column; gap: 4px; max-height: 240px; overflow-y: auto; }
  .dep-item {
    display: flex; gap: var(--s-2); align-items: center;
    padding: var(--s-2) var(--s-3); background: var(--bg-chip);
    border-radius: var(--r-sm); font-size: var(--fs-xs);
  }
  .reason-field { display: flex; flex-direction: column; gap: 4px; }
  .reason-k { font-size: 10px; color: var(--fg-muted); letter-spacing: var(--tracking-label); text-transform: uppercase; }
  .reason-field input {
    padding: var(--s-2); background: var(--bg-chip); border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm); color: var(--fg-primary); font-family: var(--font-mono); font-size: var(--fs-xs);
  }
  .res-pending { color: var(--fg-muted); }
  .res-ok { color: var(--ok); }
  .res-err { color: var(--danger); }
</style>
