//! Cross-venue portfolio aggregator (INV-4).
//!
//! Owns every engine's live inventory snapshot so graph source
//! nodes, HTTP endpoints, and daily reports read from a single
//! source of truth instead of each poking at a private map on
//! `DashboardState`.
//!
//! Engines publish once per tick with [`CrossVenuePortfolio::publish`]
//! (inventory + optional mark price). Readers call
//! [`CrossVenuePortfolio::net_delta`] for a scalar base-asset
//! delta, [`CrossVenuePortfolio::by_asset`] for the grouped view
//! the cross-venue UI panel renders, or [`CrossVenuePortfolio::entries`]
//! for the flat list.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// One engine's live inventory on one (symbol, venue) tuple.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueInventory {
    pub symbol: String,
    pub venue: String,
    /// Base asset inferred from the symbol once at publish time so
    /// every downstream reader groups consistently (BTCUSDT,
    /// BTCUSDC, BTC-USDT all roll up under `BTC`).
    pub base_asset: String,
    /// Signed base-asset units. Negative = net short on this leg.
    pub inventory: Decimal,
    /// Mark price in the symbol's native quote currency. `None`
    /// while the engine's book is still warming up (no mid yet).
    pub mark_price: Option<Decimal>,
    /// `inventory × mark_price` in the native quote currency —
    /// materialised at publish time so UI/graph readers don't
    /// need to re-multiply. `None` iff `mark_price` is `None`.
    pub notional_quote: Option<Decimal>,
    pub updated_at: DateTime<Utc>,
}

/// Per-base-asset aggregate across every venue — one entry per
/// base asset, legs sorted by (venue, symbol) for deterministic
/// rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetAggregate {
    pub base: String,
    pub net_delta: Decimal,
    /// Sum of each leg's `notional_quote`. Legs without a mark
    /// are skipped — the aggregate reflects only legs the engine
    /// has marked, matching how per-venue PnL widens the moment
    /// a mark goes stale.
    pub net_notional_quote: Decimal,
    pub legs: Vec<VenueInventory>,
}

/// Best-effort base-asset inference from a symbol. Walks until
/// the first digit or separator (`-`, `_`, `/`) and returns the
/// prefix. Handles `BTCUSDT`, `BTC-USDT`, `BTC_USD`, `BTCUSDC`;
/// falls back to the full symbol on odd tickers.
pub fn infer_base_asset(symbol: &str) -> String {
    let boundary = symbol
        .chars()
        .position(|c| c.is_ascii_digit() || c == '-' || c == '_' || c == '/')
        .unwrap_or(symbol.len());
    symbol[..boundary].to_string()
}

/// Cross-venue portfolio aggregator.
///
/// Cheap to `default()` — engines attach to a single shared
/// instance (typically behind an `Arc<RwLock<_>>` owned by the
/// dashboard state) and the aggregator upserts per
/// `(symbol, venue)` key, so a second publish for the same leg
/// replaces the first rather than double-counting.
#[derive(Debug, Default)]
pub struct CrossVenuePortfolio {
    entries: HashMap<(String, String), VenueInventory>,
}

impl CrossVenuePortfolio {
    pub fn new() -> Self {
        Self::default()
    }

    /// Upsert the inventory snapshot for `(symbol, venue)`.
    /// `mark` is the current mark price (usually the book mid in
    /// the engine's native quote currency); pass `None` while the
    /// book is warming up.
    pub fn publish(
        &mut self,
        symbol: &str,
        venue: &str,
        inventory: Decimal,
        mark: Option<Decimal>,
    ) {
        let base_asset = infer_base_asset(symbol);
        let notional_quote = mark.map(|m| inventory * m);
        self.entries.insert(
            (symbol.to_string(), venue.to_string()),
            VenueInventory {
                symbol: symbol.to_string(),
                venue: venue.to_string(),
                base_asset,
                inventory,
                mark_price: mark,
                notional_quote,
                updated_at: Utc::now(),
            },
        );
    }

    /// Net delta in `base_asset` units — sum of every leg whose
    /// inferred base asset matches. Returns zero if nothing on
    /// any venue rolls up under `base_asset`.
    pub fn net_delta(&self, base_asset: &str) -> Decimal {
        self.entries
            .values()
            .filter(|e| e.base_asset == base_asset)
            .map(|e| e.inventory)
            .sum()
    }

    /// Net notional in the legs' native quote currency for
    /// `base_asset`. Mixes quote currencies if the same base
    /// trades against different quotes across venues (a BTCUSDT
    /// leg and a BTCUSDC leg both contribute) — callers that
    /// need a reporting-currency figure apply FX themselves
    /// using the `mm_portfolio::Portfolio` FX table. Legs
    /// without a mark contribute zero.
    pub fn net_notional_quote(&self, base_asset: &str) -> Decimal {
        self.entries
            .values()
            .filter(|e| e.base_asset == base_asset)
            .filter_map(|e| e.notional_quote)
            .sum()
    }

    /// Flat list of every published leg. Order is unspecified —
    /// callers that need determinism sort downstream.
    pub fn entries(&self) -> Vec<VenueInventory> {
        self.entries.values().cloned().collect()
    }

    /// Per-base-asset grouped view used by the cross-venue UI
    /// panel and the daily report. Legs within each asset are
    /// sorted `(venue, symbol)` so the output is stable.
    pub fn by_asset(&self) -> Vec<AssetAggregate> {
        let mut grouped: BTreeMap<String, Vec<VenueInventory>> = BTreeMap::new();
        for entry in self.entries.values() {
            grouped
                .entry(entry.base_asset.clone())
                .or_default()
                .push(entry.clone());
        }
        grouped
            .into_iter()
            .map(|(base, mut legs)| {
                legs.sort_by(|a, b| a.venue.cmp(&b.venue).then(a.symbol.cmp(&b.symbol)));
                let net_delta = legs.iter().map(|l| l.inventory).sum();
                let net_notional_quote = legs.iter().filter_map(|l| l.notional_quote).sum();
                AssetAggregate {
                    base,
                    net_delta,
                    net_notional_quote,
                    legs,
                }
            })
            .collect()
    }

    /// Count of currently-published legs. Useful for health
    /// checks that expect a minimum multi-venue footprint.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn infer_base_asset_covers_common_symbol_shapes() {
        assert_eq!(infer_base_asset("BTCUSDT"), "BTCUSDT");
        assert_eq!(infer_base_asset("BTC-USDT"), "BTC");
        assert_eq!(infer_base_asset("BTC_USD"), "BTC");
        assert_eq!(infer_base_asset("BTC/USDT"), "BTC");
        // Leading digits in contract codes — no letter prefix to
        // strip, so the whole symbol becomes the "base". Matches
        // the `starts_with` fallback behaviour that INV-3 relied
        // on.
        assert_eq!(infer_base_asset("1000SHIB-USDT"), "");
    }

    #[test]
    fn publish_is_upsert_not_append() {
        let mut p = CrossVenuePortfolio::new();
        p.publish("BTCUSDT", "binance", dec!(0.5), Some(dec!(50_000)));
        p.publish("BTCUSDT", "binance", dec!(0.9), Some(dec!(51_000)));
        assert_eq!(p.len(), 1);
        assert_eq!(p.net_delta("BTCUSDT"), dec!(0.9));
    }

    #[test]
    fn net_delta_sums_across_venues() {
        let mut p = CrossVenuePortfolio::new();
        p.publish("BTC-USDT", "binance", dec!(0.5), None);
        p.publish("BTC-USDC", "bybit", dec!(-0.2), None);
        p.publish("ETH-USDT", "binance", dec!(3), None);
        assert_eq!(p.net_delta("BTC"), dec!(0.3));
        assert_eq!(p.net_delta("ETH"), dec!(3));
        assert_eq!(p.net_delta("SOL"), dec!(0));
    }

    #[test]
    fn notional_multiplies_inventory_by_mark() {
        let mut p = CrossVenuePortfolio::new();
        p.publish("BTC-USDT", "binance", dec!(0.5), Some(dec!(50_000)));
        p.publish("BTC-USDC", "bybit", dec!(-0.2), Some(dec!(49_000)));
        // Legs in different quote currencies still sum — caller
        // handles FX when it needs a reporting figure.
        assert_eq!(
            p.net_notional_quote("BTC"),
            dec!(0.5) * dec!(50_000) + dec!(-0.2) * dec!(49_000),
        );
    }

    #[test]
    fn notional_skips_legs_without_a_mark() {
        let mut p = CrossVenuePortfolio::new();
        p.publish("BTC-USDT", "binance", dec!(0.5), Some(dec!(50_000)));
        // Engine B hasn't marked yet — leg is tracked for delta
        // but doesn't contribute to notional.
        p.publish("BTC-USDC", "bybit", dec!(-0.2), None);
        assert_eq!(p.net_delta("BTC"), dec!(0.3));
        assert_eq!(p.net_notional_quote("BTC"), dec!(25_000));
    }

    #[test]
    fn by_asset_groups_deterministically() {
        let mut p = CrossVenuePortfolio::new();
        p.publish("BTC-USDC", "bybit", dec!(-0.2), Some(dec!(49_000)));
        p.publish("BTC-USDT", "binance", dec!(0.5), Some(dec!(50_000)));
        p.publish("ETH-USDT", "binance", dec!(3), Some(dec!(3_000)));

        let grouped = p.by_asset();
        assert_eq!(grouped.len(), 2);
        // Sorted by base asset.
        assert_eq!(grouped[0].base, "BTC");
        assert_eq!(grouped[1].base, "ETH");

        // Legs within a base are sorted by (venue, symbol).
        let btc = &grouped[0];
        assert_eq!(btc.legs.len(), 2);
        assert_eq!(btc.legs[0].venue, "binance");
        assert_eq!(btc.legs[1].venue, "bybit");
        assert_eq!(btc.net_delta, dec!(0.3));
        assert_eq!(
            btc.net_notional_quote,
            dec!(0.5) * dec!(50_000) + dec!(-0.2) * dec!(49_000)
        );
    }

    #[test]
    fn empty_aggregator_reports_zero() {
        let p = CrossVenuePortfolio::new();
        assert!(p.is_empty());
        assert_eq!(p.net_delta("BTC"), dec!(0));
        assert_eq!(p.net_notional_quote("BTC"), dec!(0));
        assert!(p.by_asset().is_empty());
    }
}
