//! Cross-asset hedge optimizer — Epic C sub-component #3.
//!
//! Given a portfolio exposure vector (the per-factor delta from
//! `mm-portfolio::Portfolio::factors`) and a universe of hedge
//! instruments, emit the hedge basket that minimizes residual
//! portfolio variance subject to a funding-cost penalty.
//!
//! # Theory
//!
//! The formula is classic Markowitz 1952 / Merton 1972 mean-
//! variance hedging — *not* anything from Cartea-Jaimungal-Penalva
//! despite what the original SOTA research doc claimed. The
//! transcription doc at
//! `docs/research/hedge-optimizer-and-var-formulas.md` has the
//! source-attribution correction and the full derivation with
//! our variable names.
//!
//! The **unconstrained** closed form is:
//!
//! ```text
//! h* = -(B^T·Σ·B)^(-1) · B^T·Σ·x
//! ```
//!
//! where:
//! - `x` = current exposure vector (length K, one entry per factor)
//! - `h` = hedge instrument sizes (length M — what we solve for)
//! - `B` = K×M beta matrix, `B[k][m]` = exposure of 1 unit of
//!   hedge instrument `m` to factor `k`
//! - `Σ` = K×K covariance matrix of factor returns
//!
//! # v1 simplifications
//!
//! The stage-1 implementation ships a deliberately simple version:
//!
//! 1. **Diagonal β only** (`B = I`): each hedge instrument hedges
//!    exactly one factor one-for-one. A BTC-perp hedges BTC-delta,
//!    an ETH-perp hedges ETH-delta. Cross-beta (e.g. using
//!    ETH-perp to partially hedge BTC via correlation) is deferred
//!    to stage-2 because the estimation error on off-diagonal β
//!    swamps the variance reduction at typical MM exposure levels.
//! 2. **Diagonal Σ only**: per-factor variance is estimated from
//!    a rolling window of mid prices (operator supplies via the
//!    `factor_variances` argument). Off-diagonal correlations are
//!    not modelled — same noisy-estimate argument.
//!
//! With these simplifications the unconstrained solution collapses
//! to a one-loop-over-K computation:
//!
//! ```text
//! for each factor k:
//!     h_unconstrained[k] = -x[k]
//! ```
//!
//! i.e. "sell exactly the exposure". The interesting part is the
//! L1 **funding-cost shrinkage** we add on top: perpetual hedge
//! instruments pay funding, which turns a theoretically optimal
//! hedge into a net loss if the funding cost outweighs the
//! variance reduction. We formalize this as a shrinkage term that
//! pulls the hedge toward zero proportionally to the per-factor
//! funding rate × the inverse variance (inverse variance = κ,
//! so high-variance factors shrink less because their hedges
//! deliver more variance reduction per unit funding cost).
//!
//! ```text
//! κ[k] = 1 / Σ_diag[k]
//! shrinkage[k] = λ · f[k] · κ[k]
//! |h_shrunk[k]| = max(0, |h_unconstrained[k]| - shrinkage[k])
//! h_shrunk[k] = sign(h_unconstrained[k]) · |h_shrunk[k]|
//! ```
//!
//! A hard per-instrument `position_cap` clamp runs last.
//!
//! # Pure function
//!
//! Everything is synchronous `Decimal` arithmetic. No async, no
//! IO, no randomness, no clocks. The engine calls `optimize()`
//! on every refresh tick with fresh inputs; the result is a
//! recommendation the operator can hand off to an
//! `ExecAlgorithm` or review on the dashboard.

use std::collections::HashMap;

use rust_decimal::Decimal;

/// One candidate hedge instrument. The optimizer loops over a
/// `universe: &[HedgeInstrument]` so the caller can pass any
/// subset the venue supports on the current connector.
#[derive(Debug, Clone)]
pub struct HedgeInstrument {
    /// Venue-native symbol (`"BTC-PERP"`, `"ETHUSDT"`, …). The
    /// optimizer treats this as opaque — it's just the label
    /// the basket carries back to the operator so they know
    /// which symbol to route the hedge against.
    pub symbol: String,
    /// Which factor this instrument hedges in the diagonal-β
    /// world. `"BTC"` for a BTC-perp, `"ETH"` for an ETH-perp.
    /// The optimizer compares this against the factor keys in
    /// the `exposure` vector and skips instruments whose factor
    /// is not represented.
    pub factor: String,
    /// Funding cost per holding interval in **basis points**.
    /// Perp instruments pay ~1-10 bps per 8-hour funding window;
    /// spot instruments pay 0. Positive values shrink the
    /// hedge; the shrinkage is linear in this number.
    pub funding_bps: Decimal,
    /// Maximum absolute position size the optimizer is allowed
    /// to recommend for this instrument. Normally sourced from
    /// `config.risk.max_inventory`. Hard constraint applied
    /// after the shrinkage step.
    pub position_cap: Decimal,
}

/// Output of the optimizer — one `(symbol, qty)` pair per
/// non-zero recommendation. Empty basket means "portfolio is
/// already acceptably hedged or the funding cost would swamp
/// the variance reduction". The `qty` is **signed**: positive
/// = buy the instrument, negative = sell it.
#[derive(Debug, Clone, Default)]
pub struct HedgeBasket {
    pub entries: Vec<(String, Decimal)>,
}

impl HedgeBasket {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total notional across the basket in per-instrument native
    /// units. Useful for test assertions and for the dashboard
    /// "basket size" summary.
    pub fn total_abs_qty(&self) -> Decimal {
        self.entries.iter().map(|(_, q)| q.abs()).sum()
    }
}

/// Cross-asset hedge optimizer. Holds only the operator-tunable
/// knobs; every per-tick input comes through [`Self::optimize`].
#[derive(Debug, Clone)]
pub struct HedgeOptimizer {
    /// Funding-cost penalty coefficient `λ`. Default `1.0` =
    /// funding cost weighs equally with variance reduction.
    /// `0.0` = ignore funding entirely (reproduces the
    /// unconstrained closed form). Larger values bias toward
    /// smaller hedges.
    pub funding_penalty: Decimal,
}

impl HedgeOptimizer {
    /// Construct with the supplied funding penalty. Values below
    /// zero are clamped to zero — a negative penalty would mean
    /// "reward taking funding", which is not what the
    /// formulation captures.
    pub fn new(funding_penalty: Decimal) -> Self {
        Self {
            funding_penalty: funding_penalty.max(Decimal::ZERO),
        }
    }

    /// Run the diagonal-β closed-form optimizer.
    ///
    /// - `exposure` — vector of `(factor, delta)` pairs from
    ///   `mm-portfolio::Portfolio::factors()`. Signed; a `+0.5`
    ///   BTC delta means the portfolio is long 0.5 BTC and
    ///   wants to *sell* 0.5 BTC worth of hedge.
    /// - `universe` — hedge instruments the venue supports
    ///   this tick. The optimizer picks at most one per factor
    ///   in v1 (the first matching instrument per factor wins
    ///   — operators should not pass multiple instruments for
    ///   the same factor until stage-2 cross-beta lands).
    /// - `factor_variances` — per-factor rolling variance
    ///   estimates. Missing factors default to `1.0` so the
    ///   shrinkage still applies with a sensible magnitude.
    ///
    /// Returns a basket of signed `(symbol, qty)` recommendations
    /// ready for the dashboard or an `ExecAlgorithm` dispatch.
    pub fn optimize(
        &self,
        exposure: &[(String, Decimal)],
        universe: &[HedgeInstrument],
        factor_variances: &HashMap<String, Decimal>,
    ) -> HedgeBasket {
        let mut entries: Vec<(String, Decimal)> = Vec::new();
        let mut seen_factors: Vec<&str> = Vec::new();
        for instrument in universe {
            // v1: at most one hedge instrument per factor. Skip
            // duplicates deterministically (first-match-wins).
            if seen_factors.contains(&instrument.factor.as_str()) {
                continue;
            }
            seen_factors.push(instrument.factor.as_str());

            let delta = exposure
                .iter()
                .find(|(f, _)| f == &instrument.factor)
                .map(|(_, d)| *d)
                .unwrap_or(Decimal::ZERO);
            if delta.is_zero() {
                continue;
            }

            // Unconstrained diagonal-β solution: sell exactly
            // the exposure on the one-for-one hedge.
            let h_unconstrained = -delta;

            // Inverse-variance κ. Zero-variance factors get
            // κ = 0 (no shrinkage) rather than a divide-by-zero
            // panic.
            let variance = factor_variances
                .get(&instrument.factor)
                .copied()
                .unwrap_or(Decimal::ONE);
            let kappa = if variance.is_zero() {
                Decimal::ZERO
            } else {
                Decimal::ONE / variance
            };

            // L1 funding-cost shrinkage.
            let shrinkage = self.funding_penalty * instrument.funding_bps * kappa;
            let abs_shrunk = (h_unconstrained.abs() - shrinkage).max(Decimal::ZERO);
            let h_shrunk = if h_unconstrained.is_sign_positive() || h_unconstrained.is_zero() {
                abs_shrunk
            } else {
                -abs_shrunk
            };
            if h_shrunk.is_zero() {
                continue;
            }

            // Hard per-instrument position cap.
            let capped = if h_shrunk.abs() > instrument.position_cap {
                if h_shrunk.is_sign_positive() {
                    instrument.position_cap
                } else {
                    -instrument.position_cap
                }
            } else {
                h_shrunk
            };
            if capped.is_zero() {
                continue;
            }
            entries.push((instrument.symbol.clone(), capped));
        }
        HedgeBasket { entries }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn btc_perp() -> HedgeInstrument {
        HedgeInstrument {
            symbol: "BTC-PERP".into(),
            factor: "BTC".into(),
            funding_bps: dec!(1),
            position_cap: dec!(10),
        }
    }

    fn eth_perp() -> HedgeInstrument {
        HedgeInstrument {
            symbol: "ETH-PERP".into(),
            factor: "ETH".into(),
            funding_bps: dec!(1),
            position_cap: dec!(100),
        }
    }

    fn default_variances() -> HashMap<String, Decimal> {
        let mut m = HashMap::new();
        m.insert("BTC".into(), dec!(1));
        m.insert("ETH".into(), dec!(1));
        m
    }

    /// A flat portfolio should produce an empty basket — nothing
    /// to hedge, no operator action required.
    #[test]
    fn flat_exposure_emits_empty_basket() {
        let opt = HedgeOptimizer::new(dec!(0));
        let basket = opt.optimize(&[], &[btc_perp(), eth_perp()], &default_variances());
        assert!(basket.is_empty());
    }

    /// The simplest non-trivial case: a single long BTC
    /// exposure with zero funding cost should recommend selling
    /// exactly the delta on the BTC perp.
    #[test]
    fn single_asset_trivial_hedge_with_zero_funding() {
        let opt = HedgeOptimizer::new(dec!(0));
        let exposure = vec![("BTC".to_string(), dec!(0.5))];
        let basket = opt.optimize(&exposure, &[btc_perp()], &default_variances());
        assert_eq!(basket.entries.len(), 1);
        assert_eq!(basket.entries[0], ("BTC-PERP".to_string(), dec!(-0.5)));
    }

    /// Setting `funding_penalty = 0` MUST reproduce the
    /// unconstrained closed form exactly: `h* = -x` per factor.
    /// Regression anchor for the shrinkage term not leaking
    /// into the zero-penalty path.
    #[test]
    fn zero_funding_penalty_reproduces_unconstrained() {
        let opt = HedgeOptimizer::new(dec!(0));
        let exposure = vec![("BTC".to_string(), dec!(1)), ("ETH".to_string(), dec!(-2))];
        let basket = opt.optimize(&exposure, &[btc_perp(), eth_perp()], &default_variances());
        assert_eq!(basket.entries.len(), 2);
        let btc = basket
            .entries
            .iter()
            .find(|(s, _)| s == "BTC-PERP")
            .unwrap();
        assert_eq!(btc.1, dec!(-1));
        let eth = basket
            .entries
            .iter()
            .find(|(s, _)| s == "ETH-PERP")
            .unwrap();
        assert_eq!(eth.1, dec!(2)); // -(-2) = +2
    }

    /// A very large funding penalty must shrink every hedge to
    /// zero — the penalty dominates the variance reduction term.
    /// Empty basket is the expected output.
    #[test]
    fn large_funding_penalty_shrinks_basket_to_zero() {
        let opt = HedgeOptimizer::new(dec!(10000));
        let exposure = vec![("BTC".to_string(), dec!(1))];
        let basket = opt.optimize(&exposure, &[btc_perp()], &default_variances());
        assert!(basket.is_empty());
    }

    /// An intermediate funding penalty must produce a non-zero
    /// hedge whose magnitude is strictly smaller than the
    /// unconstrained `|x|`. Pins the shrinkage arithmetic.
    #[test]
    fn intermediate_shrinkage_partial_hedge() {
        // unc = -1, kappa = 1, shrinkage = 0.5 * 1 * 1 = 0.5
        // |h_shrunk| = max(0, 1 - 0.5) = 0.5
        // h_shrunk = -0.5
        let opt = HedgeOptimizer::new(dec!(0.5));
        let mut v = HashMap::new();
        v.insert("BTC".to_string(), dec!(1));
        let exposure = vec![("BTC".to_string(), dec!(1))];
        let basket = opt.optimize(&exposure, &[btc_perp()], &v);
        assert_eq!(basket.entries.len(), 1);
        assert_eq!(basket.entries[0].1, dec!(-0.5));
    }

    /// Hard per-instrument cap clamps an oversized hedge. Catches
    /// the regression where the cap runs before the shrinkage
    /// (which would let a large funding penalty produce a
    /// mis-signed hedge after clamping).
    #[test]
    fn hard_cap_clamps_oversized_hedge() {
        let opt = HedgeOptimizer::new(dec!(0));
        let mut btc = btc_perp();
        btc.position_cap = dec!(2);
        let exposure = vec![("BTC".to_string(), dec!(10))];
        let basket = opt.optimize(&exposure, &[btc], &default_variances());
        assert_eq!(basket.entries.len(), 1);
        assert_eq!(basket.entries[0].1, dec!(-2));
    }

    /// When a factor is present in `exposure` but no matching
    /// hedge instrument exists in the universe, that factor's
    /// leg is silently absent from the basket (no panic, no
    /// misrouted hedge).
    #[test]
    fn missing_factor_in_universe_is_skipped() {
        let opt = HedgeOptimizer::new(dec!(0));
        let exposure = vec![
            ("BTC".to_string(), dec!(1)),
            ("SOL".to_string(), dec!(5)), // no SOL perp in universe
        ];
        let basket = opt.optimize(&exposure, &[btc_perp()], &default_variances());
        assert_eq!(basket.entries.len(), 1);
        assert!(basket.entries.iter().all(|(s, _)| s == "BTC-PERP"));
    }

    /// Long/short mix: a long BTC + short ETH exposure produces
    /// a sell-BTC-perp + buy-ETH-perp basket.
    #[test]
    fn long_short_mix_produces_opposite_signed_hedges() {
        let opt = HedgeOptimizer::new(dec!(0));
        let exposure = vec![("BTC".to_string(), dec!(1)), ("ETH".to_string(), dec!(-5))];
        let basket = opt.optimize(&exposure, &[btc_perp(), eth_perp()], &default_variances());
        let btc = basket
            .entries
            .iter()
            .find(|(s, _)| s == "BTC-PERP")
            .unwrap();
        let eth = basket
            .entries
            .iter()
            .find(|(s, _)| s == "ETH-PERP")
            .unwrap();
        assert!(btc.1.is_sign_negative(), "BTC hedge should sell");
        assert!(eth.1.is_sign_positive(), "ETH hedge should buy");
    }

    /// Zero-variance factor must NOT panic on the `1/variance`
    /// computation. κ falls through to zero, shrinkage is zero,
    /// and the trivial hedge applies.
    #[test]
    fn zero_variance_factor_does_not_panic() {
        let opt = HedgeOptimizer::new(dec!(1));
        let mut v = HashMap::new();
        v.insert("BTC".to_string(), dec!(0));
        let exposure = vec![("BTC".to_string(), dec!(1))];
        let basket = opt.optimize(&exposure, &[btc_perp()], &v);
        // κ = 0 → shrinkage = 0 → trivial -1 hedge.
        assert_eq!(basket.entries.len(), 1);
        assert_eq!(basket.entries[0].1, dec!(-1));
    }

    /// Duplicate hedge instruments for the same factor: only the
    /// first one wins (deterministic "first match" rule for v1).
    /// Stage-2 cross-beta will change this; pin the v1 contract
    /// explicitly so the future refactor is a conscious choice.
    #[test]
    fn duplicate_hedge_instruments_first_match_wins() {
        let opt = HedgeOptimizer::new(dec!(0));
        let btc_a = HedgeInstrument {
            symbol: "BTC-PERP-A".into(),
            factor: "BTC".into(),
            funding_bps: dec!(0),
            position_cap: dec!(10),
        };
        let btc_b = HedgeInstrument {
            symbol: "BTC-PERP-B".into(),
            factor: "BTC".into(),
            funding_bps: dec!(0),
            position_cap: dec!(10),
        };
        let exposure = vec![("BTC".to_string(), dec!(1))];
        let basket = opt.optimize(&exposure, &[btc_a, btc_b], &default_variances());
        assert_eq!(basket.entries.len(), 1);
        assert_eq!(basket.entries[0].0, "BTC-PERP-A");
    }

    /// Property-style invariant: the hedge qty for any instrument
    /// must never exceed its `position_cap` in absolute terms.
    /// Drives a handful of random-ish exposure vectors through
    /// the optimizer and pins the invariant across all of them.
    #[test]
    fn property_hedge_never_exceeds_cap() {
        let opt = HedgeOptimizer::new(dec!(0.1));
        let variances = default_variances();
        let cases: [Decimal; 6] = [dec!(0.5), dec!(1), dec!(2.5), dec!(10), dec!(50), dec!(100)];
        for delta in cases {
            let exposure = vec![("BTC".to_string(), delta)];
            let basket = opt.optimize(&exposure, &[btc_perp()], &variances);
            for (_, qty) in &basket.entries {
                assert!(
                    qty.abs() <= dec!(10),
                    "hedge {} exceeds cap 10 for delta {}",
                    qty,
                    delta
                );
            }
        }
    }

    /// `total_abs_qty` sums absolute quantities across the
    /// basket — used by the dashboard to show a single "basket
    /// size" number.
    #[test]
    fn basket_total_abs_qty_sums_absolutes() {
        let basket = HedgeBasket {
            entries: vec![
                ("BTC-PERP".to_string(), dec!(-1)),
                ("ETH-PERP".to_string(), dec!(2.5)),
            ],
        };
        assert_eq!(basket.total_abs_qty(), dec!(3.5));
    }

    /// Negative `funding_penalty` must be clamped to zero in
    /// the constructor — a negative penalty would mean
    /// "reward taking funding", which is not the formulation
    /// this module captures.
    #[test]
    fn negative_funding_penalty_is_clamped_to_zero() {
        let opt = HedgeOptimizer::new(dec!(-5));
        assert_eq!(opt.funding_penalty, dec!(0));
    }
}
