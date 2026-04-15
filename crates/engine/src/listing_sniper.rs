//! Listing sniper / new-symbol discovery (Epic F sub-component #1,
//! stage-2).
//!
//! Periodically scans every connector's
//! [`ExchangeConnector::list_symbols`] and emits events when a new
//! symbol appears that the engine has never seen before — or a
//! previously-known symbol disappears from the venue. Operators
//! wire this into their orchestration layer to auto-spawn a
//! probation engine for the new pair (probation = wide spreads,
//! small size, ~24h). Stage-2 only ships the **discovery** half;
//! the auto-spawn half is a stage-3 follow-up tracked in the Epic
//! F closure note.
//!
//! The sniper is a standalone module, deliberately parallel to
//! [`crate::pair_lifecycle::PairLifecycleManager`]: lifecycle
//! tracks `trading_status` transitions for **subscribed** symbols
//! (halt/resume/delist); the sniper tracks symbol set deltas for
//! the **whole venue universe** (listed/removed). The two share
//! no state and can run on independent cadences.
//!
//! # Behaviour
//!
//! * The very first `scan` against a venue **seeds** the cache
//!   without firing events. Otherwise every existing symbol on
//!   Binance spot would fire on startup.
//! * Subsequent scans diff the new snapshot against the cache:
//!   any symbol in `current` but not in `known` fires
//!   [`ListingEvent::Discovered`]; any symbol in `known` but not
//!   in `current` fires [`ListingEvent::Removed`]. The cache is
//!   updated in place.
//! * Connector-side failures (network, parse error, "unsupported"
//!   from the trait default) propagate as `Err`. An empty result
//!   from the venue is **not** an error — it is treated as the
//!   symbol set becoming empty and every previously-known symbol
//!   fires `Removed`. Consumers should not scan venues that
//!   return `Err(unsupported)`; the sniper does not mutate state
//!   when the connector errors out.
//!
//! # Scope (stage-2)
//!
//! This module only does **discovery**. Consumers receive
//! `Vec<ListingEvent>` and decide what to do with them (spawn a
//! probation engine, notify the operator via Telegram, log to
//! audit trail, …). There is no engine integration here on
//! purpose: the whole point of the sniper is that it runs
//! independently of any running market-maker engine so operators
//! can surface a new listing before a full engine bootstrap
//! completes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mm_common::types::ProductSpec;
use mm_exchange_core::connector::{ExchangeConnector, VenueId};

/// Standalone discovery cache for new listings and removed
/// symbols on every venue the operator points it at. See the
/// module docs for the scan/seed/diff semantics.
#[derive(Debug, Default)]
pub struct ListingSniper {
    /// Per-venue cache of known symbol names. Symbols not in this
    /// set on a fresh `scan` fire `Discovered` events.
    known: HashMap<VenueId, HashSet<String>>,
    /// Which venues have been seeded by a prior scan. The first
    /// scan against a venue populates `known` but fires no
    /// events; subsequent scans diff against it.
    seeded: HashSet<VenueId>,
}

/// An event emitted by [`ListingSniper::scan`] when the venue's
/// symbol set changes across two scans.
#[derive(Debug, Clone, PartialEq)]
pub enum ListingEvent {
    /// A symbol is present on the venue that the sniper did not
    /// see on the previous scan. Carries the full [`ProductSpec`]
    /// as the venue reported it so the consumer can spin up a
    /// probation engine without a second round-trip.
    Discovered {
        venue: VenueId,
        symbol: String,
        spec: ProductSpec,
    },
    /// A symbol the sniper saw on the previous scan is no longer
    /// present on the venue. The sniper does **not** carry a
    /// trailing [`ProductSpec`] on this event — the consumer is
    /// expected to key off `symbol` against its existing state
    /// (orchestration layer, pair lifecycle manager, …).
    Removed { venue: VenueId, symbol: String },
}

impl ListingSniper {
    /// Construct an empty sniper. No venues are seeded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan one connector and return the listing-event diff for
    /// this venue since the last scan.
    ///
    /// Behaviour matches the module docs:
    ///
    /// * First scan against a given venue seeds the cache and
    ///   returns `Ok(vec![])`.
    /// * Subsequent scans diff `current` against `known` for the
    ///   venue, emit `Discovered` / `Removed` events in the order
    ///   `Discovered` then `Removed`, then update the cache in
    ///   place.
    /// * Connector errors propagate as `Err` **without** mutating
    ///   any sniper state. The next scan retries cleanly.
    pub async fn scan(
        &mut self,
        connector: &Arc<dyn ExchangeConnector>,
    ) -> anyhow::Result<Vec<ListingEvent>> {
        let venue = connector.venue_id();
        let specs = connector.list_symbols().await?;

        // Collapse duplicates (defensive — venues shouldn't
        // return duplicates, but an HL universe with a pair name
        // collision would otherwise double-count).
        let mut by_symbol: HashMap<String, ProductSpec> = HashMap::new();
        for spec in specs {
            by_symbol.entry(spec.symbol.clone()).or_insert(spec);
        }
        let current_set: HashSet<String> = by_symbol.keys().cloned().collect();

        // First scan — seed and return.
        if !self.seeded.contains(&venue) {
            self.known.insert(venue, current_set);
            self.seeded.insert(venue);
            return Ok(Vec::new());
        }

        // Diff against cached `known` for this venue.
        let mut events = Vec::new();
        let known = self.known.entry(venue).or_default();

        // Discovered: in current, not in known.
        let mut new_symbols: Vec<&String> = current_set.difference(known).collect();
        new_symbols.sort(); // Deterministic event order for tests / replay.
        for sym in new_symbols {
            let spec = by_symbol
                .get(sym)
                .cloned()
                .expect("symbol present in current_set must have a spec");
            events.push(ListingEvent::Discovered {
                venue,
                symbol: sym.clone(),
                spec,
            });
        }

        // Removed: in known, not in current.
        let mut gone_symbols: Vec<&String> = known.difference(&current_set).collect();
        gone_symbols.sort();
        for sym in gone_symbols {
            events.push(ListingEvent::Removed {
                venue,
                symbol: sym.clone(),
            });
        }

        // Commit new state.
        *known = current_set;
        Ok(events)
    }

    /// Read-only view of the current known-symbol cache for a
    /// venue. Returns `None` when the venue has never been
    /// scanned. Useful for test assertions and operator debugging
    /// via the dashboard.
    pub fn known_symbols(&self, venue: VenueId) -> Option<&HashSet<String>> {
        self.known.get(&venue)
    }

    /// Forget one venue's cache entirely. The next `scan` against
    /// that connector will re-seed (no events), matching the
    /// first-scan semantics. Use after a long downtime window
    /// where symbol-set drift would otherwise fire a flood of
    /// `Discovered` / `Removed` events for known state.
    pub fn forget(&mut self, venue: VenueId) {
        self.known.remove(&venue);
        self.seeded.remove(&venue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockConnector;
    use mm_common::types::TradingStatus;
    use mm_exchange_core::connector::VenueProduct;
    use rust_decimal_macros::dec;

    fn spec(symbol: &str) -> ProductSpec {
        ProductSpec {
            symbol: symbol.to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: TradingStatus::Trading,
        }
    }

    fn specs<'a>(syms: impl IntoIterator<Item = &'a str>) -> Vec<ProductSpec> {
        syms.into_iter().map(spec).collect()
    }

    fn mock(venue: VenueId) -> Arc<MockConnector> {
        Arc::new(MockConnector::new(venue, VenueProduct::Spot))
    }

    fn as_trait(m: &Arc<MockConnector>) -> Arc<dyn ExchangeConnector> {
        m.clone() as Arc<dyn ExchangeConnector>
    }

    /// 1. First scan against a fresh venue seeds the cache
    ///    without firing any events.
    #[tokio::test]
    async fn first_scan_seeds_without_events() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT", "ETHUSDT"]));
        let mut sniper = ListingSniper::new();
        let events = sniper.scan(&as_trait(&m)).await.unwrap();
        assert!(events.is_empty());
        let known = sniper.known_symbols(VenueId::Binance).unwrap();
        assert_eq!(known.len(), 2);
        assert!(known.contains("BTCUSDT"));
        assert!(known.contains("ETHUSDT"));
    }

    /// 2. Second scan with the same symbols returns empty.
    #[tokio::test]
    async fn second_scan_with_same_symbols_is_empty() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT", "ETHUSDT"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&m)).await.unwrap(); // seed
        let events = sniper.scan(&as_trait(&m)).await.unwrap();
        assert!(events.is_empty());
    }

    /// 3. A new symbol fires `Discovered` with the venue's spec.
    #[tokio::test]
    async fn new_symbol_fires_discovered() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&m)).await.unwrap(); // seed with BTCUSDT

        m.set_list_symbols_ok(specs(["BTCUSDT", "NEWUSDT"]));
        let events = sniper.scan(&as_trait(&m)).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ListingEvent::Discovered { venue, symbol, .. } => {
                assert_eq!(*venue, VenueId::Binance);
                assert_eq!(symbol, "NEWUSDT");
            }
            other => panic!("expected Discovered, got {other:?}"),
        }
    }

    /// 4. A removed symbol fires `Removed`.
    #[tokio::test]
    async fn removed_symbol_fires_removed() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT", "DEADUSDT"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&m)).await.unwrap(); // seed

        m.set_list_symbols_ok(specs(["BTCUSDT"]));
        let events = sniper.scan(&as_trait(&m)).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ListingEvent::Removed { venue, symbol } => {
                assert_eq!(*venue, VenueId::Binance);
                assert_eq!(symbol, "DEADUSDT");
            }
            other => panic!("expected Removed, got {other:?}"),
        }
    }

    /// 5. Multi-venue independence — events on venue A don't
    ///    pollute venue B's cache and each is diffed separately.
    #[tokio::test]
    async fn multi_venue_independence() {
        let binance = mock(VenueId::Binance);
        let bybit = mock(VenueId::Bybit);
        binance.set_list_symbols_ok(specs(["BTCUSDT"]));
        bybit.set_list_symbols_ok(specs(["ETHUSDT"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&binance)).await.unwrap(); // seed binance
        sniper.scan(&as_trait(&bybit)).await.unwrap(); // seed bybit

        // Add NEWUSDT to Binance only — Bybit diff must stay empty.
        binance.set_list_symbols_ok(specs(["BTCUSDT", "NEWUSDT"]));
        let binance_events = sniper.scan(&as_trait(&binance)).await.unwrap();
        let bybit_events = sniper.scan(&as_trait(&bybit)).await.unwrap();
        assert_eq!(binance_events.len(), 1);
        assert!(bybit_events.is_empty());
        // Bybit cache still has exactly ETHUSDT — unaffected by
        // the Binance diff.
        let bybit_known = sniper.known_symbols(VenueId::Bybit).unwrap();
        assert_eq!(bybit_known.len(), 1);
        assert!(bybit_known.contains("ETHUSDT"));
    }

    /// 6. `forget` clears one venue — the next scan re-seeds.
    #[tokio::test]
    async fn forget_clears_one_venue() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT", "ETHUSDT"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&m)).await.unwrap();
        assert!(sniper.known_symbols(VenueId::Binance).is_some());

        sniper.forget(VenueId::Binance);
        assert!(sniper.known_symbols(VenueId::Binance).is_none());

        // Next scan re-seeds — no events even though we already
        // "saw" these symbols on an earlier scan.
        m.set_list_symbols_ok(specs(["BTCUSDT", "ETHUSDT", "NEWUSDT"]));
        let events = sniper.scan(&as_trait(&m)).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(sniper.known_symbols(VenueId::Binance).unwrap().len(), 3);
    }

    /// 7. A connector-side error propagates as `Err` and does
    ///    **not** mutate sniper state.
    #[tokio::test]
    async fn connector_error_propagates_without_mutating_state() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&m)).await.unwrap(); // seed

        m.set_list_symbols_err("venue down");
        let res = sniper.scan(&as_trait(&m)).await;
        assert!(res.is_err());

        // Cache unchanged — still exactly BTCUSDT.
        let known = sniper.known_symbols(VenueId::Binance).unwrap();
        assert_eq!(known.len(), 1);
        assert!(known.contains("BTCUSDT"));
    }

    /// 8. `Discovered` events carry the exact ProductSpec the
    ///    venue returned — round-trip the fields through a
    ///    custom-shaped spec to pin the contract.
    #[tokio::test]
    async fn discovered_event_carries_roundtripped_spec() {
        let m = mock(VenueId::HyperLiquid);
        m.set_list_symbols_ok(vec![]); // empty seed
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&m)).await.unwrap();

        let custom = ProductSpec {
            symbol: "SOL".to_string(),
            base_asset: "SOL".to_string(),
            quote_asset: "USDC".to_string(),
            tick_size: dec!(0.001),
            lot_size: dec!(0.01),
            min_notional: dec!(10),
            maker_fee: dec!(0.00015),
            taker_fee: dec!(0.00045),
            trading_status: TradingStatus::Trading,
        };
        m.set_list_symbols_ok(vec![custom.clone()]);
        let events = sniper.scan(&as_trait(&m)).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ListingEvent::Discovered { spec, .. } => {
                assert_eq!(spec, &custom);
            }
            other => panic!("expected Discovered, got {other:?}"),
        }
    }

    /// 9. Idempotency — scanning twice with the same inputs on a
    ///    seeded venue returns no events on the second call.
    #[tokio::test]
    async fn idempotent_rescan_on_seeded_venue() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT", "ETHUSDT"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&m)).await.unwrap(); // seed
        let first = sniper.scan(&as_trait(&m)).await.unwrap();
        let second = sniper.scan(&as_trait(&m)).await.unwrap();
        assert!(first.is_empty());
        assert!(second.is_empty());
    }

    /// 10. Hand-verified multi-venue fixture: 3 venues × 5
    ///     symbols each, then on the second scan one venue adds
    ///     a symbol and one venue removes a symbol. Exactly the
    ///     two corresponding events fire — no cross-venue
    ///     pollution.
    #[tokio::test]
    async fn three_venue_fixture_one_add_one_remove() {
        let binance = mock(VenueId::Binance);
        let bybit = mock(VenueId::Bybit);
        let hl = mock(VenueId::HyperLiquid);
        binance.set_list_symbols_ok(specs([
            "BTCUSDT", "ETHUSDT", "XRPUSDT", "SOLUSDT", "ADAUSDT",
        ]));
        bybit.set_list_symbols_ok(specs([
            "BTCUSDT", "ETHUSDT", "XRPUSDT", "SOLUSDT", "DOGEUSDT",
        ]));
        hl.set_list_symbols_ok(specs(["BTC", "ETH", "SOL", "ARB", "OP"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&binance)).await.unwrap();
        sniper.scan(&as_trait(&bybit)).await.unwrap();
        sniper.scan(&as_trait(&hl)).await.unwrap();

        // Binance adds a new listing; HL drops one.
        binance.set_list_symbols_ok(specs([
            "BTCUSDT", "ETHUSDT", "XRPUSDT", "SOLUSDT", "ADAUSDT", "NEWUSDT",
        ]));
        hl.set_list_symbols_ok(specs(["BTC", "ETH", "SOL", "ARB"])); // dropped OP

        let binance_events = sniper.scan(&as_trait(&binance)).await.unwrap();
        let bybit_events = sniper.scan(&as_trait(&bybit)).await.unwrap();
        let hl_events = sniper.scan(&as_trait(&hl)).await.unwrap();

        assert_eq!(binance_events.len(), 1);
        assert!(matches!(
            &binance_events[0],
            ListingEvent::Discovered { symbol, .. } if symbol == "NEWUSDT"
        ));
        assert!(bybit_events.is_empty());
        assert_eq!(hl_events.len(), 1);
        assert!(matches!(
            &hl_events[0],
            ListingEvent::Removed { symbol, .. } if symbol == "OP"
        ));
    }

    /// Bonus: a connector that returns `Err(unsupported)` from
    /// the trait default fails on the first scan — the sniper
    /// does not seed the venue and a later successful scan is
    /// still treated as first-seed (no events).
    #[tokio::test]
    async fn unsupported_venue_first_scan_errors_but_later_recovers() {
        let m = mock(VenueId::Custom);
        // Default response is None → MockConnector returns the
        // "unsupported" Err, matching real venues that don't
        // implement `list_symbols`.
        let mut sniper = ListingSniper::new();
        assert!(sniper.scan(&as_trait(&m)).await.is_err());
        assert!(sniper.known_symbols(VenueId::Custom).is_none());

        // Later the operator wires an adapter in — first
        // successful scan still seeds silently.
        m.set_list_symbols_ok(specs(["ABCUSDT"]));
        let events = sniper.scan(&as_trait(&m)).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(sniper.known_symbols(VenueId::Custom).unwrap().len(), 1);
    }

    /// Bonus: simultaneous add + remove in a single diff emits
    /// both events for the same venue in a single `scan` call.
    #[tokio::test]
    async fn simultaneous_add_and_remove_emits_both_events() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["OLD1", "OLD2"]));
        let mut sniper = ListingSniper::new();
        sniper.scan(&as_trait(&m)).await.unwrap();

        m.set_list_symbols_ok(specs(["OLD1", "NEW1"])); // drop OLD2, add NEW1
        let events = sniper.scan(&as_trait(&m)).await.unwrap();
        assert_eq!(events.len(), 2);
        let has_discovered = events
            .iter()
            .any(|e| matches!(e, ListingEvent::Discovered { symbol, .. } if symbol == "NEW1"));
        let has_removed = events
            .iter()
            .any(|e| matches!(e, ListingEvent::Removed { symbol, .. } if symbol == "OLD2"));
        assert!(has_discovered);
        assert!(has_removed);
    }
}
