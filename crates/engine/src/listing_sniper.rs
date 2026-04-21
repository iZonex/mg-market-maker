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

/// Async runner that periodically scans connectors and routes
/// discovery/removal events to the audit trail and alert manager.
///
/// Spawned once per process in the server's `main()`. Owns the
/// `ListingSniper` state machine and a list of connectors to scan.
pub struct ListingSniperRunner {
    sniper: ListingSniper,
    connectors: Vec<Arc<dyn ExchangeConnector>>,
    audit: Arc<mm_risk::audit::AuditLog>,
    alerts: Option<mm_dashboard::alerts::AlertManager>,
    scan_interval: std::time::Duration,
    alert_on_discovery: bool,
    /// Epic F stage-3 — real entry policy. `None` keeps the
    /// runner in observer-only mode (audit + alert, no
    /// orders).
    entry_cfg: Option<mm_common::config::ListingSniperEntryConfig>,
    /// Epic F stage-3 — pending entries awaiting the
    /// quarantine window to expire. Keyed by `(venue,
    /// symbol)`. Populated on every `Discovered` event;
    /// drained on every scan tick by `try_execute_pending`.
    pending_entries: std::collections::HashMap<
        (mm_exchange_core::connector::VenueId, String),
        PendingEntry,
    >,
    /// Epic F stage-3 — active entries counter. Incremented
    /// when `try_execute_pending` places a real IOC; never
    /// decremented automatically (sniper does not track
    /// fills). Operators reset on restart. Caps at
    /// `entry_cfg.max_active_entries`.
    active_entries: u32,
}

/// Epic F stage-3 — one quarantined listing waiting to fire
/// its entry IOC.
#[derive(Debug, Clone)]
struct PendingEntry {
    first_seen: std::time::Instant,
    spec: ProductSpec,
}

impl ListingSniperRunner {
    pub fn new(
        connectors: Vec<Arc<dyn ExchangeConnector>>,
        audit: Arc<mm_risk::audit::AuditLog>,
        alerts: Option<mm_dashboard::alerts::AlertManager>,
        scan_interval_secs: u64,
        alert_on_discovery: bool,
    ) -> Self {
        Self {
            sniper: ListingSniper::new(),
            connectors,
            audit,
            alerts,
            scan_interval: std::time::Duration::from_secs(scan_interval_secs),
            alert_on_discovery,
            entry_cfg: None,
            pending_entries: std::collections::HashMap::new(),
            active_entries: 0,
        }
    }

    /// Builder-style attachment of the Epic F stage-3 real-
    /// entry policy. Without this call the runner stays in
    /// observer-only mode (byte-identical to pre-stage-3
    /// behaviour).
    pub fn with_entry_policy(
        mut self,
        cfg: mm_common::config::ListingSniperEntryConfig,
    ) -> Self {
        self.entry_cfg = Some(cfg);
        self
    }

    /// Run the scan loop until the shutdown signal fires.
    pub async fn run(mut self, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
        use tracing::info;

        let mut interval = tokio::time::interval(self.scan_interval);
        // Skip immediate first tick to let the process stabilise.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("listing sniper shutting down");
                        return;
                    }
                }
                _ = interval.tick() => {
                    self.scan_all_venues().await;
                    // Epic F stage-3 — fire any pending
                    // entries that cleared the quarantine
                    // window on this tick.
                    if self.entry_cfg.is_some() {
                        self.try_execute_pending().await;
                    }
                }
            }
        }
    }

    async fn scan_all_venues(&mut self) {
        use mm_risk::audit::AuditEventType;
        use tracing::{debug, info};

        for connector in &self.connectors {
            let venue = connector.venue_id();
            match self.sniper.scan(connector).await {
                Ok(events) => {
                    for event in events {
                        match &event {
                            ListingEvent::Discovered {
                                venue,
                                symbol,
                                spec,
                            } => {
                                let detail = format!(
                                    "venue={venue:?} symbol={symbol} tick={} lot={} min_notional={}",
                                    spec.tick_size, spec.lot_size, spec.min_notional
                                );
                                info!(%detail, "new listing discovered");
                                self.audit.risk_event(
                                    symbol,
                                    AuditEventType::ListingDiscovered,
                                    &detail,
                                );
                                if self.alert_on_discovery {
                                    if let Some(alerts) = &self.alerts {
                                        alerts.alert(
                                            mm_dashboard::alerts::AlertSeverity::Info,
                                            &format!("New listing: {symbol}"),
                                            &detail,
                                            Some(symbol),
                                        );
                                    }
                                }
                                // Epic F stage-3 — stash in
                                // the pending queue so the
                                // next scan(s) can fire an
                                // entry once the quarantine
                                // window expires. Only when
                                // `entry_cfg` is set —
                                // observer-only runs skip
                                // this branch.
                                if self.entry_cfg.is_some() {
                                    self.pending_entries.insert(
                                        (*venue, symbol.clone()),
                                        PendingEntry {
                                            first_seen: std::time::Instant::now(),
                                            spec: spec.clone(),
                                        },
                                    );
                                }
                            }
                            ListingEvent::Removed { venue, symbol } => {
                                let detail = format!("venue={venue:?} symbol={symbol}");
                                info!(%detail, "listing removed");
                                self.audit.risk_event(
                                    symbol,
                                    AuditEventType::ListingRemoved,
                                    &detail,
                                );
                                if self.alert_on_discovery {
                                    if let Some(alerts) = &self.alerts {
                                        alerts.alert(
                                            mm_dashboard::alerts::AlertSeverity::Warning,
                                            &format!("Listing removed: {symbol}"),
                                            &detail,
                                            Some(symbol),
                                        );
                                    }
                                }
                                // Drop matching pending
                                // entry — the venue decided
                                // this listing is gone, we
                                // should not snipe it.
                                self.pending_entries.remove(&(*venue, symbol.clone()));
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!(venue = ?venue, error = %e, "listing sniper scan failed (venue may not support list_symbols)");
                }
            }
        }
    }

    /// Epic F stage-3 — walk the pending-entries queue and
    /// fire IOC BUYs for every symbol that has cleared the
    /// quarantine window. Respects `max_active_entries`,
    /// `require_trading_status`, `max_notional_usd`.
    /// Rejections are audited with a reason so
    /// post-mortems can reconstruct the decision.
    async fn try_execute_pending(&mut self) {
        use mm_common::types::{OrderType, Side, TimeInForce, TradingStatus};
        use mm_exchange_core::connector::NewOrder;
        use mm_risk::audit::AuditEventType;
        use rust_decimal::Decimal;
        use tracing::{info, warn};

        let Some(cfg) = self.entry_cfg.clone() else {
            return;
        };
        let now = std::time::Instant::now();
        let quarantine = std::time::Duration::from_secs(cfg.quarantine_secs);
        let ready: Vec<(mm_exchange_core::connector::VenueId, String, PendingEntry)> = self
            .pending_entries
            .iter()
            .filter(|(_, pe)| now.saturating_duration_since(pe.first_seen) >= quarantine)
            .map(|((v, s), pe)| (*v, s.clone(), pe.clone()))
            .collect();
        if ready.is_empty() {
            return;
        }

        for (venue, symbol, pe) in ready {
            // Always pop from pending so a failed attempt
            // doesn't retry forever. Ops can monitor the
            // audit `ListingEntryRejected` and re-trigger
            // manually.
            self.pending_entries.remove(&(venue, symbol.clone()));

            let audit_symbol = symbol.clone();
            let reject = |reason: &str| {
                let detail = format!("venue={venue:?} symbol={symbol} reason={reason}");
                warn!(%detail, "listing sniper entry rejected");
                self.audit.risk_event(
                    &audit_symbol,
                    AuditEventType::ListingEntryRejected,
                    &detail,
                );
            };

            // Guard 1: master switch.
            if !cfg.enter_on_discovery {
                reject("disabled");
                continue;
            }
            // Guard 2: status gate.
            if cfg.require_trading_status && pe.spec.trading_status != TradingStatus::Trading {
                reject(&format!("status={:?}", pe.spec.trading_status));
                continue;
            }
            // Guard 3: active cap.
            if self.active_entries >= cfg.max_active_entries {
                reject("max_active");
                continue;
            }
            // Guard 4: resolve connector + top-of-book.
            let Some(connector) = self.connectors.iter().find(|c| c.venue_id() == venue) else {
                reject("no_connector");
                continue;
            };
            let book = match connector.get_orderbook(&symbol, 1).await {
                Ok(b) => b,
                Err(e) => {
                    reject(&format!("book_err({e})"));
                    continue;
                }
            };
            let Some(ask_level) = book.1.first() else {
                reject("no_book");
                continue;
            };
            let ask = ask_level.price;
            if ask <= Decimal::ZERO {
                reject("zero_ask");
                continue;
            }
            // Guard 5: qty sizing + min-notional.
            let raw_qty = cfg.max_notional_usd / ask;
            let qty = pe.spec.round_qty(raw_qty);
            if qty <= Decimal::ZERO {
                reject("zero_qty");
                continue;
            }
            let notional = qty * ask;
            if notional < pe.spec.min_notional {
                reject(&format!(
                    "below_min_notional(notional={notional}, min={})",
                    pe.spec.min_notional
                ));
                continue;
            }
            // Fire an IOC BUY. Use pessimistic price =
            // ask × 1.001 (10 bps slippage tolerance) so
            // the IOC still clears a thin book moving one
            // tick between snapshot and send.
            let limit = pe.spec.round_price(ask + ask / Decimal::from(1_000u32));
            let order = NewOrder {
                symbol: symbol.clone(),
                side: Side::Buy,
                order_type: OrderType::Limit,
                price: Some(limit),
                qty,
                time_in_force: Some(TimeInForce::Ioc),
                client_order_id: Some(uuid::Uuid::new_v4().to_string()),
                // Listing sniper opens the position — reduce_only
                // would reject the very first entry.
                reduce_only: false,
            };
            match connector.place_order(&order).await {
                Ok(_) => {
                    self.active_entries = self.active_entries.saturating_add(1);
                    let detail = format!(
                        "venue={venue:?} symbol={symbol} qty={qty} price={limit} notional={notional}"
                    );
                    info!(%detail, "listing sniper entry placed");
                    self.audit.risk_event(
                        &audit_symbol,
                        AuditEventType::ListingEntered,
                        &detail,
                    );
                }
                Err(e) => {
                    reject(&format!("place_err({e})"));
                }
            }
        }
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

    /// Runner: `scan_all_venues` routes Discovered events to the
    /// audit log without panicking.
    #[tokio::test]
    async fn runner_scan_routes_discovered_to_audit() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT"]));
        let connector = as_trait(&m);

        let dir = std::env::temp_dir().join(format!("mm_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let audit_path = dir.join("audit_runner.jsonl");
        let audit = Arc::new(mm_risk::audit::AuditLog::new(&audit_path).unwrap());

        let mut runner =
            ListingSniperRunner::new(vec![connector.clone()], audit.clone(), None, 60, true);
        // First scan seeds.
        runner.scan_all_venues().await;

        // Add a new symbol.
        m.set_list_symbols_ok(specs(["BTCUSDT", "NEWUSDT"]));
        runner.scan_all_venues().await;

        // Verify the sniper state has both symbols.
        let known = runner.sniper.known_symbols(VenueId::Binance).unwrap();
        assert!(known.contains("NEWUSDT"));
        assert!(known.contains("BTCUSDT"));
        assert_eq!(known.len(), 2);
    }

    /// Runner: connector errors don't crash the scan loop.
    #[tokio::test]
    async fn runner_tolerates_connector_errors() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT"]));
        let connector = as_trait(&m);

        let dir = std::env::temp_dir().join(format!("mm_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let audit_path = dir.join("audit_runner.jsonl");
        let audit = Arc::new(mm_risk::audit::AuditLog::new(&audit_path).unwrap());

        let mut runner = ListingSniperRunner::new(vec![connector.clone()], audit, None, 60, false);
        runner.scan_all_venues().await; // seed

        // Now inject an error.
        m.set_list_symbols_err("venue down");
        runner.scan_all_venues().await; // should not panic

        // Cache unchanged.
        let known = runner.sniper.known_symbols(VenueId::Binance).unwrap();
        assert_eq!(known.len(), 1);
    }

    /// Runner: removed symbols are detected.
    #[tokio::test]
    async fn runner_detects_removed_symbols() {
        let m = mock(VenueId::Binance);
        m.set_list_symbols_ok(specs(["BTCUSDT", "ETHUSDT"]));
        let connector = as_trait(&m);

        let dir = std::env::temp_dir().join(format!("mm_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let audit_path = dir.join("audit_runner.jsonl");
        let audit = Arc::new(mm_risk::audit::AuditLog::new(&audit_path).unwrap());

        let mut runner = ListingSniperRunner::new(vec![connector.clone()], audit, None, 60, true);
        runner.scan_all_venues().await; // seed

        // Remove ETHUSDT.
        m.set_list_symbols_ok(specs(["BTCUSDT"]));
        runner.scan_all_venues().await;

        let known = runner.sniper.known_symbols(VenueId::Binance).unwrap();
        assert_eq!(known.len(), 1);
        assert!(known.contains("BTCUSDT"));
        assert!(!known.contains("ETHUSDT"));
    }
}
