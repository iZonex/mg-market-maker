//! Basis-aware quoting strategy for cross-product (spot ↔ perp) MM.
//!
//! See `docs/research/spot-mm-specifics.md` §10 and AD-9 in the
//! spot-and-cross-product epic.
//!
//! Thesis: on BTC/ETH-class markets, price discovery happens in
//! perps; spot lags by 50–200 ms during volatile regimes. A
//! spot-only market maker quoting around the spot mid is
//! systematically picked off — the perp already moved, the spot
//! book hasn't caught up, and the MM's stale quote gets filled by
//! informed flow riding the basis.
//!
//! `BasisStrategy` fixes this by computing a **basis-shifted
//! reservation price**:
//!
//! ```text
//! reservation = spot_mid + shift * (perp_mid - spot_mid)
//! ```
//!
//! where `shift ∈ [0, 1]` controls how aggressively the MM follows
//! the perp. `shift = 0` reduces to plain spot mid quoting;
//! `shift = 1` fully tracks the perp. Typical values are 0.3–0.7,
//! higher in trending regimes, lower in quiet ones.
//!
//! The basis shift is gated on `|basis| ≤ basis_threshold_bps`: if
//! the basis blows out past the threshold (regulatory event,
//! liquidity crisis, oracle dispute) the strategy pulls quotes
//! entirely rather than chase a dislocation. `basis_threshold_bps`
//! is owned by `InstrumentPair` and configurable per-pair.
//!
//! The rest of the pricing is a standard symmetric post-only
//! ladder around the basis-shifted reservation — no A-S risk term,
//! no inventory skew (Sprint H1 keeps scope tight; inventory
//! handling can layer on top exactly as it does for
//! `AvellanedaStoikov`).

use mm_common::orderbook::LocalOrderBook;
use mm_common::types::{Price, PriceLevel, Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

use crate::features::market_impact;
use crate::r#trait::{bps_to_frac, Strategy, StrategyContext};

/// Basis-shifted quoting strategy.
///
/// Consumes `StrategyContext.ref_price` (the hedge mid pushed by
/// the engine's `hedge_book`). Falls back to plain spot quoting
/// when `ref_price` is `None` — the engine sets `ref_price = None`
/// in single-connector mode or before the hedge book has seen its
/// first tick, so this fallback is the normal early-startup state
/// rather than an error path.
pub struct BasisStrategy {
    /// How far to shift the reservation price toward the perp mid.
    /// `[0, 1]` — 0 disables the basis signal, 1 fully tracks perp.
    pub shift: Decimal,
    /// Maximum allowed basis (in bps of spot mid). Above this the
    /// strategy returns an empty quote set — safer to stand down
    /// than chase a dislocated book.
    pub max_basis_bps: Decimal,
    /// Maximum acceptable hedge-book staleness in milliseconds.
    /// `None` disables the gate (the legacy same-venue
    /// behaviour); `Some(ms)` is the cross-venue mode where the
    /// strategy stands down whenever
    /// `StrategyContext.hedge_book_age_ms > ms`. Cross-venue
    /// feeds have higher latency variance than same-venue ones,
    /// so a stale hedge mid is a much louder failure mode.
    /// P1.4 stage-1.
    pub max_hedge_staleness_ms: Option<i64>,
}

impl BasisStrategy {
    /// Create a same-venue basis strategy with the given shift
    /// and max-basis threshold.
    pub fn new(shift: Decimal, max_basis_bps: Decimal) -> Self {
        Self {
            shift,
            max_basis_bps,
            max_hedge_staleness_ms: None,
        }
    }

    /// Create a cross-venue basis strategy. Identical to `new`
    /// but with a non-zero hedge-book staleness gate. Production
    /// cross-venue feeds (Coinbase spot ↔ Binance perp,
    /// Binance spot ↔ Bybit perp) routinely jitter 200-800 ms;
    /// the default 1500 ms gate stands down the strategy when
    /// the hedge feed pauses long enough that the basis signal
    /// becomes unreliable. P1.4 stage-1.
    pub fn cross_venue(shift: Decimal, max_basis_bps: Decimal, max_staleness_ms: i64) -> Self {
        Self {
            shift,
            max_basis_bps,
            max_hedge_staleness_ms: Some(max_staleness_ms),
        }
    }

    /// Compute the basis-shifted reservation price. Returns the
    /// plain spot mid when no hedge reference is available.
    pub fn reservation_price(&self, spot_mid: Price, hedge_mid: Option<Price>) -> Price {
        match hedge_mid {
            Some(perp) => spot_mid + self.shift * (perp - spot_mid),
            None => spot_mid,
        }
    }

    /// Signed basis in bps of spot mid — positive = perp above
    /// spot, negative = perp below. Zero when no hedge reference
    /// is available.
    pub fn basis_bps(&self, spot_mid: Price, hedge_mid: Option<Price>) -> Decimal {
        match hedge_mid {
            Some(perp) if !spot_mid.is_zero() => (perp - spot_mid) / spot_mid * dec!(10_000),
            _ => dec!(0),
        }
    }

    /// Expected **real** cross edge for a hypothetical maker
    /// fill on the primary leg plus a taker hedge on the hedge
    /// leg, in bps of the spot mid.
    ///
    /// - `maker_side` — side of the PRIMARY leg fill (buy = we
    ///   received base, need to hedge by selling perp;
    ///   sell = we delivered base, hedge by buying perp).
    /// - `maker_price` — the assumed primary-leg fill price.
    /// - `size` — order size in base units.
    /// - `hedge_book` — full hedge-leg order book at the moment
    ///   of the hypothetical decision.
    ///
    /// Returns the edge as `(expected_hedge_price - maker_price) /
    /// maker_price * 10_000` signed so **positive = edge is
    /// profitable** for the cross (we buy on the primary leg
    /// below the real hedge VWAP, or sell above it). Returns
    /// `None` when the hedge book cannot absorb the full size
    /// (partial fill) or is empty.
    ///
    /// Unlike the midpoint-shift logic in `compute_quotes` —
    /// which uses `ref_price` (a single scalar mid) — this
    /// helper walks the real depth via
    /// [`crate::features::market_impact`]. Callers that have
    /// access to the hedge book (engines running in
    /// dual-connector mode with `StrategyContext.hedge_book =
    /// Some(_)`) can use this to gate quote placement on a
    /// real profitable cross, not just a mid-based estimate.
    pub fn expected_cross_edge_bps(
        &self,
        maker_side: Side,
        maker_price: Price,
        size: Decimal,
        hedge_book: &LocalOrderBook,
    ) -> Option<Decimal> {
        if maker_price.is_zero() || size <= Decimal::ZERO {
            return None;
        }
        // Hedge side is always opposite of the maker side.
        let hedge_side = match maker_side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let levels: Vec<PriceLevel> = match hedge_side {
            // Hedge sell walks the hedge bids (best-first).
            Side::Sell => hedge_book
                .bids
                .iter()
                .rev()
                .map(|(p, q)| PriceLevel { price: *p, qty: *q })
                .collect(),
            // Hedge buy walks the hedge asks (best-first).
            Side::Buy => hedge_book
                .asks
                .iter()
                .map(|(p, q)| PriceLevel { price: *p, qty: *q })
                .collect(),
        };
        let impact = market_impact(&levels, hedge_side, size, maker_price)?;
        if impact.partial {
            return None;
        }
        // `market_impact::impact_bps` is signed so positive =
        // unfavourable to the taker. Edge to the maker leg is
        // the NEGATION: a taker cost of -10 bps (the taker is
        // getting a better price than the reference) is a +10
        // bps edge to the maker.
        Some(-impact.impact_bps)
    }
}

impl Strategy for BasisStrategy {
    fn name(&self) -> &str {
        "basis"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let spot_mid = ctx.mid_price;
        if spot_mid.is_zero() {
            return vec![];
        }

        // Cross-venue staleness gate (P1.4 stage-1): when the
        // strategy is configured with a non-None
        // `max_hedge_staleness_ms`, stand down entirely if the
        // hedge feed is older than the gate. Cross-venue feeds
        // jitter much more than same-venue ones, and a stale
        // reference price is a louder failure mode than a wide
        // basis — better to skip a refresh than quote against a
        // dead mid.
        if let Some(gate) = self.max_hedge_staleness_ms {
            let stale = match ctx.hedge_book_age_ms {
                Some(age) => age > gate,
                // No staleness reading at all (no hedge book yet)
                // is also unsafe in cross-venue mode.
                None => true,
            };
            if stale {
                debug!(
                    strategy = "basis",
                    age_ms = ?ctx.hedge_book_age_ms,
                    gate_ms = gate,
                    "hedge book stale — quoting disabled"
                );
                return vec![];
            }
        }

        // Basis gate: |basis| > threshold → stand down entirely.
        let abs_basis_bps = self.basis_bps(spot_mid, ctx.ref_price).abs();
        if abs_basis_bps > self.max_basis_bps {
            debug!(
                strategy = "basis",
                %abs_basis_bps,
                max = %self.max_basis_bps,
                "basis exceeds threshold — quoting disabled"
            );
            return vec![];
        }

        let mut reservation = self.reservation_price(spot_mid, ctx.ref_price);
        // P1.3 borrow-cost shim — same shape as Avellaneda. When
        // the engine threads in `borrow_cost_bps`, push the
        // basis-shifted reservation up by the carry surcharge so
        // the spot ask side compensates for the loan we'd take
        // to deliver against the fill.
        if let Some(bps) = ctx.borrow_cost_bps {
            if bps > dec!(0) {
                reservation += bps_to_frac(bps) * spot_mid;
            }
        }

        // Symmetric post-only ladder around the reservation.
        // Each level is min_spread_bps wider than the previous.
        let level_step = bps_to_frac(ctx.config.min_spread_bps) * spot_mid;
        let half_min = level_step / dec!(2);
        let max_distance = bps_to_frac(ctx.config.max_distance_bps) * spot_mid;
        let order_size = ctx.product.round_qty(ctx.config.order_size);

        // Epic D stage-3 — Cartea adverse-selection widening
        // applied to the level-0 half. Per-side path uses
        // independent rho_b / rho_a; symmetric path uses the
        // single ctx.as_prob; both safety-clamp at the
        // wave-1 half_min floor so informed flow on either
        // side never produces a sub-min-spread quote. When
        // both per-side and symmetric are absent the half
        // collapses to the wave-1 half_min byte-identically.
        let sigma = ctx.volatility;
        let sqrt_t = crate::volatility::decimal_sqrt(ctx.time_remaining);
        let (bid_half_min, ask_half_min) = match (ctx.as_prob_bid, ctx.as_prob_ask) {
            (Some(rho_b), Some(rho_a)) => {
                let bid_widen = (dec!(1) - dec!(2) * rho_b) * sigma * sqrt_t;
                let ask_widen = (dec!(1) - dec!(2) * rho_a) * sigma * sqrt_t;
                (
                    (half_min + bid_widen).max(half_min),
                    (half_min + ask_widen).max(half_min),
                )
            }
            _ => {
                let half = match ctx.as_prob {
                    None => half_min,
                    Some(rho) if rho == dec!(0.5) => half_min,
                    Some(rho) => {
                        let as_delta = (dec!(1) - dec!(2) * rho) * sigma * sqrt_t;
                        (half_min + as_delta).max(half_min)
                    }
                };
                (half, half)
            }
        };

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);
        for level in 0..ctx.config.num_levels {
            let level_offset = Decimal::from(level as u64) * level_step;
            let bid_offset = bid_half_min + level_offset;
            let ask_offset = ask_half_min + level_offset;
            let raw_bid = reservation - bid_offset;
            let raw_ask = reservation + ask_offset;

            let bid_price = ctx
                .product
                .round_price(raw_bid.max(spot_mid - max_distance).max(dec!(0)));
            let ask_price = ctx
                .product
                .round_price(raw_ask.min(spot_mid + max_distance));

            // Post-only safety: if the basis shift would cross
            // the touch on either side, drop that leg rather than
            // let the exchange reject the post.
            let best_ask = ctx.book.best_ask().unwrap_or(Decimal::MAX);
            let best_bid = ctx.book.best_bid().unwrap_or_else(|| dec!(0));

            let bid = if bid_price > dec!(0)
                && bid_price < best_ask
                && ctx.product.meets_min_notional(bid_price, order_size)
            {
                Some(Quote {
                    side: Side::Buy,
                    price: bid_price,
                    qty: order_size,
                })
            } else {
                None
            };
            let ask = if ask_price > dec!(0)
                && ask_price > best_bid
                && ctx.product.meets_min_notional(ask_price, order_size)
            {
                Some(Quote {
                    side: Side::Sell,
                    price: ask_price,
                    qty: order_size,
                })
            } else {
                None
            };

            quotes.push(QuotePair { bid, ask });
        }

        debug!(
            strategy = "basis",
            %reservation,
            %spot_mid,
            ref_price = ?ctx.ref_price,
            levels = quotes.len(),
            "computed basis quotes"
        );
        quotes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::{MarketMakerConfig, StrategyType};
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::{PriceLevel, ProductSpec};

    fn test_product() -> ProductSpec {
        ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.00001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        }
    }

    fn test_config() -> MarketMakerConfig {
        MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 2,
            order_size: dec!(0.01),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(10),
            max_distance_bps: dec!(200),
            strategy: StrategyType::AvellanedaStoikov,
            momentum_enabled: false,
            momentum_window: 200,
            basis_shift: dec!(0.5),
            market_resilience_enabled: true,
            otr_enabled: true,
            hma_enabled: true,
            adaptive_enabled: false,
            apply_pair_class_template: false,
            hma_window: 9,
            momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
            momentum_learned_microprice_online: false,
            momentum_learned_microprice_horizon: 10,
            user_stream_enabled: true,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: true,
            amend_max_ticks: 2,
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
            fee_tier_refresh_enabled: true,
            fee_tier_refresh_secs: 600,
            borrow_enabled: false,
            borrow_rate_refresh_secs: 1800,
            borrow_holding_secs: 3600,
            borrow_max_base: dec!(0),
            borrow_buffer_base: dec!(0),
            pair_lifecycle_enabled: true,
            pair_lifecycle_refresh_secs: 300,
            var_guard_enabled: false,
            var_guard_limit_95: None,
            var_guard_limit_99: None,
            var_guard_ewma_lambda: None,
            cross_venue_basis_max_staleness_ms: 1500,
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false,
            sor_dispatch_interval_secs: 5,
            sor_urgency: rust_decimal_macros::dec!(0.4),
            sor_target_qty_source: mm_common::config::SorTargetSource::InventoryExcess,
            sor_inventory_threshold: rust_decimal::Decimal::ZERO,
            sor_trade_rate_window_secs: 60,
            sor_queue_refresh_secs: 2,
        }
    }

    fn book_at(mid: Decimal, half_spread: Decimal) -> LocalOrderBook {
        // The test book is intentionally wide so that
        // basis-shifted reservations (up to ~120 price units
        // away from spot mid at 50k) still sit inside the
        // best-bid/best-ask envelope and thus survive the
        // post-only crossing check. Real production books are
        // tighter but basis shifts are proportionally smaller.
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: mid - half_spread,
                qty: dec!(10),
            }],
            vec![PriceLevel {
                price: mid + half_spread,
                qty: dec!(10),
            }],
            1,
        );
        book
    }

    fn ctx<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        mid: Price,
        ref_price: Option<Price>,
    ) -> StrategyContext<'a> {
        StrategyContext {
            book,
            product,
            config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: mid,
            ref_price,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        }
    }

    fn ctx_with_age<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        mid: Price,
        ref_price: Option<Price>,
        age_ms: Option<i64>,
    ) -> StrategyContext<'a> {
        let mut c = ctx(book, product, config, mid, ref_price);
        c.hedge_book_age_ms = age_ms;
        c
    }

    #[test]
    fn reservation_falls_back_to_spot_mid_without_hedge() {
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        let r = s.reservation_price(dec!(50_000), None);
        assert_eq!(r, dec!(50_000));
    }

    #[test]
    fn reservation_shifts_toward_perp_mid() {
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        // Spot 50_000, perp 50_100 (+20 bps basis).
        // shift=0.5 → reservation = 50_000 + 0.5 * 100 = 50_050.
        let r = s.reservation_price(dec!(50_000), Some(dec!(50_100)));
        assert_eq!(r, dec!(50_050));
    }

    #[test]
    fn basis_bps_is_signed() {
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        assert_eq!(
            s.basis_bps(dec!(50_000), Some(dec!(50_100))),
            dec!(20),
            "perp above spot = positive basis"
        );
        assert_eq!(
            s.basis_bps(dec!(50_000), Some(dec!(49_900))),
            dec!(-20),
            "perp below spot = negative basis"
        );
        assert_eq!(s.basis_bps(dec!(50_000), None), dec!(0));
    }

    #[test]
    fn quotes_center_on_reservation_not_spot_mid() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(500));
        let strategy = BasisStrategy::new(dec!(0.6), dec!(100));

        // Perp 50_200 → reservation = 50_000 + 0.6 * 200 = 50_120.
        let quotes = strategy.compute_quotes(&ctx(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_200)),
        ));
        assert!(!quotes.is_empty());

        // Midpoint of level-0 bid/ask must be the reservation, not spot mid.
        let q0 = &quotes[0];
        let bid0 = q0.bid.as_ref().expect("bid level 0");
        let ask0 = q0.ask.as_ref().expect("ask level 0");
        let quote_mid = (bid0.price + ask0.price) / dec!(2);
        assert_eq!(
            quote_mid,
            dec!(50_120),
            "bid={} ask={} mid={} reservation=50120",
            bid0.price,
            ask0.price,
            quote_mid
        );
    }

    #[test]
    fn basis_gate_pulls_quotes_when_threshold_exceeded() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(500));
        let strategy = BasisStrategy::new(dec!(0.5), dec!(20));

        // +30 bps basis — exceeds 20 bps threshold.
        let quotes = strategy.compute_quotes(&ctx(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_150)),
        ));
        let all_empty =
            quotes.iter().all(|q| q.bid.is_none() && q.ask.is_none()) || quotes.is_empty();
        assert!(
            all_empty,
            "basis > threshold must produce no actionable quotes"
        );
    }

    #[test]
    fn basis_gate_allows_quotes_at_exact_threshold() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(500));
        let strategy = BasisStrategy::new(dec!(0.5), dec!(20));

        // +20 bps basis — exactly at threshold, should still quote.
        let quotes = strategy.compute_quotes(&ctx(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_100)),
        ));
        assert!(quotes.iter().any(|q| q.bid.is_some() || q.ask.is_some()));
    }

    #[test]
    fn fallback_to_spot_mid_when_ref_price_absent() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(500));
        let strategy = BasisStrategy::new(dec!(0.5), dec!(50));

        // ref_price=None → reservation == spot_mid == 50_000.
        // Quote midpoint must equal 50_000.
        let quotes = strategy.compute_quotes(&ctx(&book, &product, &config, dec!(50_000), None));
        let q0 = &quotes[0];
        let bid0 = q0.bid.as_ref().unwrap();
        let ask0 = q0.ask.as_ref().unwrap();
        assert_eq!((bid0.price + ask0.price) / dec!(2), dec!(50_000));
    }

    #[test]
    fn zero_shift_is_plain_spot_quoting() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(500));
        let strategy = BasisStrategy::new(dec!(0), dec!(100));

        // shift=0 must ignore the perp mid entirely.
        let quotes_with_perp = strategy.compute_quotes(&ctx(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_500)),
        ));
        let quotes_without_perp =
            strategy.compute_quotes(&ctx(&book, &product, &config, dec!(50_000), None));
        assert_eq!(
            quotes_with_perp[0].bid.as_ref().unwrap().price,
            quotes_without_perp[0].bid.as_ref().unwrap().price
        );
    }

    #[test]
    fn negative_basis_shifts_reservation_below_spot_mid() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(500));
        let strategy = BasisStrategy::new(dec!(0.5), dec!(100));

        // Perp 49_900 → reservation = 50_000 + 0.5 * (-100) = 49_950.
        let quotes = strategy.compute_quotes(&ctx(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(49_900)),
        ));
        let q0 = &quotes[0];
        let bid0 = q0.bid.as_ref().unwrap();
        let ask0 = q0.ask.as_ref().unwrap();
        assert_eq!((bid0.price + ask0.price) / dec!(2), dec!(49_950));
    }

    // ---- expected_cross_edge_bps ----

    fn hedge_book_at(bid: Decimal, ask: Decimal, depth: Decimal) -> LocalOrderBook {
        let mut hb = LocalOrderBook::new("BTC-PERP".into());
        hb.apply_snapshot(
            vec![PriceLevel {
                price: bid,
                qty: depth,
            }],
            vec![PriceLevel {
                price: ask,
                qty: depth,
            }],
            1,
        );
        hb
    }

    #[test]
    fn cross_edge_buy_primary_sell_hedge_above_reference_is_positive() {
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        // Maker buys at 50_000 on primary. Hedge bid is 50_050
        // (perp above spot → positive basis → sell perp at
        // higher price than we paid → positive edge).
        let hb = hedge_book_at(dec!(50_050), dec!(50_060), dec!(10));
        let edge = s
            .expected_cross_edge_bps(Side::Buy, dec!(50_000), dec!(1), &hb)
            .unwrap();
        // 50_050 - 50_000 = +50 on 50_000 → +10 bps.
        assert!(edge > dec!(9) && edge < dec!(11));
    }

    #[test]
    fn cross_edge_returns_none_on_thin_hedge_book() {
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        // Only 0.5 BTC on the hedge bid — can't absorb our 1 BTC size.
        let hb = hedge_book_at(dec!(50_050), dec!(50_060), dec!(0.5));
        assert!(s
            .expected_cross_edge_bps(Side::Buy, dec!(50_000), dec!(1), &hb)
            .is_none());
    }

    #[test]
    fn cross_edge_sell_primary_buy_hedge_flips_sign() {
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        // Maker sells at 50_000 on primary; hedge asks at
        // 49_950 (perp below spot → buy perp cheap → +10 bps).
        let hb = hedge_book_at(dec!(49_940), dec!(49_950), dec!(10));
        let edge = s
            .expected_cross_edge_bps(Side::Sell, dec!(50_000), dec!(1), &hb)
            .unwrap();
        assert!(edge > dec!(9) && edge < dec!(11));
    }

    #[test]
    fn cross_edge_negative_when_basis_moves_against_maker() {
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        // Maker buys at 50_000; hedge bid is 49_950 → selling
        // hedge at 49_950 means we lock in -10 bps loss.
        let hb = hedge_book_at(dec!(49_950), dec!(49_960), dec!(10));
        let edge = s
            .expected_cross_edge_bps(Side::Buy, dec!(50_000), dec!(1), &hb)
            .unwrap();
        assert!(edge < dec!(-9) && edge > dec!(-11));
    }

    #[test]
    fn cross_edge_zero_size_returns_none() {
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        let hb = hedge_book_at(dec!(50_000), dec!(50_010), dec!(10));
        assert!(s
            .expected_cross_edge_bps(Side::Buy, dec!(50_000), dec!(0), &hb)
            .is_none());
    }

    /// P1.4 stage-1: cross-venue mode with a fresh hedge book
    /// must quote normally. Regression anchor for the
    /// happy-path branch through the staleness gate.
    #[test]
    fn cross_venue_fresh_book_quotes_normally() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(50));
        let s = BasisStrategy::cross_venue(dec!(0.5), dec!(50), 1500);
        let context = ctx_with_age(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            Some(200),
        );
        let quotes = s.compute_quotes(&context);
        assert!(!quotes.is_empty());
    }

    /// Cross-venue mode with a stale hedge book (age > gate)
    /// must stand down — empty quote set, regardless of how
    /// reasonable the basis is.
    #[test]
    fn cross_venue_stale_hedge_book_stands_down() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(50));
        let s = BasisStrategy::cross_venue(dec!(0.5), dec!(50), 1500);
        let context = ctx_with_age(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            Some(2000),
        );
        assert!(s.compute_quotes(&context).is_empty());
    }

    /// Cross-venue mode without any age reading at all (no
    /// hedge book yet, or engine forgot to thread it) must
    /// also stand down — better to skip the refresh than
    /// quote against an unknown freshness.
    #[test]
    fn cross_venue_no_age_reading_stands_down() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(50));
        let s = BasisStrategy::cross_venue(dec!(0.5), dec!(50), 1500);
        let context = ctx_with_age(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            None,
        );
        assert!(s.compute_quotes(&context).is_empty());
    }

    /// Same-venue mode (`max_hedge_staleness_ms = None`) must
    /// preserve the legacy behaviour: a stale or missing age
    /// reading does not cause stand-down. Regression anchor
    /// for the "P1.4 must not break P0/P1.x" invariant.
    #[test]
    fn same_venue_mode_ignores_staleness() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(50));
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        let context = ctx_with_age(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            Some(60_000),
        );
        assert!(!s.compute_quotes(&context).is_empty());
    }

    // ---- Epic D stage-3 — Cartea AS + per-side ρ on Basis ----

    #[allow(clippy::too_many_arguments)]
    fn ctx_with_as<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        mid: Price,
        ref_price: Option<Price>,
        as_prob: Option<Decimal>,
        as_prob_bid: Option<Decimal>,
        as_prob_ask: Option<Decimal>,
    ) -> StrategyContext<'a> {
        let mut c = ctx(book, product, config, mid, ref_price);
        c.as_prob = as_prob;
        c.as_prob_bid = as_prob_bid;
        c.as_prob_ask = as_prob_ask;
        // Use a large sigma so the AS perturbation is
        // visible above the level-step floor.
        c.volatility = dec!(50);
        c
    }

    #[test]
    fn basis_as_prob_none_is_byte_identical_to_wave1() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(50));
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        let baseline = ctx(&book, &product, &config, dec!(50_000), Some(dec!(50_010)));
        let with_none = ctx_with_as(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            None,
            None,
            None,
        );
        // Override the volatility back to baseline so the
        // identity check is meaningful.
        let mut with_none = with_none;
        with_none.volatility = baseline.volatility;
        let q_base = s.compute_quotes(&baseline);
        let q_none = s.compute_quotes(&with_none);
        assert_eq!(q_base.len(), q_none.len());
        for (a, b) in q_base.iter().zip(q_none.iter()) {
            assert_eq!(
                a.bid.as_ref().map(|q| q.price),
                b.bid.as_ref().map(|q| q.price)
            );
            assert_eq!(
                a.ask.as_ref().map(|q| q.price),
                b.ask.as_ref().map(|q| q.price)
            );
        }
    }

    #[test]
    fn basis_symmetric_low_rho_widens_both_sides() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(50));
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        let neutral = ctx_with_as(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            Some(dec!(0.5)),
            None,
            None,
        );
        let widen = ctx_with_as(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            Some(dec!(0)),
            None,
            None,
        );
        let q_n = s.compute_quotes(&neutral);
        let q_w = s.compute_quotes(&widen);
        let bid_n = q_n[0].bid.as_ref().unwrap().price;
        let bid_w = q_w[0].bid.as_ref().unwrap().price;
        let ask_n = q_n[0].ask.as_ref().unwrap().price;
        let ask_w = q_w[0].ask.as_ref().unwrap().price;
        // Low ρ widens — bid moves down, ask moves up.
        assert!(bid_w < bid_n, "bid should move down: {bid_w} vs {bid_n}");
        assert!(ask_w > ask_n, "ask should move up: {ask_w} vs {ask_n}");
    }

    #[test]
    fn basis_per_side_widens_one_side_independently() {
        let product = test_product();
        let config = test_config();
        let book = book_at(dec!(50_000), dec!(50));
        let s = BasisStrategy::new(dec!(0.5), dec!(50));
        let neutral = ctx_with_as(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            None,
            Some(dec!(0.5)),
            Some(dec!(0.5)),
        );
        let widen_bid = ctx_with_as(
            &book,
            &product,
            &config,
            dec!(50_000),
            Some(dec!(50_010)),
            None,
            Some(dec!(0)),
            Some(dec!(0.5)),
        );
        let q_n = s.compute_quotes(&neutral);
        let q_w = s.compute_quotes(&widen_bid);
        let bid_n = q_n[0].bid.as_ref().unwrap().price;
        let bid_w = q_w[0].bid.as_ref().unwrap().price;
        let ask_n = q_n[0].ask.as_ref().unwrap().price;
        let ask_w = q_w[0].ask.as_ref().unwrap().price;
        // Bid widens (moves down); ask unchanged.
        assert!(bid_w < bid_n);
        assert_eq!(ask_w, ask_n);
    }
}
