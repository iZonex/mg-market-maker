<script>
  /*
   * <Button> — the single source of truth for button styling.
   *
   * Design-system contract:
   *   - Consumes tokens only. Never use hex/rgb literals here —
   *     every colour routes through `var(--*)` so a theme
   *     swap via `tokens.css` re-colours every button in the app.
   *   - Variants: primary / ghost / danger / warn / ok.
   *   - Sizes:    xs / sm / md / lg. Default `md`.
   *   - Icon-only mode (`iconOnly={true}`) collapses to a square
   *     button, good for toolbar actions where an `<Icon/>` is
   *     the only child.
   *
   * All unrecognised native `<button>` attributes are forwarded
   * via `...rest` — so `type`, `disabled`, `aria-*`, `title`,
   * `onclick`, `onkeydown` all pass through.
   */

  let {
    /** @type {'primary' | 'ghost' | 'danger' | 'warn' | 'ok'} */
    variant = 'primary',
    /** @type {'xs' | 'sm' | 'md' | 'lg'} */
    size = 'md',
    /** Square icon-only button. Pair with a child `<Icon />`. */
    iconOnly = false,
    /** Spinner-style loading state. Disables the button. */
    loading = false,
    /** Renderable child content (label + optional `<Icon />`). */
    children,
    ...rest
  } = $props()
</script>

<button
  class="btn variant-{variant} size-{size}"
  class:icon-only={iconOnly}
  class:loading
  disabled={rest.disabled || loading}
  {...rest}
>
  {@render children?.()}
</button>

<style>
  .btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--s-2);
    border-radius: var(--r-sm);
    border: 1px solid transparent;
    font-family: inherit;
    font-weight: 500;
    letter-spacing: var(--tracking-normal, normal);
    cursor: pointer;
    transition: background var(--motion-fast, 120ms) ease,
                border-color var(--motion-fast, 120ms) ease,
                color var(--motion-fast, 120ms) ease,
                transform var(--motion-fast, 120ms) ease;
    white-space: nowrap;
  }
  .btn:focus-visible {
    outline: 2px solid var(--accent-ring);
    outline-offset: 1px;
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .btn:not(:disabled):active { transform: translateY(1px); }

  /* ── Sizes ──────────────────────────────────────────────── */
  .size-xs { padding: 2px 8px;  font-size: 10px;           min-height: 20px; }
  .size-sm { padding: 4px 10px; font-size: var(--fs-xs);   min-height: 26px; }
  .size-md { padding: 6px 14px; font-size: var(--fs-sm);   min-height: 32px; }
  .size-lg { padding: 10px 20px; font-size: var(--fs-base); min-height: 40px; }

  /* ── Icon-only (square) overrides ───────────────────────── */
  .icon-only.size-xs { padding: 2px;  min-width: 20px; }
  .icon-only.size-sm { padding: 4px;  min-width: 26px; }
  .icon-only.size-md { padding: 6px;  min-width: 32px; }
  .icon-only.size-lg { padding: 10px; min-width: 40px; }

  /* ── Primary — MG accent fill ───────────────────────────── */
  .variant-primary {
    background: var(--accent);
    color: var(--bg-base);
    border-color: var(--accent);
  }
  .variant-primary:not(:disabled):hover {
    background: var(--accent-2);
    border-color: var(--accent-2);
  }

  /* ── Ghost — low-emphasis, borderless until hover ───────── */
  .variant-ghost {
    background: transparent;
    color: var(--fg-primary);
    border-color: var(--border-subtle);
  }
  .variant-ghost:not(:disabled):hover {
    background: var(--bg-chip-hover);
    border-color: var(--border-default);
  }

  /* ── Danger — destructive actions ───────────────────────── */
  .variant-danger {
    background: transparent;
    color: var(--danger);
    border-color: color-mix(in srgb, var(--danger) 50%, transparent);
  }
  .variant-danger:not(:disabled):hover {
    background: color-mix(in srgb, var(--danger) 14%, transparent);
    border-color: var(--danger);
  }

  /* ── Warn — caution actions ─────────────────────────────── */
  .variant-warn {
    background: transparent;
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 50%, transparent);
  }
  .variant-warn:not(:disabled):hover {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    border-color: var(--warn);
  }

  /* ── Ok — positive confirmation ─────────────────────────── */
  .variant-ok {
    background: transparent;
    color: var(--pos);
    border-color: color-mix(in srgb, var(--pos) 50%, transparent);
  }
  .variant-ok:not(:disabled):hover {
    background: color-mix(in srgb, var(--pos) 14%, transparent);
    border-color: var(--pos);
  }

  /* ── Loading spinner overlay ────────────────────────────── */
  .loading { position: relative; color: transparent; }
  .loading::after {
    content: '';
    position: absolute;
    width: 1em; height: 1em;
    border: 2px solid currentColor;
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
    color: var(--fg-primary);
  }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
