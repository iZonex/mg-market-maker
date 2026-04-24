<script>
  /*
   * Global kill-switch badge (23-UX-10). Shows max kill_level
   * across every symbol the WS has published. Click jumps to
   * the Admin page where Controls is mounted.
   *
   * Severity maps: L0 → ok (green), L1 → warn (amber),
   * L2+ → neg (red + pulsing dot).
   */
  let { level = 0, onClick = () => {} } = $props()

  const sev = $derived(level === 0 ? 'ok' : level === 1 ? 'warn' : 'neg')
  const label = $derived({
    0: 'NOMINAL',
    1: 'WIDEN',
    2: 'STOP',
    3: 'CANCEL',
    4: 'FLATTEN',
    5: 'DISC',
  }[level] || 'NOMINAL')
</script>

<button
  type="button"
  class="kill-badge kill-{sev}"
  class:kill-alarm={level >= 2}
  onclick={onClick}
  title={`Kill switch (max across symbols): L${level} — click to open Admin`}
>
  <span class="kill-dot"></span>
  <span class="kill-level">L{level}</span>
  <span class="kill-label">{label}</span>
</button>

<style>
  .kill-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    padding: 0 var(--s-3);
    border-radius: var(--r-pill);
    border: 1px solid var(--border-subtle);
    background: var(--bg-chip);
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-size: var(--fs-2xs);
    font-weight: 700;
    letter-spacing: var(--tracking-label);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out),
                border-color var(--dur-fast) var(--ease-out);
  }
  .kill-badge:hover { border-color: var(--border-default); }
  .kill-badge.kill-ok   { color: var(--pos);  background: var(--pos-bg);  border-color: color-mix(in srgb, var(--pos) 30%, transparent); }
  .kill-badge.kill-warn { color: var(--warn); background: var(--warn-bg); border-color: color-mix(in srgb, var(--warn) 30%, transparent); }
  .kill-badge.kill-neg  { color: var(--neg);  background: var(--neg-bg);  border-color: color-mix(in srgb, var(--neg) 35%, transparent); }
  .kill-dot {
    width: 6px; height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
  .kill-badge.kill-alarm .kill-dot {
    animation: killPulse 0.7s ease-in-out infinite;
  }
  @keyframes killPulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.3; transform: scale(0.6); }
  }
  .kill-level { opacity: 0.85; }
  .kill-label { font-weight: 600; }
</style>
