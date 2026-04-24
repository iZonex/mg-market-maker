<script>
  /*
   * <StatusPill> — larger-than-Chip status marker with a leading
   * dot. Use for top-of-page indicators (kill-switch level,
   * regime, feed health) where the status IS the message.
   *
   * Design-system contract:
   *   - Tokens only.
   *   - `severity` drives the dot colour; `label` is the body
   *     text. Children optional — either provide `label` prop or
   *     default children, not both.
   */

  let {
    /** @type {'ok'|'info'|'warn'|'danger'|'muted'} */
    severity = 'muted',
    /** Optional short text; overrides children. */
    label,
    /** When true, pulse the dot to draw attention. */
    pulse = false,
    children,
    ...rest
  } = $props()
</script>

<span class="pill sev-{severity}" {...rest}>
  <span class="dot" class:pulse></span>
  <span class="label">
    {#if label}{label}{:else}{@render children?.()}{/if}
  </span>
</span>

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px;
    border-radius: var(--r-pill);
    background: var(--bg-chip);
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    color: var(--fg-secondary);
    line-height: 1;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg-muted);
    flex: none;
  }
  .dot.pulse {
    animation: dot-pulse 1.4s ease-in-out infinite;
  }
  @keyframes dot-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.55; transform: scale(1.2); }
  }

  .sev-ok     .dot { background: var(--pos); }
  .sev-ok          { color: var(--pos); }
  .sev-info   .dot { background: var(--info); }
  .sev-info        { color: var(--info); }
  .sev-warn   .dot { background: var(--warn); }
  .sev-warn        { color: var(--warn); }
  .sev-danger .dot { background: var(--danger); }
  .sev-danger      { color: var(--danger); font-weight: 600; }
  .sev-muted  .dot { background: var(--fg-muted); }
</style>
