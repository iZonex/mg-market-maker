<script>
  /*
   * Brand mark — glyph + optional wordmark. The short glyph
   * ("MG" by default) and the rest-of-name ("Market Maker") both
   * come from the `branding.js` constants so re-skinning the
   * project is a single-file edit.
   *
   * Height is the control dimension; width follows (ratio ≈ 1.6).
   *
   * Props:
   *   size      — pixel height of the mark (default 28)
   *   withText  — render wordmark after the glyph; default false
   */
  import { BRAND } from '../branding.js'

  let { size = 28, withText = false } = $props()
  // Split the product name on the first whitespace — "<short> <rest>".
  // `short` goes into the glyph; `rest` is the wordmark.
  const _short = BRAND.shortName
  const _rest = $derived.by(() => {
    const parts = BRAND.productName.trim().split(/\s+/)
    if (parts[0] === _short) return parts.slice(1).join(' ')
    // Fallback: product name without the shortName prefix/suffix match
    return BRAND.productName.startsWith(_short)
      ? BRAND.productName.slice(_short.length).trim()
      : BRAND.productName
  })
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
    >{_short}</text>
    <path
      d="M70 4 V 28"
      stroke="var(--accent)"
      stroke-width="4"
      stroke-linecap="round"
    />
  </svg>

  {#if withText}
    <span class="brand-rest">{_rest}</span>
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
