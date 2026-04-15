use mm_common::types::{Balance, Price, Qty, Side, WalletType};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::warn;

/// Local balance cache with order reservation.
///
/// Before placing an order, we "reserve" the required balance —
/// this prevents over-submitting orders beyond available funds.
///
/// The cache is keyed on `(asset, WalletType)` so two connectors
/// reporting the "same" asset on different sub-accounts (e.g.
/// Binance spot BTC vs Binance USDⓈ-M margin BTC) never overwrite
/// each other. See `docs/research/spot-mm-specifics.md` §5 "Wallet
/// topology" for the motivation.
///
/// Single-product engines construct the cache via
/// [`BalanceCache::new_for`] with their connector's default wallet
/// and use the convenience methods (`available`, `can_afford`,
/// `reserve`, `release`) which implicitly consult that wallet.
/// Dual-connector engines (Sprint G) use `available_in` /
/// `can_afford_in` to target a specific wallet explicitly.
pub struct BalanceCache {
    balances: HashMap<(String, WalletType), AssetBalance>,
    /// Wallet the convenience methods (without a wallet arg) act on.
    /// Set by the engine from its connector's `product().default_wallet()`.
    configured_wallet: WalletType,
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
    /// Default constructor — assumes spot. Prefer `new_for` in new
    /// code so the wallet is explicit.
    pub fn new() -> Self {
        Self::new_for(WalletType::Spot)
    }

    /// Construct a cache whose convenience methods (no wallet arg)
    /// implicitly act on `wallet`. The engine passes its connector's
    /// `product().default_wallet()` here.
    pub fn new_for(wallet: WalletType) -> Self {
        Self {
            balances: HashMap::new(),
            configured_wallet: wallet,
        }
    }

    /// Update from exchange balance query. Each balance is stored
    /// under `(asset, balance.wallet)` so balances from different
    /// sub-accounts coexist without overwriting each other.
    pub fn update_from_exchange(&mut self, balances: &[Balance]) {
        for b in balances {
            let entry = self
                .balances
                .entry((b.asset.clone(), b.wallet))
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

    /// Get available balance on the configured wallet.
    pub fn available(&self, asset: &str) -> Decimal {
        self.available_in(asset, self.configured_wallet)
    }

    /// Get available balance on a specific wallet.
    pub fn available_in(&self, asset: &str, wallet: WalletType) -> Decimal {
        self.balances
            .get(&(asset.to_string(), wallet))
            .map(|b| b.available())
            .unwrap_or(dec!(0))
    }

    /// Get the **total** balance on a specific wallet —
    /// `free + locked`, before our pending-order reservations
    /// are subtracted. This is the figure the inventory-vs-
    /// wallet drift reconciler compares against: it reflects
    /// the true on-venue holding of the asset, independent of
    /// our local quoting state.
    pub fn total_in(&self, asset: &str, wallet: WalletType) -> Decimal {
        self.balances
            .get(&(asset.to_string(), wallet))
            .map(|b| b.total)
            .unwrap_or(dec!(0))
    }

    /// Check if we can afford an order on the configured wallet.
    pub fn can_afford(
        &self,
        side: Side,
        price: Price,
        qty: Qty,
        base_asset: &str,
        quote_asset: &str,
    ) -> bool {
        self.can_afford_in(
            side,
            price,
            qty,
            base_asset,
            quote_asset,
            self.configured_wallet,
        )
    }

    /// Check if we can afford an order on an explicit wallet.
    pub fn can_afford_in(
        &self,
        side: Side,
        price: Price,
        qty: Qty,
        base_asset: &str,
        quote_asset: &str,
        wallet: WalletType,
    ) -> bool {
        match side {
            Side::Buy => {
                let required = price * qty;
                self.available_in(quote_asset, wallet) >= required
            }
            Side::Sell => self.available_in(base_asset, wallet) >= qty,
        }
    }

    /// Reserve balance for a pending order on the configured wallet.
    pub fn reserve(
        &mut self,
        side: Side,
        price: Price,
        qty: Qty,
        base_asset: &str,
        quote_asset: &str,
    ) -> bool {
        self.reserve_in(
            side,
            price,
            qty,
            base_asset,
            quote_asset,
            self.configured_wallet,
        )
    }

    /// Reserve balance on an explicit wallet.
    pub fn reserve_in(
        &mut self,
        side: Side,
        price: Price,
        qty: Qty,
        base_asset: &str,
        quote_asset: &str,
        wallet: WalletType,
    ) -> bool {
        let (asset, amount) = match side {
            Side::Buy => (quote_asset, price * qty),
            Side::Sell => (base_asset, qty),
        };

        let entry = self.balances.get_mut(&(asset.to_string(), wallet));
        match entry {
            Some(b) if b.available() >= amount => {
                b.reserved += amount;
                true
            }
            _ => {
                warn!(
                    asset = asset,
                    wallet = ?wallet,
                    required = %amount,
                    available = %self.available_in(asset, wallet),
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
        self.release_in(
            side,
            price,
            qty,
            base_asset,
            quote_asset,
            self.configured_wallet,
        );
    }

    /// Release a reservation on an explicit wallet.
    pub fn release_in(
        &mut self,
        side: Side,
        price: Price,
        qty: Qty,
        base_asset: &str,
        quote_asset: &str,
        wallet: WalletType,
    ) {
        let (asset, amount) = match side {
            Side::Buy => (quote_asset, price * qty),
            Side::Sell => (base_asset, qty),
        };
        if let Some(b) = self.balances.get_mut(&(asset.to_string(), wallet)) {
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
                wallet: mm_common::types::WalletType::Spot,
                total: dec!(1000),
                locked: dec!(0),
                available: dec!(1000),
            },
            Balance {
                asset: "BTC".into(),
                wallet: mm_common::types::WalletType::Spot,
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

        let _ = "marker — stop";
    }

    /// Regression: two balances for the same asset but different
    /// wallets MUST NOT overwrite each other in the cache. Before
    /// Sprint B, `BalanceCache` keyed on `asset: String` and the
    /// second `update_from_exchange` call would silently clobber
    /// the first.
    #[test]
    fn wallet_types_do_not_collide() {
        let mut cache = BalanceCache::new_for(WalletType::Spot);
        cache.update_from_exchange(&[
            Balance {
                asset: "BTC".into(),
                wallet: WalletType::Spot,
                total: dec!(1),
                locked: dec!(0),
                available: dec!(1),
            },
            Balance {
                asset: "BTC".into(),
                wallet: WalletType::UsdMarginedFutures,
                total: dec!(0.5),
                locked: dec!(0),
                available: dec!(0.5),
            },
        ]);
        // Configured wallet is Spot; `.available` returns the spot
        // balance, not the futures one.
        assert_eq!(cache.available("BTC"), dec!(1));
        // Explicit wallet query returns the right bucket per side.
        assert_eq!(cache.available_in("BTC", WalletType::Spot), dec!(1));
        assert_eq!(
            cache.available_in("BTC", WalletType::UsdMarginedFutures),
            dec!(0.5)
        );
    }

    /// The legacy trailing block of the test that was already there;
    /// kept so nothing below it drifts.
    #[test]
    fn legacy_reserve_flow_still_works() {
        let mut cache = BalanceCache::new();
        cache.update_from_exchange(&[
            Balance {
                asset: "USDT".into(),
                wallet: WalletType::Spot,
                total: dec!(1000),
                locked: dec!(0),
                available: dec!(1000),
            },
            Balance {
                asset: "BTC".into(),
                wallet: WalletType::Spot,
                total: dec!(0.1),
                locked: dec!(0),
                available: dec!(0.1),
            },
        ]);
        assert!(cache.can_afford(Side::Buy, dec!(50000), dec!(0.01), "BTC", "USDT"));
        assert!(cache.reserve(Side::Buy, dec!(50000), dec!(0.01), "BTC", "USDT"));
        assert_eq!(cache.available("USDT"), dec!(500));
        assert!(!cache.can_afford(Side::Buy, dec!(50000), dec!(0.02), "BTC", "USDT"));

        // Release reservation.
        cache.release(Side::Buy, dec!(50000), dec!(0.01), "BTC", "USDT");
        assert_eq!(cache.available("USDT"), dec!(1000));
    }
}
