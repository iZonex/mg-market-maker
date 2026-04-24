<script>
  /*
   * One violation row — severity chip, metric, detail, and the
   * action cluster (Pause / Widen L1 / Open incident / Hide).
   * Parent handles the actual dispatch via callbacks; this
   * component only renders the current busy/status state.
   */

  let {
    violation,
    busy = false,
    status = null,
    onPause,
    onWidenL1,
    onOpenIncident,
    onHide,
  } = $props()

  const hasDeps = $derived((violation.deployments || []).length > 0)
</script>

<tr class="sev-{violation.severity}">
  <td><span class="sev-chip sev-{violation.severity}">{violation.severity}</span></td>
  <td class="mono">{violation.category}</td>
  <td class="mono">{violation.target}</td>
  <td class="mono">{violation.metric}</td>
  <td>
    {violation.detail}
    {#if hasDeps && violation.deployments.length > 1}
      <span class="muted"> · {violation.deployments.length} deployments</span>
    {/if}
  </td>
  <td class="actions-cell">
    {#if status}
      <span class="action-status {status.phase}">{status.text}</span>
    {/if}
    {#if hasDeps}
      <button type="button" class="row-btn" disabled={busy} onclick={() => onPause(violation)} title="Flip paused=true on every affected deployment">Pause</button>
      <button type="button" class="row-btn" disabled={busy} onclick={() => onWidenL1(violation)} title="Escalate to L1 (widen spreads) on every affected deployment">Widen L1</button>
    {/if}
    <button type="button" class="row-btn" disabled={busy} onclick={() => onOpenIncident(violation)} title="Open a tracked incident on the controller — persistent, supports ack/resolve + post-mortem">Open incident</button>
    <button type="button" class="row-btn ghost" onclick={() => onHide(violation.key)} title="Hide this row from the current session — doesn't change anything on the engine">Hide</button>
  </td>
</tr>

<style>
  tr.sev-high td { background: color-mix(in srgb, var(--danger) 8%, transparent); }
  .sev-chip {
    padding: 2px 8px; font-size: 10px; font-family: var(--font-mono);
    border-radius: var(--r-sm); font-weight: 500;
    text-transform: uppercase; letter-spacing: var(--tracking-label);
  }
  .sev-chip.sev-high { background: color-mix(in srgb, var(--danger) 25%, transparent); color: var(--danger); font-weight: 600; }
  .sev-chip.sev-med  { background: color-mix(in srgb, var(--warn) 20%, transparent); color: var(--warn); }
  .sev-chip.sev-low  { background: color-mix(in srgb, var(--accent) 18%, transparent); color: var(--accent); }

  td {
    padding: var(--s-2); font-size: var(--fs-xs); text-align: left;
    border-bottom: 1px solid var(--border-subtle);
  }
  .actions-cell {
    text-align: right;
    display: flex; gap: 4px; justify-content: flex-end;
    flex-wrap: wrap; align-items: center;
  }
  .row-btn {
    padding: 2px 8px; font-size: 10px; font-family: var(--font-mono);
    background: var(--bg-chip); color: var(--fg-secondary);
    border: 1px solid var(--border-subtle); border-radius: var(--r-sm);
    cursor: pointer;
  }
  .row-btn:hover { border-color: var(--warn); color: var(--warn); }
  .row-btn.ghost { background: transparent; }
  .row-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .action-status {
    font-size: 10px; font-family: var(--font-mono);
    padding: 1px 6px; border-radius: var(--r-sm);
    margin-right: 4px;
  }
  .action-status.pending { background: var(--bg-raised); color: var(--fg-muted); }
  .action-status.ok { background: color-mix(in srgb, var(--ok) 18%, transparent); color: var(--ok); }
  .action-status.warn { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .action-status.err { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
</style>
