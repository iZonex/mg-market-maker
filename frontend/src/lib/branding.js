/*
 * Brand constants — the single place the app's name, tagline, and
 * logo live. If you forked this repo to re-brand, you edit this
 * file + `tokens.css` + drop a replacement `public/logo.svg` and
 * you're done. That is the entire re-branding surface by design.
 *
 * Rule: no other file in the codebase hardcodes the product name
 * or logo path. Grep for "MG Market Maker" / "/logo.svg" should
 * return only this file.
 */

export const BRAND = {
  /** Full product name shown in `<title>` + TopBar + Login header. */
  productName: 'MG Market Maker',

  /** Short name for breadcrumbs + tight UI (~4 chars). */
  shortName: 'MG',

  /** One-liner shown on the Login + First-install screens. */
  tagline: 'Production-grade algorithmic market making',

  /** Public path to the brand mark; SVG preferred. */
  logoUrl: '/logo.svg',

  /** Small favicon / touch icon; keep in sync with `index.html`. */
  faviconUrl: '/favicon.svg',

  /** Shown in footer + About dialog. */
  copyright: `© ${new Date().getFullYear()} MG Market Maker`,
}
