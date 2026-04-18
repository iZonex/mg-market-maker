//! Multi-Venue Level 3.C — global portfolio balance tracker.
//!
//! Aggregates `(venue, asset) → Balance` entries (published by each
//! engine via the DataBus) into a single portfolio view. Two readers:
//!
//!   · Per-asset net delta — long BTC-spot + short BTC-perp = 0,
//!     so a strategy looking at "my BTC exposure" sees neutral
//!     rather than "I'm long on Binance AND short on Bybit".
//!   · Per-venue totals — how much USDT is parked where, for
//!     rebalancer decisions.
//!
//! Intentionally a thin aggregator over the existing DataBus
//! `balances` map. The bus already serialises writes; this type
//! just reads + derives. That keeps it stateless enough to be
//! rebuilt on every graph tick without a persistence pass.

use rust_decimal::Decimal;
use std::collections::HashMap;

/// Per-asset aggregate. `long` + `short` are quote-denominated
/// where they need to be combined (perp shorts carry a negative
/// sign in the `net` tally).
#[derive(Debug, Clone, Default)]
pub struct AssetPortfolio {
    /// Sum of balances across every venue where we hold this
    /// asset. Matches the wallet-style "how much do I own in
    /// total" accounting.
    pub wallet_total: Decimal,
    /// Sum of `available` balances — wallet minus reserved /
    /// locked / margin. The strategy's working capital view.
    pub available: Decimal,
    /// Net delta = long spot + long perp − short perp (etc.).
    /// Positive = net long. Hedge-pair wallets cancel here even
    /// though they have a positive `wallet_total`.
    pub net_delta: Decimal,
    /// Count of venues we have any holdings on. Used by the
    /// rebalancer decision ("keep capital on ≥ N venues for
    /// withdrawal latency").
    pub venue_count: usize,
}

/// Portfolio snapshot — derived from a DataBus balances map.
#[derive(Debug, Default)]
pub struct PortfolioBalanceTracker {
    /// Per-asset aggregates. Rebuilt on every `refresh`.
    assets: HashMap<String, AssetPortfolio>,
    /// Sum of `available` USD-quoted balances per venue, built on
    /// the fly from the quote-asset entries (USDT/USDC). Useful
    /// for rebalancer snapshots.
    per_venue_quote_available: HashMap<String, Decimal>,
}

/// Stream shape the tracker expects. Matches the DataBus's
/// `BalanceEntry` by field names so the engine can plug them
/// directly — kept independent of the dashboard crate to avoid a
/// dep cycle (mm-risk ← mm-dashboard would loop).
#[derive(Debug, Clone)]
pub struct BalanceInput {
    pub venue: String,
    pub asset: String,
    pub total: Decimal,
    pub available: Decimal,
    /// Reserved / locked / margin portion. Subtracted from
    /// `total` when computing `available`.
    pub reserved: Decimal,
    /// True when this balance represents a short leg (perp). The
    /// net-delta accounting subtracts instead of adds.
    pub short_leg: bool,
}

/// Common quote assets — used to populate `per_venue_quote_available`.
const QUOTE_ASSETS: &[&str] = &["USDT", "USDC", "USD", "BUSD", "DAI"];

impl PortfolioBalanceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from a fresh snapshot. Callers typically iterate
    /// the DataBus and push every entry into an input vec, then
    /// hand it to this method on every graph tick.
    pub fn refresh(&mut self, entries: &[BalanceInput]) {
        self.assets.clear();
        self.per_venue_quote_available.clear();
        for e in entries {
            let port = self.assets.entry(e.asset.clone()).or_default();
            port.wallet_total += e.total;
            port.available += e.available;
            let contribution = if e.short_leg { -e.total } else { e.total };
            port.net_delta += contribution;
            port.venue_count += 1;
            if QUOTE_ASSETS.iter().any(|q| e.asset.eq_ignore_ascii_case(q)) {
                *self
                    .per_venue_quote_available
                    .entry(e.venue.clone())
                    .or_default() += e.available;
            }
        }
    }

    /// Per-asset aggregate lookup.
    pub fn asset(&self, asset: &str) -> Option<&AssetPortfolio> {
        self.assets.get(asset)
    }

    /// Net delta shortcut — zero when we have no position in the
    /// asset, otherwise the sum net of short legs.
    pub fn net_delta(&self, asset: &str) -> Decimal {
        self.assets
            .get(asset)
            .map(|p| p.net_delta)
            .unwrap_or(Decimal::ZERO)
    }

    /// Total available working capital in the named quote asset
    /// on this venue. Rebalancer reads this to decide whether to
    /// move USDT from an over-funded venue.
    pub fn quote_available(&self, venue: &str) -> Decimal {
        self.per_venue_quote_available
            .get(venue)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    /// How many unique assets we hold anything in. Portfolio view
    /// counter.
    pub fn asset_count(&self) -> usize {
        self.assets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn e(
        venue: &str,
        asset: &str,
        total: Decimal,
        available: Decimal,
        short_leg: bool,
    ) -> BalanceInput {
        BalanceInput {
            venue: venue.into(),
            asset: asset.into(),
            total,
            available,
            reserved: total - available,
            short_leg,
        }
    }

    #[test]
    fn long_spot_plus_short_perp_nets_to_zero() {
        let mut p = PortfolioBalanceTracker::new();
        p.refresh(&[
            e("binance", "BTC", dec!(1), dec!(1), false),
            e("bybit", "BTC", dec!(1), dec!(1), true),
        ]);
        let btc = p.asset("BTC").unwrap();
        assert_eq!(btc.wallet_total, dec!(2));
        assert_eq!(btc.venue_count, 2);
        assert_eq!(btc.net_delta, Decimal::ZERO);
        assert_eq!(p.net_delta("BTC"), Decimal::ZERO);
    }

    #[test]
    fn quote_available_accumulates_across_stablecoins() {
        let mut p = PortfolioBalanceTracker::new();
        p.refresh(&[
            e("binance", "USDT", dec!(500), dec!(400), false),
            e("binance", "USDC", dec!(100), dec!(90), false),
            e("bybit", "USDT", dec!(1_000), dec!(950), false),
        ]);
        assert_eq!(p.quote_available("binance"), dec!(490));
        assert_eq!(p.quote_available("bybit"), dec!(950));
        assert_eq!(p.quote_available("unknown"), Decimal::ZERO);
    }

    #[test]
    fn refresh_is_idempotent() {
        let mut p = PortfolioBalanceTracker::new();
        let input = [e("binance", "BTC", dec!(2), dec!(2), false)];
        p.refresh(&input);
        p.refresh(&input);
        assert_eq!(p.asset("BTC").unwrap().wallet_total, dec!(2));
    }
}
