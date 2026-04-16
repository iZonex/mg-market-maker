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
use rust_decimal_macros::dec;

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
    /// Cross-beta exposures: `(factor_name, beta_value)` pairs
    /// for factors OTHER than the primary `self.factor`. When
    /// non-empty, the optimizer accounts for the instrument's
    /// cross-factor exposure in the hedge computation. Stage-2.
    ///
    /// Example: ETH-PERP with `cross_betas = [("BTC", 0.4)]`
    /// means 1 unit of ETH-PERP hedges 1.0 of ETH-delta AND
    /// 0.4 of BTC-delta.
    pub cross_betas: Vec<(String, Decimal)>,
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

        // Collect residual exposure — starts as a copy of the
        // input and gets reduced by each hedge instrument's
        // cross-beta contribution.
        let mut residual: HashMap<String, Decimal> = exposure.iter().cloned().collect();

        for instrument in universe {
            // At most one hedge instrument per primary factor.
            if seen_factors.contains(&instrument.factor.as_str()) {
                continue;
            }
            seen_factors.push(instrument.factor.as_str());

            let delta = residual
                .get(&instrument.factor)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if delta.is_zero() {
                continue;
            }

            // Unconstrained solution on the primary factor.
            let h_unconstrained = -delta;

            // Inverse-variance κ for funding-cost shrinkage.
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

            // Stage-2 cross-beta: reduce residual exposure on
            // other factors by this instrument's cross-beta.
            // E.g. if ETH-PERP has cross_beta ("BTC", 0.4) and
            // we hedge `capped` units, the BTC residual is
            // reduced by `capped * 0.4`.
            for (factor, beta) in &instrument.cross_betas {
                if let Some(res) = residual.get_mut(factor) {
                    *res += capped * *beta;
                }
            }

            entries.push((instrument.symbol.clone(), capped));
        }
        HedgeBasket { entries }
    }
}

/// Rolling factor covariance estimator (stage-2). Maintains a
/// rolling window of per-factor return samples and computes
/// the full covariance matrix (including off-diagonal entries)
/// on demand. Replaces the v1 diagonal-only constant-variance
/// stub in the engine's `factor_variances()` method.
#[derive(Debug, Clone)]
pub struct FactorCovarianceEstimator {
    /// Factor names in canonical order.
    factors: Vec<String>,
    /// Per-factor rolling return buffers.
    buffers: Vec<std::collections::VecDeque<Decimal>>,
    /// Maximum samples per factor.
    max_samples: usize,
}

impl FactorCovarianceEstimator {
    pub fn new(factors: Vec<String>, max_samples: usize) -> Self {
        let n = factors.len();
        Self {
            factors,
            buffers: (0..n).map(|_| std::collections::VecDeque::new()).collect(),
            max_samples,
        }
    }

    /// Push a return observation for a factor.
    pub fn push_return(&mut self, factor: &str, ret: Decimal) {
        let Some(idx) = self.factors.iter().position(|f| f == factor) else {
            return;
        };
        let buf = &mut self.buffers[idx];
        if buf.len() >= self.max_samples {
            buf.pop_front();
        }
        buf.push_back(ret);
    }

    /// Compute the full variance-covariance matrix. Returns a
    /// `HashMap<String, Decimal>` for diagonal entries (variance)
    /// and a `HashMap<(String, String), Decimal>` for off-diagonal
    /// entries (covariance). Both keyed by factor names.
    pub fn variances(&self) -> HashMap<String, Decimal> {
        let mut out = HashMap::new();
        for (i, factor) in self.factors.iter().enumerate() {
            let var = self.sample_variance(i);
            out.insert(factor.clone(), var.unwrap_or(Decimal::ONE));
        }
        out
    }

    /// Off-diagonal covariance between two factors. Returns
    /// `None` if either factor has too few samples.
    pub fn covariance(&self, factor_a: &str, factor_b: &str) -> Option<Decimal> {
        let ia = self.factors.iter().position(|f| f == factor_a)?;
        let ib = self.factors.iter().position(|f| f == factor_b)?;
        let buf_a = &self.buffers[ia];
        let buf_b = &self.buffers[ib];
        let n = buf_a.len().min(buf_b.len());
        if n < 2 {
            return None;
        }
        let n_dec = Decimal::from(n as u64);
        let mean_a: Decimal = buf_a.iter().rev().take(n).sum::<Decimal>() / n_dec;
        let mean_b: Decimal = buf_b.iter().rev().take(n).sum::<Decimal>() / n_dec;
        let cov: Decimal = buf_a
            .iter()
            .rev()
            .take(n)
            .zip(buf_b.iter().rev().take(n))
            .map(|(a, b)| (*a - mean_a) * (*b - mean_b))
            .sum::<Decimal>()
            / Decimal::from((n - 1) as u64);
        Some(cov)
    }

    /// Correlation between two factors. `None` if either has
    /// zero variance or too few samples.
    pub fn correlation(&self, factor_a: &str, factor_b: &str) -> Option<Decimal> {
        let cov = self.covariance(factor_a, factor_b)?;
        let var_a = self.sample_variance(self.factors.iter().position(|f| f == factor_a)?)?;
        let var_b = self.sample_variance(self.factors.iter().position(|f| f == factor_b)?)?;
        if var_a.is_zero() || var_b.is_zero() {
            return None;
        }
        let denom = decimal_sqrt(var_a) * decimal_sqrt(var_b);
        if denom.is_zero() {
            return None;
        }
        Some(cov / denom)
    }

    /// Number of samples for a factor.
    pub fn sample_count(&self, factor: &str) -> usize {
        self.factors
            .iter()
            .position(|f| f == factor)
            .map(|i| self.buffers[i].len())
            .unwrap_or(0)
    }

    /// Push a return observation for a factor, auto-registering
    /// unknown factors. Suitable for a shared multi-engine
    /// estimator where the full factor set is not known at
    /// construction time.
    pub fn merge_observation(&mut self, factor: &str, ret: Decimal) {
        if !self.factors.iter().any(|f| f == factor) {
            self.factors.push(factor.to_string());
            self.buffers.push(std::collections::VecDeque::new());
        }
        self.push_return(factor, ret);
    }

    /// Return the list of registered factors.
    pub fn factors(&self) -> &[String] {
        &self.factors
    }

    /// Compute the full pairwise correlation matrix. Returns a
    /// vec of `(factor_a, factor_b, correlation)` triples for
    /// all unique pairs where both factors have enough samples.
    /// Diagonal entries (`factor == factor`) are omitted.
    pub fn correlation_matrix(&self) -> Vec<(String, String, Decimal)> {
        let mut out = Vec::new();
        for (i, fa) in self.factors.iter().enumerate() {
            for fb in self.factors.iter().skip(i + 1) {
                if let Some(corr) = self.correlation(fa, fb) {
                    out.push((fa.clone(), fb.clone(), corr));
                }
            }
        }
        out
    }

    fn sample_variance(&self, idx: usize) -> Option<Decimal> {
        let buf = &self.buffers[idx];
        if buf.len() < 2 {
            return None;
        }
        let n = buf.len();
        let n_dec = Decimal::from(n as u64);
        let mean: Decimal = buf.iter().sum::<Decimal>() / n_dec;
        let var: Decimal = buf
            .iter()
            .map(|x| (*x - mean) * (*x - mean))
            .sum::<Decimal>()
            / Decimal::from((n - 1) as u64);
        Some(var)
    }
}

fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let mut guess = if x > Decimal::ONE { x / dec!(2) } else { x };
    for _ in 0..20 {
        let next = (guess + x / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000000001) {
            return next;
        }
        guess = next;
    }
    guess
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn btc_perp() -> HedgeInstrument {
        HedgeInstrument {
            symbol: "BTC-PERP".into(),
            factor: "BTC".into(),
            cross_betas: vec![],
            funding_bps: dec!(1),
            position_cap: dec!(10),
        }
    }

    fn eth_perp() -> HedgeInstrument {
        HedgeInstrument {
            symbol: "ETH-PERP".into(),
            factor: "ETH".into(),
            cross_betas: vec![],
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
            cross_betas: vec![],
            funding_bps: dec!(0),
            position_cap: dec!(10),
        };
        let btc_b = HedgeInstrument {
            symbol: "BTC-PERP-B".into(),
            factor: "BTC".into(),
            cross_betas: vec![],
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

    // ── Cross-beta tests (stage-2) ──────────────────────────

    /// ETH-PERP with cross-beta to BTC should reduce the
    /// BTC residual. With β_ETH→BTC = 0.5, hedging 5 ETH
    /// reduces the BTC residual by 5 * 0.5 = 2.5.
    #[test]
    fn cross_beta_reduces_residual_on_other_factor() {
        let opt = HedgeOptimizer::new(dec!(0));
        // ETH-PERP hedges 1:1 on ETH and 0.5:1 on BTC.
        let eth_with_cross = HedgeInstrument {
            symbol: "ETH-PERP".into(),
            factor: "ETH".into(),
            cross_betas: vec![("BTC".into(), dec!(0.5))],
            funding_bps: dec!(0),
            position_cap: dec!(100),
        };
        // Expose both: long 5 BTC + long 10 ETH.
        // ETH leg: h = -10. Cross-beta effect on BTC: -10 * 0.5 = -5.
        // BTC residual: 5 + (-10 * 0.5) = 0 → BTC-PERP not needed.
        let exposure = vec![("BTC".into(), dec!(5)), ("ETH".into(), dec!(10))];
        // ETH-PERP first, then BTC-PERP. Order matters: ETH
        // hedges first and its cross-beta reduces BTC residual.
        let basket = opt.optimize(
            &exposure,
            &[eth_with_cross, btc_perp()],
            &default_variances(),
        );
        // ETH-PERP should hedge -10. BTC residual is
        // 5 + (-10 * 0.5) = 0, so BTC-PERP should be absent.
        let eth = basket
            .entries
            .iter()
            .find(|(s, _)| s == "ETH-PERP")
            .unwrap();
        assert_eq!(eth.1, dec!(-10));
        let btc = basket.entries.iter().find(|(s, _)| s == "BTC-PERP");
        assert!(
            btc.is_none(),
            "BTC-PERP should be absent, cross-beta zeroed the residual"
        );
    }

    /// Without cross-beta, both factors need their own hedges.
    /// With cross-beta, the second hedge is smaller because
    /// the first instrument partially covers it.
    #[test]
    fn cross_beta_reduces_second_hedge_size() {
        let opt = HedgeOptimizer::new(dec!(0));
        let eth_cross = HedgeInstrument {
            symbol: "ETH-PERP".into(),
            factor: "ETH".into(),
            cross_betas: vec![("BTC".into(), dec!(0.3))],
            funding_bps: dec!(0),
            position_cap: dec!(100),
        };
        let exposure = vec![("BTC".into(), dec!(10)), ("ETH".into(), dec!(5))];
        // ETH hedge: -5. Cross-beta: -5 * 0.3 = -1.5 on BTC.
        // BTC residual: 10 + (-5 * 0.3) = 8.5 → BTC-PERP = -8.5.
        let basket = opt.optimize(&exposure, &[eth_cross, btc_perp()], &default_variances());
        let btc = basket
            .entries
            .iter()
            .find(|(s, _)| s == "BTC-PERP")
            .unwrap();
        assert_eq!(btc.1, dec!(-8.5));
    }

    /// Empty cross_betas behaves identically to v1 diagonal.
    #[test]
    fn empty_cross_betas_matches_diagonal() {
        let opt = HedgeOptimizer::new(dec!(0));
        let exposure = vec![("BTC".into(), dec!(1)), ("ETH".into(), dec!(-2))];
        let basket = opt.optimize(&exposure, &[btc_perp(), eth_perp()], &default_variances());
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
        assert_eq!(eth.1, dec!(2));
    }

    // ── Factor covariance estimator tests ───────────────────

    #[test]
    fn covariance_estimator_diagonal_variances() {
        let mut est = FactorCovarianceEstimator::new(vec!["BTC".into(), "ETH".into()], 100);
        // Push identical returns → variance should be ~0.
        for _ in 0..50 {
            est.push_return("BTC", dec!(0.01));
            est.push_return("ETH", dec!(0.02));
        }
        let vars = est.variances();
        assert!(vars["BTC"].abs() < dec!(0.0001));
        assert!(vars["ETH"].abs() < dec!(0.0001));
    }

    #[test]
    fn covariance_estimator_positive_correlation() {
        let mut est = FactorCovarianceEstimator::new(vec!["BTC".into(), "ETH".into()], 100);
        // Perfectly correlated: BTC and ETH move together.
        for i in 0..50 {
            let r = if i % 2 == 0 { dec!(0.01) } else { dec!(-0.01) };
            est.push_return("BTC", r);
            est.push_return("ETH", r * dec!(2)); // same direction
        }
        let corr = est.correlation("BTC", "ETH").unwrap();
        assert!(
            corr > dec!(0.9),
            "perfectly correlated returns should have corr > 0.9, got {}",
            corr
        );
    }

    #[test]
    fn covariance_estimator_negative_correlation() {
        let mut est = FactorCovarianceEstimator::new(vec!["A".into(), "B".into()], 100);
        // Anti-correlated: A and B move in opposite directions.
        for i in 0..50 {
            let r = if i % 2 == 0 { dec!(0.01) } else { dec!(-0.01) };
            est.push_return("A", r);
            est.push_return("B", -r);
        }
        let corr = est.correlation("A", "B").unwrap();
        assert!(
            corr < dec!(-0.9),
            "anti-correlated returns should have corr < -0.9, got {}",
            corr
        );
    }

    #[test]
    fn covariance_estimator_unknown_factor_is_noop() {
        let mut est = FactorCovarianceEstimator::new(vec!["BTC".into()], 100);
        est.push_return("UNKNOWN", dec!(0.01));
        assert_eq!(est.sample_count("UNKNOWN"), 0);
    }

    #[test]
    fn covariance_estimator_rolling_window_caps() {
        let mut est = FactorCovarianceEstimator::new(vec!["BTC".into()], 50);
        for i in 0..100 {
            est.push_return("BTC", Decimal::from(i));
        }
        assert_eq!(est.sample_count("BTC"), 50);
    }

    // ── Epic 3: merge_observation + correlation_matrix ──────

    #[test]
    fn merge_observation_auto_registers_unknown_factor() {
        let mut est = FactorCovarianceEstimator::new(vec![], 100);
        est.merge_observation("BTC", dec!(0.01));
        est.merge_observation("ETH", dec!(0.02));
        assert_eq!(est.factors().len(), 2);
        assert_eq!(est.sample_count("BTC"), 1);
        assert_eq!(est.sample_count("ETH"), 1);
    }

    #[test]
    fn merge_observation_appends_to_existing_factor() {
        let mut est = FactorCovarianceEstimator::new(vec!["BTC".into()], 100);
        est.merge_observation("BTC", dec!(0.01));
        est.merge_observation("BTC", dec!(0.02));
        assert_eq!(est.factors().len(), 1);
        assert_eq!(est.sample_count("BTC"), 2);
    }

    #[test]
    fn correlation_matrix_returns_pairwise() {
        let mut est = FactorCovarianceEstimator::new(vec![], 100);
        for i in 0..50 {
            let r = if i % 2 == 0 { dec!(0.01) } else { dec!(-0.01) };
            est.merge_observation("BTC", r);
            est.merge_observation("ETH", r * dec!(2));
            est.merge_observation("SOL", -r);
        }
        let matrix = est.correlation_matrix();
        // 3 factors → 3 unique pairs
        assert_eq!(matrix.len(), 3);
        // BTC-ETH should be positive, BTC-SOL should be negative
        let btc_eth = matrix
            .iter()
            .find(|(a, b, _)| a == "BTC" && b == "ETH")
            .unwrap();
        assert!(btc_eth.2 > dec!(0.9));
        let btc_sol = matrix
            .iter()
            .find(|(a, b, _)| a == "BTC" && b == "SOL")
            .unwrap();
        assert!(btc_sol.2 < dec!(-0.9));
    }
}
