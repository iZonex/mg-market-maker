<script>
  /*
   * 23-UX-9 — reusable loading spinner. Default size matches
   * the icon library so it swaps one-for-one with any panel's
   * inline icon slot.
   */
  let { size = 16, label = 'Loading' } = $props()
</script>

<span class="spinner" style="--sz: {size}px" role="status" aria-label={label}>
  <svg viewBox="0 0 50 50" width={size} height={size}>
    <circle cx="25" cy="25" r="20" stroke="currentColor" stroke-width="4" fill="none" stroke-opacity="0.2" />
    <circle class="dash" cx="25" cy="25" r="20" stroke="currentColor" stroke-width="4" fill="none"
            stroke-linecap="round" stroke-dasharray="31.4 94.2" />
  </svg>
</span>

<style>
  .spinner {
    display: inline-flex;
    color: var(--accent);
    animation: rotate 1s linear infinite;
    width: var(--sz);
    height: var(--sz);
  }
  @keyframes rotate {
    to { transform: rotate(360deg); }
  }
</style>
