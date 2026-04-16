use chrono::{DateTime, Utc};
use mm_common::types::{Price, Qty, Quote, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info};

/// Time-Weighted Average Price (TWAP) execution algorithm.
///
/// Used for:
/// - Graceful inventory unwinding (kill switch level 4)
/// - Large position liquidation
/// - Scheduled rebalancing
///
/// Splits a large order into equal slices over a time window,
/// placing each slice as a limit order near the mid.
pub struct TwapExecutor {
    /// Target: total quantity to execute.
    pub target_qty: Qty,
    /// Side to execute.
    pub side: Side,
    /// Symbol.
    pub symbol: String,
    /// Total duration in seconds.
    pub duration_secs: u64,
    /// Number of slices.
    pub num_slices: u32,
    /// Quantity per slice.
    slice_qty: Qty,
    /// Executed so far.
    pub executed_qty: Qty,
    /// Current slice index.
    current_slice: u32,
    /// Start time.
    started_at: DateTime<Utc>,
    /// Aggressiveness: how far from mid to place (in bps).
    /// 0 = at mid (aggressive), 10+ = passive.
    pub aggressiveness_bps: Decimal,
    /// Is the TWAP active?
    pub active: bool,
}

impl TwapExecutor {
    pub fn new(
        symbol: String,
        side: Side,
        target_qty: Qty,
        duration_secs: u64,
        num_slices: u32,
        aggressiveness_bps: Decimal,
    ) -> Self {
        let slice_qty = target_qty / Decimal::from(num_slices.max(1));

        info!(
            %symbol, ?side, %target_qty, duration_secs, num_slices,
            slice_qty = %slice_qty,
            "TWAP executor created"
        );

        Self {
            target_qty,
            side,
            symbol,
            duration_secs,
            num_slices,
            slice_qty,
            executed_qty: dec!(0),
            current_slice: 0,
            started_at: Utc::now(),
            aggressiveness_bps,
            active: true,
        }
    }

    /// Check if it's time for the next slice. Returns a Quote if yes.
    pub fn next_slice(&mut self, mid_price: Price) -> Option<Quote> {
        if !self.active || self.is_complete() {
            return None;
        }

        let elapsed = (Utc::now() - self.started_at).num_seconds() as u64;
        let expected_slice = (elapsed * self.num_slices as u64)
            .checked_div(self.duration_secs)
            .unwrap_or(self.num_slices as u64) as u32;

        if self.current_slice >= expected_slice {
            return None; // Not time yet.
        }

        self.current_slice = expected_slice;
        let remaining = self.target_qty - self.executed_qty;
        let qty = self.slice_qty.min(remaining);

        if qty <= dec!(0) {
            self.active = false;
            return None;
        }

        // Price: slightly aggressive from mid.
        let offset = mid_price * self.aggressiveness_bps / dec!(10_000);
        let price = match self.side {
            Side::Buy => mid_price + offset, // Willing to pay slightly above mid.
            Side::Sell => mid_price - offset, // Willing to sell slightly below mid.
        };

        debug!(
            slice = self.current_slice,
            total_slices = self.num_slices,
            %qty,
            %price,
            executed = %self.executed_qty,
            remaining = %remaining,
            "TWAP slice"
        );

        Some(Quote {
            side: self.side,
            price,
            qty,
        })
    }

    /// Record execution of a slice.
    pub fn on_fill(&mut self, qty: Qty) {
        self.executed_qty += qty;
        if self.is_complete() {
            self.active = false;
            info!(
                symbol = %self.symbol,
                total = %self.executed_qty,
                "TWAP execution complete"
            );
        }
    }

    /// Is the TWAP complete?
    pub fn is_complete(&self) -> bool {
        self.executed_qty >= self.target_qty
    }

    /// Progress as a fraction [0, 1].
    pub fn progress(&self) -> Decimal {
        if self.target_qty.is_zero() {
            return dec!(1);
        }
        (self.executed_qty / self.target_qty).min(dec!(1))
    }

    /// Cancel the TWAP.
    pub fn cancel(&mut self) {
        info!(
            symbol = %self.symbol,
            executed = %self.executed_qty,
            target = %self.target_qty,
            "TWAP cancelled"
        );
        self.active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_twap_slicing() {
        let mut twap = TwapExecutor::new("BTCUSDT".into(), Side::Sell, dec!(0.1), 10, 5, dec!(2));
        // Immediately, slice 0 should fire (elapsed=0, expected=0, current=0).
        // Actually need time to pass. For testing, set started_at in the past.
        twap.started_at = Utc::now() - chrono::Duration::seconds(3);

        let quote = twap.next_slice(dec!(50000));
        assert!(quote.is_some());
        let q = quote.unwrap();
        assert_eq!(q.side, Side::Sell);
        assert_eq!(q.qty, dec!(0.02)); // 0.1 / 5 slices.
    }

    #[test]
    fn test_twap_completion() {
        let mut twap = TwapExecutor::new("BTCUSDT".into(), Side::Buy, dec!(0.05), 1, 2, dec!(0));

        twap.on_fill(dec!(0.025));
        assert!(!twap.is_complete());
        assert!(twap.progress() == dec!(0.5));

        twap.on_fill(dec!(0.025));
        assert!(twap.is_complete());
        assert!(!twap.active);
    }
}
