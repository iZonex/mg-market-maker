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
}

impl BasisStrategy {
    /// Create a basis strategy with the given shift and max-basis
    /// threshold.
    pub fn new(shift: Decimal, max_basis_bps: Decimal) -> Self {
        Self {
            shift,
            max_basis_bps,
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

        let reservation = self.reservation_price(spot_mid, ctx.ref_price);

        // Symmetric post-only ladder around the reservation.
        // Each level is min_spread_bps wider than the previous.
        let level_step = bps_to_frac(ctx.config.min_spread_bps) * spot_mid;
        let half_min = level_step / dec!(2);
        let max_distance = bps_to_frac(ctx.config.max_distance_bps) * spot_mid;
        let order_size = ctx.product.round_qty(ctx.config.order_size);

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);
        for level in 0..ctx.config.num_levels {
            let offset = half_min + Decimal::from(level as u64) * level_step;
            let raw_bid = reservation - offset;
            let raw_ask = reservation + offset;

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
        }
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
}
