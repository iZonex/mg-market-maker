//! Multi-Venue 3.D — BasisArb cross-venue composite.
//!
//! Quotes spot-perp basis carry: post a **maker bid on spot** + a
//! **maker ask on perp** (or the mirror short-basis trade). A graph
//! using this strategy emits `VenueQuotes` with entries tagged for
//! both venues; the Level 3.B dispatcher routes each entry to the
//! right engine's order manager.
//!
//! This crate doesn't depend on mm-strategy-graph (cycle avoidance),
//! so the "graph-facing" shape is an opaque config + a
//! `compute_venue_quotes` free function that the engine adapter
//! wires into the strategy pool.
//!
//! The function is deliberately side-effect free: given a snapshot
//! of spot / perp mids + portfolio net delta, produce the next
//! VenueQuote bundle. The engine snapshots those inputs from the
//! DataBus + PortfolioBalanceTracker at tick time.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// One entry of the basis-arb bundle. Stays engine-type-free (string
/// venue / side) so this crate remains free of mm-strategy-graph.
#[derive(Debug, Clone, PartialEq)]
pub struct ArbLeg {
    pub venue: String,
    pub symbol: String,
    pub product: String,
    pub side: Side,
    pub price: Decimal,
    pub qty: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// Snapshot inputs the strategy needs — caller fills from whatever
/// source (DataBus, engine state, portfolio tracker).
#[derive(Debug, Clone)]
pub struct BasisSnapshot {
    pub spot_venue: String,
    pub spot_symbol: String,
    pub spot_mid: Decimal,
    pub perp_venue: String,
    pub perp_symbol: String,
    pub perp_mid: Decimal,
    /// Current net delta in the base asset — positive = long.
    pub net_delta: Decimal,
}

/// Per-tick knobs. Defaults chosen so a fresh deploy doesn't
/// accidentally lean into a position.
#[derive(Debug, Clone)]
pub struct BasisArbConfig {
    /// Order size on each leg (base asset).
    pub leg_size: Decimal,
    /// Half-spread the maker-post leg sits behind the mid, in bps.
    pub maker_offset_bps: Decimal,
    /// Minimum basis gap (perp_mid - spot_mid, in bps) before we
    /// enter at all. Guards against entering on a flat basis where
    /// fees eat the carry.
    pub min_basis_bps: Decimal,
    /// Max base-asset net delta the strategy allows itself to
    /// accumulate before standing down a leg to rebalance.
    pub max_delta: Decimal,
}

impl Default for BasisArbConfig {
    fn default() -> Self {
        Self {
            leg_size: dec!(0.001),
            maker_offset_bps: dec!(2),
            min_basis_bps: dec!(10),
            max_delta: dec!(0.05),
        }
    }
}

/// Compute the VenueQuote bundle this tick. Returns an empty vec
/// when the guard conditions fail (zero mids, basis below
/// threshold, delta over limit) — the caller treats an empty
/// bundle as "cancel all basis-arb quotes".
pub fn compute_basis_arb_legs(snap: &BasisSnapshot, cfg: &BasisArbConfig) -> Vec<ArbLeg> {
    if snap.spot_mid <= Decimal::ZERO || snap.perp_mid <= Decimal::ZERO {
        return Vec::new();
    }
    let bp = dec!(10_000);
    let basis_bps = (snap.perp_mid - snap.spot_mid) / snap.spot_mid * bp;
    if basis_bps.abs() < cfg.min_basis_bps {
        return Vec::new();
    }

    // Positive basis (perp > spot) → buy spot, sell perp.
    // Negative basis → sell spot, buy perp.
    let long_basis = basis_bps > Decimal::ZERO;
    let offset_frac = cfg.maker_offset_bps / bp;

    // Delta guard — if we're already too long, skip the long-spot
    // leg; if we're already too short, skip the short-perp leg.
    let over_long = snap.net_delta >= cfg.max_delta;
    let over_short = snap.net_delta <= -cfg.max_delta;

    let mut legs = Vec::with_capacity(2);

    if long_basis {
        // Buy spot @ mid - offset.
        if !over_long {
            legs.push(ArbLeg {
                venue: snap.spot_venue.clone(),
                symbol: snap.spot_symbol.clone(),
                product: "spot".into(),
                side: Side::Buy,
                price: snap.spot_mid * (Decimal::ONE - offset_frac),
                qty: cfg.leg_size,
            });
        }
        // Sell perp @ mid + offset.
        if !over_short {
            legs.push(ArbLeg {
                venue: snap.perp_venue.clone(),
                symbol: snap.perp_symbol.clone(),
                product: "linear_perp".into(),
                side: Side::Sell,
                price: snap.perp_mid * (Decimal::ONE + offset_frac),
                qty: cfg.leg_size,
            });
        }
    } else {
        // Mirror for negative basis.
        if !over_short {
            legs.push(ArbLeg {
                venue: snap.spot_venue.clone(),
                symbol: snap.spot_symbol.clone(),
                product: "spot".into(),
                side: Side::Sell,
                price: snap.spot_mid * (Decimal::ONE + offset_frac),
                qty: cfg.leg_size,
            });
        }
        if !over_long {
            legs.push(ArbLeg {
                venue: snap.perp_venue.clone(),
                symbol: snap.perp_symbol.clone(),
                product: "linear_perp".into(),
                side: Side::Buy,
                price: snap.perp_mid * (Decimal::ONE - offset_frac),
                qty: cfg.leg_size,
            });
        }
    }

    legs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(spot: Decimal, perp: Decimal, delta: Decimal) -> BasisSnapshot {
        BasisSnapshot {
            spot_venue: "binance".into(),
            spot_symbol: "BTCUSDT".into(),
            spot_mid: spot,
            perp_venue: "bybit".into(),
            perp_symbol: "BTCUSDT".into(),
            perp_mid: perp,
            net_delta: delta,
        }
    }

    #[test]
    fn positive_basis_buys_spot_sells_perp() {
        let s = snap(dec!(100), dec!(100.5), Decimal::ZERO);
        let legs = compute_basis_arb_legs(&s, &BasisArbConfig::default());
        assert_eq!(legs.len(), 2);
        let spot = legs.iter().find(|l| l.venue == "binance").unwrap();
        let perp = legs.iter().find(|l| l.venue == "bybit").unwrap();
        assert_eq!(spot.side, Side::Buy);
        assert_eq!(perp.side, Side::Sell);
    }

    #[test]
    fn basis_below_threshold_produces_no_legs() {
        let s = snap(dec!(100), dec!(100.01), Decimal::ZERO);
        let legs = compute_basis_arb_legs(&s, &BasisArbConfig::default());
        assert!(legs.is_empty(), "basis 1bps < 10bps threshold");
    }

    #[test]
    fn over_long_drops_long_leg() {
        let cfg = BasisArbConfig {
            max_delta: dec!(0.01),
            ..BasisArbConfig::default()
        };
        // Positive basis → long spot + short perp. Already over
        // long → skip spot leg, keep perp short.
        let s = snap(dec!(100), dec!(100.5), dec!(0.02));
        let legs = compute_basis_arb_legs(&s, &cfg);
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].venue, "bybit");
        assert_eq!(legs[0].side, Side::Sell);
    }

    #[test]
    fn zero_mids_short_circuits() {
        let s = snap(dec!(0), dec!(100), Decimal::ZERO);
        assert!(compute_basis_arb_legs(&s, &BasisArbConfig::default()).is_empty());
    }
}
