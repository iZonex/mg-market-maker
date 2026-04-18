//! Microstructure feature extractors.
//!
//! Pure numerical primitives that turn raw order-book and trade
//! events into features suitable for a downstream alpha model. No
//! training loop, no PyTorch, no ONNX in this module — just the
//! feature engineering layer. A separate predictor crate (future)
//! can consume the `FeatureVector` output directly.
//!
//! Shipped extractors:
//!
//! - **Book imbalance at depth k** — signed ratio of bid vs ask
//!   volume in the top k levels. Range `[-1, +1]`. Positive = more
//!   buy pressure.
//! - **Trade flow EWMA** — exponentially weighted signed volume of
//!   recent trades. Positive = buyers dominating taker flow.
//! - **Micro-price** — `(bid_qty * ask_px + ask_qty * bid_px) / (bid_qty
//!   + ask_qty)`. Robust to momentary quote pulls.
//! - **Micro-price drift** — EWMA of micro-price changes, a
//!   directional-pressure estimate.
//! - **Realised volatility term structure** — parallel EWMAs at
//!   multiple half-lives; ratio of short over long gives the
//!   short-term volatility regime.
//!
//! All extractors are **lookahead-safe by construction** — each
//! `update()` reads only new data, and `value()` returns the latest
//! snapshot. (The dedicated `check_lookahead` property test lives in
//! the backtester crate; the construction here is the primary
//! guarantee.)

use mm_common::types::{PriceLevel, Side};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

// ---------------------------------------------------------------------------
// Book imbalance
// ---------------------------------------------------------------------------

/// Signed order-book imbalance at depth `k`:
///
/// `imbalance = (bid_qty_top_k - ask_qty_top_k) / (bid_qty_top_k + ask_qty_top_k)`
///
/// Returns `0` if both sides are empty.
pub fn book_imbalance(bids: &[PriceLevel], asks: &[PriceLevel], k: usize) -> Decimal {
    let bid_qty: Decimal = bids.iter().take(k).map(|l| l.qty).sum();
    let ask_qty: Decimal = asks.iter().take(k).map(|l| l.qty).sum();
    let total = bid_qty + ask_qty;
    if total.is_zero() {
        return Decimal::ZERO;
    }
    (bid_qty - ask_qty) / total
}

/// Weighted book imbalance — inner levels count more via a linear
/// decay. Useful when top-of-book depth dominates execution and
/// deeper levels are mostly noise.
pub fn book_imbalance_weighted(bids: &[PriceLevel], asks: &[PriceLevel], k: usize) -> Decimal {
    let mut bid_w = Decimal::ZERO;
    let mut ask_w = Decimal::ZERO;
    for i in 0..k {
        let weight = Decimal::from((k - i) as i64);
        if let Some(level) = bids.get(i) {
            bid_w += level.qty * weight;
        }
        if let Some(level) = asks.get(i) {
            ask_w += level.qty * weight;
        }
    }
    let total = bid_w + ask_w;
    if total.is_zero() {
        return Decimal::ZERO;
    }
    (bid_w - ask_w) / total
}

// ---------------------------------------------------------------------------
// Trade flow
// ---------------------------------------------------------------------------

/// EWMA of signed trade volume. Buy trades add `+qty`, sell trades
/// add `-qty`. Positive values indicate net buying pressure.
#[derive(Debug, Clone)]
pub struct TradeFlow {
    alpha: Decimal,
    state: Option<Decimal>,
}

impl TradeFlow {
    /// `half_life_trades` is the number of trades over which the
    /// influence of a single event decays to half. Smaller values
    /// react faster.
    pub fn new(half_life_trades: usize) -> Self {
        assert!(half_life_trades > 0);
        // α such that (1-α)^half_life = 0.5 → α = 1 - 2^(-1/half_life).
        let n = half_life_trades as f64;
        let alpha_f = 1.0 - 0.5f64.powf(1.0 / n);
        Self {
            alpha: Decimal::from_f64(alpha_f).unwrap_or(dec!(0.1)),
            state: None,
        }
    }

    pub fn update(&mut self, taker_side: Side, qty: Decimal) {
        let signed = match taker_side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };
        self.state = Some(match self.state {
            None => signed,
            Some(prev) => self.alpha * signed + (Decimal::ONE - self.alpha) * prev,
        });
    }

    pub fn value(&self) -> Option<Decimal> {
        self.state
    }
}

// ---------------------------------------------------------------------------
// Micro-price
// ---------------------------------------------------------------------------

/// Micro-price weighted by the opposite side's quantity. Used as a
/// more robust mid than `(bid + ask) / 2` because it favours
/// whichever side has more resting liquidity.
///
/// `None` if either side is empty.
pub fn micro_price(bids: &[PriceLevel], asks: &[PriceLevel]) -> Option<Decimal> {
    let bid = bids.first()?;
    let ask = asks.first()?;
    let total = bid.qty + ask.qty;
    if total.is_zero() {
        return None;
    }
    Some((bid.qty * ask.price + ask.qty * bid.price) / total)
}

/// Multi-level microprice with linearly decaying depth weights.
///
/// For each of the top `depth` levels, the per-level microprice is
/// `(bid_px * ask_qty + ask_px * bid_qty) / (bid_qty + ask_qty)` —
/// the classic opposite-side-weighted fair value. The final output
/// is the weighted average of those per-level values with weight
/// `w(i) = depth - i` on level `i` (i.e. top-of-book matters most,
/// deep levels contribute less).
///
/// A multi-level microprice is more robust than top-of-book alone
/// when the inside quote is thin: one dusting order at the touch
/// can't dominate the fair-value estimate. `BasisStrategy` and any
/// alpha model that shifts reservation price around a fair value
/// should prefer this when `depth ≥ 3`.
///
/// Returns `None` if either side is empty. `depth` is clamped to
/// `min(bids.len(), asks.len())`.
pub fn micro_price_weighted(
    bids: &[PriceLevel],
    asks: &[PriceLevel],
    depth: usize,
) -> Option<Decimal> {
    if bids.is_empty() || asks.is_empty() {
        return None;
    }
    let d = depth.min(bids.len()).min(asks.len());
    if d == 0 {
        return None;
    }

    let mut num = Decimal::ZERO;
    let mut den = Decimal::ZERO;
    for i in 0..d {
        let bid = &bids[i];
        let ask = &asks[i];
        let w = Decimal::from(d - i);
        let level_total = bid.qty + ask.qty;
        if level_total.is_zero() {
            continue;
        }
        // Accumulate numerator and denominator without
        // computing each per-level microprice individually —
        // algebraically identical but avoids d divisions.
        num += (bid.qty * ask.price + ask.qty * bid.price) * w;
        den += level_total * w;
    }
    if den.is_zero() {
        return None;
    }
    Some(num / den)
}

// ---------------------------------------------------------------------------
// Market-impact (taker cost) walker
// ---------------------------------------------------------------------------

/// Outcome of a hypothetical taker order walked against a book side.
///
/// All fields are expressed in the native asset denominations the
/// `PriceLevel` entries carry, so no `Decimal`→f64 round-trip at the
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketImpact {
    /// Volume-weighted average fill price — the price the taker
    /// actually pays / receives once the order walks the book.
    pub vwap: Decimal,
    /// Base-asset quantity filled. Equals the input `target_qty`
    /// when the book is deep enough, and is clamped to total
    /// available qty on the side otherwise.
    pub filled_qty: Decimal,
    /// Notional (quote asset) actually consumed by the walk.
    pub notional: Decimal,
    /// Signed slippage vs `reference_price`, in basis points.
    /// Positive = unfavourable (buy pays above reference, sell
    /// receives below). Use this directly to size urgency or to
    /// reject a taker that would cost more than `min_edge_bps`.
    pub impact_bps: Decimal,
    /// `true` when the book had less liquidity than `target_qty`
    /// requested — callers that must fill the full size should
    /// treat this as a reject.
    pub partial: bool,
}

/// Walk `levels` against an incoming taker order of size
/// `target_qty` and compute the resulting VWAP + slippage in bps
/// versus `reference_price`.
///
/// `side` is the taker side:
/// - `Side::Buy`  → walks ASK levels (pay up the book), impact is
///   positive when VWAP > reference.
/// - `Side::Sell` → walks BID levels (hit down the book), impact
///   is positive when VWAP < reference.
///
/// Levels must already be sorted in the correct walk direction
/// (best price first on both sides, i.e. ascending for asks,
/// descending for bids). `LocalOrderBook::best_*` side iterators
/// already satisfy this.
///
/// The classic use cases:
/// - `XEMM::on_maker_fill` sanity-checks the hedge leg is still
///   within `max_slippage_bps`.
/// - `BasisStrategy` prices the cross using the real taker cost
///   on the hedge leg instead of the touch.
/// - `PairedUnwindExecutor` derives slice urgency from expected
///   impact.
///
/// `None` is returned only when `levels` is empty. A partial walk
/// (book thinner than `target_qty`) sets `partial = true` and
/// reports the VWAP of whatever it could consume.
pub fn market_impact(
    levels: &[PriceLevel],
    side: Side,
    target_qty: Decimal,
    reference_price: Decimal,
) -> Option<MarketImpact> {
    if levels.is_empty() || target_qty <= Decimal::ZERO {
        return None;
    }

    let mut remaining = target_qty;
    let mut notional = Decimal::ZERO;
    let mut filled = Decimal::ZERO;

    for level in levels {
        if remaining <= Decimal::ZERO {
            break;
        }
        let take = remaining.min(level.qty);
        notional += take * level.price;
        filled += take;
        remaining -= take;
    }

    if filled <= Decimal::ZERO {
        return None;
    }

    let vwap = notional / filled;
    let partial = remaining > Decimal::ZERO;

    // Signed slippage in bps — the sign convention makes positive
    // always unfavourable to the taker so callers can compare the
    // impact against a budget without a per-side case.
    let impact_bps = if reference_price.is_zero() {
        Decimal::ZERO
    } else {
        let raw = match side {
            Side::Buy => (vwap - reference_price) / reference_price,
            Side::Sell => (reference_price - vwap) / reference_price,
        };
        raw * dec!(10_000)
    };

    Some(MarketImpact {
        vwap,
        filled_qty: filled,
        notional,
        impact_bps,
        partial,
    })
}

// ---------------------------------------------------------------------------
// Lead-lag path transform
// ---------------------------------------------------------------------------

/// Interleaved lead-lag encoding of a 1-D price series.
///
/// Given prices `[p₀, p₁, …, pₙ]`, returns
/// `[(p₀, p₀), (p₁, p₀), (p₁, p₁), (p₂, p₁), …]`. Each pair
/// `(lead, lag)` has the lead component one step ahead of the lag
/// component at every other position. Downstream consumers
/// (path-signature features, simple autocorrelation estimators)
/// use the 2-D path to capture first-order serial dependence
/// without having to recompute rolling windows.
///
/// Reference: Gyurkó, Lyons, Kontkowski, Field (2013),
/// "Extracting information from the signature of a financial
/// data stream", §2.
pub fn lead_lag_transform(prices: &[Decimal]) -> Vec<(Decimal, Decimal)> {
    if prices.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(2 * prices.len() - 1);
    out.push((prices[0], prices[0]));
    for i in 1..prices.len() {
        out.push((prices[i], prices[i - 1]));
        out.push((prices[i], prices[i]));
    }
    out
}

// ---------------------------------------------------------------------------
// Hurst exponent (rescaled-range R/S method)
// ---------------------------------------------------------------------------

/// Result of a rescaled-range Hurst analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct HurstResult {
    /// Estimated Hurst exponent `H ∈ (0, 1)`.
    /// - `H < 0.5` → anti-persistent / mean-reverting
    /// - `H ≈ 0.5` → random walk
    /// - `H > 0.5` → persistent / trending
    pub hurst: f64,
    /// 95 % confidence interval `(low, high)` on `H`. Derived
    /// from the residuals of the `log R/S` vs `log n` regression.
    pub ci_95: (f64, f64),
    /// `true` when the upper bound of the confidence interval
    /// sits strictly below `0.5` — i.e. we can reject the random-
    /// walk null at the 95 % level in favour of mean-reversion.
    pub is_mean_reverting: bool,
    /// Number of window sizes the estimator actually used after
    /// filtering degenerate windows. Fewer than 3 means the
    /// result is likely unreliable.
    pub window_count: usize,
}

/// Estimate the Hurst exponent of a time series via the
/// rescaled-range (R/S) method of Mandelbrot & Wallis (1969).
///
/// For each window size `n` in a logarithmically-spaced grid,
/// split the series into non-overlapping windows and compute the
/// rescaled range `R/S`:
///
/// ```text
/// y_t = Σ_{k=1..t} (x_k - mean)
/// R   = max(y_t) - min(y_t)
/// S   = stddev(x)
/// ```
///
/// Then a linear regression of `log(R/S)` on `log(n)` gives the
/// slope `H` — the Hurst exponent. Intercept is discarded.
///
/// # Use inside the MM engine
///
/// The existing `AutoTuner::RegimeDetector` classifies regimes
/// from price velocity + realised vol ratios. Hurst is an
/// **orthogonal** statistical measure of persistence — combining
/// both (velocity-based + Hurst-based) gives a stronger signal
/// for switching between the mean-reverting and trending parameter
/// profiles in `autotune.rs`.
///
/// Returns `None` when the series is too short (`< 20` samples)
/// or degenerate (all equal).
pub fn hurst_exponent(series: &[f64]) -> Option<HurstResult> {
    const MIN_SAMPLES: usize = 20;
    let n = series.len();
    if n < MIN_SAMPLES {
        return None;
    }
    let min_window = 10_usize.max(n / 100);
    let max_window = n / 2;
    if max_window <= min_window {
        return None;
    }

    // Logarithmically-spaced window sizes.
    let num_windows = 20;
    let log_min = (min_window as f64).ln();
    let log_max = (max_window as f64).ln();
    let step = (log_max - log_min) / (num_windows as f64 - 1.0);
    let mut window_sizes: Vec<usize> = (0..num_windows)
        .map(|i| (log_min + i as f64 * step).exp() as usize)
        .filter(|&w| w >= min_window && w <= max_window && w > 0)
        .collect();
    window_sizes.sort();
    window_sizes.dedup();

    let mut log_n = Vec::new();
    let mut log_rs = Vec::new();
    for &w in &window_sizes {
        let chunks = n / w;
        if chunks == 0 {
            continue;
        }
        let mut rs_values = Vec::with_capacity(chunks);
        for c in 0..chunks {
            let start = c * w;
            let end = start + w;
            let window = &series[start..end];
            let mean = window.iter().sum::<f64>() / w as f64;

            // Cumulative deviations from the window mean.
            let mut cumdev = 0.0;
            let mut max_y = f64::NEG_INFINITY;
            let mut min_y = f64::INFINITY;
            for &x in window {
                cumdev += x - mean;
                if cumdev > max_y {
                    max_y = cumdev;
                }
                if cumdev < min_y {
                    min_y = cumdev;
                }
            }
            let range = max_y - min_y;
            let variance: f64 = window.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / w as f64;
            let stddev = variance.sqrt();
            if stddev > 1e-10 && range > 0.0 {
                rs_values.push(range / stddev);
            }
        }
        if rs_values.is_empty() {
            continue;
        }
        let avg = rs_values.iter().sum::<f64>() / rs_values.len() as f64;
        if avg > 0.0 {
            log_n.push((w as f64).ln());
            log_rs.push(avg.ln());
        }
    }

    let window_count = log_n.len();
    if window_count < 3 {
        return None;
    }

    let (slope, intercept) = linear_regression_f64(&log_n, &log_rs);

    // 95 % CI on the slope via residual std error.
    let residuals: Vec<f64> = log_n
        .iter()
        .zip(log_rs.iter())
        .map(|(&x, &y)| y - (slope * x + intercept))
        .collect();
    let rss: f64 = residuals.iter().map(|&r| r * r).sum();
    let std_err_resid = if window_count > 2 {
        (rss / (window_count as f64 - 2.0)).sqrt()
    } else {
        0.0
    };
    let x_mean = log_n.iter().sum::<f64>() / window_count as f64;
    let ss_x: f64 = log_n.iter().map(|&x| (x - x_mean).powi(2)).sum();
    let se_slope = if ss_x > 1e-15 {
        std_err_resid / ss_x.sqrt()
    } else {
        0.0
    };
    let t_value = 1.96;
    let ci_low = slope - t_value * se_slope;
    let ci_high = slope + t_value * se_slope;

    Some(HurstResult {
        hurst: slope,
        ci_95: (ci_low, ci_high),
        is_mean_reverting: ci_high < 0.5,
        window_count,
    })
}

/// OLS slope + intercept `y = slope * x + intercept`. Returns
/// `(0, mean_y)` on a degenerate `x` range so the caller does not
/// have to special-case the all-equal input.
fn linear_regression_f64(x: &[f64], y: &[f64]) -> (f64, f64) {
    let n = x.len() as f64;
    let sum_x: f64 = x.iter().sum();
    let sum_y: f64 = y.iter().sum();
    let sum_xx: f64 = x.iter().map(|&xi| xi * xi).sum();
    let sum_xy: f64 = x.iter().zip(y.iter()).map(|(&xi, &yi)| xi * yi).sum();

    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < 1e-15 {
        return (0.0, sum_y / n);
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denom;
    let intercept = (sum_y - slope * sum_x) / n;
    (slope, intercept)
}

// ---------------------------------------------------------------------------
// Best-bid/ask top-of-book imbalance
// ---------------------------------------------------------------------------

/// Normalised best-bid/best-ask imbalance in `[-1, +1]`:
///
/// ```text
/// bba = 2 * (bid_qty / (bid_qty + ask_qty) - 0.5)
///     = (bid_qty - ask_qty) / (bid_qty + ask_qty)
/// ```
///
/// Computed over the **top-of-book only** — a single-level
/// variant of `book_imbalance`. This is a classic fast-
/// microstructure signal: it reacts on every touch update and
/// is the fastest cue for imminent touch pressure when the
/// inner depths are thin. Combine with the existing multi-level
/// `book_imbalance_weighted` for a faster-plus-robust pair.
///
/// Returns `0` when either side is empty or the combined qty is
/// zero.
pub fn bba_imbalance(bids: &[PriceLevel], asks: &[PriceLevel]) -> Decimal {
    let bid_qty = bids.first().map(|l| l.qty).unwrap_or(Decimal::ZERO);
    let ask_qty = asks.first().map(|l| l.qty).unwrap_or(Decimal::ZERO);
    let total = bid_qty + ask_qty;
    if total.is_zero() {
        return Decimal::ZERO;
    }
    (bid_qty - ask_qty) / total
}

// ---------------------------------------------------------------------------
// Log price ratio — cross-venue or basis proxy
// ---------------------------------------------------------------------------

/// Log-price differential between two quotes, scaled to
/// percentage points: `100 × ln(base / follow)`.
///
/// Symmetric interpretation: the sign flips when base and
/// follow swap, and tiny spreads map to tiny numbers. Useful
/// as a cross-venue basis proxy that composes linearly — the
/// sum of log ratios across a chain of venues equals the log
/// ratio of the chain endpoints.
///
/// Returns `None` if either input is non-positive (log of zero
/// or negative is undefined).
///
/// Uses an `f64` round-trip for the natural log because
/// `rust_decimal` has no built-in `ln`. The boundary is clean:
/// callers use this for **feature values** (passed to alpha
/// models / skew calculations), not PnL arithmetic, so the
/// rounding on `Decimal::to_f64` and back is acceptable.
pub fn log_price_ratio(base: Decimal, follow: Decimal) -> Option<Decimal> {
    if base <= Decimal::ZERO || follow <= Decimal::ZERO {
        return None;
    }
    let b = base.to_f64()?;
    let f = follow.to_f64()?;
    let ratio = b / f;
    if !ratio.is_finite() || ratio <= 0.0 {
        return None;
    }
    Decimal::from_f64(ratio.ln() * 100.0)
}

// ---------------------------------------------------------------------------
// Multi-depth order-book imbalance
// ---------------------------------------------------------------------------

/// Order-book imbalance aggregated across **multiple depth
/// horizons** with geometrically decaying weights.
///
/// For each `d` in `depths`, compute `book_imbalance(bids,
/// asks, d)` (the top-d qty imbalance in `[-1, +1]`) and then
/// return the EMA-weighted average of those per-depth values
/// with weights `w(i) = alpha · (1 − alpha)^i`, where `i = 0`
/// is the first (usually shallowest) depth in `depths`. Weights
/// are normalised so they sum to one.
///
/// Why prefer this over the existing `book_imbalance_weighted`
/// (which linearly weights **levels within a single depth**):
///
/// - Linear per-level weighting assumes a single "right" depth
///   horizon. Multi-depth aggregation instead computes the
///   imbalance at each of several horizons (10, 25, 50, 100 bps
///   … or simply top-1, top-5, top-20) and combines them.
///   Robust to liquidity-distribution changes — a sudden fat
///   deep-book order does not distort the short-horizon signal.
/// - The weight vector is a knob the caller tunes separately
///   from the depth grid.
///
/// Returns `0` when `depths` is empty.
pub fn ob_imbalance_multi_depth(
    bids: &[PriceLevel],
    asks: &[PriceLevel],
    depths: &[usize],
    alpha: Decimal,
) -> Decimal {
    if depths.is_empty() {
        return Decimal::ZERO;
    }
    let one_minus_alpha = Decimal::ONE - alpha;

    // Raw weights `w_i = alpha * (1-alpha)^i`. We build them
    // lazily and normalise at the end so a caller passing a
    // non-normalising alpha (e.g. 0.5 on 4 depths) still gets
    // a valid convex combination.
    let mut num = Decimal::ZERO;
    let mut den = Decimal::ZERO;
    let mut w = alpha;
    for &d in depths {
        let imb = book_imbalance(bids, asks, d);
        num += w * imb;
        den += w;
        w *= one_minus_alpha;
    }
    if den.is_zero() {
        return Decimal::ZERO;
    }
    num / den
}

// ---------------------------------------------------------------------------
// Windowed trade-flow snapshot (log-qty-weighted)
// ---------------------------------------------------------------------------

/// Fixed-window snapshot of signed trade flow with log-qty
/// weighting, a companion to the continuous-EWMA [`TradeFlow`].
///
/// Rolling window of the last `window` trades (caller-sized),
/// each contributing `ln(1 + qty) × sign(side)` to a signed
/// sum. The `log(1 + qty)` term dampens the influence of a
/// single outsized print so one whale trade doesn't swamp the
/// signal; the rolling window gives a bounded-memory reaction
/// horizon that the continuous EWMA lacks.
///
/// The normalised output is in `[-1, +1]`:
///
/// ```text
/// snapshot = (Σ log(1 + q_i) · s_i) / Σ log(1 + q_i)
/// ```
///
/// where `s_i ∈ {-1, +1}` is the taker side sign. Positive =
/// net buy pressure over the window, negative = net sell.
///
/// Use alongside `TradeFlow`: the EWMA gives you the slow
/// trend, this gives you the fast snapshot, and the difference
/// between them is itself a flow-acceleration signal.
#[derive(Debug, Clone)]
pub struct WindowedTradeFlow {
    window: usize,
    // (weight, signed_weight) per trade, oldest-first.
    entries: std::collections::VecDeque<(Decimal, Decimal)>,
    total_weight: Decimal,
    total_signed: Decimal,
}

impl WindowedTradeFlow {
    pub fn new(window: usize) -> Self {
        assert!(window > 0, "WindowedTradeFlow: window must be > 0");
        Self {
            window,
            entries: std::collections::VecDeque::with_capacity(window),
            total_weight: Decimal::ZERO,
            total_signed: Decimal::ZERO,
        }
    }

    /// Record a trade. `qty` is the trade quantity (positive);
    /// `side` is the taker side.
    pub fn on_trade(&mut self, qty: Decimal, side: Side) {
        if qty <= Decimal::ZERO {
            return;
        }
        // Weight = log(1 + qty) computed via an f64 round-trip
        // (rust_decimal has no native log). Feature-scale math,
        // Decimal boundary is not money-critical here.
        let Some(q_f) = qty.to_f64() else { return };
        let w_f = (1.0 + q_f).ln();
        if !w_f.is_finite() || w_f <= 0.0 {
            return;
        }
        let Some(weight) = Decimal::from_f64(w_f) else {
            return;
        };
        let sign = match side {
            Side::Buy => Decimal::ONE,
            Side::Sell => -Decimal::ONE,
        };
        let signed = weight * sign;

        self.entries.push_back((weight, signed));
        self.total_weight += weight;
        self.total_signed += signed;

        while self.entries.len() > self.window {
            if let Some((old_w, old_s)) = self.entries.pop_front() {
                self.total_weight -= old_w;
                self.total_signed -= old_s;
            }
        }
    }

    /// Normalised window snapshot in `[-1, +1]`. `None` when
    /// the window has not seen a trade yet.
    pub fn value(&self) -> Option<Decimal> {
        if self.total_weight.is_zero() {
            return None;
        }
        Some(self.total_signed / self.total_weight)
    }

    /// Number of trades currently in the window.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the window is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Hawkes trade flow
// ---------------------------------------------------------------------------

/// Hawkes-intensity-weighted trade flow feature. Wraps
/// `BivariateHawkes` to track self-exciting buy/sell arrival
/// intensities and exposes the intensity imbalance as a
/// feature in `[-1, +1]`.
///
/// Unlike `TradeFlow` (EWMA of signed volume) this captures
/// **clustering** — a burst of 10 buys in 100 ms scores
/// much higher than 10 buys spread over 10 seconds, because
/// the self-excitation kernel amplifies clustered arrivals.
pub struct HawkesTradeFlow {
    hawkes: mm_indicators::BivariateHawkes,
}

impl HawkesTradeFlow {
    /// Construct with parameters for the bivariate Hawkes
    /// process. `alpha_self + alpha_cross < beta` is required
    /// for stationarity.
    pub fn new(mu: Decimal, alpha_self: Decimal, alpha_cross: Decimal, beta: Decimal) -> Self {
        Self {
            hawkes: mm_indicators::BivariateHawkes::new(mu, alpha_self, alpha_cross, beta),
        }
    }

    /// Default parameters tuned for crypto MM: μ=1, α_self=0.5,
    /// α_cross=0.2, β=2.0 (half-life ~0.35s).
    pub fn default_crypto() -> Self {
        Self::new(dec!(1), dec!(0.5), dec!(0.2), dec!(2))
    }

    /// Register a trade. `t_secs` is the trade timestamp in
    /// seconds (monotonic, e.g. from `Instant`).
    pub fn on_trade(&mut self, side: Side, t_secs: Decimal) {
        match side {
            Side::Buy => {
                self.hawkes.on_buy(t_secs);
            }
            Side::Sell => {
                self.hawkes.on_sell(t_secs);
            }
        }
    }

    /// Intensity imbalance at time `t_secs` in `[-1, +1]`.
    /// Positive = buy pressure dominates. `None` before any
    /// trade has been registered.
    pub fn value(&self, t_secs: Decimal) -> Option<Decimal> {
        if self.hawkes.event_count() == 0 {
            return None;
        }
        Some(self.hawkes.intensity_imbalance_at(t_secs))
    }

    /// Total events observed.
    pub fn event_count(&self) -> u64 {
        self.hawkes.event_count()
    }

    /// Reset all state.
    pub fn reset(&mut self) {
        self.hawkes.reset();
    }
}

// ---------------------------------------------------------------------------
// Micro-price drift
// ---------------------------------------------------------------------------

/// EWMA of the first difference of the micro-price. Gives an
/// instantaneous directional pressure estimate.
#[derive(Debug, Clone)]
pub struct MicroPriceDrift {
    alpha: Decimal,
    last_mp: Option<Decimal>,
    state: Option<Decimal>,
}

impl MicroPriceDrift {
    pub fn new(half_life_ticks: usize) -> Self {
        assert!(half_life_ticks > 0);
        let n = half_life_ticks as f64;
        let alpha_f = 1.0 - 0.5f64.powf(1.0 / n);
        Self {
            alpha: Decimal::from_f64(alpha_f).unwrap_or(dec!(0.1)),
            last_mp: None,
            state: None,
        }
    }

    pub fn update(&mut self, bids: &[PriceLevel], asks: &[PriceLevel]) {
        let Some(mp) = micro_price(bids, asks) else {
            return;
        };
        let Some(prev) = self.last_mp else {
            self.last_mp = Some(mp);
            return;
        };
        let delta = mp - prev;
        self.state = Some(match self.state {
            None => delta,
            Some(s) => self.alpha * delta + (Decimal::ONE - self.alpha) * s,
        });
        self.last_mp = Some(mp);
    }

    pub fn value(&self) -> Option<Decimal> {
        self.state
    }
}

// ---------------------------------------------------------------------------
// Realised volatility term structure
// ---------------------------------------------------------------------------

/// Two parallel EWMA volatility estimators at different half-lives.
/// The ratio `short / long` gives a unitless short-term vol regime
/// indicator: `> 1` means short-term vol is running hot relative to
/// long-term baseline.
#[derive(Debug, Clone)]
pub struct VolTermStructure {
    short: VolEwma,
    long: VolEwma,
}

impl VolTermStructure {
    pub fn new(short_half_life: usize, long_half_life: usize) -> Self {
        Self {
            short: VolEwma::new(short_half_life),
            long: VolEwma::new(long_half_life),
        }
    }

    pub fn update(&mut self, price: Decimal) {
        self.short.update(price);
        self.long.update(price);
    }

    /// Short half-life vol.
    pub fn short(&self) -> Option<Decimal> {
        self.short.value()
    }

    /// Long half-life vol.
    pub fn long(&self) -> Option<Decimal> {
        self.long.value()
    }

    /// Ratio `short / long`. `None` if either is unavailable or
    /// long is zero.
    pub fn ratio(&self) -> Option<Decimal> {
        let s = self.short.value()?;
        let l = self.long.value()?;
        if l.is_zero() {
            return None;
        }
        Some(s / l)
    }
}

#[derive(Debug, Clone)]
struct VolEwma {
    alpha: Decimal,
    last_price: Option<Decimal>,
    var: Option<Decimal>,
}

impl VolEwma {
    fn new(half_life: usize) -> Self {
        assert!(half_life > 0);
        let n = half_life as f64;
        let alpha_f = 1.0 - 0.5f64.powf(1.0 / n);
        Self {
            alpha: Decimal::from_f64(alpha_f).unwrap_or(dec!(0.1)),
            last_price: None,
            var: None,
        }
    }

    fn update(&mut self, price: Decimal) {
        let Some(prev) = self.last_price else {
            self.last_price = Some(price);
            return;
        };
        let ret = if prev.is_zero() {
            Decimal::ZERO
        } else {
            (price - prev) / prev
        };
        let sq = ret * ret;
        self.var = Some(match self.var {
            None => sq,
            Some(prev_var) => self.alpha * sq + (Decimal::ONE - self.alpha) * prev_var,
        });
        self.last_price = Some(price);
    }

    fn value(&self) -> Option<Decimal> {
        let v = self.var?;
        // sqrt via f64 — we've already conceded precision for the
        // variance input anyway.
        v.to_f64().map(|v| v.sqrt()).and_then(Decimal::from_f64)
    }
}

// ---------------------------------------------------------------------------
// Bundled feature vector
// ---------------------------------------------------------------------------

/// One-shot feature vector snapshot. Consumers that want all the
/// standard microstructure features in one place feed this struct
/// to their predictor.
#[derive(Debug, Clone, Default)]
pub struct FeatureVector {
    pub imbalance_top5: Decimal,
    pub imbalance_weighted_top10: Decimal,
    pub micro_price: Option<Decimal>,
    pub micro_price_drift: Option<Decimal>,
    pub trade_flow: Option<Decimal>,
    pub vol_short: Option<Decimal>,
    pub vol_long: Option<Decimal>,
    pub vol_ratio: Option<Decimal>,
}

// ---------------------------------------------------------------------------
// Immediacy-weighted depth (rank-churn invariant)
// ---------------------------------------------------------------------------

/// Immediacy-weighted depth on the bid side:
///
/// `D_bid = Σ qty_i · 1 / (1 + d_i)²`, where
/// `d_i = (best_bid - price_i) / spread` is the distance from the
/// best bid in **spread units**.
///
/// Inner levels dominate the sum and outer levels fall off
/// quadratically. Unlike a plain top-k qty sum, this metric is
/// **invariant to rank churn**: when an inner level disappears
/// and an outer level bubbles up, the top-k sum stays flat but
/// immediacy-weighted depth drops because the surviving mass
/// sits farther from the touch.
///
/// Used by the Market Resilience detector to track depth
/// depletion and recovery. Returns `0` if the bid side is
/// empty or `spread_basis` is non-positive.
pub fn immediacy_depth_bid(bids: &[PriceLevel], spread_basis: Decimal) -> Decimal {
    if bids.is_empty() || spread_basis <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let best = bids[0].price;
    let mut acc = Decimal::ZERO;
    for level in bids {
        let raw = best - level.price;
        let d = if raw < Decimal::ZERO {
            Decimal::ZERO
        } else {
            raw / spread_basis
        };
        let x = Decimal::ONE + d;
        let w = Decimal::ONE / (x * x);
        acc += level.qty * w;
    }
    acc
}

/// Immediacy-weighted depth on the ask side. Symmetric to
/// [`immediacy_depth_bid`] — see that function for the formula
/// and rationale.
pub fn immediacy_depth_ask(asks: &[PriceLevel], spread_basis: Decimal) -> Decimal {
    if asks.is_empty() || spread_basis <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let best = asks[0].price;
    let mut acc = Decimal::ZERO;
    for level in asks {
        let raw = level.price - best;
        let d = if raw < Decimal::ZERO {
            Decimal::ZERO
        } else {
            raw / spread_basis
        };
        let x = Decimal::ONE + d;
        let w = Decimal::ONE / (x * x);
        acc += level.qty * w;
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(price: Decimal, qty: Decimal) -> PriceLevel {
        PriceLevel { price, qty }
    }

    #[test]
    fn imbalance_positive_when_bid_heavier() {
        let bids = vec![bid(dec!(100), dec!(10)), bid(dec!(99), dec!(5))];
        let asks = vec![bid(dec!(101), dec!(2)), bid(dec!(102), dec!(3))];
        let ib = book_imbalance(&bids, &asks, 2);
        // (15 - 5) / 20 = 0.5
        assert_eq!(ib, dec!(0.5));
    }

    #[test]
    fn imbalance_zero_on_balanced_book() {
        let bids = vec![bid(dec!(100), dec!(5))];
        let asks = vec![bid(dec!(101), dec!(5))];
        assert_eq!(book_imbalance(&bids, &asks, 5), Decimal::ZERO);
    }

    #[test]
    fn imbalance_empty_book_is_zero() {
        assert_eq!(book_imbalance(&[], &[], 5), Decimal::ZERO);
    }

    #[test]
    fn weighted_imbalance_gives_more_weight_to_top() {
        // Bid side tiny at top but huge deeper; ask side flat.
        // Weighted version should still lean slightly ask-heavy.
        let bids = vec![bid(dec!(100), dec!(1)), bid(dec!(99), dec!(100))];
        let asks = vec![bid(dec!(101), dec!(5)), bid(dec!(102), dec!(5))];
        let flat = book_imbalance(&bids, &asks, 2);
        let w = book_imbalance_weighted(&bids, &asks, 2);
        // Flat imbalance is strongly bid (tons of volume deeper).
        assert!(flat > dec!(0.5));
        // Weighted is less extreme because deep level loses weight.
        assert!(w < flat);
    }

    #[test]
    fn micro_price_between_bid_and_ask() {
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(101), dec!(10))];
        let mp = micro_price(&bids, &asks).unwrap();
        assert!(mp > dec!(100) && mp < dec!(101));
    }

    #[test]
    fn micro_price_anchors_to_heavier_side() {
        // Formula: (bid_qty * ask_px + ask_qty * bid_px) / total.
        // Heavy ask means the `ask_qty * bid_px` term dominates, so
        // the micro-price sits near the bid price — the next trade
        // is most likely to sweep the thin bid before the wall of
        // asks is consumed.
        let bids = vec![bid(dec!(100), dec!(1))]; // thin bid
        let asks = vec![bid(dec!(101), dec!(100))]; // heavy ask wall
        let mp = micro_price(&bids, &asks).unwrap();
        assert!(mp < dec!(100.5), "expected mp near bid, got {mp}");
    }

    #[test]
    fn trade_flow_positive_on_net_buying() {
        let mut tf = TradeFlow::new(10);
        for _ in 0..20 {
            tf.update(Side::Buy, dec!(1));
        }
        assert!(tf.value().unwrap() > Decimal::ZERO);
    }

    #[test]
    fn trade_flow_negative_on_net_selling() {
        let mut tf = TradeFlow::new(10);
        for _ in 0..20 {
            tf.update(Side::Sell, dec!(1));
        }
        assert!(tf.value().unwrap() < Decimal::ZERO);
    }

    #[test]
    fn micro_price_drift_detects_upward_trend() {
        let mut mpd = MicroPriceDrift::new(5);
        for i in 0..20 {
            let p = dec!(100) + Decimal::from(i);
            let bids = vec![bid(p, dec!(10))];
            let asks = vec![bid(p + dec!(1), dec!(10))];
            mpd.update(&bids, &asks);
        }
        let d = mpd.value().unwrap();
        assert!(d > Decimal::ZERO);
    }

    #[test]
    fn vol_term_structure_ratio_rises_with_short_term_burst() {
        let mut vts = VolTermStructure::new(3, 30);
        // Long stable regime.
        for _ in 0..50 {
            vts.update(dec!(100));
        }
        let quiet_ratio = vts.ratio();
        // Short spike.
        for i in 0..10 {
            let p = if i % 2 == 0 { dec!(100) } else { dec!(105) };
            vts.update(p);
        }
        let spike_ratio = vts.ratio().unwrap();
        // Quiet period ratio may be None (all zeros) or near zero.
        // Spike ratio should be materially positive.
        assert!(spike_ratio > Decimal::ZERO);
        if let Some(q) = quiet_ratio {
            assert!(spike_ratio > q);
        }
    }

    /// Canonical sign convention pinned: all-bid → +1, all-ask →
    /// −1, symmetric → 0. Matches Cont, Stoikov, Talreja (2010)
    /// "A stochastic model for order book dynamics" and the Hasbrouck
    /// order-flow imbalance definition `(bid − ask)/(bid + ask)`.
    #[test]
    fn imbalance_canonical_extremes_and_sign() {
        let b = vec![bid(dec!(100), dec!(10))];
        let empty: Vec<PriceLevel> = Vec::new();
        assert_eq!(book_imbalance(&b, &empty, 5), dec!(1));
        assert_eq!(book_imbalance(&empty, &b, 5), dec!(-1));
        assert_eq!(book_imbalance(&b, &[bid(dec!(101), dec!(10))], 5), dec!(0));
    }

    /// Linear-decay weighting with `weight = k - i`. Pinned hand-
    /// computed example for k = 3:
    ///
    ///   bids qtys [10, 10, 10] → weighted 3*10 + 2*10 + 1*10 = 60
    ///   asks qtys [1,   1,  1] → weighted 3 + 2 + 1 = 6
    ///   imbalance = (60 - 6) / 66 = 54/66 = 9/11 ≈ 0.8181…
    ///
    /// The inner level dominates the outer levels, as promised by
    /// the weighting.
    #[test]
    fn weighted_imbalance_hand_computed() {
        let bids = vec![
            bid(dec!(100), dec!(10)),
            bid(dec!(99), dec!(10)),
            bid(dec!(98), dec!(10)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(1)),
            bid(dec!(102), dec!(1)),
            bid(dec!(103), dec!(1)),
        ];
        let w = book_imbalance_weighted(&bids, &asks, 3);
        // 54/66 = 9/11
        let expected = dec!(9) / dec!(11);
        let diff = (w - expected).abs();
        assert!(
            diff < dec!(0.0000001),
            "expected 9/11, got {w} (|diff| = {diff})"
        );
    }

    /// Pinned micro-price example from Cartea, Jaimungal & Penalva
    /// (2015), *Algorithmic and High-Frequency Trading*, §"Order-flow
    /// imbalance and micro-price":
    ///
    ///   P_micro = (Q_a × P_b + Q_b × P_a) / (Q_a + Q_b)
    ///
    /// where `Q_a` is the ASK size and `P_b` is the BID price. With
    /// bid = (100, 10) and ask = (101, 30):
    ///
    ///   P_micro = (30 × 100 + 10 × 101) / (10 + 30)
    ///           = (3000 + 1010) / 40
    ///           = 4010 / 40
    ///           = 100.25
    ///
    /// Heavier ask side pulls the micro-price toward the bid, as
    /// expected.
    #[test]
    fn micro_price_canonical_hand_computed_value() {
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(101), dec!(30))];
        let mp = micro_price(&bids, &asks).unwrap();
        assert_eq!(mp, dec!(100.25));
    }

    /// EWMA half-life formula: `α = 1 - 2^(-1/half_life)` gives the
    /// weight such that `(1-α)^half_life = 0.5`. Standard RiskMetrics
    /// convention. After enough steps of a monotone-delta sequence,
    /// the state should converge toward the common delta.
    #[test]
    fn micro_price_drift_converges_to_constant_delta() {
        let mut mpd = MicroPriceDrift::new(3);
        // Feed a perfectly linear micro-price sequence. Each
        // micro-price is the midpoint because both sides are equal,
        // so the delta is exactly 1 per step.
        for i in 0..50 {
            let mid = dec!(100) + Decimal::from(i);
            let bids = vec![bid(mid - dec!(0.5), dec!(5))];
            let asks = vec![bid(mid + dec!(0.5), dec!(5))];
            mpd.update(&bids, &asks);
        }
        let state = mpd.value().unwrap();
        // After 50 steps of constant delta=1, the EWMA should have
        // converged to within a tiny fraction of 1.
        let diff = (state - dec!(1)).abs();
        assert!(diff < dec!(0.001), "expected EWMA near 1, got {state}");
    }

    #[test]
    fn vol_term_structure_both_legs_populate_after_enough_ticks() {
        let mut vts = VolTermStructure::new(3, 10);
        for i in 0..20 {
            vts.update(dec!(100) + Decimal::from(i));
        }
        assert!(vts.short().is_some());
        assert!(vts.long().is_some());
    }

    // ---- micro_price_weighted ----

    #[test]
    fn weighted_micro_price_single_level_matches_plain_micro_price() {
        // `depth = 1` must reduce to the classic top-of-book
        // microprice (Cartea/Jaimungal formula).
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(101), dec!(30))];
        let plain = micro_price(&bids, &asks).unwrap();
        let weighted = micro_price_weighted(&bids, &asks, 1).unwrap();
        assert_eq!(plain, dec!(100.25));
        assert_eq!(weighted, dec!(100.25));
    }

    #[test]
    fn weighted_micro_price_returns_none_on_empty_side() {
        let b: Vec<PriceLevel> = Vec::new();
        let a = vec![bid(dec!(101), dec!(1))];
        assert!(micro_price_weighted(&b, &a, 3).is_none());
        assert!(micro_price_weighted(&a, &b, 3).is_none());
        assert!(micro_price_weighted(&[], &[], 3).is_none());
    }

    #[test]
    fn weighted_micro_price_clamps_depth_to_available_levels() {
        // Only one level on each side but caller asks for depth=5.
        let bids = vec![bid(dec!(100), dec!(10))];
        let asks = vec![bid(dec!(101), dec!(10))];
        let mp = micro_price_weighted(&bids, &asks, 5).unwrap();
        // Equal qtys → midpoint.
        assert_eq!(mp, dec!(100.5));
    }

    /// Pinned hand-computed 3-level example. Per level the inner
    /// microprice formula yields:
    ///
    ///   lvl 0: bid=100 q=10, ask=101 q=10 → mp=100.5,  total_qty=20
    ///   lvl 1: bid=99  q=20, ask=102 q=20 → mp=100.5,  total_qty=40
    ///   lvl 2: bid=98  q=30, ask=103 q=30 → mp=100.5,  total_qty=60
    ///
    /// With weights `w(i) = 3 - i` → `[3, 2, 1]`:
    ///
    ///   numerator   = (100.5*20)*3 + (100.5*40)*2 + (100.5*60)*1
    ///               = 6030 + 8040 + 6030 = 20 100
    ///   denominator = 20*3 + 40*2 + 60*1 = 60 + 80 + 60 = 200
    ///   weighted_mp = 20 100 / 200 = 100.5
    ///
    /// All three levels are symmetric so the weighted value is
    /// exactly the midpoint.
    #[test]
    fn weighted_micro_price_symmetric_book_equals_midpoint() {
        let bids = vec![
            bid(dec!(100), dec!(10)),
            bid(dec!(99), dec!(20)),
            bid(dec!(98), dec!(30)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(10)),
            bid(dec!(102), dec!(20)),
            bid(dec!(103), dec!(30)),
        ];
        let mp = micro_price_weighted(&bids, &asks, 3).unwrap();
        assert_eq!(mp, dec!(100.5));
    }

    /// Asymmetric book — heavy asks on the inside, light bids.
    /// Weighted microprice should be pulled toward the bid side
    /// (fewer contrarian orders on that side) relative to the
    /// plain midpoint `100.5`.
    #[test]
    fn weighted_micro_price_heavy_ask_side_leans_toward_bid() {
        let bids = vec![
            bid(dec!(100), dec!(1)),
            bid(dec!(99), dec!(1)),
            bid(dec!(98), dec!(1)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(9)),
            bid(dec!(102), dec!(9)),
            bid(dec!(103), dec!(9)),
        ];
        let mp = micro_price_weighted(&bids, &asks, 3).unwrap();
        assert!(
            mp < dec!(100.5),
            "heavy ask → fair value should lean below midpoint, got {mp}"
        );
    }

    #[test]
    fn weighted_micro_price_skips_levels_with_zero_total_qty() {
        // Level 1 is a degenerate entry with zero on both
        // sides — the weighted average must skip it cleanly.
        let bids = vec![
            bid(dec!(100), dec!(10)),
            bid(dec!(99), dec!(0)),
            bid(dec!(98), dec!(10)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(10)),
            bid(dec!(102), dec!(0)),
            bid(dec!(103), dec!(10)),
        ];
        let mp = micro_price_weighted(&bids, &asks, 3).unwrap();
        // Levels 0 and 2 only; both symmetric → midpoint.
        assert_eq!(mp, dec!(100.5));
    }

    // ---- market_impact ----

    #[test]
    fn market_impact_returns_none_on_empty_side() {
        assert!(market_impact(&[], Side::Buy, dec!(1), dec!(100)).is_none());
    }

    #[test]
    fn market_impact_returns_none_on_zero_target_qty() {
        let asks = vec![bid(dec!(100), dec!(10))];
        assert!(market_impact(&asks, Side::Buy, dec!(0), dec!(100)).is_none());
    }

    /// Buy walk against a two-level ask book. Target qty = 5 lies
    /// entirely inside level 0, so VWAP = level-0 price and the
    /// impact vs reference = ((100 - 99.5) / 99.5) * 10000 ≈
    /// 50.25 bps.
    #[test]
    fn market_impact_buy_fills_inside_first_level_reports_touch_slippage() {
        let asks = vec![bid(dec!(100), dec!(10)), bid(dec!(101), dec!(10))];
        let out = market_impact(&asks, Side::Buy, dec!(5), dec!(99.5)).unwrap();
        assert_eq!(out.vwap, dec!(100));
        assert_eq!(out.filled_qty, dec!(5));
        assert_eq!(out.notional, dec!(500));
        assert!(!out.partial);
        // Signed: buy walks up from 99.5 to 100 → positive impact.
        let expected_bps = dec!(0.5) / dec!(99.5) * dec!(10_000);
        let diff = (out.impact_bps - expected_bps).abs();
        assert!(diff < dec!(0.000001), "bps drift {diff}");
    }

    /// Buy walk that consumes level 0 entirely (5 @ 100) and
    /// spills into level 1 (2 @ 101). VWAP = (500 + 202)/7 =
    /// 702/7 ≈ 100.2857...
    #[test]
    fn market_impact_buy_walks_into_second_level() {
        let asks = vec![bid(dec!(100), dec!(5)), bid(dec!(101), dec!(10))];
        let out = market_impact(&asks, Side::Buy, dec!(7), dec!(100)).unwrap();
        assert_eq!(out.filled_qty, dec!(7));
        assert_eq!(out.notional, dec!(702));
        // 702/7 = 100.2857142857142857142857142... —
        // rust_decimal yields a high-precision result; compare
        // to the closed-form.
        let expected_vwap = dec!(702) / dec!(7);
        assert_eq!(out.vwap, expected_vwap);
        assert!(!out.partial);
        // Reference = touch (100) → impact is purely the
        // spillover cost.
        assert!(out.impact_bps > dec!(0));
    }

    #[test]
    fn market_impact_sell_side_flips_sign_convention() {
        // Walking a bid book with a sell. Best bid is 99, taker
        // receives VWAP that is the same or worse. Positive
        // impact_bps still means "unfavourable to the taker".
        let bids = vec![bid(dec!(99), dec!(5)), bid(dec!(98), dec!(10))];
        let out = market_impact(&bids, Side::Sell, dec!(7), dec!(100)).unwrap();
        assert_eq!(out.filled_qty, dec!(7));
        // 99 * 5 + 98 * 2 = 495 + 196 = 691.
        assert_eq!(out.notional, dec!(691));
        // Reference = 100, VWAP < 100 → sell is leaving money
        // on the table → impact > 0.
        assert!(out.impact_bps > dec!(0));
    }

    #[test]
    fn market_impact_partial_fill_flag_fires_when_book_is_thin() {
        let asks = vec![bid(dec!(100), dec!(3))];
        let out = market_impact(&asks, Side::Buy, dec!(10), dec!(100)).unwrap();
        assert_eq!(out.filled_qty, dec!(3));
        assert!(out.partial, "book had only 3 qty, partial flag must fire");
    }

    #[test]
    fn market_impact_impact_bps_is_zero_at_reference_equal_to_vwap() {
        let asks = vec![bid(dec!(100), dec!(10))];
        let out = market_impact(&asks, Side::Buy, dec!(1), dec!(100)).unwrap();
        assert_eq!(out.vwap, dec!(100));
        assert_eq!(out.impact_bps, dec!(0));
    }

    // ---- lead_lag_transform ----

    #[test]
    fn lead_lag_empty_input_returns_empty_output() {
        assert!(lead_lag_transform(&[]).is_empty());
    }

    #[test]
    fn lead_lag_single_point_returns_one_pair() {
        let out = lead_lag_transform(&[dec!(42)]);
        assert_eq!(out, vec![(dec!(42), dec!(42))]);
    }

    /// Canonical example from Gyurkó et al. — the lead-lag
    /// transform of `[p0, p1, p2]` is `[(p0,p0), (p1,p0), (p1,p1),
    /// (p2,p1), (p2,p2)]`. Length is `2*n - 1` for `n > 0`.
    #[test]
    fn lead_lag_three_point_canonical_shape() {
        let prices = vec![dec!(100), dec!(101), dec!(99)];
        let out = lead_lag_transform(&prices);
        assert_eq!(
            out,
            vec![
                (dec!(100), dec!(100)),
                (dec!(101), dec!(100)),
                (dec!(101), dec!(101)),
                (dec!(99), dec!(101)),
                (dec!(99), dec!(99)),
            ]
        );
        assert_eq!(out.len(), 2 * prices.len() - 1);
    }

    // ---- hurst_exponent ----

    #[test]
    fn hurst_returns_none_on_too_short_series() {
        let series: Vec<f64> = (0..10).map(|i| i as f64).collect();
        assert!(hurst_exponent(&series).is_none());
    }

    #[test]
    fn hurst_returns_none_on_constant_series() {
        // All samples equal → std is zero for every window, no
        // R/S values survive the filter → not enough points to
        // regress.
        let series = vec![100.0_f64; 200];
        assert!(hurst_exponent(&series).is_none());
    }

    /// On iid white noise (returns of a random walk) the
    /// theoretical Hurst exponent is `0.5`. The R/S method is
    /// applied to the returns series — applying it directly to
    /// the cumulative price levels would yield `H ≈ 1` instead,
    /// because the levels themselves are non-stationary and
    /// behave like a trend under R/S.
    ///
    /// With 2000 samples of Bernoulli ±1 the R/S estimator
    /// should land comfortably within `[0.35, 0.7]` — tighter
    /// bounds depend on seed luck.
    #[test]
    fn hurst_white_noise_returns_are_close_to_half() {
        // Deterministic ±1 iid stream via xorshift64 + popcount
        // parity. Avoids the high-bit bias of a naive LCG and
        // sidesteps pulling in a PRNG crate for a test.
        let n = 2000;
        let mut returns = Vec::with_capacity(n);
        let mut state: u64 = 0x1234_5678_9abc_def0;
        for _ in 0..n {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let parity = state.count_ones() & 1;
            returns.push(if parity == 0 { -1.0_f64 } else { 1.0 });
        }
        let r = hurst_exponent(&returns).expect("hurst on 2000 white-noise samples");
        assert!(
            (0.35..=0.7).contains(&r.hurst),
            "Hurst of iid white noise should be near 0.5, got {}",
            r.hurst
        );
        assert!(r.window_count >= 3);
    }

    /// On a strongly trending series (monotonic increments) the
    /// Hurst exponent is close to 1. This test uses a pure linear
    /// trend as the extreme case — R/S must classify it as
    /// persistent.
    #[test]
    fn hurst_monotonic_trend_is_close_to_one() {
        let series: Vec<f64> = (0..500).map(|i| i as f64).collect();
        let r = hurst_exponent(&series).expect("hurst on trend");
        assert!(
            r.hurst > 0.8,
            "Hurst of a linear trend should be near 1.0, got {}",
            r.hurst
        );
        assert!(!r.is_mean_reverting);
    }

    // ---- bba_imbalance ----

    #[test]
    fn bba_imbalance_empty_sides_return_zero() {
        let b: Vec<PriceLevel> = Vec::new();
        let a: Vec<PriceLevel> = Vec::new();
        assert_eq!(bba_imbalance(&b, &a), dec!(0));
        assert_eq!(bba_imbalance(&[bid(dec!(100), dec!(5))], &a), dec!(1));
        assert_eq!(bba_imbalance(&b, &[bid(dec!(100), dec!(5))]), dec!(-1));
    }

    #[test]
    fn bba_imbalance_symmetric_book_is_zero() {
        let b = vec![bid(dec!(100), dec!(5))];
        let a = vec![bid(dec!(101), dec!(5))];
        assert_eq!(bba_imbalance(&b, &a), dec!(0));
    }

    #[test]
    fn bba_imbalance_heavy_bid_side_is_positive() {
        let b = vec![bid(dec!(100), dec!(9))];
        let a = vec![bid(dec!(101), dec!(1))];
        // (9 - 1) / (9 + 1) = 0.8
        assert_eq!(bba_imbalance(&b, &a), dec!(0.8));
    }

    #[test]
    fn bba_imbalance_ignores_deeper_levels() {
        // Deep-level qty must not affect the top-of-book reading.
        let b = vec![bid(dec!(100), dec!(1)), bid(dec!(99), dec!(100))];
        let a = vec![bid(dec!(101), dec!(1))];
        assert_eq!(bba_imbalance(&b, &a), dec!(0));
    }

    // ---- log_price_ratio ----

    #[test]
    fn log_price_ratio_equal_prices_is_zero() {
        let r = log_price_ratio(dec!(100), dec!(100)).unwrap();
        assert!(r.abs() < dec!(0.0001));
    }

    #[test]
    fn log_price_ratio_sign_flips_on_argument_swap() {
        let a = log_price_ratio(dec!(100), dec!(99)).unwrap();
        let b = log_price_ratio(dec!(99), dec!(100)).unwrap();
        // a ≈ -b (up to floating rounding).
        let sum = a + b;
        assert!(sum.abs() < dec!(0.0001));
    }

    #[test]
    fn log_price_ratio_rejects_non_positive_inputs() {
        assert!(log_price_ratio(dec!(0), dec!(100)).is_none());
        assert!(log_price_ratio(dec!(100), dec!(0)).is_none());
        assert!(log_price_ratio(dec!(-1), dec!(100)).is_none());
    }

    /// Pinned hand-computed value: `100 × ln(1.01) ≈ 0.9950`.
    /// Using the standard Taylor expansion
    /// `ln(1+x) ≈ x − x²/2 + x³/3 − …` with `x = 0.01` yields
    /// `0.00995 033…`, so multiplied by 100 ≈ `0.99503`. The
    /// `f64::ln` implementation matches to ~16 significant digits;
    /// we allow a 1e-3 tolerance because `Decimal::from_f64`
    /// rounds to a finite representation.
    #[test]
    fn log_price_ratio_matches_textbook_value() {
        let r = log_price_ratio(dec!(101), dec!(100)).unwrap();
        let diff = (r - dec!(0.99503)).abs();
        assert!(diff < dec!(0.001), "expected ≈ 0.995, got {r}");
    }

    // ---- ob_imbalance_multi_depth ----

    #[test]
    fn ob_imbalance_multi_depth_empty_depths_is_zero() {
        let b = vec![bid(dec!(100), dec!(10))];
        let a = vec![bid(dec!(101), dec!(10))];
        assert_eq!(ob_imbalance_multi_depth(&b, &a, &[], dec!(0.3)), dec!(0));
    }

    #[test]
    fn ob_imbalance_multi_depth_single_depth_matches_book_imbalance() {
        // With a single depth the multi-depth wrapper must
        // reduce to plain `book_imbalance(bids, asks, d)` —
        // up to the ULP of Decimal divisions (the wrapper
        // divides twice via the weighted accumulator).
        let bids = vec![bid(dec!(100), dec!(10)), bid(dec!(99), dec!(20))];
        let asks = vec![bid(dec!(101), dec!(10)), bid(dec!(102), dec!(5))];
        let direct = book_imbalance(&bids, &asks, 2);
        let multi = ob_imbalance_multi_depth(&bids, &asks, &[2], dec!(0.5));
        let diff = (direct - multi).abs();
        assert!(
            diff < dec!(0.0000000001),
            "expected direct ≈ multi, diff = {diff}"
        );
    }

    #[test]
    fn ob_imbalance_multi_depth_gives_more_weight_to_first_depth() {
        // Depth 1: bids have 10, asks have 1 → imbalance near +1.
        // Depth 5: overall balanced → imbalance near 0.
        // With alpha close to 1 the wrapper should favour the
        // first (shallow) depth → positive answer.
        let bids = vec![
            bid(dec!(100), dec!(10)),
            bid(dec!(99), dec!(1)),
            bid(dec!(98), dec!(1)),
            bid(dec!(97), dec!(1)),
            bid(dec!(96), dec!(1)),
        ];
        let asks = vec![
            bid(dec!(101), dec!(1)),
            bid(dec!(102), dec!(5)),
            bid(dec!(103), dec!(5)),
            bid(dec!(104), dec!(5)),
            bid(dec!(105), dec!(5)),
        ];
        let v = ob_imbalance_multi_depth(&bids, &asks, &[1, 5], dec!(0.9));
        // Top-1 is strongly positive (9/11), depth-5 is slightly
        // negative; weighted heavily on top-1 → net positive.
        assert!(v > dec!(0));
    }

    // ---- WindowedTradeFlow ----

    #[test]
    fn windowed_trade_flow_empty_returns_none() {
        let w = WindowedTradeFlow::new(10);
        assert!(w.is_empty());
        assert!(w.value().is_none());
    }

    #[test]
    fn windowed_trade_flow_pure_buy_stream_is_positive_one() {
        let mut w = WindowedTradeFlow::new(10);
        for _ in 0..5 {
            w.on_trade(dec!(1), Side::Buy);
        }
        assert_eq!(w.len(), 5);
        assert_eq!(w.value().unwrap(), dec!(1));
    }

    #[test]
    fn windowed_trade_flow_pure_sell_stream_is_negative_one() {
        let mut w = WindowedTradeFlow::new(10);
        for _ in 0..5 {
            w.on_trade(dec!(1), Side::Sell);
        }
        assert_eq!(w.value().unwrap(), dec!(-1));
    }

    #[test]
    fn windowed_trade_flow_log_weight_damps_whale_trade() {
        // A single whale sell of qty 1000 vs five normal buys of
        // qty 1 should tilt the signal toward sell but not
        // completely. With a plain signed-qty weighting the
        // whale would dominate the signal entirely.
        let mut w = WindowedTradeFlow::new(10);
        for _ in 0..5 {
            w.on_trade(dec!(1), Side::Buy);
        }
        w.on_trade(dec!(1000), Side::Sell);
        let v = w.value().unwrap();
        // Log weights: log(2)×5 ≈ 3.466 on the buy side vs
        // log(1001) ≈ 6.909 on the sell side → net negative but
        // bounded away from -1.
        assert!(v < dec!(0));
        assert!(v > dec!(-0.9));
    }

    #[test]
    fn windowed_trade_flow_drops_oldest_when_window_full() {
        let mut w = WindowedTradeFlow::new(3);
        w.on_trade(dec!(1), Side::Buy);
        w.on_trade(dec!(1), Side::Buy);
        w.on_trade(dec!(1), Side::Buy);
        // Now overflow with a sell — the oldest buy rotates out.
        w.on_trade(dec!(1), Side::Sell);
        assert_eq!(w.len(), 3);
        // Two buys + one sell in the window → positive but
        // not saturated.
        let v = w.value().unwrap();
        assert!(v > dec!(0));
        assert!(v < dec!(1));
    }

    #[test]
    fn windowed_trade_flow_ignores_non_positive_qty() {
        let mut w = WindowedTradeFlow::new(10);
        w.on_trade(dec!(0), Side::Buy);
        w.on_trade(dec!(-5), Side::Sell);
        assert!(w.is_empty());
        assert!(w.value().is_none());
    }

    // ----- immediacy-weighted depth tests -----

    #[test]
    fn immediacy_depth_empty_side_is_zero() {
        let empty: Vec<PriceLevel> = vec![];
        assert_eq!(immediacy_depth_bid(&empty, dec!(1)), Decimal::ZERO);
        assert_eq!(immediacy_depth_ask(&empty, dec!(1)), Decimal::ZERO);
    }

    #[test]
    fn immediacy_depth_non_positive_spread_basis_is_zero() {
        let bids = vec![bid(dec!(100), dec!(5))];
        assert_eq!(immediacy_depth_bid(&bids, Decimal::ZERO), Decimal::ZERO);
        assert_eq!(immediacy_depth_bid(&bids, dec!(-1)), Decimal::ZERO);
    }

    /// Rank-churn invariance: the metric must DROP when an
    /// inner level is removed and an outer level of equal size
    /// is revealed. A plain top-k sum would stay flat.
    #[test]
    fn immediacy_depth_penalises_rank_churn() {
        let spread = dec!(1);
        // Before: touch at 100, next level at 99.
        let before = vec![bid(dec!(100), dec!(10)), bid(dec!(98), dec!(10))];
        // After: inner level drained, an outer level at 97 is
        // now visible. Same total qty, worse immediacy.
        let after = vec![bid(dec!(100), dec!(10)), bid(dec!(97), dec!(10))];
        let d_before = immediacy_depth_bid(&before, spread);
        let d_after = immediacy_depth_bid(&after, spread);
        assert!(
            d_after < d_before,
            "immediacy must fall when inner depth is replaced with outer depth: \
             before={d_before}, after={d_after}"
        );
    }

    /// Symmetric sanity: a mirror-symmetric book must produce
    /// identical bid and ask immediacy.
    #[test]
    fn immediacy_depth_is_symmetric_on_mirrored_book() {
        let spread = dec!(1);
        let bids = vec![bid(dec!(100), dec!(5)), bid(dec!(99), dec!(8))];
        let asks = vec![bid(dec!(101), dec!(5)), bid(dec!(102), dec!(8))];
        let db = immediacy_depth_bid(&bids, spread);
        let da = immediacy_depth_ask(&asks, spread);
        assert_eq!(db, da);
    }

    // ── Hawkes trade flow tests ─────────────────────────────

    #[test]
    fn hawkes_flow_none_before_any_trade() {
        let h = HawkesTradeFlow::default_crypto();
        assert!(h.value(dec!(0)).is_none());
    }

    #[test]
    fn hawkes_flow_buy_cluster_positive() {
        let mut h = HawkesTradeFlow::default_crypto();
        h.on_trade(Side::Buy, dec!(0));
        h.on_trade(Side::Buy, dec!(0.1));
        h.on_trade(Side::Buy, dec!(0.2));
        let v = h.value(dec!(0.3)).unwrap();
        assert!(
            v > Decimal::ZERO,
            "buy cluster should produce positive imbalance, got {}",
            v
        );
    }

    #[test]
    fn hawkes_flow_sell_cluster_negative() {
        let mut h = HawkesTradeFlow::default_crypto();
        h.on_trade(Side::Sell, dec!(0));
        h.on_trade(Side::Sell, dec!(0.1));
        h.on_trade(Side::Sell, dec!(0.2));
        let v = h.value(dec!(0.3)).unwrap();
        assert!(
            v < Decimal::ZERO,
            "sell cluster should produce negative imbalance, got {}",
            v
        );
    }

    #[test]
    fn hawkes_flow_decays_to_neutral() {
        let mut h = HawkesTradeFlow::default_crypto();
        h.on_trade(Side::Buy, dec!(0));
        let v_soon = h.value(dec!(1)).unwrap();
        let v_later = h.value(dec!(100)).unwrap();
        assert!(
            v_soon.abs() > v_later.abs(),
            "imbalance should decay: soon={} > later={}",
            v_soon.abs(),
            v_later.abs()
        );
    }

    // ── Property-based tests (Epic 19) ────────────────────────

    use proptest::prelude::*;

    prop_compose! {
        fn qty_strat()(raw in 1i64..1_000_000i64) -> Decimal {
            Decimal::new(raw, 2)
        }
    }

    prop_compose! {
        fn price_strat()(raw in 1i64..10_000_000i64) -> Decimal {
            Decimal::new(raw, 2)
        }
    }

    fn levels_strat(n: usize) -> impl Strategy<Value = Vec<PriceLevel>> {
        proptest::collection::vec((price_strat(), qty_strat()), n..=n)
            .prop_map(|v| v.into_iter().map(|(p, q)| PriceLevel { price: p, qty: q }).collect())
    }

    proptest! {
        /// book_imbalance is always in [-1, +1] and zero when the
        /// book is empty. Catches a division-by-zero regression or
        /// a sign flip that overshoots the canonical range.
        #[test]
        fn book_imbalance_range_is_canonical(
            bids in proptest::collection::vec((price_strat(), qty_strat()), 0..8),
            asks in proptest::collection::vec((price_strat(), qty_strat()), 0..8),
            k in 1usize..10,
        ) {
            let bids: Vec<_> = bids.into_iter()
                .map(|(p, q)| PriceLevel { price: p, qty: q }).collect();
            let asks: Vec<_> = asks.into_iter()
                .map(|(p, q)| PriceLevel { price: p, qty: q }).collect();
            let ib = book_imbalance(&bids, &asks, k);
            prop_assert!(ib >= dec!(-1) && ib <= dec!(1),
                "imbalance {} out of [-1, 1]", ib);
        }

        /// book_imbalance sign matches bid vs ask qty strictly.
        #[test]
        fn book_imbalance_sign_matches_heavier_side(
            bids in levels_strat(3),
            asks in levels_strat(3),
        ) {
            let bq: Decimal = bids.iter().take(3).map(|l| l.qty).sum();
            let aq: Decimal = asks.iter().take(3).map(|l| l.qty).sum();
            let ib = book_imbalance(&bids, &asks, 3);
            if bq > aq {
                prop_assert!(ib > dec!(0), "{} vs {} → ib {}", bq, aq, ib);
            } else if aq > bq {
                prop_assert!(ib < dec!(0), "{} vs {} → ib {}", bq, aq, ib);
            } else {
                prop_assert_eq!(ib, dec!(0));
            }
        }

        /// book_imbalance_weighted stays bounded in [-1, +1].
        #[test]
        fn weighted_imbalance_is_bounded(
            bids in proptest::collection::vec((price_strat(), qty_strat()), 0..8),
            asks in proptest::collection::vec((price_strat(), qty_strat()), 0..8),
            k in 1usize..10,
        ) {
            let bids: Vec<_> = bids.into_iter()
                .map(|(p, q)| PriceLevel { price: p, qty: q }).collect();
            let asks: Vec<_> = asks.into_iter()
                .map(|(p, q)| PriceLevel { price: p, qty: q }).collect();
            let ib = book_imbalance_weighted(&bids, &asks, k);
            prop_assert!(ib >= dec!(-1) && ib <= dec!(1));
        }

        /// bba_imbalance stays bounded in [-1, +1].
        #[test]
        fn bba_imbalance_is_bounded(
            bid_price in price_strat(),
            bid_qty in qty_strat(),
            ask_price in price_strat(),
            ask_qty in qty_strat(),
        ) {
            let bids = vec![PriceLevel { price: bid_price, qty: bid_qty }];
            let asks = vec![PriceLevel { price: ask_price, qty: ask_qty }];
            let ib = bba_imbalance(&bids, &asks);
            prop_assert!(ib >= dec!(-1) && ib <= dec!(1));
        }

        /// micro_price always sits between best_bid and best_ask
        /// (inclusive) — by construction a weighted average of the
        /// two prices cannot escape their interval.
        #[test]
        fn micro_price_bracketed_by_touch(
            bid_price_raw in 1i64..100_000,
            gap in 1i64..10_000,
            bid_qty in qty_strat(),
            ask_qty in qty_strat(),
        ) {
            let bid_price = Decimal::new(bid_price_raw, 2);
            let ask_price = bid_price + Decimal::new(gap, 2);
            let bids = vec![PriceLevel { price: bid_price, qty: bid_qty }];
            let asks = vec![PriceLevel { price: ask_price, qty: ask_qty }];
            let mp = micro_price(&bids, &asks).unwrap();
            prop_assert!(mp >= bid_price && mp <= ask_price,
                "mp {} not in [{}, {}]", mp, bid_price, ask_price);
        }

        /// micro_price_weighted with depth=1 matches plain
        /// micro_price on a 1-level book.
        #[test]
        fn weighted_mp_depth1_matches_plain(
            bid_price in price_strat(),
            ask_price in price_strat(),
            bid_qty in qty_strat(),
            ask_qty in qty_strat(),
        ) {
            let bids = vec![PriceLevel { price: bid_price, qty: bid_qty }];
            let asks = vec![PriceLevel { price: ask_price, qty: ask_qty }];
            let plain = micro_price(&bids, &asks).unwrap();
            let weighted = micro_price_weighted(&bids, &asks, 1).unwrap();
            prop_assert_eq!(plain, weighted);
        }

        /// log_price_ratio is antisymmetric: swapping base and
        /// follow negates the result (to within f64 round-trip
        /// precision).
        #[test]
        fn log_price_ratio_is_antisymmetric(
            a in price_strat(),
            b in price_strat(),
        ) {
            let ab = log_price_ratio(a, b).unwrap();
            let ba = log_price_ratio(b, a).unwrap();
            let sum = ab + ba;
            // Allow a small tolerance for f64 round-trip.
            prop_assert!(sum.abs() < Decimal::new(1, 8),
                "ab {} + ba {} = {} > 1e-8", ab, ba, sum);
        }

        /// log_price_ratio of equal prices is zero.
        #[test]
        fn log_price_ratio_of_equal_is_zero(p in price_strat()) {
            let r = log_price_ratio(p, p).unwrap();
            prop_assert_eq!(r, dec!(0));
        }

        /// market_impact: filled_qty never exceeds target, and
        /// `partial` is set iff the book was actually too thin.
        #[test]
        fn market_impact_filled_and_partial_consistent(
            levels in levels_strat(5),
            target_raw in 1i64..100_000,
        ) {
            let target = Decimal::new(target_raw, 2);
            let ref_price = levels[0].price;
            let mi = market_impact(&levels, Side::Buy, target, ref_price).unwrap();
            prop_assert!(mi.filled_qty <= target, "filled {} > target {}", mi.filled_qty, target);
            let total_depth: Decimal = levels.iter().map(|l| l.qty).sum();
            if target <= total_depth {
                prop_assert!(!mi.partial, "full fill flagged partial");
                prop_assert_eq!(mi.filled_qty, target);
            } else {
                prop_assert!(mi.partial, "partial fill not flagged");
                prop_assert_eq!(mi.filled_qty, total_depth);
            }
        }

        /// market_impact: vwap = notional / filled. An invariant
        /// the impact_bps computation hinges on.
        #[test]
        fn market_impact_vwap_matches_notional_over_filled(
            levels in levels_strat(5),
            target_raw in 1i64..100_000,
        ) {
            let target = Decimal::new(target_raw, 2);
            let ref_price = levels[0].price;
            let mi = market_impact(&levels, Side::Buy, target, ref_price).unwrap();
            prop_assert_eq!(mi.vwap, mi.notional / mi.filled_qty);
        }

        /// WindowedTradeFlow: after pushing only buys, value
        /// converges to +1. After only sells, -1. Sanity check
        /// for normalisation.
        #[test]
        fn windowed_pure_one_side_is_extreme(
            qtys in proptest::collection::vec(1i64..10_000, 1..20),
            buys in any::<bool>(),
        ) {
            let mut w = WindowedTradeFlow::new(30);
            let side = if buys { Side::Buy } else { Side::Sell };
            for q in &qtys {
                w.on_trade(Decimal::new(*q, 2), side);
            }
            let v = w.value().unwrap();
            if buys {
                prop_assert_eq!(v, dec!(1));
            } else {
                prop_assert_eq!(v, dec!(-1));
            }
        }

        /// WindowedTradeFlow value always in [-1, +1] across any
        /// mix of trades.
        #[test]
        fn windowed_value_is_bounded(
            ops in proptest::collection::vec((1i64..10_000, any::<bool>()), 1..40),
        ) {
            let mut w = WindowedTradeFlow::new(20);
            for (q, is_buy) in &ops {
                let side = if *is_buy { Side::Buy } else { Side::Sell };
                w.on_trade(Decimal::new(*q, 2), side);
            }
            if let Some(v) = w.value() {
                prop_assert!(v >= dec!(-1) && v <= dec!(1),
                    "value {} out of [-1, 1]", v);
            }
        }

        /// ob_imbalance_multi_depth: output in [-1, +1] for any
        /// alpha ∈ (0, 1) and non-empty depths.
        #[test]
        fn multi_depth_is_bounded(
            bids in proptest::collection::vec((price_strat(), qty_strat()), 0..6),
            asks in proptest::collection::vec((price_strat(), qty_strat()), 0..6),
            depths in proptest::collection::vec(1usize..6, 1..5),
            alpha_raw in 1i64..99,
        ) {
            let bids: Vec<_> = bids.into_iter()
                .map(|(p, q)| PriceLevel { price: p, qty: q }).collect();
            let asks: Vec<_> = asks.into_iter()
                .map(|(p, q)| PriceLevel { price: p, qty: q }).collect();
            let alpha = Decimal::new(alpha_raw, 2); // 0.01..=0.99
            let v = ob_imbalance_multi_depth(&bids, &asks, &depths, alpha);
            prop_assert!(v >= dec!(-1) && v <= dec!(1));
        }
    }
}
