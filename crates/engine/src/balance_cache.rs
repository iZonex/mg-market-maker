use mm_common::types::{Balance, Price, Qty, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::warn;

/// Local balance cache with order reservation.
///
/// Before placing an order, we "reserve" the required balance.
/// This prevents over-submitting orders beyond available funds.
pub struct BalanceCache {
    /// Available balances per asset (total - locked - reserved).
    balances: HashMap<String, AssetBalance>,
}

#[derive(Debug, Clone)]
struct AssetBalance {
    total: Decimal,
    locked: Decimal,
    /// Our local reservations for pending orders.
    reserved: Decimal,
}

impl AssetBalance {
    fn available(&self) -> Decimal {
        (self.total - self.locked - self.reserved).max(dec!(0))
    }
}

impl BalanceCache {
    pub fn new() -> Self {
        Self {
            balances: HashMap::new(),
        }
    }

    /// Update from exchange balance query.
    pub fn update_from_exchange(&mut self, balances: &[Balance]) {
        for b in balances {
            let entry = self
                .balances
                .entry(b.asset.clone())
                .or_insert(AssetBalance {
                    total: dec!(0),
                    locked: dec!(0),
                    reserved: dec!(0),
                });
            entry.total = b.total;
            entry.locked = b.locked;
            // Don't reset reserved — those are our pending orders.
        }
    }

    /// Get available balance for an asset.
    pub fn available(&self, asset: &str) -> Decimal {
        self.balances
            .get(asset)
            .map(|b| b.available())
            .unwrap_or(dec!(0))
    }

    /// Check if we can afford an order. Returns true if sufficient balance.
    pub fn can_afford(
        &self,
        side: Side,
        price: Price,
        qty: Qty,
        base_asset: &str,
        quote_asset: &str,
    ) -> bool {
        match side {
            Side::Buy => {
                // Need quote asset: price * qty.
                let required = price * qty;
                self.available(quote_asset) >= required
            }
            Side::Sell => {
                // Need base asset: qty.
                self.available(base_asset) >= qty
            }
        }
    }

    /// Reserve balance for a pending order.
    pub fn reserve(
        &mut self,
        side: Side,
        price: Price,
        qty: Qty,
        base_asset: &str,
        quote_asset: &str,
    ) -> bool {
        let (asset, amount) = match side {
            Side::Buy => (quote_asset, price * qty),
            Side::Sell => (base_asset, qty),
        };

        let entry = self.balances.get_mut(asset);
        match entry {
            Some(b) if b.available() >= amount => {
                b.reserved += amount;
                true
            }
            _ => {
                warn!(
                    asset = asset,
                    required = %amount,
                    available = %self.available(asset),
                    "insufficient balance to reserve"
                );
                false
            }
        }
    }

    /// Release a reservation (on cancel or fill).
    pub fn release(
        &mut self,
        side: Side,
        price: Price,
        qty: Qty,
        base_asset: &str,
        quote_asset: &str,
    ) {
        let (asset, amount) = match side {
            Side::Buy => (quote_asset, price * qty),
            Side::Sell => (base_asset, qty),
        };
        if let Some(b) = self.balances.get_mut(asset) {
            b.reserved = (b.reserved - amount).max(dec!(0));
        }
    }

    /// Reset all reservations (e.g., after cancel_all).
    pub fn reset_reservations(&mut self) {
        for b in self.balances.values_mut() {
            b.reserved = dec!(0);
        }
    }
}

impl Default for BalanceCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reserve_and_check() {
        let mut cache = BalanceCache::new();
        cache.update_from_exchange(&[
            Balance {
                asset: "USDT".into(),
                total: dec!(1000),
                locked: dec!(0),
                available: dec!(1000),
            },
            Balance {
                asset: "BTC".into(),
                total: dec!(0.1),
                locked: dec!(0),
                available: dec!(0.1),
            },
        ]);

        // Can afford a buy of 0.01 BTC at 50000.
        assert!(cache.can_afford(Side::Buy, dec!(50000), dec!(0.01), "BTC", "USDT"));

        // Reserve it.
        assert!(cache.reserve(Side::Buy, dec!(50000), dec!(0.01), "BTC", "USDT"));

        // Available USDT reduced by 500.
        assert_eq!(cache.available("USDT"), dec!(500));

        // Can't afford another 0.02 BTC (would need 1000 more).
        assert!(!cache.can_afford(Side::Buy, dec!(50000), dec!(0.02), "BTC", "USDT"));

        // Release reservation.
        cache.release(Side::Buy, dec!(50000), dec!(0.01), "BTC", "USDT");
        assert_eq!(cache.available("USDT"), dec!(1000));
    }
}
