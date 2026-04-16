//! Smart Order Router — per-venue snapshot aggregator
//! (Epic A sub-component #2).
//!
//! Walks every connector in a `ConnectorBundle` (primary +
//! optional hedge + the `extra` vec added in Sprint A-2) and
//! produces one [`VenueSnapshot`] per venue per tick. The
//! snapshot is a pure data struct that the cost model and
//! the router consume without touching any connector
//! directly.
//!
//! # What the aggregator carries
//!
//! | Field | Source | Notes |
//! |---|---|---|
//! | `venue` | `ExchangeConnector::venue_id()` | Stable per-connector identity |
//! | `symbol` | `Aggregator::register_venue(...)` seed | v1 stores one symbol per venue; stage-2 will carry a `Vec<String>` |
//! | `available_qty` | Seed + optional runtime refresh | Operator-supplied cap in v1 |
//! | `rate_limit_remaining` | `ExchangeConnector::rate_limit_remaining()` | New trait method from Sprint A-2 |
//! | `maker_fee_bps` / `taker_fee_bps` | Seeded `ProductSpec` | P1.2 hot-swaps these on the engine side; the SOR reads them through the seed, not the connector |
//! | `best_bid` / `best_ask` | Seeded + optional runtime refresh | v1 stores the last-seen mid; stage-2 will pull live |
//! | `queue_wait_secs` | Config constant | Fixed for v1; stage-2 wires real trade-rate estimator |
//!
//! The **"seed + optional runtime refresh"** split is
//! deliberate: the aggregator can be driven deterministically
//! from a test fixture (pure data), and the live engine path
//! overlays whatever runtime accessors each connector
//! supports on top. v1 only uses the seed path — stage-2
//! will add a `refresh(bundle).await` method that pulls
//! live rate-limit and book-state from every connector.
//!
//! # Pure async where needed
//!
//! `collect` is async solely because
//! `ExchangeConnector::rate_limit_remaining` is async (the
//! underlying `RateLimiter` uses a `tokio::Mutex`). Every
//! other piece of the aggregator is synchronous.

use std::collections::HashMap;
use std::sync::Arc;

use mm_common::types::{ProductSpec, Side};
use mm_exchange_core::connector::{ExchangeConnector, VenueId};
use rust_decimal::Decimal;

use crate::connector_bundle::ConnectorBundle;

/// One per-venue state snapshot the SOR consumes. All fields
/// are in the venue's native native units except `*_bps`
/// which are always basis points.
#[derive(Debug, Clone)]
pub struct VenueSnapshot {
    /// Venue tag (Binance / Bybit / HL / Custom / …).
    pub venue: VenueId,
    /// Primary symbol this snapshot is keyed on. v1 assumes
    /// one symbol per venue per aggregator; stage-2 will
    /// relax this.
    pub symbol: String,
    /// Maximum qty the router is allowed to allocate to
    /// this venue on the current refresh tick. Sourced from
    /// the register-venue seed — operators use this to cap
    /// per-venue exposure without changing their existing
    /// inventory limits.
    pub available_qty: Decimal,
    /// Remaining rate-limit budget in the venue's native
    /// token unit. Pulled live from
    /// [`ExchangeConnector::rate_limit_remaining`]. The
    /// router treats a venue with `remaining == 0` as
    /// temporarily unavailable.
    pub rate_limit_remaining: u32,
    /// Maker fee in basis points. Negative values are
    /// rebates. Preserves end-to-end through the cost
    /// model.
    pub maker_fee_bps: Decimal,
    /// Taker fee in basis points. Always non-negative on
    /// every venue this codebase currently targets.
    pub taker_fee_bps: Decimal,
    /// Best bid price in the venue's native quote currency.
    pub best_bid: Decimal,
    /// Best ask price in the venue's native quote currency.
    pub best_ask: Decimal,
    /// Expected seconds until a passive maker quote at the
    /// best bid / ask clears. v1 uses a seeded constant;
    /// stage-2 will compute it from a live trade-rate feed.
    pub queue_wait_secs: Decimal,
}

impl VenueSnapshot {
    /// Mid price derived from the bid/ask pair. Zero when
    /// the snapshot has not been seeded with a book yet.
    pub fn mid_price(&self) -> Decimal {
        if self.best_bid.is_zero() || self.best_ask.is_zero() {
            return Decimal::ZERO;
        }
        (self.best_bid + self.best_ask) / Decimal::from(2u32)
    }

    /// Whether this snapshot has enough headroom to accept
    /// **any** additional qty. Used by the router to skip
    /// exhausted venues before computing a cost.
    pub fn is_available(&self) -> bool {
        self.available_qty > Decimal::ZERO && self.rate_limit_remaining > 0
    }
}

/// Seed data for one registered venue. The aggregator owns
/// a `HashMap<VenueId, VenueSeed>` keyed by venue, and the
/// engine pushes fresh seeds through [`Aggregator::register_venue`]
/// at startup plus on every fee-tier refresh.
#[derive(Debug, Clone)]
pub struct VenueSeed {
    pub symbol: String,
    pub product: ProductSpec,
    pub available_qty: Decimal,
    pub queue_wait_secs: Decimal,
    pub best_bid: Decimal,
    pub best_ask: Decimal,
}

impl VenueSeed {
    /// Construct a seed with empty book state. The engine
    /// later refreshes `best_bid` / `best_ask` on every
    /// market-data tick through `update_book`.
    pub fn new(symbol: &str, product: ProductSpec, available_qty: Decimal) -> Self {
        Self {
            symbol: symbol.to_string(),
            product,
            available_qty,
            queue_wait_secs: Decimal::ZERO,
            best_bid: Decimal::ZERO,
            best_ask: Decimal::ZERO,
        }
    }
}

/// Aggregator that produces `VenueSnapshot`s from the live
/// bundle on demand. v1 keeps one seeded `VenueSeed` per
/// venue and refreshes only the rate-limit budget on
/// `collect`.
#[derive(Debug, Clone, Default)]
pub struct VenueStateAggregator {
    seeds: HashMap<VenueId, VenueSeed>,
}

impl VenueStateAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or overwrite) a venue seed. Idempotent —
    /// re-registering the same venue with new fee rates is
    /// the update path used by the P1.2 fee-tier refresh
    /// task.
    pub fn register_venue(&mut self, venue: VenueId, seed: VenueSeed) {
        self.seeds.insert(venue, seed);
    }

    /// Update the best-bid / best-ask pair for a registered
    /// venue. Called from the engine's market-data refresh
    /// path. No-op if the venue is not registered.
    pub fn update_book(&mut self, venue: VenueId, best_bid: Decimal, best_ask: Decimal) {
        if let Some(seed) = self.seeds.get_mut(&venue) {
            seed.best_bid = best_bid;
            seed.best_ask = best_ask;
        }
    }

    /// Update the fee rates for a registered venue. Called
    /// from the engine's P1.2 fee-tier refresh task when
    /// the venue returns updated maker/taker fees. No-op
    /// if the venue is not registered. Stage-2 auto-refresh.
    pub fn update_fees(&mut self, venue: VenueId, maker_fee: Decimal, taker_fee: Decimal) {
        if let Some(seed) = self.seeds.get_mut(&venue) {
            seed.product.maker_fee = maker_fee;
            seed.product.taker_fee = taker_fee;
        }
    }

    /// Update the queue-wait estimate for a registered
    /// venue. Engine calls this per tick with the
    /// `config.market_maker.sor_queue_wait_bps_per_sec`
    /// cadence × a per-symbol multiplier (stage-2). v1
    /// takes a fixed constant from the config.
    pub fn update_queue_wait(&mut self, venue: VenueId, queue_wait_secs: Decimal) {
        if let Some(seed) = self.seeds.get_mut(&venue) {
            seed.queue_wait_secs = queue_wait_secs;
        }
    }

    /// Read-only accessor for a seeded venue — used by the
    /// router + by tests to confirm registration.
    pub fn seed(&self, venue: VenueId) -> Option<&VenueSeed> {
        self.seeds.get(&venue)
    }

    /// Iterator over every registered venue tag. Deterministic
    /// (sorted by venue enum ordinal) so test assertions are
    /// stable.
    pub fn venues(&self) -> Vec<VenueId> {
        let mut v: Vec<VenueId> = self.seeds.keys().copied().collect();
        v.sort_by_key(|venue| *venue as u8);
        v
    }

    /// Walk the bundle's connectors, pull live rate-limit
    /// budgets, and combine with the seeded state to produce
    /// one `VenueSnapshot` per registered venue.
    ///
    /// A connector in the bundle whose venue is **not**
    /// seeded is silently skipped — the aggregator only
    /// emits snapshots for venues the engine has explicitly
    /// registered. This keeps the seed map authoritative
    /// and avoids half-built snapshots from a connector the
    /// operator forgot to configure.
    pub async fn collect(&self, bundle: &ConnectorBundle, _side: Side) -> Vec<VenueSnapshot> {
        let mut out = Vec::new();
        for connector in bundle.all_connectors() {
            let venue = connector.venue_id();
            let Some(seed) = self.seeds.get(&venue) else {
                continue;
            };
            let rate_limit_remaining = connector.rate_limit_remaining().await;
            out.push(snapshot_from_seed(connector, seed, rate_limit_remaining));
        }
        out
    }

    /// Synchronous variant of `collect` used by unit tests.
    /// Bypasses the live rate-limit query and uses a
    /// test-supplied `rate_limit_remaining` for every venue.
    /// Keeps the test surface pure-data and avoids
    /// bringing tokio into every cost-model test.
    pub fn collect_synthetic(&self, venues: &[(VenueId, u32)]) -> Vec<VenueSnapshot> {
        venues
            .iter()
            .filter_map(|(venue, remaining)| {
                let seed = self.seeds.get(venue)?;
                Some(synthetic_snapshot(*venue, seed, *remaining))
            })
            .collect()
    }
}

fn snapshot_from_seed(
    _connector: &Arc<dyn ExchangeConnector>,
    seed: &VenueSeed,
    rate_limit_remaining: u32,
) -> VenueSnapshot {
    VenueSnapshot {
        venue: _connector.venue_id(),
        symbol: seed.symbol.clone(),
        available_qty: seed.available_qty,
        rate_limit_remaining,
        maker_fee_bps: seed.product.maker_fee * Decimal::from(10_000u32),
        taker_fee_bps: seed.product.taker_fee * Decimal::from(10_000u32),
        best_bid: seed.best_bid,
        best_ask: seed.best_ask,
        queue_wait_secs: seed.queue_wait_secs,
    }
}

fn synthetic_snapshot(
    venue: VenueId,
    seed: &VenueSeed,
    rate_limit_remaining: u32,
) -> VenueSnapshot {
    VenueSnapshot {
        venue,
        symbol: seed.symbol.clone(),
        available_qty: seed.available_qty,
        rate_limit_remaining,
        maker_fee_bps: seed.product.maker_fee * Decimal::from(10_000u32),
        taker_fee_bps: seed.product.taker_fee * Decimal::from(10_000u32),
        best_bid: seed.best_bid,
        best_ask: seed.best_ask,
        queue_wait_secs: seed.queue_wait_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample_product() -> ProductSpec {
        ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001), // 1 bps
            taker_fee: dec!(0.0005), // 5 bps
            trading_status: Default::default(),
        }
    }

    fn seed(available: Decimal) -> VenueSeed {
        VenueSeed {
            symbol: "BTCUSDT".into(),
            product: sample_product(),
            available_qty: available,
            queue_wait_secs: dec!(10),
            best_bid: dec!(49990),
            best_ask: dec!(50010),
        }
    }

    /// `register_venue` inserts a new seed; re-registering
    /// the same venue overwrites.
    #[test]
    fn register_venue_inserts_and_overwrites() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed(dec!(1)));
        assert!(agg.seed(VenueId::Binance).is_some());
        assert_eq!(agg.seed(VenueId::Binance).unwrap().available_qty, dec!(1));

        agg.register_venue(VenueId::Binance, seed(dec!(2)));
        assert_eq!(agg.seed(VenueId::Binance).unwrap().available_qty, dec!(2));
    }

    /// `update_book` mutates the bid/ask of a seeded venue
    /// without touching unrelated fields.
    #[test]
    fn update_book_mutates_bid_ask_only() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed(dec!(1)));
        agg.update_book(VenueId::Binance, dec!(50000), dec!(50020));
        let s = agg.seed(VenueId::Binance).unwrap();
        assert_eq!(s.best_bid, dec!(50000));
        assert_eq!(s.best_ask, dec!(50020));
        assert_eq!(s.available_qty, dec!(1));
        assert_eq!(s.queue_wait_secs, dec!(10));
    }

    /// `update_book` on an unregistered venue is a silent
    /// no-op (no panic, no insert).
    #[test]
    fn update_book_on_unregistered_venue_is_noop() {
        let mut agg = VenueStateAggregator::new();
        agg.update_book(VenueId::Bybit, dec!(1), dec!(2));
        assert!(agg.seed(VenueId::Bybit).is_none());
    }

    /// `update_queue_wait` mutates only the queue field.
    #[test]
    fn update_queue_wait_mutates_wait_only() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed(dec!(1)));
        agg.update_queue_wait(VenueId::Binance, dec!(25));
        let s = agg.seed(VenueId::Binance).unwrap();
        assert_eq!(s.queue_wait_secs, dec!(25));
        assert_eq!(s.best_bid, dec!(49990));
    }

    /// `venues()` is sorted deterministically so test
    /// assertions across the aggregator API are stable.
    #[test]
    fn venues_accessor_is_deterministic() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::HyperLiquid, seed(dec!(1)));
        agg.register_venue(VenueId::Binance, seed(dec!(1)));
        agg.register_venue(VenueId::Bybit, seed(dec!(1)));
        let v = agg.venues();
        assert_eq!(v.len(), 3);
        // Same list on every invocation.
        assert_eq!(v, agg.venues());
    }

    /// `collect_synthetic` converts a `(venue, remaining)`
    /// list into VenueSnapshot vec using the seed. Rate
    /// limit comes from the test input.
    #[test]
    fn collect_synthetic_produces_one_snapshot_per_known_venue() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed(dec!(5)));
        agg.register_venue(VenueId::Bybit, seed(dec!(3)));
        let snaps = agg.collect_synthetic(&[
            (VenueId::Binance, 100),
            (VenueId::Bybit, 50),
            (VenueId::HyperLiquid, 1000), // unknown — skipped
        ]);
        assert_eq!(snaps.len(), 2);
        let binance = snaps.iter().find(|s| s.venue == VenueId::Binance).unwrap();
        assert_eq!(binance.rate_limit_remaining, 100);
        assert_eq!(binance.available_qty, dec!(5));
        // 1 bps maker fee, 5 bps taker fee (from sample_product).
        assert_eq!(binance.maker_fee_bps, dec!(1));
        assert_eq!(binance.taker_fee_bps, dec!(5));
    }

    /// `VenueSnapshot::mid_price` averages bid and ask.
    /// Zero when the book is unseeded — the router skips
    /// these rather than computing a nonsensical midpoint.
    #[test]
    fn mid_price_averages_bid_and_ask() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed(dec!(1)));
        agg.update_book(VenueId::Binance, dec!(100), dec!(102));
        let snaps = agg.collect_synthetic(&[(VenueId::Binance, 1000)]);
        assert_eq!(snaps[0].mid_price(), dec!(101));
    }

    /// `mid_price` returns zero when either side of the
    /// book is missing.
    #[test]
    fn mid_price_is_zero_on_unseeded_book() {
        let mut agg = VenueStateAggregator::new();
        let mut s = seed(dec!(1));
        s.best_bid = Decimal::ZERO;
        s.best_ask = Decimal::ZERO;
        agg.register_venue(VenueId::Binance, s);
        let snaps = agg.collect_synthetic(&[(VenueId::Binance, 1000)]);
        assert_eq!(snaps[0].mid_price(), Decimal::ZERO);
    }

    /// `is_available` rejects exhausted inventory and
    /// drained rate limits.
    #[test]
    fn is_available_gates_on_qty_and_rate_limit() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed(dec!(5)));
        // Healthy path.
        let healthy = agg.collect_synthetic(&[(VenueId::Binance, 100)]);
        assert!(healthy[0].is_available());

        // Drained rate limit.
        let drained = agg.collect_synthetic(&[(VenueId::Binance, 0)]);
        assert!(!drained[0].is_available());

        // Exhausted qty.
        agg.register_venue(VenueId::Binance, seed(dec!(0)));
        let exhausted = agg.collect_synthetic(&[(VenueId::Binance, 100)]);
        assert!(!exhausted[0].is_available());
    }
}
