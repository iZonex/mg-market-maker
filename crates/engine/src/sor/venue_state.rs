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
    /// Epic A stage-2 #4 — book-depth slippage coefficient.
    /// Marginal slippage (bps) the router should charge per
    /// base-asset unit of qty routed to this venue on top of
    /// the flat fee. Derived from the venue's live L1 book
    /// depth: `slippage_bps_per_unit = spread_bps /
    /// best_depth_qty`. A thin book → high coefficient → the
    /// router prefers thicker books even when their fees are
    /// marginally higher.
    ///
    /// A zero value disables the convex term → the greedy +
    /// convex routers produce identical allocations.
    pub slippage_bps_per_unit: Decimal,
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
    /// Market-type discriminator — Spot / LinearPerp / InversePerp.
    /// Used together with `venue` as the aggregator's key so a
    /// single process can run (Binance spot) + (Binance linear
    /// perp) without the dispatcher lookup colliding on VenueId
    /// alone.
    pub venue_product: mm_exchange_core::connector::VenueProduct,
    pub available_qty: Decimal,
    pub queue_wait_secs: Decimal,
    pub best_bid: Decimal,
    pub best_ask: Decimal,
}

impl VenueSeed {
    /// Construct a seed with empty book state. The engine
    /// later refreshes `best_bid` / `best_ask` on every
    /// market-data tick through `update_book`.
    /// Defaults `venue_product` to `Spot` — callers that trade
    /// perps should use `with_venue_product` immediately after.
    pub fn new(symbol: &str, product: ProductSpec, available_qty: Decimal) -> Self {
        Self {
            symbol: symbol.to_string(),
            product,
            venue_product: mm_exchange_core::connector::VenueProduct::Spot,
            available_qty,
            queue_wait_secs: Decimal::ZERO,
            best_bid: Decimal::ZERO,
            best_ask: Decimal::ZERO,
        }
    }

    /// Builder-style override for the product type. Engine boot
    /// path calls this after `new()` when the venue is a perp,
    /// keeping the default-argument ergonomic for the common
    /// spot case.
    pub fn with_venue_product(
        mut self,
        product: mm_exchange_core::connector::VenueProduct,
    ) -> Self {
        self.venue_product = product;
        self
    }
}

/// Aggregator that produces `VenueSnapshot`s from the live
/// bundle on demand. v1 keeps one seeded `VenueSeed` per
/// venue and refreshes only the rate-limit budget on
/// `collect`.
///
/// Epic A stage-2 #5 — multi-symbol support. The aggregator
/// is now keyed by `(VenueId, symbol)` so operators running
/// more than one base asset on the same venue (BTCUSDT +
/// ETHUSDT on Binance) get one registrable slot per symbol.
/// Legacy single-symbol call sites keep working: the short
/// accessors (`register_venue`, `seed`, `venues`,
/// `update_book`, `update_fees`, `update_queue_wait`)
/// identify the implicit symbol from the venue's first
/// registered seed, so a deployment with exactly one symbol
/// per venue sees byte-identical behaviour.
/// Aggregator slot key: venue + product type + symbol. The
/// triple uniquely identifies a market so a single process can
/// seed both `(Binance, Spot, BTCUSDT)` and `(Binance, LinearPerp,
/// BTCUSDT)` at the same time — the collision the old
/// `(VenueId, symbol)` key produced is gone. Kept as a plain
/// tuple alias (not a named struct) so the existing HashMap
/// `get`/`insert` patterns stay ergonomic.
pub type VenueSlot = (VenueId, mm_exchange_core::connector::VenueProduct, String);

#[derive(Debug, Clone, Default)]
pub struct VenueStateAggregator {
    seeds: HashMap<VenueSlot, VenueSeed>,
}

impl VenueStateAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or overwrite) a venue seed. The seed's
    /// `symbol` field is the key used alongside the venue
    /// id, so multi-symbol deployments call this once per
    /// `(venue, symbol)` combination. Idempotent on re-
    /// registration.
    pub fn register_venue(&mut self, venue: VenueId, seed: VenueSeed) {
        self.seeds
            .insert((venue, seed.venue_product, seed.symbol.clone()), seed);
    }

    /// Update the best-bid / best-ask pair for the seed at
    /// `(venue, symbol)`. No-op if the slot is unregistered.
    /// Back-compat: scans all products on the venue and updates
    /// the first match. For multi-product-per-venue deployments,
    /// use `update_book_for_slot`.
    pub fn update_book_for(
        &mut self,
        venue: VenueId,
        symbol: &str,
        best_bid: Decimal,
        best_ask: Decimal,
    ) {
        if let Some(key) = self.find_slot_key(venue, symbol) {
            if let Some(seed) = self.seeds.get_mut(&key) {
                seed.best_bid = best_bid;
                seed.best_ask = best_ask;
            }
        }
    }

    /// Product-aware book update — targets exactly the seed at
    /// `(venue, product, symbol)`. Use this when the same venue
    /// has multiple products (spot + perp).
    pub fn update_book_for_slot(
        &mut self,
        venue: VenueId,
        product: mm_exchange_core::connector::VenueProduct,
        symbol: &str,
        best_bid: Decimal,
        best_ask: Decimal,
    ) {
        if let Some(seed) = self.seeds.get_mut(&(venue, product, symbol.to_string())) {
            seed.best_bid = best_bid;
            seed.best_ask = best_ask;
        }
    }

    /// Convenience wrapper for single-symbol-per-venue
    /// deployments — finds the first seed on `venue` and
    /// updates its book.
    pub fn update_book(&mut self, venue: VenueId, best_bid: Decimal, best_ask: Decimal) {
        if let Some(sym) = self.first_symbol_on(venue) {
            self.update_book_for(venue, &sym, best_bid, best_ask);
        }
    }

    /// Keyed fee-rate update. Targets the first matching
    /// `(venue, *, symbol)` slot.
    pub fn update_fees_for(
        &mut self,
        venue: VenueId,
        symbol: &str,
        maker_fee: Decimal,
        taker_fee: Decimal,
    ) {
        if let Some(key) = self.find_slot_key(venue, symbol) {
            if let Some(seed) = self.seeds.get_mut(&key) {
                seed.product.maker_fee = maker_fee;
                seed.product.taker_fee = taker_fee;
            }
        }
    }

    /// Single-symbol shorthand.
    pub fn update_fees(&mut self, venue: VenueId, maker_fee: Decimal, taker_fee: Decimal) {
        if let Some(sym) = self.first_symbol_on(venue) {
            self.update_fees_for(venue, &sym, maker_fee, taker_fee);
        }
    }

    /// Keyed queue-wait update.
    pub fn update_queue_wait_for(
        &mut self,
        venue: VenueId,
        symbol: &str,
        queue_wait_secs: Decimal,
    ) {
        if let Some(key) = self.find_slot_key(venue, symbol) {
            if let Some(seed) = self.seeds.get_mut(&key) {
                seed.queue_wait_secs = queue_wait_secs;
            }
        }
    }

    /// Single-symbol shorthand.
    pub fn update_queue_wait(&mut self, venue: VenueId, queue_wait_secs: Decimal) {
        if let Some(sym) = self.first_symbol_on(venue) {
            self.update_queue_wait_for(venue, &sym, queue_wait_secs);
        }
    }

    /// Keyed accessor — first matching `(venue, *, symbol)`.
    pub fn seed_for(&self, venue: VenueId, symbol: &str) -> Option<&VenueSeed> {
        let key = self.find_slot_key(venue, symbol)?;
        self.seeds.get(&key)
    }

    /// Product-aware accessor.
    pub fn seed_for_slot(
        &self,
        venue: VenueId,
        product: mm_exchange_core::connector::VenueProduct,
        symbol: &str,
    ) -> Option<&VenueSeed> {
        self.seeds.get(&(venue, product, symbol.to_string()))
    }

    /// Single-symbol convenience accessor. Returns the first
    /// seed on `venue` in symbol-sorted order.
    pub fn seed(&self, venue: VenueId) -> Option<&VenueSeed> {
        let sym = self.first_symbol_on(venue)?;
        self.seed_for(venue, &sym)
    }

    /// Distinct venue ids across every registered seed,
    /// sorted by venue enum ordinal. Legacy accessor.
    pub fn venues(&self) -> Vec<VenueId> {
        use std::collections::HashSet;
        let set: HashSet<VenueId> = self.seeds.keys().map(|(v, _, _)| *v).collect();
        let mut v: Vec<VenueId> = set.into_iter().collect();
        v.sort_by_key(|venue| *venue as u8);
        v
    }

    /// Every `(VenueId, Symbol)` pair registered, sorted
    /// deterministically. Dedupes products so a single venue
    /// running spot+perp on BTCUSDT shows once — legacy
    /// call sites don't care about the product distinction.
    pub fn venue_symbols(&self) -> Vec<(VenueId, String)> {
        use std::collections::HashSet;
        let set: HashSet<(VenueId, String)> =
            self.seeds.keys().map(|(v, _, s)| (*v, s.clone())).collect();
        let mut out: Vec<(VenueId, String)> = set.into_iter().collect();
        out.sort_by(|a, b| (a.0 as u8).cmp(&(b.0 as u8)).then_with(|| a.1.cmp(&b.1)));
        out
    }

    /// Product-aware enumeration — every registered
    /// `(venue, product, symbol)` slot.
    pub fn venue_slots(&self) -> Vec<VenueSlot> {
        let mut out: Vec<VenueSlot> = self.seeds.keys().cloned().collect();
        out.sort_by(|a, b| {
            (a.0 as u8)
                .cmp(&(b.0 as u8))
                .then_with(|| (a.1 as u8).cmp(&(b.1 as u8)))
                .then_with(|| a.2.cmp(&b.2))
        });
        out
    }

    /// First symbol registered on `venue` in symbol-sort
    /// order. `None` when the venue has no seeds. Private
    /// helper for the single-symbol convenience wrappers.
    fn first_symbol_on(&self, venue: VenueId) -> Option<String> {
        let mut syms: Vec<String> = self
            .seeds
            .keys()
            .filter_map(|(v, _, s)| if *v == venue { Some(s.clone()) } else { None })
            .collect();
        syms.sort();
        syms.into_iter().next()
    }

    /// Find the first slot key matching `(venue, *, symbol)`.
    /// Deterministic: scans products in enum-ordinal order.
    fn find_slot_key(&self, venue: VenueId, symbol: &str) -> Option<VenueSlot> {
        let mut hits: Vec<VenueSlot> = self
            .seeds
            .keys()
            .filter(|(v, _, s)| *v == venue && s == symbol)
            .cloned()
            .collect();
        hits.sort_by_key(|(_, p, _)| *p as u8);
        hits.into_iter().next()
    }

    /// Walk the bundle's connectors, pull live rate-limit
    /// budgets, and combine with the seeded state to produce
    /// one `VenueSnapshot` per registered slot. The same
    /// connector may contribute multiple snapshots when the
    /// aggregator holds multiple symbols or products on that
    /// venue.
    pub async fn collect(&self, bundle: &ConnectorBundle, _side: Side) -> Vec<VenueSnapshot> {
        let mut out = Vec::new();
        for connector in bundle.all_connectors() {
            let venue = connector.venue_id();
            let product = connector.product();
            let rate_limit_remaining = connector.rate_limit_remaining().await;
            for ((v, p, _sym), seed) in &self.seeds {
                if *v != venue || *p != product {
                    continue;
                }
                out.push(snapshot_from_seed(connector, seed, rate_limit_remaining));
            }
        }
        out
    }

    /// Synchronous variant of `collect` used by unit tests.
    /// Each `(venue, remaining)` tuple produces one snapshot
    /// per seeded `(venue, symbol)` pair sharing that venue
    /// id — so a test that seeds `(Binance, "BTCUSDT")` and
    /// `(Binance, "ETHUSDT")` and queries `(Binance, 1000)`
    /// receives two snapshots, one per symbol.
    pub fn collect_synthetic(&self, venues: &[(VenueId, u32)]) -> Vec<VenueSnapshot> {
        let mut out = Vec::new();
        for (venue, remaining) in venues {
            for ((v, _p, _sym), seed) in &self.seeds {
                if *v != *venue {
                    continue;
                }
                out.push(synthetic_snapshot(*venue, seed, *remaining));
            }
        }
        out
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
        slippage_bps_per_unit: derive_slippage_coef(seed),
    }
}

/// Derive the convex-slippage coefficient from a seed.
/// `slippage_bps_per_unit ≈ spread_bps / available_qty` —
/// routes toward venues with tighter spread / thicker top-
/// of-book. A seed with no book (best_bid / best_ask both
/// zero) returns `0` so the cost model falls back to
/// linear behaviour and we don't produce spurious huge
/// slippage figures.
fn derive_slippage_coef(seed: &VenueSeed) -> Decimal {
    if seed.best_bid <= Decimal::ZERO || seed.best_ask <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    if seed.available_qty <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let mid = (seed.best_bid + seed.best_ask) / Decimal::from(2u32);
    if mid <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let spread = seed.best_ask - seed.best_bid;
    let spread_bps = spread / mid * Decimal::from(10_000u32);
    spread_bps / seed.available_qty
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
        slippage_bps_per_unit: derive_slippage_coef(seed),
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
            venue_product: mm_exchange_core::connector::VenueProduct::Spot,
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

    // ---------------------------------------------------------
    // Epic A stage-2 #5 — multi-symbol per venue
    // ---------------------------------------------------------

    fn seed_named(symbol: &str, available: Decimal) -> VenueSeed {
        VenueSeed {
            symbol: symbol.into(),
            product: ProductSpec {
                symbol: symbol.into(),
                base_asset: symbol[..3].to_string(),
                quote_asset: "USDT".into(),
                tick_size: dec!(0.01),
                lot_size: dec!(0.00001),
                min_notional: dec!(10),
                maker_fee: dec!(0.0001),
                taker_fee: dec!(0.0005),
                trading_status: Default::default(),
            },
            venue_product: mm_exchange_core::connector::VenueProduct::Spot,
            available_qty: available,
            queue_wait_secs: dec!(10),
            best_bid: dec!(49990),
            best_ask: dec!(50010),
        }
    }

    /// Two symbols on the same venue produce two distinct
    /// snapshots from `collect_synthetic`.
    #[test]
    fn multi_symbol_same_venue_produces_two_snapshots() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed_named("BTCUSDT", dec!(1)));
        agg.register_venue(VenueId::Binance, seed_named("ETHUSDT", dec!(10)));
        let snaps = agg.collect_synthetic(&[(VenueId::Binance, 100)]);
        assert_eq!(snaps.len(), 2, "expected 2 snapshots, got {snaps:?}");
        let symbols: std::collections::HashSet<String> =
            snaps.iter().map(|s| s.symbol.clone()).collect();
        assert!(symbols.contains("BTCUSDT"));
        assert!(symbols.contains("ETHUSDT"));
    }

    /// Keyed `seed_for` / `update_book_for` target a specific
    /// `(venue, symbol)` slot without touching siblings.
    #[test]
    fn keyed_update_book_does_not_leak_to_sibling_symbol() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed_named("BTCUSDT", dec!(1)));
        agg.register_venue(VenueId::Binance, seed_named("ETHUSDT", dec!(10)));
        agg.update_book_for(VenueId::Binance, "BTCUSDT", dec!(100), dec!(200));
        let btc = agg.seed_for(VenueId::Binance, "BTCUSDT").unwrap();
        let eth = agg.seed_for(VenueId::Binance, "ETHUSDT").unwrap();
        assert_eq!(btc.best_bid, dec!(100));
        assert_eq!(btc.best_ask, dec!(200));
        // ETH sibling is untouched.
        assert_eq!(eth.best_bid, dec!(49990));
        assert_eq!(eth.best_ask, dec!(50010));
    }

    /// `venue_symbols` returns every registered slot,
    /// deterministically sorted.
    #[test]
    fn venue_symbols_lists_all_slots_sorted() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Bybit, seed_named("ETHUSDT", dec!(10)));
        agg.register_venue(VenueId::Binance, seed_named("BTCUSDT", dec!(1)));
        agg.register_venue(VenueId::Binance, seed_named("ETHUSDT", dec!(10)));
        let pairs = agg.venue_symbols();
        assert_eq!(pairs.len(), 3);
        // Sort order: venue ordinal then symbol alpha.
        assert_eq!(pairs[0], (VenueId::Binance, "BTCUSDT".to_string()));
        assert_eq!(pairs[1], (VenueId::Binance, "ETHUSDT".to_string()));
        assert_eq!(pairs[2], (VenueId::Bybit, "ETHUSDT".to_string()));
    }

    /// `venues` de-dupes — the multi-symbol slot list
    /// above projects to two distinct venue tags.
    #[test]
    fn venues_dedupes_across_symbol_slots() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed_named("BTCUSDT", dec!(1)));
        agg.register_venue(VenueId::Binance, seed_named("ETHUSDT", dec!(10)));
        agg.register_venue(VenueId::Bybit, seed_named("ETHUSDT", dec!(10)));
        let vs = agg.venues();
        assert_eq!(vs.len(), 2);
        assert_eq!(vs[0], VenueId::Binance);
        assert_eq!(vs[1], VenueId::Bybit);
    }

    /// Single-symbol legacy `seed(venue)` accessor still
    /// works — picks the first registered symbol.
    #[test]
    fn legacy_seed_accessor_returns_first_registered_symbol() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed_named("BTCUSDT", dec!(1)));
        assert_eq!(agg.seed(VenueId::Binance).unwrap().symbol, "BTCUSDT");
    }

    /// `update_queue_wait_for` targets only the keyed slot.
    #[test]
    fn keyed_update_queue_wait_targets_slot() {
        let mut agg = VenueStateAggregator::new();
        agg.register_venue(VenueId::Binance, seed_named("BTCUSDT", dec!(1)));
        agg.register_venue(VenueId::Binance, seed_named("ETHUSDT", dec!(10)));
        agg.update_queue_wait_for(VenueId::Binance, "ETHUSDT", dec!(42));
        assert_eq!(
            agg.seed_for(VenueId::Binance, "BTCUSDT")
                .unwrap()
                .queue_wait_secs,
            dec!(10),
            "BTC slot unchanged"
        );
        assert_eq!(
            agg.seed_for(VenueId::Binance, "ETHUSDT")
                .unwrap()
                .queue_wait_secs,
            dec!(42)
        );
    }
}
