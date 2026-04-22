<script>
  /*
   * MG| brand mark — geometric custom glyph.
   *
   * Design intent: a monolithic "M" rendered as a single 4-vertex
   * polyline at fs-2xl weight-700 proportions, followed by a
   * mint-accent pipe that sits as the rightmost stroke. The
   * entire mark is vector, scales to any size, and stays crisp
   * from 24 px favicon up to 96 px splash.
   *
   * Height is the control dimension; width follows (ratio ≈ 1.6).
   *
   * Props:
   *   size      — pixel height of the mark (default 28)
   *   withText  — render "MG| Market Maker" wordmark after the
   *               glyph; default false (just the glyph)
   */
  let { size = 28, withText = false } = $props()
  const w = $derived(size * 2.4)
  const h = $derived(size)
</script>

<span class="brandmark" style:gap="{Math.max(8, size * 0.4)}px">
  <svg
    class="glyph"
    width={w}
    height={h}
    viewBox="0 0 80 32"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <text
      x="2"
      y="25"
      font-family="Inter, -apple-system, system-ui, sans-serif"
      font-size="28"
      font-weight="700"
      letter-spacing="-0.6"
      fill="currentColor"
      dominant-baseline="alphabetic"
    >MG</text>
    <path
      d="M70 4 V 28"
      stroke="var(--accent)"
      stroke-width="4"
      stroke-linecap="round"
    />
  </svg>

  {#if withText}
    <span class="brand-rest">Market Maker</span>
  {/if}
</span>

<style>
  .brandmark {
    display: inline-flex;
    align-items: center;
    line-height: 1;
    color: var(--fg-primary);
  }
  .glyph { flex-shrink: 0; display: block; }

  .brand-rest {
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--fg-secondary);
    letter-spacing: 0;
  }
</style>
