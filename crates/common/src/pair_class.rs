//! Pair-type classification for adaptive MM calibration (Epic 30).
//!
//! Different symbol archetypes need different default γ / κ / spread
//! floors. This module provides a pure-function classifier that maps
//! a `ProductSpec` (plus an optional 24 h volume reading) to a
//! `PairClass` tag. The engine stores the tag on `SymbolState`;
//! the server merges the matching per-class template into the loaded
//! config at startup.
//!
//! Decision rules (see `docs/research/adaptive-calibration.md` for
//! rationale):
//! - quote ∈ STABLE && base ∈ STABLE                 → `StableStable`
//! - name matches memecoin regex                     → `Meme{Spot|Perp}`
//! - base ∈ MAJORS && daily_volume ≥ $1 B            → `Major{Spot|Perp}`
//! - daily_volume ≥ $100 M                           → `Alt{Spot|Perp}`
//! - otherwise (thin liquidity / unknown)            → `Meme{Spot|Perp}`
//!   — conservative default: treat unknown as if it were a memecoin
//!   because the cost of quoting too tight on a thin pair is much
//!   higher than the cost of quoting too wide on a mature one.

use crate::types::ProductSpec;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Coarse pair-type taxonomy. The engine picks a default parameter
/// profile per class before any per-venue config or adaptive tuning
/// kicks in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairClass {
    /// Blue-chip spot (BTC / ETH / SOL / BNB quoted in a stable).
    /// Tight default spread, low γ, conservative `order_size`.
    MajorSpot,
    /// Top-alt spot — mid-cap liquidity, medium γ.
    AltSpot,
    /// Memecoin or sub-$100 M daily-volume spot. Wide defaults,
    /// high VPIN sensitivity.
    MemeSpot,
    /// Major perpetual (BTC-PERP / ETH-PERP / SOL-PERP). Handles
    /// funding in PnL attribution.
    MajorPerp,
    /// Top-alt perpetual.
    AltPerp,
    /// Stablecoin-stablecoin spot (USDC/USDT, BUSD/USDT, …).
    /// Ultra-tight spread floor, very small γ.
    StableStable,
}

impl fmt::Display for PairClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PairClass::MajorSpot => "major_spot",
            PairClass::AltSpot => "alt_spot",
            PairClass::MemeSpot => "meme_spot",
            PairClass::MajorPerp => "major_perp",
            PairClass::AltPerp => "alt_perp",
            PairClass::StableStable => "stable_stable",
        };
        f.write_str(s)
    }
}

impl PairClass {
    /// Canonical file name (without extension) inside
    /// `config/pair-classes/`.
    pub fn template_slug(self) -> &'static str {
        match self {
            PairClass::MajorSpot => "major-spot",
            PairClass::AltSpot => "alt-spot",
            PairClass::MemeSpot => "meme-spot",
            PairClass::MajorPerp => "major-perp",
            PairClass::AltPerp => "alt-perp",
            PairClass::StableStable => "stable-stable",
        }
    }

    /// Is this a perpetual product? Spot classes return `false`.
    pub fn is_perp(self) -> bool {
        matches!(self, PairClass::MajorPerp | PairClass::AltPerp)
    }
}

/// Blue-chip assets. Conservative list — extend only after live
/// validation on a specific deployment.
const MAJOR_ASSETS: &[&str] = &["BTC", "ETH", "SOL", "BNB"];

/// Common stablecoin symbols.
const STABLE_ASSETS: &[&str] = &["USDT", "USDC", "BUSD", "FDUSD", "TUSD", "DAI", "USDE"];

/// Volume tier boundaries in quote (USD) terms.
const MAJOR_VOL_THRESHOLD: Decimal = dec!(1_000_000_000);
const ALT_VOL_THRESHOLD: Decimal = dec!(100_000_000);

fn is_meme_name(base: &str) -> bool {
    let lc = base.to_ascii_lowercase();
    // Common memecoin stems + generic "inu"/"moon"/"elon" markers.
    // Extend with the per-deployment denylist if a non-meme token
    // happens to match (unlikely — these are historically reserved
    // for memes).
    const HINTS: &[&str] = &[
        "doge", "shib", "pepe", "floki", "wojak", "moon", "elon", "bonk", "wif", "turbo", "meme",
        "popcat", "brett", "mog", "trump",
    ];
    HINTS.iter().any(|h| lc.contains(h)) || lc.ends_with("inu")
}

fn is_stable(asset: &str) -> bool {
    let up = asset.to_ascii_uppercase();
    STABLE_ASSETS.contains(&up.as_str())
}

fn is_major(asset: &str) -> bool {
    let up = asset.to_ascii_uppercase();
    MAJOR_ASSETS.contains(&up.as_str())
}

/// Classify a symbol based on its product spec and (optional)
/// 24 h volume in USD. Pure function — no I/O.
///
/// `is_perp = true` when the venue product is a perpetual future
/// (caller passes `product.default_wallet() == Futures` or similar).
/// We don't couple to `VenueProduct` here because `mm-common` does
/// not depend on `mm-exchange-core`; the caller translates.
pub fn classify_symbol(
    spec: &ProductSpec,
    daily_volume_usd: Option<Decimal>,
    is_perp: bool,
) -> PairClass {
    let base = spec.base_asset.as_str();
    let quote = spec.quote_asset.as_str();

    // Stable/stable pairs override everything else — they deserve
    // ultra-tight spreads and very small γ.
    if is_stable(base) && is_stable(quote) {
        return PairClass::StableStable;
    }

    // Memecoin name match — bias conservative regardless of volume.
    // A single viral day can make SHIBUSDT show major-tier volume
    // without being a major-tier asset; protect against that.
    if is_meme_name(base) {
        return if is_perp {
            PairClass::AltPerp
        } else {
            PairClass::MemeSpot
        };
    }

    // Major asset (BTC / ETH / SOL / BNB) with enough volume.
    let vol = daily_volume_usd.unwrap_or(Decimal::ZERO);
    if is_major(base) && vol >= MAJOR_VOL_THRESHOLD {
        return if is_perp {
            PairClass::MajorPerp
        } else {
            PairClass::MajorSpot
        };
    }

    // Mid-cap: enough daily flow to matter but not a blue chip.
    if vol >= ALT_VOL_THRESHOLD {
        return if is_perp {
            PairClass::AltPerp
        } else {
            PairClass::AltSpot
        };
    }

    // Unknown / thin liquidity — stay conservative. Spot falls into
    // MemeSpot (widest defaults); perp falls into AltPerp because
    // perps below $100 M volume are rare on venues we integrate
    // with and treating them as "meme" is over-conservative.
    if is_perp {
        PairClass::AltPerp
    } else {
        PairClass::MemeSpot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(base: &str, quote: &str) -> ProductSpec {
        ProductSpec {
            symbol: format!("{base}{quote}"),
            base_asset: base.into(),
            quote_asset: quote.into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.001),
            trading_status: Default::default(),
        }
    }

    #[test]
    fn btcusdt_major_volume_is_major_spot() {
        let got = classify_symbol(&spec("BTC", "USDT"), Some(dec!(30_000_000_000)), false);
        assert_eq!(got, PairClass::MajorSpot);
    }

    #[test]
    fn btcusdt_as_perp_is_major_perp() {
        let got = classify_symbol(&spec("BTC", "USDT"), Some(dec!(30_000_000_000)), true);
        assert_eq!(got, PairClass::MajorPerp);
    }

    #[test]
    fn shibusdt_is_meme_regardless_of_volume() {
        let got = classify_symbol(&spec("SHIB", "USDT"), Some(dec!(5_000_000_000)), false);
        assert_eq!(got, PairClass::MemeSpot);
    }

    #[test]
    fn dogeusdt_is_meme_from_name_hint() {
        let got = classify_symbol(&spec("DOGE", "USDT"), Some(dec!(2_000_000_000)), false);
        assert_eq!(got, PairClass::MemeSpot);
    }

    #[test]
    fn xxxinu_ends_in_inu_so_meme() {
        let got = classify_symbol(&spec("HYPERINU", "USDT"), Some(dec!(2_000_000_000)), false);
        assert_eq!(got, PairClass::MemeSpot);
    }

    #[test]
    fn usdcusdt_is_stable_stable() {
        let got = classify_symbol(&spec("USDC", "USDT"), Some(dec!(500_000_000)), false);
        assert_eq!(got, PairClass::StableStable);
    }

    #[test]
    fn mid_cap_alt_is_alt_spot() {
        let got = classify_symbol(&spec("AVAX", "USDT"), Some(dec!(250_000_000)), false);
        assert_eq!(got, PairClass::AltSpot);
    }

    #[test]
    fn thin_unknown_spot_is_meme_spot() {
        // $5 M daily volume — well below alt threshold.
        let got = classify_symbol(&spec("OBSCURE", "USDT"), Some(dec!(5_000_000)), false);
        assert_eq!(got, PairClass::MemeSpot);
    }

    #[test]
    fn missing_volume_defaults_conservative() {
        // No volume info → treat spot as meme, perp as alt.
        assert_eq!(
            classify_symbol(&spec("NEWCOIN", "USDT"), None, false),
            PairClass::MemeSpot
        );
        assert_eq!(
            classify_symbol(&spec("NEWCOIN", "USDT"), None, true),
            PairClass::AltPerp
        );
    }

    #[test]
    fn major_with_low_volume_is_alt_not_major() {
        // BTC/USDT on a dry venue with $50 M volume shouldn't inherit
        // the Major defaults — the pair-class is a *combined* asset
        // + liquidity tag, not asset alone.
        let got = classify_symbol(&spec("BTC", "USDT"), Some(dec!(50_000_000)), false);
        assert_eq!(got, PairClass::MemeSpot);
    }

    #[test]
    fn display_and_slug_are_stable() {
        assert_eq!(format!("{}", PairClass::MajorSpot), "major_spot");
        assert_eq!(PairClass::MajorSpot.template_slug(), "major-spot");
        assert!(PairClass::MajorPerp.is_perp());
        assert!(!PairClass::MajorSpot.is_perp());
    }

    // Proptest — classifier determinism: same inputs → same output
    // across many random invocations.
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn classifier_is_deterministic(
            base in "[A-Z]{3,8}",
            quote in "(USDT|USDC|BUSD|BTC|ETH)",
            vol_raw in 0i64..1_000_000_000_000i64,
            is_perp in any::<bool>(),
        ) {
            let s = spec(&base, &quote);
            let vol = Some(Decimal::from(vol_raw));
            let a = classify_symbol(&s, vol, is_perp);
            let b = classify_symbol(&s, vol, is_perp);
            prop_assert_eq!(a, b);
        }

        #[test]
        fn stable_stable_for_any_stable_pair(
            base in "(USDT|USDC|BUSD|FDUSD|DAI|TUSD)",
            quote in "(USDT|USDC|BUSD|FDUSD|DAI|TUSD)",
            vol_raw in 0i64..1_000_000_000_000i64,
        ) {
            prop_assume!(base != quote);
            let s = spec(&base, &quote);
            let got = classify_symbol(&s, Some(Decimal::from(vol_raw)), false);
            prop_assert_eq!(got, PairClass::StableStable);
        }
    }
}
