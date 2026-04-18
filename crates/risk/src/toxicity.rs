use mm_common::types::{Side, Trade};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::warn;

/// Volume-Synchronized Probability of Informed Trading (VPIN).
///
/// Measures order flow toxicity — high VPIN means informed traders
/// are aggressively taking liquidity (adverse selection risk).
///
/// When VPIN > threshold, the MM should widen spreads or pause quoting.
pub struct VpinEstimator {
    /// Volume bucket size (in quote terms).
    bucket_size: Decimal,
    /// Number of buckets to keep in the window.
    num_buckets: usize,
    /// Current bucket: accumulated buy/sell volume.
    current_buy_vol: Decimal,
    current_sell_vol: Decimal,
    current_total_vol: Decimal,
    /// Completed buckets: (|buy - sell|, total) pairs.
    buckets: VecDeque<(Decimal, Decimal)>,
}

impl VpinEstimator {
    /// Create a new VPIN estimator.
    ///
    /// - `bucket_size`: volume (in quote asset) per bucket. Typical: daily_volume / 50.
    /// - `num_buckets`: window size. Typical: 50.
    pub fn new(bucket_size: Decimal, num_buckets: usize) -> Self {
        Self {
            bucket_size,
            num_buckets,
            current_buy_vol: dec!(0),
            current_sell_vol: dec!(0),
            current_total_vol: dec!(0),
            buckets: VecDeque::with_capacity(num_buckets),
        }
    }

    /// Feed a trade into the VPIN calculator.
    pub fn on_trade(&mut self, trade: &Trade) {
        let vol = trade.price * trade.qty;
        match trade.taker_side {
            Side::Buy => self.current_buy_vol += vol,
            Side::Sell => self.current_sell_vol += vol,
        }
        self.current_total_vol += vol;

        // If bucket is full, finalize it. The bucket's imbalance
        // must be computed on the portion of volume attributed
        // to THIS bucket (= bucket_size), not on the full
        // current_total_vol — otherwise a single trade that
        // overflows multiple buckets would record an imbalance
        // > bucket_size and violate the mathematical bound
        // |buy - sell| ≤ buy + sell, driving vpin() > 1.
        // Property-based test `vpin_is_bounded_in_0_1` caught
        // this latent bug.
        while self.current_total_vol >= self.bucket_size {
            let buy_ratio = if self.current_total_vol > dec!(0) {
                self.current_buy_vol / self.current_total_vol
            } else {
                dec!(0.5)
            };
            let bucket_buy = self.bucket_size * buy_ratio;
            let bucket_sell = self.bucket_size - bucket_buy;
            let imbalance = (bucket_buy - bucket_sell).abs();
            self.buckets.push_back((imbalance, self.bucket_size));
            if self.buckets.len() > self.num_buckets {
                self.buckets.pop_front();
            }

            // Carry overflow into next bucket using the same ratio.
            let overflow = self.current_total_vol - self.bucket_size;
            self.current_buy_vol = overflow * buy_ratio;
            self.current_sell_vol = overflow - self.current_buy_vol;
            self.current_total_vol = overflow;
        }
    }

    /// Get current VPIN value [0, 1].
    /// 0 = balanced flow, 1 = completely one-sided (toxic).
    pub fn vpin(&self) -> Option<Decimal> {
        if self.buckets.len() < self.num_buckets / 2 {
            return None; // Not enough data.
        }
        let sum_imbalance: Decimal = self.buckets.iter().map(|(imb, _)| *imb).sum();
        let sum_volume: Decimal = self.buckets.iter().map(|(_, vol)| *vol).sum();
        if sum_volume.is_zero() {
            return None;
        }
        Some(sum_imbalance / sum_volume)
    }

    /// Check if flow is toxic (above threshold).
    pub fn is_toxic(&self, threshold: Decimal) -> bool {
        self.vpin().map(|v| v > threshold).unwrap_or(false)
    }

    /// Epic D sub-component #3 — Bulk Volume Classification
    /// entry point. Feeds *already-classified* buy / sell
    /// volumes directly into the bucketiser, bypassing the
    /// per-trade tick-rule path in [`Self::on_trade`].
    ///
    /// Operators pick the classification path per config:
    /// `on_trade` retains the classic Lee-Ready tick rule via
    /// `Trade::taker_side`; `on_bvc_bar` accepts the
    /// CDF-classified output from [`BvcClassifier::classify`].
    /// Both paths coexist and produce the same
    /// `vpin()` output shape — only the classification step
    /// upstream differs.
    pub fn on_bvc_bar(&mut self, buy_vol: Decimal, sell_vol: Decimal) {
        let total = buy_vol + sell_vol;
        self.current_buy_vol += buy_vol;
        self.current_sell_vol += sell_vol;
        self.current_total_vol += total;

        while self.current_total_vol >= self.bucket_size {
            let overflow = self.current_total_vol - self.bucket_size;
            let imbalance = (self.current_buy_vol - self.current_sell_vol).abs();
            self.buckets.push_back((imbalance, self.bucket_size));
            if self.buckets.len() > self.num_buckets {
                self.buckets.pop_front();
            }
            // Carry overflow into next bucket, proportionally
            // to the current buy/sell split.
            if self.current_total_vol > dec!(0) {
                let buy_ratio = self.current_buy_vol / self.current_total_vol;
                self.current_buy_vol = overflow * buy_ratio;
                self.current_sell_vol = overflow * (dec!(1) - buy_ratio);
            } else {
                self.current_buy_vol = dec!(0);
                self.current_sell_vol = dec!(0);
            }
            self.current_total_vol = overflow;
        }
    }
}

// ---------------------------------------------------------------------------
// Epic D sub-component #3 — Bulk Volume Classification
// ---------------------------------------------------------------------------

/// Easley-López de Prado-O'Hara 2012 Bulk Volume Classification.
///
/// Splits a bar's total traded volume into *buy* and *sell*
/// fractions based on the bar's price change — no trade-level
/// tick-rule classification required. The split uses the
/// Student-t CDF of the standardised price change:
///
/// ```text
/// V_buy  = V · CDF_ν((ΔP − μ) / σ)
/// V_sell = V − V_buy
/// ```
///
/// Source: Easley, D., López de Prado, M., O'Hara, M. —
/// "Flow Toxicity and Liquidity in a High-Frequency World,"
/// *Review of Financial Studies*, 25(5), 1457–1493 (2012),
/// eq. 4.
///
/// v1 default `ν = 0.25` matches the source paper on
/// S&P E-minis. Crypto's heavier tails may warrant tuning —
/// operators override in config per venue.
pub struct BvcClassifier {
    nu: Decimal,
    window: VecDeque<Decimal>,
    window_size: usize,
    sum: Decimal,
    sum_sq: Decimal,
}

impl BvcClassifier {
    /// New classifier with Student-t degrees of freedom `nu`
    /// and rolling-window size `window_size` for the mean/std
    /// of bar price changes.
    pub fn new(nu: Decimal, window_size: usize) -> Self {
        assert!(window_size >= 2, "window_size must be >= 2");
        assert!(nu > Decimal::ZERO, "nu must be positive");
        Self {
            nu,
            window: VecDeque::with_capacity(window_size),
            window_size,
            sum: Decimal::ZERO,
            sum_sq: Decimal::ZERO,
        }
    }

    /// Classify one bar's total volume into `(buy, sell)`
    /// fractions. Returns `None` during warmup (window <
    /// 10 samples) or when the rolling std is zero (no
    /// signal). After warmup the caller feeds the split
    /// directly into [`VpinEstimator::on_bvc_bar`].
    pub fn classify(&mut self, bar_dp: Decimal, bar_volume: Decimal) -> Option<(Decimal, Decimal)> {
        self.push(bar_dp);
        if self.window.len() < 10 {
            return None;
        }
        let mean = self.mean()?;
        let std = self.std()?;
        if std.is_zero() {
            return None;
        }
        let z = (bar_dp - mean) / std;
        let cdf_z = student_t_cdf(z, self.nu);
        let buy = bar_volume * cdf_z;
        let sell = bar_volume - buy;
        Some((buy, sell))
    }

    /// Rolling mean of `bar_dp`, `None` during warmup.
    pub fn rolling_mean(&self) -> Option<Decimal> {
        self.mean()
    }

    /// Rolling std of `bar_dp`, `None` during warmup.
    pub fn rolling_std(&self) -> Option<Decimal> {
        self.std()
    }

    fn push(&mut self, dp: Decimal) {
        if self.window.len() == self.window_size {
            let evicted = self.window.pop_front().expect("len == window_size");
            self.sum -= evicted;
            self.sum_sq -= evicted * evicted;
        }
        self.window.push_back(dp);
        self.sum += dp;
        self.sum_sq += dp * dp;
    }

    fn mean(&self) -> Option<Decimal> {
        if self.window.is_empty() {
            return None;
        }
        Some(self.sum / Decimal::from(self.window.len()))
    }

    fn std(&self) -> Option<Decimal> {
        let n = self.window.len();
        if n < 2 {
            return None;
        }
        let mean = self.mean()?;
        let n_dec = Decimal::from(n);
        let ss = self.sum_sq - n_dec * mean * mean;
        if ss <= Decimal::ZERO {
            return Some(Decimal::ZERO);
        }
        let var = ss / (n_dec - Decimal::ONE);
        Some(decimal_sqrt_newton(var))
    }
}

/// Time-bucketed aggregator that feeds a [`BvcClassifier`]
/// (Epic D stage-2). Collects every trade inside a fixed-length
/// bar window (by wall-clock ns) and emits one
/// `(delta_price, total_volume_quote)` observation per closed
/// bar. Volume is quote-asset notional (`price × qty`), price
/// change is the difference between the bar's first and last
/// trade print — matches the Easley-Prado 2012 input shape.
///
/// Time injection keeps the aggregator deterministic: the
/// engine owns the clock and calls [`Self::flush_if_due`] on
/// each trade and on every `tick_second`.
///
/// The aggregator is silent on empty bars (no trades in the
/// window) — the classifier's rolling mean/std must not be
/// polluted with zero-volume anchor points, which would make
/// σ collapse toward zero and push the classifier into its
/// `std.is_zero()` short-circuit.
#[derive(Debug, Clone)]
pub struct BvcBarAggregator {
    bar_len_ns: i64,
    /// Wall-clock ns at which the current bar opened. `None`
    /// until the first trade arrives.
    bar_open_ns: Option<i64>,
    first_px: Decimal,
    last_px: Decimal,
    total_vol: Decimal,
}

impl BvcBarAggregator {
    /// `bar_secs` is the bar length in seconds. Values `< 1` are
    /// rounded up to 1 s so the aggregator never gets stuck in
    /// a zero-window hot loop.
    pub fn new(bar_secs: u64) -> Self {
        let secs = bar_secs.max(1);
        Self {
            bar_len_ns: (secs as i64) * 1_000_000_000,
            bar_open_ns: None,
            first_px: Decimal::ZERO,
            last_px: Decimal::ZERO,
            total_vol: Decimal::ZERO,
        }
    }

    /// Ingest one trade and (if the bar just closed) return the
    /// newly-finalised `(delta_price, total_volume)` pair. The
    /// trade itself is counted in the NEXT bar — the closing
    /// print anchors the finalised bar. This matches the
    /// standard bar-compile convention (low / high / close
    /// windows aren't bled into the next).
    pub fn push(
        &mut self,
        now_ns: i64,
        price: Decimal,
        qty: Decimal,
    ) -> Option<(Decimal, Decimal)> {
        let notional = price * qty;
        match self.bar_open_ns {
            None => {
                // First-ever trade: seed the bar and return nothing.
                self.bar_open_ns = Some(now_ns);
                self.first_px = price;
                self.last_px = price;
                self.total_vol = notional;
                None
            }
            Some(open_ns) if now_ns - open_ns >= self.bar_len_ns => {
                // This trade crossed the bar boundary — finalise
                // the previous bar, start a new one anchored at
                // `now_ns`.
                let dp = self.last_px - self.first_px;
                let vol = self.total_vol;
                self.bar_open_ns = Some(now_ns);
                self.first_px = price;
                self.last_px = price;
                self.total_vol = notional;
                Some((dp, vol))
            }
            Some(_) => {
                // Same bar — fold the trade in.
                self.last_px = price;
                self.total_vol += notional;
                None
            }
        }
    }

    /// Called from the engine's 1 Hz tick so a quiet symbol
    /// (no trades in the window) still surfaces its closed bar
    /// instead of stalling forever on the last trade's stale
    /// price anchor. Returns `None` when the current bar is
    /// still open or there are no trades to report.
    ///
    /// On flush the aggregator resets `bar_open_ns` to `None`
    /// — a follow-up `push` will seed a new bar.
    pub fn flush_if_due(&mut self, now_ns: i64) -> Option<(Decimal, Decimal)> {
        let open_ns = self.bar_open_ns?;
        if now_ns - open_ns < self.bar_len_ns {
            return None;
        }
        let dp = self.last_px - self.first_px;
        let vol = self.total_vol;
        self.bar_open_ns = None;
        self.first_px = Decimal::ZERO;
        self.last_px = Decimal::ZERO;
        self.total_vol = Decimal::ZERO;
        if vol.is_zero() {
            return None;
        }
        Some((dp, vol))
    }

    /// Exposed for the engine's observability surface.
    pub fn bar_len_ns(&self) -> i64 {
        self.bar_len_ns
    }
}

/// Newton's method sqrt for `Decimal`. Local copy to keep
/// `mm-risk::toxicity` free of cross-module helper
/// dependencies (same pattern as `var_guard`'s local copy).
fn decimal_sqrt_newton(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let mut guess = x / dec!(2);
    if guess.is_zero() {
        guess = dec!(1);
    }
    for _ in 0..30 {
        let next = (guess + x / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000000001) {
            return next;
        }
        guess = next;
    }
    guess
}

/// Student-t CDF. Delegates to f64 internally via the
/// regularized incomplete beta function — same boundary-
/// conversion pattern as `features::log_price_ratio` and
/// `features::hurst_exponent`. Transcendentals (ln, pow, exp)
/// have no closed-form `Decimal` implementation, so this is
/// the canonical escape hatch.
///
/// For `ν ≥ 30` we fall back to the Normal approximation
/// (Abramowitz-Stegun erf 7.1.26). For smaller `ν` we use
/// the identity
///
/// ```text
/// CDF_ν(z) = 1 − (1/2) · I_x(ν/2, 1/2)    for z > 0
/// ```
///
/// with `x = ν / (ν + z²)`, symmetric for `z < 0`.
pub fn student_t_cdf(z: Decimal, nu: Decimal) -> Decimal {
    let z_f = z.to_f64().unwrap_or(0.0);
    let nu_f = nu.to_f64().unwrap_or(1.0);
    let p = student_t_cdf_f64(z_f, nu_f);
    Decimal::from_f64(p).unwrap_or(dec!(0.5))
}

fn student_t_cdf_f64(z: f64, nu: f64) -> f64 {
    if !z.is_finite() || !nu.is_finite() || nu <= 0.0 {
        return 0.5;
    }
    if nu >= 30.0 {
        return normal_cdf(z);
    }
    if z.abs() < 1e-15 {
        return 0.5;
    }
    let x = nu / (nu + z * z);
    let ibeta = regularized_incomplete_beta(x, nu / 2.0, 0.5);
    if z > 0.0 {
        1.0 - 0.5 * ibeta
    } else {
        0.5 * ibeta
    }
}

fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf_as(z / std::f64::consts::SQRT_2))
}

/// Abramowitz-Stegun 7.1.26 erf approximation, max error
/// ≈ 1.5e-7 on all of `ℝ`.
fn erf_as(x: f64) -> f64 {
    const A1: f64 = 0.254_829_592;
    const A2: f64 = -0.284_496_736;
    const A3: f64 = 1.421_413_741;
    const A4: f64 = -1.453_152_027;
    const A5: f64 = 1.061_405_429;
    const P: f64 = 0.327_591_1;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let ax = x.abs();
    let t = 1.0 / (1.0 + P * ax);
    let y = 1.0 - (((((A5 * t + A4) * t) + A3) * t + A2) * t + A1) * t * (-ax * ax).exp();
    sign * y
}

/// Regularized incomplete beta function `I_x(a, b)` via the
/// Numerical Recipes in C §6.4 continued fraction. ~10
/// decimal places of accuracy for `a, b > 0` and
/// `x ∈ [0, 1]`. Uses the `I_x(a,b) = 1 − I_{1−x}(b,a)`
/// symmetry to pick the branch where the continued fraction
/// converges fastest.
fn regularized_incomplete_beta(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let bt = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * betacf(x, a, b) / a
    } else {
        1.0 - bt * betacf(1.0 - x, b, a) / b
    }
}

/// Lentz's method continued-fraction expansion of the
/// regularized incomplete beta. From Numerical Recipes in C
/// §6.4 "betacf".
fn betacf(x: f64, a: f64, b: f64) -> f64 {
    const MAX_ITER: usize = 200;
    const EPS: f64 = 3e-7;
    const TINY: f64 = 1e-30;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=MAX_ITER {
        let m_f = m as f64;
        let m2 = 2.0 * m_f;
        let aa1 = m_f * (b - m_f) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa1 * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa1 / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa2 = -(a + m_f) * (qab + m_f) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa2 * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa2 / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            return h;
        }
    }
    h
}

/// Lanczos approximation to log-gamma, good to ~12 decimals
/// for `x > 0`. From Numerical Recipes in C §6.1 "gammln".
fn ln_gamma(x: f64) -> f64 {
    const COEF: [f64; 6] = [
        76.180_091_729_471_46,
        -86.505_320_329_416_77,
        24.014_098_240_830_91,
        -1.231_739_572_450_155,
        0.001_208_650_973_866_179,
        -0.000_005_395_239_384_953,
    ];
    let y = x;
    let mut xx = x;
    let tmp = xx + 5.5;
    let tmp = (xx + 0.5) * tmp.ln() - tmp;
    let mut ser = 1.000_000_000_190_015;
    for c in &COEF {
        xx += 1.0;
        ser += c / xx;
    }
    tmp + (2.506_628_274_631 * ser / y).ln()
}

/// Kyle's Lambda — price impact estimator.
///
/// Measures how much price moves per unit of signed order flow.
/// High lambda = low liquidity or informed trading.
///
/// λ = Cov(ΔP, OFI) / Var(OFI)
/// where OFI = signed volume (buy+ / sell-).
pub struct KyleLambda {
    /// Window of (price_change, signed_volume) observations.
    observations: VecDeque<(Decimal, Decimal)>,
    window_size: usize,
}

impl KyleLambda {
    pub fn new(window_size: usize) -> Self {
        Self {
            observations: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    /// Record a time-bar observation.
    /// `price_change`: mid price change over the bar.
    /// `signed_volume`: net buy - sell volume over the bar.
    pub fn update(&mut self, price_change: Decimal, signed_volume: Decimal) {
        self.observations.push_back((price_change, signed_volume));
        if self.observations.len() > self.window_size {
            self.observations.pop_front();
        }
    }

    /// Estimate Kyle's Lambda (price impact coefficient).
    pub fn lambda(&self) -> Option<Decimal> {
        let n = self.observations.len();
        if n < 10 {
            return None;
        }
        let nd = Decimal::from(n as u64);

        let mean_dp: Decimal = self.observations.iter().map(|(dp, _)| *dp).sum::<Decimal>() / nd;
        let mean_ofi: Decimal = self
            .observations
            .iter()
            .map(|(_, ofi)| *ofi)
            .sum::<Decimal>()
            / nd;

        let mut cov = dec!(0);
        let mut var_ofi = dec!(0);

        for (dp, ofi) in &self.observations {
            let d_dp = *dp - mean_dp;
            let d_ofi = *ofi - mean_ofi;
            cov += d_dp * d_ofi;
            var_ofi += d_ofi * d_ofi;
        }

        if var_ofi.is_zero() {
            return None;
        }

        Some(cov / var_ofi)
    }
}

/// Adverse selection tracker — monitors fill quality.
///
/// After each fill, tracks how price moves against us.
/// If fills consistently precede adverse moves, flow is toxic.
pub struct AdverseSelectionTracker {
    /// Recent fill events: (fill_price, mid_price_after_N_seconds).
    fills: VecDeque<FillOutcome>,
    window_size: usize,
}

#[derive(Debug, Clone)]
struct FillOutcome {
    fill_price: Decimal,
    side: Side,
    mid_at_fill: Decimal,
    mid_after: Option<Decimal>,
    timestamp_ms: i64,
}

impl AdverseSelectionTracker {
    pub fn new(window_size: usize) -> Self {
        Self {
            fills: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    /// Record a fill. Call this when our order gets filled.
    pub fn on_fill(&mut self, fill_price: Decimal, side: Side, current_mid: Decimal) {
        let ts = chrono::Utc::now().timestamp_millis();
        self.fills.push_back(FillOutcome {
            fill_price,
            side,
            mid_at_fill: current_mid,
            mid_after: None,
            timestamp_ms: ts,
        });
        if self.fills.len() > self.window_size {
            self.fills.pop_front();
        }
    }

    /// Update with current mid price — fills the "mid_after" for recent fills.
    /// Call this periodically (e.g., 1-5 seconds after fills).
    pub fn update_mid(&mut self, current_mid: Decimal, lookback_ms: i64) {
        let now = chrono::Utc::now().timestamp_millis();
        for fill in self.fills.iter_mut() {
            if fill.mid_after.is_none() && (now - fill.timestamp_ms) >= lookback_ms {
                fill.mid_after = Some(current_mid);
            }
        }
    }

    /// Calculate adverse selection cost in bps.
    /// Positive = we're losing money on average after fills.
    pub fn adverse_selection_bps(&self) -> Option<Decimal> {
        self.adverse_selection_bps_filter(None)
    }

    /// Epic D stage-3 — per-side adverse selection bps.
    ///
    /// Filters the fill window to one side and returns the
    /// average adverse cost in bps for that side. Used by the
    /// per-side `cartea_spread::quoted_half_spread_per_side`
    /// path so the strategy can widen each side independently
    /// when only one side is being run over by informed flow.
    ///
    /// `Side::Buy` returns the adverse-selection bps over our
    /// **bid fills** (we bought at the touch — informed sells
    /// hit us); `Side::Sell` returns it over our **ask fills**
    /// (we sold at the touch — informed buys lifted us).
    /// Returns `None` when fewer than 5 completed fills are
    /// available on the requested side.
    pub fn adverse_selection_bps_for_side(&self, side: Side) -> Option<Decimal> {
        self.adverse_selection_bps_filter(Some(side))
    }

    /// Convenience for the bid side (our buy fills).
    pub fn adverse_selection_bps_bid(&self) -> Option<Decimal> {
        self.adverse_selection_bps_for_side(Side::Buy)
    }

    /// Convenience for the ask side (our sell fills).
    pub fn adverse_selection_bps_ask(&self) -> Option<Decimal> {
        self.adverse_selection_bps_for_side(Side::Sell)
    }

    fn adverse_selection_bps_filter(&self, side_filter: Option<Side>) -> Option<Decimal> {
        let completed: Vec<&FillOutcome> = self
            .fills
            .iter()
            .filter(|f| f.mid_after.is_some())
            .filter(|f| side_filter.is_none_or(|s| f.side == s))
            .collect();
        if completed.len() < 5 {
            return None;
        }

        let n = Decimal::from(completed.len() as u64);
        let mut total_adverse = dec!(0);

        for fill in &completed {
            let mid_after = fill.mid_after.unwrap();
            let adverse = match fill.side {
                // We bought — if price dropped after, that's adverse.
                Side::Buy => fill.fill_price - mid_after,
                // We sold — if price rose after, that's adverse.
                Side::Sell => mid_after - fill.fill_price,
            };
            if !fill.mid_at_fill.is_zero() {
                total_adverse += adverse / fill.mid_at_fill * dec!(10_000); // bps
            }
        }

        let avg = total_adverse / n;
        if side_filter.is_none() && avg > dec!(5) {
            warn!(
                adverse_bps = %avg,
                "high adverse selection detected"
            );
        }
        Some(avg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn trade(price: &str, qty: &str, side: Side) -> Trade {
        Trade {
            trade_id: 1,
            symbol: "BTCUSDT".into(),
            price: price.parse().unwrap(),
            qty: qty.parse().unwrap(),
            taker_side: side,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_vpin_balanced_flow() {
        let mut vpin = VpinEstimator::new(dec!(1000), 10);
        // Equal buy and sell volume — should be low VPIN.
        for _ in 0..50 {
            vpin.on_trade(&trade("100", "5", Side::Buy));
            vpin.on_trade(&trade("100", "5", Side::Sell));
        }
        let v = vpin.vpin().unwrap();
        assert!(v < dec!(0.1), "balanced flow should have low VPIN, got {v}");
    }

    #[test]
    fn test_vpin_toxic_flow() {
        let mut vpin = VpinEstimator::new(dec!(1000), 10);
        // All buy volume — completely toxic.
        for _ in 0..100 {
            vpin.on_trade(&trade("100", "5", Side::Buy));
        }
        let v = vpin.vpin().unwrap();
        assert!(
            v > dec!(0.8),
            "one-sided flow should have high VPIN, got {v}"
        );
    }

    #[test]
    fn test_kyle_lambda() {
        let mut kl = KyleLambda::new(50);
        // Simulate: price goes up when buy volume is positive.
        for i in 0..30 {
            let signed_vol = if i % 2 == 0 { dec!(100) } else { dec!(-100) };
            let dp = signed_vol * dec!(0.001); // Lambda should be ~0.001.
            kl.update(dp, signed_vol);
        }
        let lambda = kl.lambda().unwrap();
        assert!(lambda > dec!(0), "lambda should be positive");
    }

    // ---------------------------------------------------------------
    // Epic D sub-component #3 — BVC classifier + VPIN on_bvc_bar
    // ---------------------------------------------------------------

    fn approx(a: Decimal, b: Decimal, eps: Decimal) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn bvc_warmup_returns_none() {
        let mut b = BvcClassifier::new(dec!(0.25), 50);
        // Fewer than 10 observations → None.
        for i in 0..5 {
            let res = b.classify(Decimal::from(i), dec!(100));
            assert!(res.is_none(), "warmup i={i}");
        }
    }

    #[test]
    fn bvc_zero_variance_window_returns_none() {
        let mut b = BvcClassifier::new(dec!(0.25), 50);
        // All bars identical → zero std → None.
        for _ in 0..15 {
            let res = b.classify(dec!(1), dec!(100));
            assert!(res.is_none());
        }
    }

    #[test]
    fn bvc_positive_price_change_classifies_majority_buy() {
        let mut b = BvcClassifier::new(dec!(0.25), 50);
        // Warm up the window with mean-zero dp.
        for i in 0..20 {
            let dp = if i % 2 == 0 { dec!(-1) } else { dec!(1) };
            let _ = b.classify(dp, dec!(100));
        }
        // Now feed a strongly positive dp — should land in
        // the right tail of the Student-t, producing
        // majority buy.
        let (buy, sell) = b.classify(dec!(5), dec!(100)).expect("warmup done");
        assert!(buy > sell, "buy={buy} sell={sell}");
        assert!(approx(buy + sell, dec!(100), dec!(0.0001)));
    }

    #[test]
    fn bvc_negative_price_change_classifies_majority_sell() {
        let mut b = BvcClassifier::new(dec!(0.25), 50);
        for i in 0..20 {
            let dp = if i % 2 == 0 { dec!(-1) } else { dec!(1) };
            let _ = b.classify(dp, dec!(100));
        }
        let (buy, sell) = b.classify(dec!(-5), dec!(100)).expect("warmup done");
        assert!(sell > buy, "buy={buy} sell={sell}");
        assert!(approx(buy + sell, dec!(100), dec!(0.0001)));
    }

    #[test]
    fn bvc_total_volume_invariant() {
        let mut b = BvcClassifier::new(dec!(0.25), 50);
        for i in 0..15 {
            let dp = Decimal::from(i as i64 % 5 - 2);
            if let Some((buy, sell)) = b.classify(dp, dec!(200)) {
                assert!(
                    approx(buy + sell, dec!(200), dec!(0.0001)),
                    "buy+sell != 200 (buy={buy}, sell={sell})"
                );
            }
        }
    }

    #[test]
    fn student_t_cdf_at_zero_is_half_for_any_nu() {
        assert!(approx(
            student_t_cdf(dec!(0), dec!(0.25)),
            dec!(0.5),
            dec!(0.0001)
        ));
        assert!(approx(
            student_t_cdf(dec!(0), dec!(3)),
            dec!(0.5),
            dec!(0.0001)
        ));
        assert!(approx(
            student_t_cdf(dec!(0), dec!(30)),
            dec!(0.5),
            dec!(0.0001)
        ));
    }

    #[test]
    fn student_t_cdf_saturates_in_tails() {
        // Very large positive z → ~1, very large negative → ~0.
        let high = student_t_cdf(dec!(1000), dec!(5));
        let low = student_t_cdf(dec!(-1000), dec!(5));
        assert!(high > dec!(0.99));
        assert!(low < dec!(0.01));
    }

    #[test]
    fn student_t_cdf_large_nu_matches_normal() {
        // At ν = 30+, the Student-t CDF collapses to Normal.
        // CDF(1.0) for Normal ≈ 0.8413.
        let v = student_t_cdf(dec!(1), dec!(50));
        assert!(
            approx(v, dec!(0.8413), dec!(0.001)),
            "Φ(1) ≈ 0.8413, got {v}"
        );
    }

    #[test]
    fn on_bvc_bar_produces_same_vpin_shape_as_on_trade() {
        // Feed two VpinEstimators the same underlying
        // buy/sell split — one via the tick-rule path, one
        // via the BVC bar path. VPIN outputs should be
        // byte-identical.
        let mut vpin_tick = VpinEstimator::new(dec!(1000), 10);
        let mut vpin_bvc = VpinEstimator::new(dec!(1000), 10);

        for _ in 0..100 {
            // 60 qty @ 100 buy + 40 qty @ 100 sell per iteration
            // → 60/100 buy-share.
            vpin_tick.on_trade(&trade("100", "6", Side::Buy));
            vpin_tick.on_trade(&trade("100", "4", Side::Sell));

            // Same split via BVC path: 600 quote buy + 400 quote sell.
            vpin_bvc.on_bvc_bar(dec!(600), dec!(400));
        }

        let v_tick = vpin_tick.vpin();
        let v_bvc = vpin_bvc.vpin();
        assert_eq!(v_tick, v_bvc);
    }

    #[test]
    fn bvc_classifier_rolling_mean_and_std_accessors() {
        let mut b = BvcClassifier::new(dec!(0.25), 10);
        for i in 1..=5 {
            b.classify(Decimal::from(i), dec!(100));
        }
        let mean = b.rolling_mean().unwrap();
        // Mean of 1..=5 is 3.
        assert_eq!(mean, dec!(3));
        let std = b.rolling_std().unwrap();
        assert!(std > Decimal::ZERO);
    }

    #[test]
    #[should_panic(expected = "window_size must be >= 2")]
    fn bvc_panics_on_tiny_window() {
        BvcClassifier::new(dec!(0.25), 1);
    }

    #[test]
    #[should_panic(expected = "nu must be positive")]
    fn bvc_panics_on_nonpositive_nu() {
        BvcClassifier::new(Decimal::ZERO, 10);
    }

    // ---------------------------------------------------------
    // Epic D stage-2 — BvcBarAggregator tests
    // ---------------------------------------------------------

    const BAR_NS: i64 = 1_000_000_000;

    #[test]
    fn aggregator_first_trade_seeds_bar_without_emitting() {
        let mut agg = BvcBarAggregator::new(1);
        let out = agg.push(100, dec!(100), dec!(1));
        assert!(out.is_none());
    }

    #[test]
    fn aggregator_same_bar_folds_trades() {
        let mut agg = BvcBarAggregator::new(1);
        agg.push(100, dec!(100), dec!(1));
        agg.push(200, dec!(101), dec!(2));
        // Still inside the 1s bar — no emission, last_px tracks
        // rolling latest.
        let out = agg.push(500, dec!(102), dec!(3));
        assert!(out.is_none());
    }

    #[test]
    fn aggregator_emits_on_boundary_crossing() {
        let mut agg = BvcBarAggregator::new(1);
        agg.push(0, dec!(100), dec!(1));
        agg.push(500_000_000, dec!(102), dec!(2));
        // Crosses 1s boundary.
        let emitted = agg.push(BAR_NS, dec!(105), dec!(1));
        assert_eq!(emitted, Some((dec!(2), dec!(100) + dec!(204)))); // dp = 102-100, vol = 100+204
    }

    #[test]
    fn aggregator_flush_if_due_emits_when_bar_closed_and_quiet() {
        let mut agg = BvcBarAggregator::new(1);
        agg.push(0, dec!(100), dec!(1));
        agg.push(100_000_000, dec!(100), dec!(2));
        // Well past boundary but no trade to carry it over —
        // flush_if_due surfaces the closed bar.
        let emitted = agg.flush_if_due(2 * BAR_NS);
        assert_eq!(emitted, Some((dec!(0), dec!(300))));
        // Subsequent flush with no new push is a no-op.
        assert_eq!(agg.flush_if_due(10 * BAR_NS), None);
    }

    #[test]
    fn aggregator_flush_before_boundary_is_noop() {
        let mut agg = BvcBarAggregator::new(1);
        agg.push(0, dec!(100), dec!(1));
        assert_eq!(agg.flush_if_due(BAR_NS / 2), None);
    }

    #[test]
    fn aggregator_flush_without_any_trade_is_noop() {
        let mut agg = BvcBarAggregator::new(1);
        assert_eq!(agg.flush_if_due(BAR_NS * 10), None);
    }

    #[test]
    fn aggregator_bar_secs_zero_gets_clamped_to_one() {
        let agg = BvcBarAggregator::new(0);
        assert_eq!(agg.bar_len_ns(), BAR_NS);
    }

    // ---------------------------------------------------------
    // Epic D stage-3 — per-side adverse-selection bps
    // ---------------------------------------------------------

    fn seed_completed_fill(
        tracker: &mut AdverseSelectionTracker,
        side: Side,
        fill_price: Decimal,
        mid_at_fill: Decimal,
        mid_after: Decimal,
    ) {
        tracker.fills.push_back(FillOutcome {
            fill_price,
            side,
            mid_at_fill,
            mid_after: Some(mid_after),
            timestamp_ms: 0,
        });
    }

    #[test]
    fn per_side_bps_returns_none_below_threshold() {
        // Fewer than 5 completed fills on a side → None.
        let mut t = AdverseSelectionTracker::new(50);
        for _ in 0..3 {
            seed_completed_fill(&mut t, Side::Buy, dec!(100), dec!(100), dec!(99));
        }
        assert!(t.adverse_selection_bps_bid().is_none());
        assert!(t.adverse_selection_bps_ask().is_none());
        // Symmetric path also requires 5 — only 3 total.
        assert!(t.adverse_selection_bps().is_none());
    }

    #[test]
    fn per_side_bps_filters_buy_only() {
        let mut t = AdverseSelectionTracker::new(50);
        // 6 buy fills — adverse 100 bps each (bought at 100, mid dropped to 99).
        for _ in 0..6 {
            seed_completed_fill(&mut t, Side::Buy, dec!(100), dec!(100), dec!(99));
        }
        // 5 sell fills — neutral (sold at 100, mid stayed at 100).
        for _ in 0..5 {
            seed_completed_fill(&mut t, Side::Sell, dec!(100), dec!(100), dec!(100));
        }
        // Bid path sees only the buys → ~+100 bps adverse.
        let bid = t.adverse_selection_bps_bid().unwrap();
        assert!((bid - dec!(100)).abs() < dec!(0.001));
        // Ask path sees only the sells → 0 bps.
        let ask = t.adverse_selection_bps_ask().unwrap();
        assert_eq!(ask, dec!(0));
    }

    #[test]
    fn per_side_bps_filters_sell_only() {
        let mut t = AdverseSelectionTracker::new(50);
        // 5 sell fills — adverse 50 bps each (sold at 100, mid rose to 100.5).
        for _ in 0..5 {
            seed_completed_fill(&mut t, Side::Sell, dec!(100), dec!(100), dec!(100.5));
        }
        // 6 buy fills — neutral.
        for _ in 0..6 {
            seed_completed_fill(&mut t, Side::Buy, dec!(100), dec!(100), dec!(100));
        }
        let ask = t.adverse_selection_bps_ask().unwrap();
        assert!((ask - dec!(50)).abs() < dec!(0.001));
        let bid = t.adverse_selection_bps_bid().unwrap();
        assert_eq!(bid, dec!(0));
    }

    #[test]
    fn per_side_average_matches_symmetric_when_one_sided() {
        // When all fills are on one side, that side's per-side
        // average equals the symmetric average.
        let mut t = AdverseSelectionTracker::new(50);
        for _ in 0..7 {
            seed_completed_fill(&mut t, Side::Buy, dec!(50_000), dec!(50_000), dec!(49_995));
        }
        let symmetric = t.adverse_selection_bps().unwrap();
        let bid = t.adverse_selection_bps_bid().unwrap();
        assert_eq!(symmetric, bid);
    }

    // ── Property-based tests (Epic 12) ───────────────────────

    use proptest::prelude::*;
    use proptest::sample::select;

    fn side_strat() -> impl Strategy<Value = Side> {
        select(vec![Side::Buy, Side::Sell])
    }
    prop_compose! {
        fn price_strat_tox()(cents in 100i64..100_000_000i64) -> Decimal {
            Decimal::new(cents, 2)
        }
    }
    prop_compose! {
        fn qty_strat_tox()(units in 1i64..1_000_000i64) -> Decimal {
            Decimal::new(units, 4)
        }
    }

    fn make_trade(side: Side, price: Decimal, qty: Decimal) -> Trade {
        Trade {
            trade_id: 1,
            symbol: "TEST".into(),
            price,
            qty,
            taker_side: side,
            timestamp: chrono::Utc::now(),
        }
    }

    // ── VPIN ─────────────────────────────────────────────────

    proptest! {
        // Heavier than the default — proptest runs 256 cases of
        // up-to-60-trade sequences. 32 cases keep CI under
        // 10 seconds while still covering enough random shapes
        // to exercise the overflow math.
        #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]

        /// VPIN is bounded in [0, 1] for any trade sequence. A
        /// value > 1 would be mathematically impossible (the
        /// imbalance can't exceed the volume). This property
        /// caught a real bug in the bucket-overflow path where
        /// imbalance was computed against current_total_vol
        /// instead of bucket_size, letting VPIN exceed 1 on
        /// one-sided flow.
        #[test]
        fn vpin_is_bounded_in_0_1(
            trades in proptest::collection::vec(
                (side_strat(), price_strat_tox(), qty_strat_tox()),
                10..60,
            ),
        ) {
            let mut v = VpinEstimator::new(dec!(10_000), 20);
            for (side, p, q) in &trades {
                v.on_trade(&make_trade(*side, *p, *q));
            }
            if let Some(vpin) = v.vpin() {
                prop_assert!(vpin >= dec!(0), "VPIN {} < 0", vpin);
                prop_assert!(vpin <= dec!(1), "VPIN {} > 1", vpin);
            }
        }

        /// A completely one-sided flow (all buys) saturates
        /// toward VPIN = 1. Verifies the imbalance is picked
        /// up as toxic.
        #[test]
        fn one_sided_flow_has_high_vpin(
            qty in qty_strat_tox(),
            price in price_strat_tox(),
            n_trades in 30usize..60usize,
        ) {
            // Bucket size = one-trade notional so each trade
            // finalises its own bucket.
            let bucket_size = price * qty;
            let mut v = VpinEstimator::new(bucket_size, 20);
            for _ in 0..n_trades {
                v.on_trade(&make_trade(Side::Buy, price, qty));
            }
            if let Some(vpin) = v.vpin() {
                prop_assert!(vpin >= dec!(0.5),
                    "all-buy flow produced VPIN {} < 0.5", vpin);
            }
        }
    }

    // ── Kyle Lambda ──────────────────────────────────────────

    proptest! {
        /// Fewer than 10 observations always returns None — the
        /// estimator refuses to produce λ without enough samples
        /// for a meaningful regression.
        #[test]
        fn kyle_requires_10_samples(n in 0usize..10usize) {
            let mut kl = KyleLambda::new(100);
            for i in 0..n {
                kl.update(Decimal::new(i as i64, 2), Decimal::new(i as i64, 0));
            }
            prop_assert!(kl.lambda().is_none());
        }

        /// Zero OFI variance always returns None — the linear
        /// regression has no slope to compute. Catches a
        /// regression where divide-by-zero would surface as a
        /// spurious λ = 0.
        #[test]
        fn zero_variance_ofi_returns_none(
            n in 10usize..50usize,
            constant_ofi in -1000i64..1000i64,
            price_changes in proptest::collection::vec(
                -100_000i64..100_000i64,
                10..50,
            ),
        ) {
            let mut kl = KyleLambda::new(100);
            let ofi = Decimal::from(constant_ofi);
            for dp in price_changes.iter().take(n).copied() {
                kl.update(Decimal::new(dp, 4), ofi);
            }
            prop_assert!(kl.lambda().is_none(),
                "constant OFI should yield no λ");
        }

        /// With a perfect linear relationship ΔP = α·OFI (α > 0),
        /// the estimator recovers λ ≈ α. Verifies the covariance
        /// / variance arithmetic is correctly assembled.
        #[test]
        fn kyle_recovers_linear_coefficient(
            alpha_raw in 1i64..100i64,
            ofis in proptest::collection::vec(-100i64..100i64, 10..30),
        ) {
            // Require OFI variance > 0 — otherwise lambda() returns None
            // (handled in a separate property above).
            prop_assume!(ofis.iter().collect::<std::collections::HashSet<_>>().len() >= 2);
            let alpha = Decimal::new(alpha_raw, 4);  // 0.0001 .. 0.0100
            let mut kl = KyleLambda::new(100);
            for ofi_raw in &ofis {
                let ofi = Decimal::from(*ofi_raw);
                let dp = alpha * ofi;
                kl.update(dp, ofi);
            }
            let lambda = kl.lambda().expect("lambda should be defined for varied OFI");
            let diff = (lambda - alpha).abs();
            prop_assert!(diff < dec!(0.0001),
                "recovered λ={} far from α={} (diff={})", lambda, alpha, diff);
        }

        /// Window is bounded — feeding more than window_size
        /// observations does not overflow or break the estimator.
        /// Returns a finite Decimal when window is full.
        #[test]
        fn window_bounded(
            n in 20usize..300usize,
        ) {
            let mut kl = KyleLambda::new(50);
            for i in 0..n {
                let ofi = Decimal::from(((i as i64) % 7) - 3);  // -3..3 cycle
                let dp = ofi * Decimal::new(2, 3);  // 0.002·ofi
                kl.update(dp, ofi);
            }
            // Window keeps only the last 50.
            let lambda = kl.lambda();
            prop_assert!(lambda.is_some(), "full window should give λ");
        }
    }

    // ── AdverseSelection ─────────────────────────────────────

    proptest! {
        /// Fewer than 5 completed fills on a side always returns
        /// None — the estimator refuses to report until the
        /// window has enough samples for a meaningful average.
        #[test]
        fn per_side_requires_5_fills(
            n in 0usize..5usize,
            side in side_strat(),
            fill_price in price_strat_tox(),
        ) {
            let mut t = AdverseSelectionTracker::new(50);
            for _ in 0..n {
                seed_completed_fill(&mut t, side, fill_price, fill_price, fill_price);
            }
            prop_assert!(t.adverse_selection_bps_for_side(side).is_none());
        }

        /// Zero adverse selection — when mid_after equals the
        /// fill's benchmark, bps should be zero. Exact equality
        /// after averaging — catches rounding drift.
        #[test]
        fn no_price_move_yields_zero_bps(
            side in side_strat(),
            price in price_strat_tox(),
            n in 5usize..20usize,
        ) {
            let mut t = AdverseSelectionTracker::new(50);
            for _ in 0..n {
                seed_completed_fill(&mut t, side, price, price, price);
            }
            let bps = t.adverse_selection_bps_for_side(side).unwrap();
            prop_assert_eq!(bps, dec!(0));
        }

        /// The symmetric average is bounded by the per-side
        /// averages — i.e., the overall figure cannot exceed the
        /// worse of the two sides when both are populated.
        /// Catches weighting errors across the bid/ask split.
        #[test]
        fn symmetric_between_per_side(
            price in price_strat_tox(),
            n in 5usize..15usize,
        ) {
            let mut t = AdverseSelectionTracker::new(50);
            for _ in 0..n {
                seed_completed_fill(&mut t, Side::Buy, price, price, price - dec!(1));
                seed_completed_fill(&mut t, Side::Sell, price, price, price + dec!(2));
            }
            let bid = t.adverse_selection_bps_bid().unwrap();
            let ask = t.adverse_selection_bps_ask().unwrap();
            let sym = t.adverse_selection_bps().unwrap();
            let lo = bid.min(ask);
            let hi = bid.max(ask);
            prop_assert!(sym >= lo - dec!(0.01) && sym <= hi + dec!(0.01),
                "symmetric {} outside [{}, {}]", sym, lo, hi);
        }
    }
}
