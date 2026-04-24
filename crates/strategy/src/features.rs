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
mod tests;
