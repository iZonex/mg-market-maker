use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use std::sync::Mutex;
use tracing::debug;

use crate::r#trait::{bps_to_frac, CalibrationState, FillObservation, Strategy, StrategyContext};
use crate::volatility::decimal_sqrt;

/// Guéant-Lehalle-Fernandez-Tapia (GLFT) optimal market making model.
///
/// Extension of Avellaneda-Stoikov with:
/// - Execution risk from the order arrival process
/// - Bounded inventory constraints
/// - Calibrated intensity parameters from real order flow
///
/// Core formulas:
///   half_spread = (1 / (ξδ)) · ln(1 + ξδ/k)
///   skew = σ · C2
///   C2 = sqrt( (γ / (2Aδk)) · (1 + ξδ/k)^(k/(ξδ)+1) )
///
///   bid = fair_price - (half_spread + skew · q)
///   ask = fair_price + (half_spread - skew · q)
pub struct GlftStrategy {
    /// Calibrated intensity parameters. Behind a `Mutex` so the
    /// stateful `Strategy::on_fill` hook can recalibrate without
    /// a `&mut self` borrow — the trait is `Send + Sync` and the
    /// engine owns the strategy behind `Box<dyn Strategy>`.
    calibration: Mutex<IntensityCalibration>,
}

impl std::fmt::Debug for GlftStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.calibration.lock();
        match g {
            Ok(cal) => f.debug_struct("GlftStrategy")
                .field("a", &cal.a)
                .field("k", &cal.k)
                .field("samples", &cal.fill_depths.len())
                .finish(),
            Err(_) => f.debug_struct("GlftStrategy<poisoned>").finish(),
        }
    }
}

/// Parameters from fitting λ = A · exp(-k · δ).
#[derive(Debug, Clone)]
struct IntensityCalibration {
    /// Arrival rate constant A.
    a: Decimal,
    /// Depth sensitivity k.
    k: Decimal,
    /// Recent fill depths for recalibration.
    fill_depths: VecDeque<Decimal>,
    max_samples: usize,
    /// S5.4 — epoch-millis of the last successful recalibration.
    /// Used by `recalibrate_if_due` to enforce a 30-second floor
    /// between retunes so a burst of fills does not spin the
    /// smoothing filter into a local minimum.
    last_recalibrated_ms: Option<i64>,
}

impl IntensityCalibration {
    fn new() -> Self {
        Self {
            a: dec!(1.0),
            k: dec!(1.5),
            fill_depths: VecDeque::with_capacity(500),
            max_samples: 500,
            last_recalibrated_ms: None,
        }
    }

    /// Record a fill at a certain depth from mid.
    pub fn record_fill_depth(&mut self, depth_from_mid: Decimal) {
        self.fill_depths.push_back(depth_from_mid.abs());
        if self.fill_depths.len() > self.max_samples {
            self.fill_depths.pop_front();
        }
        if self.fill_depths.len() >= 50 {
            self.recalibrate(None);
        }
    }

    /// Recalibrate A and k from observed fill depths.
    ///
    /// Method: bin depths, count fills per bin, fit ln(λ) = ln(A) - k·δ.
    fn recalibrate(&mut self, now_ms: Option<i64>) {
        if self.fill_depths.len() < 50 {
            return;
        }

        // Simple: compute mean depth and use it to estimate k.
        // k ≈ 1 / mean_depth (the higher the mean depth, the lower the sensitivity).
        let n = Decimal::from(self.fill_depths.len() as u64);
        let mean_depth: Decimal = self.fill_depths.iter().sum::<Decimal>() / n;

        if mean_depth > dec!(0.000001) {
            let new_k = dec!(1) / mean_depth;
            // Smooth update.
            self.k = self.k * dec!(0.9) + new_k * dec!(0.1);
        }

        // A ≈ fill_rate (fills per second).
        // Simplified: we'll keep A at 1.0 since it cancels in the spread formula.
        debug!(k = %self.k, a = %self.a, samples = self.fill_depths.len(), "GLFT recalibrated");
        self.last_recalibrated_ms = now_ms.or(self.last_recalibrated_ms);
    }
}

impl GlftStrategy {
    pub fn new() -> Self {
        Self {
            calibration: Mutex::new(IntensityCalibration::new()),
        }
    }

    /// Test / external-caller hook — same semantics as invoking
    /// `Strategy::on_fill` via the trait, but exposed so the
    /// engine's legacy (non-graph) single-strategy slot can still
    /// drive calibration without going through the pool.
    pub fn record_fill_depth(&self, depth_from_mid: Decimal) {
        if let Ok(mut cal) = self.calibration.lock() {
            cal.record_fill_depth(depth_from_mid);
        }
    }
}

impl Default for GlftStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for GlftStrategy {
    fn name(&self) -> &str {
        "glft"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let gamma = ctx.config.gamma;
        let sigma = ctx.volatility;
        let t = ctx.time_remaining;
        let q = ctx.inventory;
        let (a, k) = match self.calibration.lock() {
            Ok(cal) => (cal.a, cal.k),
            // Poisoned mutex: fall back to the constructor defaults
            // so quoting continues rather than silently disappearing.
            Err(_) => (dec!(1.0), dec!(1.5)),
        };

        // ξ = γ (standard simplification).
        let xi = gamma;
        let delta = dec!(1);

        // C1 = (1 / (ξδ)) · ln(1 + ξδ/k)
        let xi_delta = xi * delta;
        let c1 = if xi_delta.is_zero() || k.is_zero() {
            bps_to_frac(ctx.config.min_spread_bps) * ctx.mid_price / dec!(2)
        } else {
            let ln_arg = dec!(1) + xi_delta / k;
            let ln_val = decimal_ln_positive(ln_arg);
            ln_val / xi_delta
        };

        // C2 = sqrt( (γ / (2Aδk)) · (1 + ξδ/k)^(k/(ξδ)+1) )
        let c2 = if gamma.is_zero() || a.is_zero() || k.is_zero() || xi_delta.is_zero() {
            dec!(0.001)
        } else {
            let base = dec!(1) + xi_delta / k;
            let exponent = k / xi_delta + dec!(1);
            // Approximate base^exponent via exp(exponent * ln(base)).
            let ln_base = decimal_ln_positive(base);
            let power = decimal_exp(exponent * ln_base);
            let inner = gamma / (dec!(2) * a * delta * k) * power;
            decimal_sqrt(inner.max(dec!(0)))
        };

        // half_spread and skew.
        let half_spread = c1 * sigma;
        let skew = sigma * c2;

        // Apply time scaling.
        let half_spread_t = half_spread * t;
        let skew_t = skew * t;

        // Optimal quotes.
        let fair = ctx.mid_price;
        let min_spread = bps_to_frac(ctx.config.min_spread_bps) * fair;

        // Epic D stage-2 sub-component 2B — Cartea
        // adverse-selection closed-form spread widening
        // (Cartea-Jaimungal-Penalva 2015 ch.4 §4.3 eq. 4.20).
        // Stage-3 promotes the symmetric path to a per-side
        // variant when both `as_prob_bid` AND `as_prob_ask`
        // are populated — each side gets its own additive
        // `(1 − 2·ρ_side) · σ · √(T − t)` term.
        //
        // `ρ > 0.5` narrows toward the floor; `ρ < 0.5`
        // widens. When per-side fields are absent the
        // symmetric `as_prob` path is the fallback. `as_prob
        // == None` OR `Some(0.5)` produce a byte-identical
        // half-spread to the pre-stage-2 wave-1 path.
        let min_half_spread = min_spread / dec!(2);
        let sqrt_t_remaining = decimal_sqrt(ctx.time_remaining);
        let (bid_half_spread_t, ask_half_spread_t) = match (ctx.as_prob_bid, ctx.as_prob_ask) {
            (Some(rho_b), Some(rho_a)) => {
                // Per-side path — Epic D stage-3.
                let bid_widen = (dec!(1) - dec!(2) * rho_b) * sigma * sqrt_t_remaining;
                let ask_widen = (dec!(1) - dec!(2) * rho_a) * sigma * sqrt_t_remaining;
                (
                    (half_spread_t + bid_widen).max(min_half_spread),
                    (half_spread_t + ask_widen).max(min_half_spread),
                )
            }
            _ => {
                // Symmetric fallback (Epic D stage-2 path).
                let half = match ctx.as_prob {
                    None => half_spread_t,
                    Some(rho) if rho == dec!(0.5) => half_spread_t,
                    Some(rho) => {
                        let as_delta = (dec!(1) - dec!(2) * rho) * sigma * sqrt_t_remaining;
                        (half_spread_t + as_delta).max(min_half_spread)
                    }
                };
                (half, half)
            }
        };

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);

        for level in 0..ctx.config.num_levels {
            // Per-side level offsets — symmetric on the
            // existing average half-spread for backward
            // compat with the wave-1 level-spreading
            // semantics.
            let avg_half = (bid_half_spread_t + ask_half_spread_t) / dec!(2);
            let level_offset = Decimal::from(level as u64) * avg_half;

            let bid_price = fair - (bid_half_spread_t + skew_t * q + level_offset);
            let ask_price = fair + (ask_half_spread_t - skew_t * q + level_offset);

            // Enforce minimum spread.
            let actual_spread = ask_price - bid_price;
            let (bid_price, ask_price) = if actual_spread < min_spread {
                let adjustment = (min_spread - actual_spread) / dec!(2);
                (bid_price - adjustment, ask_price + adjustment)
            } else {
                (bid_price, ask_price)
            };

            let bid_price = ctx.product.round_price(bid_price);
            let ask_price = ctx.product.round_price(ask_price);
            let order_size = ctx.product.round_qty(ctx.config.order_size);

            let bid =
                if bid_price > dec!(0) && ctx.product.meets_min_notional(bid_price, order_size) {
                    Some(Quote {
                        side: Side::Buy,
                        price: bid_price,
                        qty: order_size,
                    })
                } else {
                    None
                };

            let ask =
                if ask_price > dec!(0) && ctx.product.meets_min_notional(ask_price, order_size) {
                    Some(Quote {
                        side: Side::Sell,
                        price: ask_price,
                        qty: order_size,
                    })
                } else {
                    None
                };

            quotes.push(QuotePair { bid, ask });
        }

        debug!(
            strategy = "glft",
            c1 = %c1,
            c2 = %c2,
            bid_half_spread = %bid_half_spread_t,
            ask_half_spread = %ask_half_spread_t,
            skew = %skew_t,
            k = %k,
            inventory = %q,
            "computed GLFT quotes"
        );

        quotes
    }

    /// MM-2 — recalibrate the intensity curve on every real fill.
    /// Passive + take-out fills both carry usable depth-from-mid
    /// information, so we don't filter on `is_maker`.
    fn on_fill(&self, obs: &FillObservation) {
        self.record_fill_depth(obs.depth_from_mid);
    }

    /// S5.4 — surface the fitted `(A, k)` + sample count for the
    /// calibration monitor panel. Returns `None` only if the
    /// internal mutex is poisoned, so the panel treats missing
    /// rows as "strategy has not reported".
    fn calibration_state(&self) -> Option<CalibrationState> {
        let cal = self.calibration.lock().ok()?;
        Some(CalibrationState {
            strategy: "glft".to_string(),
            a: cal.a,
            k: cal.k,
            samples: cal.fill_depths.len(),
            last_recalibrated_ms: cal.last_recalibrated_ms,
        })
    }

    /// S5.4 — periodic retune. Engines call this on their
    /// minute-cadence tick; the calibrator runs only if ≥30 s
    /// have elapsed since the previous retune AND the sample
    /// buffer has crossed the ≥50-fill threshold. Without this
    /// hook, calibration only fires on fills, so a symbol
    /// mid-low-activity window drifts on a stale `k`.
    fn recalibrate_if_due(&self, now_ms: i64) {
        let mut cal = match self.calibration.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if cal.fill_depths.len() < 50 {
            return;
        }
        let due = match cal.last_recalibrated_ms {
            Some(ts) => now_ms.saturating_sub(ts) >= 30_000,
            None => true,
        };
        if !due {
            return;
        }
        cal.recalibrate(Some(now_ms));
    }

    /// 22B-1 — serialise the fitted (A, k) + fill depth buffer
    /// + last-recalibrated stamp so a restart doesn't nuke the
    /// 50+ samples of accumulated calibration. Without this,
    /// every restart runs at defaults `a=1.0, k=1.5` for the
    /// first 50+ fills before the filter re-warms. Returns
    /// `None` only if the mutex is poisoned — the checkpoint
    /// writer falls through to "no state" rather than spamming
    /// errors on a pathological poisoning.
    fn checkpoint_state(&self) -> Option<serde_json::Value> {
        let cal = self.calibration.lock().ok()?;
        Some(serde_json::json!({
            "schema_version": 1,
            "a": cal.a.to_string(),
            "k": cal.k.to_string(),
            "fill_depths": cal.fill_depths.iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>(),
            "last_recalibrated_ms": cal.last_recalibrated_ms,
        }))
    }

    /// 22B-1 — restore the fitted (A, k) + fill depth buffer
    /// from a previously captured checkpoint. Schema-versioned
    /// so a breaking change flips the gate without crashing.
    /// `max_samples` is a constructor-time constant (500) and
    /// is NOT persisted — a checkpoint with more samples than
    /// the current cap is silently truncated.
    fn restore_state(&self, state: &serde_json::Value) -> Result<(), String> {
        let schema = state.get("schema_version").and_then(|v| v.as_u64());
        if schema != Some(1) {
            return Err(format!(
                "glft checkpoint has unsupported schema_version {schema:?}"
            ));
        }
        let a = state
            .get("a")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Decimal>().ok())
            .ok_or_else(|| "glft: missing/invalid field `a`".to_string())?;
        let k = state
            .get("k")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Decimal>().ok())
            .ok_or_else(|| "glft: missing/invalid field `k`".to_string())?;
        let last_recal = state
            .get("last_recalibrated_ms")
            .and_then(|v| v.as_i64());
        let depths: VecDeque<Decimal> = state
            .get("fill_depths")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "glft: missing/invalid field `fill_depths`".to_string())?
            .iter()
            .filter_map(|d| d.as_str()?.parse::<Decimal>().ok())
            .collect();

        let mut cal = self
            .calibration
            .lock()
            .map_err(|_| "glft: calibration mutex poisoned".to_string())?;
        cal.a = a;
        cal.k = k;
        cal.last_recalibrated_ms = last_recal;
        cal.fill_depths = depths;
        // Truncate if the on-disk buffer is larger than the
        // current cap (supports future max_samples tuning
        // without rejecting legacy checkpoints).
        while cal.fill_depths.len() > cal.max_samples {
            cal.fill_depths.pop_front();
        }
        Ok(())
    }
}

/// Natural log for positive Decimal values.
fn decimal_ln_positive(x: Decimal) -> Decimal {
    if x <= dec!(0) {
        return dec!(0);
    }
    if x == dec!(1) {
        return dec!(0);
    }
    let u = (x - dec!(1)) / (x + dec!(1));
    let u2 = u * u;
    let mut term = u;
    let mut sum = u;
    for k in 1..20 {
        term *= u2;
        sum += term / Decimal::from(2 * k + 1);
    }
    dec!(2) * sum
}

/// Exponential function for Decimal via Taylor series.
fn decimal_exp(x: Decimal) -> Decimal {
    // Clamp to avoid overflow.
    let x = x.min(dec!(20)).max(dec!(-20));
    let mut sum = dec!(1);
    let mut term = dec!(1);
    for i in 1..30 {
        term *= x / Decimal::from(i);
        sum += term;
        if term.abs() < dec!(0.0000000001) {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests;
