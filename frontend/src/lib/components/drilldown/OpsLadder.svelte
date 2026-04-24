<script>
  /*
   * Kill-ladder + aux ops buttons for a single deployment.
   *
   * L1 widen → L5 disconnect. Auxiliary row: reset, pause,
   * resume, DCA cancel. Parent handles the actual dispatch via
   * `onKillOp(op, verb)` (prompts for a reason inside the
   * parent) and `onOp(op)` for ops that don't need a reason.
   */

  let {
    canControl = false,
    onKillOp,
    onOp,
  } = $props()
</script>

<div class="ops-row">
  <button type="button" class="op-btn kl-L1" disabled={!canControl}
    onclick={() => onKillOp('widen', 'WIDEN')}>
    <span class="kl-tag">L1</span><span>Widen</span>
  </button>
  <button type="button" class="op-btn kl-L2" disabled={!canControl}
    onclick={() => onKillOp('stop', 'STOP NEW')}>
    <span class="kl-tag">L2</span><span>Stop</span>
  </button>
  <button type="button" class="op-btn kl-L3" disabled={!canControl}
    onclick={() => onKillOp('cancel-all', 'CANCEL ALL')}>
    <span class="kl-tag">L3</span><span>Cancel</span>
  </button>
  <button type="button" class="op-btn kl-L4" disabled={!canControl}
    onclick={() => onKillOp('flatten', 'FLATTEN')}>
    <span class="kl-tag">L4</span><span>Flatten</span>
  </button>
  <button type="button" class="op-btn kl-L5" disabled={!canControl}
    onclick={() => onKillOp('disconnect', 'DISCONNECT')}>
    <span class="kl-tag">L5</span><span>Disconnect</span>
  </button>
</div>
<div class="ops-row ops-row-aux">
  <button type="button" class="op-btn aux" disabled={!canControl}
    onclick={() => onKillOp('reset', 'RESET')}>
    Reset kill switch
  </button>
  <button type="button" class="op-btn aux" disabled={!canControl}
    onclick={() => onOp('pause')}>
    Pause quoting
  </button>
  <button type="button" class="op-btn aux" disabled={!canControl}
    onclick={() => onOp('resume')}>
    Resume quoting
  </button>
  <button type="button" class="op-btn aux" disabled={!canControl}
    onclick={() => onOp('dca-cancel')}>
    Cancel DCA
  </button>
</div>
{#if !canControl}
  <p class="ops-hint">Read-only — operator role required.</p>
{:else}
  <p class="ops-hint">
    Ladder actions prompt for a reason; reset clears the manual
    escalation recorded in the audit trail. Pause/Resume flip the
    <code>paused</code> variable live.
  </p>
{/if}

<style>
  .ops-row {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: var(--s-2);
  }
  .ops-row-aux {
    grid-template-columns: repeat(4, 1fr);
    margin-top: var(--s-2);
  }
  .op-btn {
    display: inline-flex; align-items: center; justify-content: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    color: var(--fg-secondary);
    font-family: var(--font-sans);
    font-size: var(--fs-xs);
    font-weight: 600;
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease-out), background var(--dur-fast) var(--ease-out);
  }
  .op-btn:hover:not(:disabled) {
    border-color: var(--border-strong); color: var(--fg-primary);
  }
  .op-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .op-btn .kl-tag {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--r-sm);
    font-weight: 700;
  }
  .op-btn.kl-L1 .kl-tag { background: color-mix(in srgb, var(--kl-L1) 22%, transparent); color: var(--kl-L1); }
  .op-btn.kl-L1:hover:not(:disabled) { border-color: color-mix(in srgb, var(--kl-L1) 45%, transparent); }
  .op-btn.kl-L2 .kl-tag { background: color-mix(in srgb, var(--kl-L2) 28%, transparent); color: var(--kl-L2); }
  .op-btn.kl-L2:hover:not(:disabled) { border-color: color-mix(in srgb, var(--kl-L2) 55%, transparent); }
  .op-btn.kl-L3 .kl-tag { background: color-mix(in srgb, var(--kl-L3) 20%, transparent); color: var(--kl-L3); }
  .op-btn.kl-L3:hover:not(:disabled) { border-color: color-mix(in srgb, var(--kl-L3) 50%, transparent); }
  .op-btn.kl-L4 .kl-tag { background: color-mix(in srgb, var(--kl-L4) 40%, transparent); color: var(--fg-primary); }
  .op-btn.kl-L4:hover:not(:disabled) { border-color: color-mix(in srgb, var(--kl-L4) 75%, transparent); color: var(--kl-L4); }
  .op-btn.kl-L5 .kl-tag { background: color-mix(in srgb, var(--kl-L5) 85%, transparent); color: var(--fg-primary); }
  .op-btn.kl-L5:hover:not(:disabled) { border-color: color-mix(in srgb, var(--kl-L5) 90%, transparent); color: var(--kl-L5); }
  .op-btn.aux { font-size: var(--fs-2xs); }
  .ops-hint {
    margin: var(--s-2) 0 0;
    font-size: var(--fs-2xs);
    color: var(--fg-muted);
    line-height: var(--lh-snug);
  }
  .ops-hint code { font-family: var(--font-mono); background: var(--bg-chip); padding: 0 4px; border-radius: 3px; }
</style>
