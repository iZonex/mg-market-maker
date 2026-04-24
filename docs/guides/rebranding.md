# Rebranding Guide

If your firm forks this repo to deploy the MM under your own brand, here's the minimum surface to touch. By design, the full re-skin lives in **3 files + 1 asset**.

## The re-branding surface

| File | What lives there |
|------|------------------|
| `frontend/src/lib/branding.js` | Product name, short name, tagline, logo/favicon paths, copyright string |
| `frontend/src/lib/tokens.css` | Colours, spacing, radii, typography, shadows — every palette variable |
| `frontend/public/logo.svg` | Brand mark vector (also the `favicon.svg`) |
| `frontend/public/favicon.svg` | Browser tab icon |

Nothing else in the frontend hardcodes the product name or the accent colour. If you find something, that's a bug — it should be using a token or the `branding.js` const.

## Step-by-step

### 1. Fork + set up

```bash
git clone https://github.com/YOUR_FORK/market-maker.git acme-maker
cd acme-maker
cd frontend && npm install
```

### 2. Edit `branding.js`

```js
export const BRAND = {
  productName: 'Acme Capital Market Maker',
  shortName:   'AC',
  tagline:     'Algorithmic liquidity for tier-one counterparties',
  logoUrl:     '/logo.svg',
  faviconUrl:  '/favicon.svg',
  copyright:   `© ${new Date().getFullYear()} Acme Capital`,
}
```

This propagates to:
- `<title>` in the browser tab (set from `main.js` at boot)
- Sidebar brand row
- `<BrandMark>` glyph (short name) + wordmark (rest of product name)
- Login + first-install wizard headers (once they start reading from `branding.js`)
- Footer copyright

### 3. Edit `tokens.css`

Change the accent hue + role colours to match your palette. The minimum-change set:

```css
:root {
  --accent:      #ff6b35;       /* your brand accent */
  --accent-2:    #e85a29;       /* slightly darker hover state */
  --accent-dim:  rgba(255, 107, 53, 0.12);
  --accent-ring: rgba(255, 107, 53, 0.32);
}
```

That's often enough to feel re-branded. If you want deeper changes:
- Surfaces (`--bg-base`, `--bg-raised`, `--bg-raised-2`) — dark vs light
- Foregrounds (`--fg-primary`, `--fg-secondary`, `--fg-muted`)
- Semantic (`--pos`, `--neg`/`--danger`, `--warn`, `--info`) — only if your brand's error/success have specific hues
- Typography — swap `Inter` for your corporate font in the `@font-face` block at the top

**Rule:** every visual change goes through `tokens.css`. If you find yourself editing a `.svelte` file's `<style>` block during a re-brand, you're fixing the wrong thing — add the missing token instead.

### 4. Replace logo + favicon

```bash
# SVG preferred for crisp rendering at any size
cp ~/acme-brand/logo.svg       frontend/public/logo.svg
cp ~/acme-brand/favicon.svg    frontend/public/favicon.svg
```

If your logo is raster-only, convert to SVG (or set the path to a PNG in `branding.js`). The built-in `<BrandMark>` is a glyph + wordmark vector you can also override by replacing the component.

### 5. Build + verify

```bash
npm run build
# Serve dist/ from wherever you host the frontend
```

Checklist:
- [ ] Browser tab title reads `<your product name> Dashboard`
- [ ] Sidebar brand shows your short name + product name
- [ ] Accent-coloured UI (primary buttons, focus rings, active nav item) uses your hue
- [ ] Login page + first-install wizard show your tagline
- [ ] Favicon renders your logo

### 6. Backend branding (optional)

The backend doesn't own branding beyond the MiCA report template. If you need custom HTML templates for exported reports, swap them in `crates/dashboard/src/mica_report.rs` + `report_export.rs` or wire a config-driven template path.

## Things you should NOT change when re-branding

Unless you have a deliberate reason:
- The token **names** (e.g. `--accent`, `--fg-primary`) — renaming means every component has to be updated
- The primitive API (`<Button>` props, `<Modal>` snippets) — swapping to a different component library is a separate project
- The directory layout (`primitives/` / `components/` / `pages/`)

These are the interfaces that keep the theming consistent; changing them is a redesign, not a rebrand.

## Anti-patterns

- ❌ **Grep-and-replace the product name across 65 component files.** It should only appear in `branding.js`. If grep finds it elsewhere, file an issue — that's a leak of the single-source rule.
- ❌ **Edit a component's `<style>` block to change colours.** Add a token; update the token.
- ❌ **Keep a separate fork that drifts.** The intent is that `branding.js` + `tokens.css` diverge, but `primitives/` + core `components/` stay mergeable with upstream.

## Staying in sync with upstream

If you track upstream:
```bash
git remote add upstream https://github.com/mg-markets/market-maker.git
git fetch upstream
git merge upstream/main
```
Your branding changes will conflict only in `branding.js`, `tokens.css`, and the two asset files — trivial to resolve. Everything else comes through clean.
