//! Epic Multi-Venue Level 2.A — cross-engine data bus.
//!
//! One hub every engine publishes to and every graph can read from,
//! keyed on `(venue, symbol, product)`. This is the skeleton only:
//! engines push, but graph-side consumption lands in 2.B with the
//! parameterised source nodes.
//!
//! Design: `docs/research/multi-venue-architecture.md` §Level 2.
//!
//! ## Shape rationale
//!
//! - `Arc<RwLock<HashMap<...>>>` per stream kind — every engine only
//!   ever writes its own key, so lock contention is near-zero under
//!   normal traffic. Readers take a brief read-lock to snapshot.
//! - No history — just the latest snapshot per stream (for L1,
//!   funding, balances). Trade tape keeps a bounded deque because
//!   several detectors (MomentumIgnition, CancelOnReaction) need
//!   the rolling window, not just the last tick.
//! - No serialisation here — this is an in-process hub, not a wire
//!   protocol.

use chrono::{DateTime, Utc};
use mm_common::config::ProductType;
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

/// Per-stream key. Three-tuple because a venue can quote both spot
/// and perp on the same symbol with different books.
pub type StreamKey = (String /* venue */, String /* symbol */, ProductType);

/// Best bid / ask + spread at a moment in time. Cheap to clone so
/// readers can snapshot under the lock and release quickly.
#[derive(Debug, Clone, Default)]
pub struct BookL1Snapshot {
    pub bid_px: Option<Decimal>,
    pub ask_px: Option<Decimal>,
    pub mid: Option<Decimal>,
    pub spread_bps: Option<Decimal>,
    pub ts: Option<DateTime<Utc>>,
}

/// Top-N levels per side. Zero-level snapshots (fresh engine) are
/// possible; consumers should treat `levels.is_empty()` as "no data
/// yet".
#[derive(Debug, Clone, Default)]
pub struct BookL2Snapshot {
    pub bids: Vec<(Decimal, Decimal)>, // (price, qty) — outer first
    pub asks: Vec<(Decimal, Decimal)>, // outer first
    pub ts: Option<DateTime<Utc>>,
}

/// One public trade tick. `side` reflects the aggressor side
/// reported by the venue; None when the venue doesn't surface it.
#[derive(Debug, Clone)]
pub struct TradeTick {
    pub price: Decimal,
    pub qty: Decimal,
    pub aggressor: Option<TradeSide>,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeSide {
    Buy,
    Sell,
}

/// Per-perp funding rate + next funding timestamp.
#[derive(Debug, Clone, Default)]
pub struct FundingRate {
    pub rate: Option<Decimal>, // per-hour as a fraction (e.g. 0.0001 = 1 bps/h)
    pub next_funding_ts: Option<DateTime<Utc>>,
}

/// UX-VENUE-2 — latest regime label classified off a venue's
/// mid stream. `label` mirrors the `Debug` encoding used by
/// the autotuner's primary regime detector ("Quiet", "Trending",
/// "Volatile", "MeanReverting"), so the agent-side `regime_label`
/// table and metrics encoder can stay shared.
#[derive(Debug, Clone, Default)]
pub struct VenueRegimeSnapshot {
    pub label: String,
    pub ts: Option<DateTime<Utc>>,
}

/// Wallet balance entry. `reserved` covers margin + open orders.
#[derive(Debug, Clone, Default)]
pub struct BalanceEntry {
    pub total: Decimal,
    pub available: Decimal,
    pub reserved: Decimal,
    pub ts: Option<DateTime<Utc>>,
}

/// Rolling public-trade tape window, in seconds. Detectors that
/// need a longer view read multiple snapshots over time (we don't
/// stash centuries in a hot path).
pub const TAPE_WINDOW_SECS: i64 = 60;

/// Central in-process hub. Cheap to clone — every field is an Arc.
#[derive(Debug, Clone, Default)]
pub struct DataBus {
    pub books_l1: Arc<RwLock<HashMap<StreamKey, BookL1Snapshot>>>,
    pub books_l2: Arc<RwLock<HashMap<StreamKey, BookL2Snapshot>>>,
    pub trades: Arc<RwLock<HashMap<StreamKey, VecDeque<TradeTick>>>>,
    pub funding: Arc<RwLock<HashMap<StreamKey, FundingRate>>>,
    pub balances: Arc<RwLock<HashMap<(String, String), BalanceEntry>>>,
    /// UX-VENUE-2 — latest regime label per `(venue, symbol,
    /// product)` stream. Populated by the engine's per-venue
    /// classifier; consumed by `/api/v1/venues/book_state` so the
    /// Overview strip can render one regime chip per venue row.
    pub venue_regimes: Arc<RwLock<HashMap<StreamKey, VenueRegimeSnapshot>>>,
}

impl DataBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Upsert an L1 snapshot. Publisher-side.
    pub fn publish_l1(&self, key: StreamKey, snap: BookL1Snapshot) {
        if let Ok(mut map) = self.books_l1.write() {
            map.insert(key, snap);
        }
    }

    pub fn publish_l2(&self, key: StreamKey, snap: BookL2Snapshot) {
        if let Ok(mut map) = self.books_l2.write() {
            map.insert(key, snap);
        }
    }

    pub fn publish_funding(&self, key: StreamKey, f: FundingRate) {
        if let Ok(mut map) = self.funding.write() {
            map.insert(key, f);
        }
    }

    /// UX-VENUE-2 — upsert the regime label for a venue stream.
    pub fn publish_regime(&self, key: StreamKey, snap: VenueRegimeSnapshot) {
        if let Ok(mut map) = self.venue_regimes.write() {
            map.insert(key, snap);
        }
    }

    pub fn publish_balance(
        &self,
        venue: impl Into<String>,
        asset: impl Into<String>,
        bal: BalanceEntry,
    ) {
        if let Ok(mut map) = self.balances.write() {
            map.insert((venue.into(), asset.into()), bal);
        }
    }

    /// Append one trade tick. Evicts entries older than
    /// `TAPE_WINDOW_SECS` under the write lock so readers never see
    /// stale data.
    pub fn publish_trade(&self, key: StreamKey, tick: TradeTick) {
        if let Ok(mut map) = self.trades.write() {
            let queue = map.entry(key).or_default();
            queue.push_back(tick);
            let cutoff = Utc::now() - chrono::Duration::seconds(TAPE_WINDOW_SECS);
            while queue.front().is_some_and(|t| t.ts < cutoff) {
                queue.pop_front();
            }
        }
    }

    // ─── Reader-side helpers ─────────────────────────────────

    pub fn get_l1(&self, key: &StreamKey) -> Option<BookL1Snapshot> {
        self.books_l1.read().ok().and_then(|m| m.get(key).cloned())
    }

    pub fn get_l2(&self, key: &StreamKey) -> Option<BookL2Snapshot> {
        self.books_l2.read().ok().and_then(|m| m.get(key).cloned())
    }

    pub fn get_funding(&self, key: &StreamKey) -> Option<FundingRate> {
        self.funding.read().ok().and_then(|m| m.get(key).cloned())
    }

    /// 23-UX-4 — snapshot of every (venue, symbol, product) that
    /// has a funding-rate entry. Used by
    /// `/api/v1/venues/funding_state` to render the countdown
    /// panel on Overview without a per-leg polling loop.
    pub fn funding_entries(&self) -> Vec<(StreamKey, FundingRate)> {
        self.funding
            .read()
            .ok()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    /// 23-UX-5 — snapshot of every L1 book so the BasisMonitor
    /// can compute spot-vs-perp basis + cross-venue mid
    /// divergence without per-key lookups.
    pub fn l1_entries(&self) -> Vec<(StreamKey, BookL1Snapshot)> {
        self.books_l1
            .read()
            .ok()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    pub fn get_regime(&self, key: &StreamKey) -> Option<VenueRegimeSnapshot> {
        self.venue_regimes
            .read()
            .ok()
            .and_then(|m| m.get(key).cloned())
    }

    /// UX-VENUE-2 — snapshot of every per-venue regime label so
    /// `/api/v1/venues/book_state` can zip regimes into the rows
    /// it already returns without a per-key lookup.
    pub fn regime_entries(&self) -> Vec<(StreamKey, VenueRegimeSnapshot)> {
        self.venue_regimes
            .read()
            .ok()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    pub fn get_balance(&self, venue: &str, asset: &str) -> Option<BalanceEntry> {
        self.balances
            .read()
            .ok()
            .and_then(|m| m.get(&(venue.to_string(), asset.to_string())).cloned())
    }

    /// Clone the current trade tape for the symbol. Detectors that
    /// only need a count can use [`Self::trade_count`] for a
    /// cheaper read.
    pub fn get_trades(&self, key: &StreamKey) -> Vec<TradeTick> {
        self.trades
            .read()
            .ok()
            .and_then(|m| m.get(key).cloned())
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
    }

    pub fn trade_count(&self, key: &StreamKey) -> usize {
        self.trades
            .read()
            .ok()
            .and_then(|m| m.get(key).map(|q| q.len()))
            .unwrap_or(0)
    }

    /// Number of stream keys currently tracked. Useful for tests
    /// and for the `/api/v1/data_bus/health` probe Phase 2.B will
    /// add.
    pub fn stream_count(&self) -> usize {
        self.books_l1.read().map(|m| m.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn key(v: &str, s: &str, p: ProductType) -> StreamKey {
        (v.to_string(), s.to_string(), p)
    }

    #[test]
    fn l1_roundtrip() {
        let bus = DataBus::new();
        let k = key("bybit", "BTCUSDT", ProductType::Spot);
        bus.publish_l1(
            k.clone(),
            BookL1Snapshot {
                bid_px: Some(dec!(100)),
                ask_px: Some(dec!(100.1)),
                mid: Some(dec!(100.05)),
                spread_bps: Some(dec!(10)),
                ts: Some(Utc::now()),
            },
        );
        let got = bus.get_l1(&k).unwrap();
        assert_eq!(got.bid_px, Some(dec!(100)));
        assert_eq!(got.ask_px, Some(dec!(100.1)));
    }

    #[test]
    fn trade_tape_evicts_stale_entries() {
        let bus = DataBus::new();
        let k = key("bybit", "BTCUSDT", ProductType::Spot);
        // Stale entry (90 s ago) + fresh entry (now).
        let stale = Utc::now() - chrono::Duration::seconds(90);
        bus.publish_trade(
            k.clone(),
            TradeTick {
                price: dec!(100),
                qty: dec!(1),
                aggressor: None,
                ts: stale,
            },
        );
        bus.publish_trade(
            k.clone(),
            TradeTick {
                price: dec!(101),
                qty: dec!(1),
                aggressor: Some(TradeSide::Buy),
                ts: Utc::now(),
            },
        );
        let tape = bus.get_trades(&k);
        assert_eq!(tape.len(), 1);
        assert_eq!(tape[0].price, dec!(101));
    }

    #[test]
    fn balance_keyed_by_venue_asset_pair() {
        let bus = DataBus::new();
        bus.publish_balance(
            "binance",
            "USDT",
            BalanceEntry {
                total: dec!(1000),
                available: dec!(900),
                reserved: dec!(100),
                ts: None,
            },
        );
        let got = bus.get_balance("binance", "USDT").unwrap();
        assert_eq!(got.available, dec!(900));
        assert!(bus.get_balance("bybit", "USDT").is_none());
    }

    /// UX-VENUE-2 — regime publish/read/snapshot paths round-trip
    /// through the bus and keep the newest writer wins.
    #[test]
    fn regime_roundtrip_and_overwrite() {
        let bus = DataBus::new();
        let k = key("binance", "BTCUSDT", ProductType::Spot);
        bus.publish_regime(
            k.clone(),
            VenueRegimeSnapshot {
                label: "Quiet".to_string(),
                ts: Some(Utc::now()),
            },
        );
        assert_eq!(bus.get_regime(&k).unwrap().label, "Quiet");
        bus.publish_regime(
            k.clone(),
            VenueRegimeSnapshot {
                label: "Volatile".to_string(),
                ts: Some(Utc::now()),
            },
        );
        assert_eq!(bus.get_regime(&k).unwrap().label, "Volatile");
        let entries = bus.regime_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.label, "Volatile");
    }

    /// 23-UX-4 — funding_entries snapshot exposes every (venue,
    /// symbol, product) with a funding-rate entry so the
    /// frontend panel can render one row per perp leg.
    #[test]
    fn funding_entries_returns_all_published() {
        let bus = DataBus::new();
        bus.publish_funding(
            key("binance", "BTCUSDT", ProductType::LinearPerp),
            FundingRate {
                rate: Some(dec!(0.0001)),
                next_funding_ts: Some(Utc::now()),
            },
        );
        bus.publish_funding(
            key("bybit", "ETHUSDT", ProductType::LinearPerp),
            FundingRate {
                rate: Some(dec!(-0.0002)),
                next_funding_ts: Some(Utc::now()),
            },
        );
        let entries = bus.funding_entries();
        assert_eq!(entries.len(), 2);
        let venues: Vec<&String> = entries.iter().map(|(k, _)| &k.0).collect();
        assert!(venues.iter().any(|v| v == &"binance"));
        assert!(venues.iter().any(|v| v == &"bybit"));
    }
}
