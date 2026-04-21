/**
 * Strategy-aware layout profiles for the Overview page.
 *
 * Each strategy name maps to a profile that tells Overview which
 * panels to render. The idea — from the MM-expert audit: "Overview
 * is the operator's primary cockpit, it should show what matters
 * for THE strategy they're running, not a kitchen sink."
 *
 * Profile keys:
 *   kind                 — human label for the header chip
 *   family               — grouping: 'single_venue' | 'cross_venue' |
 *                          'driver' | 'pentest'
 *   panels               — { [panel_key]: true|false } — which panels
 *                          the Overview template checks against
 *   primary_metrics      — ordered list of KPI hints for the
 *                          HeroKpis strip (future: custom per family)
 *
 * Panels the Overview template knows about:
 *   orderbook · pnl_chart · spread_chart · signals · adaptive ·
 *   market_quality · inventory_panel · inventory_chart ·
 *   cross_venue_portfolio · per_leg_inventory · per_leg_pnl ·
 *   funding · basis · venue_orders_strip · adverse_banner
 *
 * The default profile is conservative — shows everything, so an
 * unknown strategy name falls back to the old behaviour.
 */

const DEFAULT_PANELS = {
  orderbook: true,
  pnl_chart: true,
  spread_chart: true,
  signals: true,
  adaptive: true,
  market_quality: true,
  inventory_panel: true,
  inventory_chart: true,
  cross_venue_portfolio: true,
  per_leg_inventory: true,
  per_leg_pnl: true,
  funding: true,
  basis: true,
  venue_orders_strip: true,
  adverse_banner: true,
}

const PROFILES = {
  'avellaneda-stoikov': {
    kind: 'Avellaneda-Stoikov',
    family: 'single_venue',
    panels: {
      ...DEFAULT_PANELS,
      // Single-venue strategy — nothing to aggregate cross-venue.
      cross_venue_portfolio: false,
      per_leg_inventory: false,
      per_leg_pnl: false,
      basis: false,
      funding: false,
      venue_orders_strip: false,
    },
  },
  glft: {
    kind: 'GLFT',
    family: 'single_venue',
    panels: {
      ...DEFAULT_PANELS,
      cross_venue_portfolio: false,
      per_leg_inventory: false,
      per_leg_pnl: false,
      basis: false,
      funding: false,
      venue_orders_strip: false,
    },
  },
  grid: {
    kind: 'Grid',
    family: 'single_venue',
    panels: {
      ...DEFAULT_PANELS,
      cross_venue_portfolio: false,
      per_leg_inventory: false,
      per_leg_pnl: false,
      basis: false,
      funding: false,
      venue_orders_strip: false,
      // Grid strategy lives on adaptive levels — signals panel
      // is noise for it.
      signals: false,
    },
  },
  'cross-exchange': {
    kind: 'Cross-exchange',
    family: 'cross_venue',
    panels: {
      ...DEFAULT_PANELS,
      // Cross-venue MM is where the multi-leg panels earn their
      // keep. Keep signals off on the primary cockpit — they're
      // second-order for a hedger.
      signals: false,
      adaptive: false,
    },
  },
  basis: {
    kind: 'Basis',
    family: 'cross_venue',
    panels: {
      ...DEFAULT_PANELS,
      signals: false,
      adaptive: false,
    },
  },
  funding_arb: {
    kind: 'Funding arb',
    family: 'driver',
    panels: {
      ...DEFAULT_PANELS,
      // Driver strategies live outside the per-tick quote loop.
      // Operator cares about funding + pair events, NOT spread /
      // signals / adaptive.
      spread_chart: false,
      signals: false,
      adaptive: false,
      inventory_chart: false,
      basis: true,
      funding: true,
    },
  },
  stat_arb: {
    kind: 'Stat arb',
    family: 'driver',
    panels: {
      ...DEFAULT_PANELS,
      signals: false,
      adaptive: false,
      funding: false,
    },
  },
}

/**
 * Resolve the active layout for a strategy name. Fallback to a
 * permissive "show everything" profile when the strategy is
 * unknown or absent.
 *
 * @param {string|null|undefined} strategy
 * @returns {{ kind: string, family: string, panels: Record<string,boolean>, unknown: boolean }}
 */
export function layoutForStrategy(strategy) {
  if (!strategy) {
    return {
      kind: '—',
      family: 'unknown',
      panels: { ...DEFAULT_PANELS },
      unknown: true,
    }
  }
  const profile = PROFILES[strategy]
  if (!profile) {
    return {
      kind: strategy,
      family: 'unknown',
      panels: { ...DEFAULT_PANELS },
      unknown: true,
    }
  }
  return { ...profile, unknown: false }
}

/**
 * Human-readable description of the family — used in the
 * Overview header as the subtitle under the strategy chip.
 */
export function familyLabel(family) {
  switch (family) {
    case 'single_venue': return 'Single-venue quoter'
    case 'cross_venue': return 'Cross-venue maker + hedge'
    case 'driver': return 'Async driver (cointegration / funding)'
    case 'pentest': return 'Pentest — restricted'
    default: return 'Unknown'
  }
}
