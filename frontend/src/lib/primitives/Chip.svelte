<script>
  /*
   * <Chip> — compact status / category marker.
   *
   * Design-system contract:
   *   - Variant names map to token-backed colour palettes: the
   *     tokens.css file owns the hue, this component only names
   *     intent.
   *   - `tone` selects the semantic category, `size` the typography
   *     scale. Use for roles, product tags, regimes, severity.
   */

  let {
    /** @type {'neutral'|'accent'|'positive'|'warn'|'danger'|'info'|'spot'|'perp'|'invperp'|'admin'|'operator'|'viewer'|'client'} */
    tone = 'neutral',
    /** @type {'xs' | 'sm'} */
    size = 'sm',
    /** Optional label override; if absent, uses children. */
    children,
    ...rest
  } = $props()
</script>

<span class="chip tone-{tone} size-{size}" {...rest}>
  {@render children?.()}
</span>

<style>
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    border-radius: var(--r-pill);
    font-family: var(--font-mono);
    letter-spacing: var(--tracking-label);
    text-transform: uppercase;
    font-weight: 500;
    white-space: nowrap;
  }
  .size-xs { padding: 1px 6px; font-size: 10px; }
  .size-sm { padding: 2px 8px; font-size: 11px; }

  /* ── Semantic tones ─────────────────────────────────────── */
  .tone-neutral  { background: var(--bg-chip);  color: var(--fg-secondary); }
  .tone-accent   { background: var(--accent-dim); color: var(--accent); }
  .tone-positive { background: var(--pos-bg, color-mix(in srgb, var(--pos) 14%, transparent)); color: var(--pos); }
  .tone-warn     { background: var(--warn-bg);  color: var(--warn); }
  .tone-danger   { background: var(--danger-bg, color-mix(in srgb, var(--danger) 14%, transparent)); color: var(--danger); }
  .tone-info     { background: var(--info-bg, color-mix(in srgb, var(--info) 14%, transparent)); color: var(--info); }

  /* ── Product tones ──────────────────────────────────────── */
  .tone-spot    { background: color-mix(in srgb, var(--info) 14%, transparent); color: var(--info); }
  .tone-perp    { background: var(--pos-bg, color-mix(in srgb, var(--pos) 14%, transparent)); color: var(--pos); }
  .tone-invperp { background: var(--warn-bg); color: var(--warn); }

  /* ── Role tones (from tokens — roles have their own badges) */
  .tone-admin    { background: var(--role-admin-bg,    color-mix(in srgb, var(--danger) 14%, transparent)); color: var(--role-admin-fg,    var(--danger)); }
  .tone-operator { background: var(--role-operator-bg, color-mix(in srgb, var(--accent) 14%, transparent)); color: var(--role-operator-fg, var(--accent)); }
  .tone-viewer   { background: var(--role-viewer-bg,   var(--bg-chip)); color: var(--role-viewer-fg,   var(--fg-secondary)); }
  .tone-client   { background: var(--role-client-bg,   color-mix(in srgb, var(--info) 14%, transparent)); color: var(--role-client-fg, var(--info)); }
</style>
