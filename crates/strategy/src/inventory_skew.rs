use mm_common::types::{Qty, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

/// Advanced inventory management — beyond linear skew.
///
/// Professional MMs use multiple techniques:
/// 1. Quadratic penalty — aggressiveness increases exponentially near limits
/// 2. Dynamic sizing — order size shrinks as inventory grows
/// 3. Urgency unwinding — if stuck with inventory too long, actively unwind
/// 4. Asymmetric levels — more levels on the reducing side
pub struct AdvancedInventoryManager {
    /// Maximum allowed inventory.
    max_inventory: Decimal,
    /// Urgency timer: seconds we've held inventory above threshold.
    urgency_secs: u64,
    /// Threshold for urgency (fraction of max_inventory).
    urgency_threshold: Decimal,
    /// How long before urgency kicks in (seconds).
    urgency_delay_secs: u64,
}

impl AdvancedInventoryManager {
    pub fn new(max_inventory: Decimal) -> Self {
        Self {
            max_inventory,
            urgency_secs: 0,
            urgency_threshold: dec!(0.7), // 70% of max.
            urgency_delay_secs: 60,       // 1 minute.
        }
    }

    /// Quadratic penalty function.
    ///
    /// Returns a skew factor that grows quadratically as inventory approaches limit.
    /// q_frac ∈ [-1, 1] where 1 = max long, -1 = max short.
    /// Output: price offset in terms of spread multiplier.
    pub fn quadratic_skew(&self, inventory: Decimal) -> Decimal {
        if self.max_inventory.is_zero() {
            return dec!(0);
        }
        let q_frac = inventory / self.max_inventory;
        // Quadratic: sign(q) * q^2.
        // This is more aggressive near limits than linear.
        let sign = if q_frac >= dec!(0) { dec!(1) } else { dec!(-1) };
        sign * q_frac * q_frac
    }

    /// Dynamic order sizing — reduce size as inventory grows.
    ///
    /// At 0 inventory: full size.
    /// At max inventory on the INCREASING side: 0 size.
    /// On the REDUCING side: 1.5x size (incentivize reducing).
    pub fn dynamic_size(&self, base_size: Qty, inventory: Decimal, side: Side) -> Qty {
        if self.max_inventory.is_zero() {
            return base_size;
        }

        let q_frac = (inventory / self.max_inventory).abs();

        match side {
            Side::Buy => {
                if inventory > dec!(0) {
                    // Long inventory, buying more → decrease size.
                    let scale = (dec!(1) - q_frac).max(dec!(0));
                    base_size * scale
                } else {
                    // Short inventory, buying to reduce → increase size.
                    let scale = dec!(1) + q_frac * dec!(0.5);
                    base_size * scale
                }
            }
            Side::Sell => {
                if inventory < dec!(0) {
                    // Short inventory, selling more → decrease size.
                    let scale = (dec!(1) - q_frac).max(dec!(0));
                    base_size * scale
                } else {
                    // Long inventory, selling to reduce → increase size.
                    let scale = dec!(1) + q_frac * dec!(0.5);
                    base_size * scale
                }
            }
        }
    }

    /// Update urgency timer. Call this every second.
    pub fn tick(&mut self, inventory: Decimal) {
        let q_frac = (inventory / self.max_inventory).abs();
        if q_frac > self.urgency_threshold {
            self.urgency_secs += 1;
        } else {
            self.urgency_secs = 0;
        }
    }

    /// Is urgency mode active? If so, we need to actively unwind.
    pub fn is_urgent(&self) -> bool {
        self.urgency_secs > self.urgency_delay_secs
    }

    /// Urgency level [0, 1]. Higher = more aggressive unwinding.
    pub fn urgency_level(&self) -> Decimal {
        if !self.is_urgent() {
            return dec!(0);
        }
        let extra_secs = self.urgency_secs - self.urgency_delay_secs;
        // Ramp up over 60 seconds.
        let level = Decimal::from(extra_secs) / dec!(60);
        level.min(dec!(1))
    }

    /// Apply urgency to quotes — makes the reducing side more aggressive.
    pub fn apply_urgency(&self, quotes: &mut [QuotePair], inventory: Decimal, mid_price: Decimal) {
        if !self.is_urgent() {
            return;
        }

        let urgency = self.urgency_level();
        // Move the reducing side closer to mid by urgency * half_spread.
        let adjustment = mid_price * dec!(0.0005) * urgency; // Up to 5 bps.

        debug!(
            urgency = %urgency,
            adjustment = %adjustment,
            "urgency unwinding active"
        );

        for q in quotes.iter_mut() {
            if inventory > dec!(0) {
                // Long — make asks more aggressive (lower ask price).
                if let Some(ask) = &mut q.ask {
                    ask.price -= adjustment;
                }
            } else {
                // Short — make bids more aggressive (higher bid price).
                if let Some(bid) = &mut q.bid {
                    bid.price += adjustment;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quadratic_skew() {
        let mgr = AdvancedInventoryManager::new(dec!(1.0));

        // At 50% inventory: skew = 0.25 (quadratic).
        let skew = mgr.quadratic_skew(dec!(0.5));
        assert_eq!(skew, dec!(0.25));

        // At 100% inventory: skew = 1.0.
        let skew = mgr.quadratic_skew(dec!(1.0));
        assert_eq!(skew, dec!(1.0));

        // Negative inventory.
        let skew = mgr.quadratic_skew(dec!(-0.5));
        assert_eq!(skew, dec!(-0.25));
    }

    #[test]
    fn test_dynamic_sizing() {
        let mgr = AdvancedInventoryManager::new(dec!(1.0));
        let base = dec!(0.01);

        // No inventory — full size both sides.
        assert_eq!(mgr.dynamic_size(base, dec!(0), Side::Buy), base);
        assert_eq!(mgr.dynamic_size(base, dec!(0), Side::Sell), base);

        // Long 50% — buy size reduced, sell size increased.
        let buy = mgr.dynamic_size(base, dec!(0.5), Side::Buy);
        let sell = mgr.dynamic_size(base, dec!(0.5), Side::Sell);
        assert!(buy < base);
        assert!(sell > base);

        // At max inventory — buy size = 0.
        let buy = mgr.dynamic_size(base, dec!(1.0), Side::Buy);
        assert_eq!(buy, dec!(0));
    }

    #[test]
    fn test_urgency() {
        let mut mgr = AdvancedInventoryManager::new(dec!(1.0));
        let inv = dec!(0.8); // Above 70% threshold.

        for _ in 0..61 {
            mgr.tick(inv);
        }
        assert!(mgr.is_urgent());
        assert!(mgr.urgency_level() > dec!(0));
    }
}
