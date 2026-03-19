use mm_common::types::{Side, Trade};
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

        // If bucket is full, finalize it.
        while self.current_total_vol >= self.bucket_size {
            let overflow = self.current_total_vol - self.bucket_size;
            let imbalance = (self.current_buy_vol - self.current_sell_vol).abs();

            self.buckets.push_back((imbalance, self.bucket_size));
            if self.buckets.len() > self.num_buckets {
                self.buckets.pop_front();
            }

            // Carry overflow into next bucket.
            // Approximate: attribute overflow proportionally.
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
        let completed: Vec<&FillOutcome> = self
            .fills
            .iter()
            .filter(|f| f.mid_after.is_some())
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
        if avg > dec!(5) {
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
}
