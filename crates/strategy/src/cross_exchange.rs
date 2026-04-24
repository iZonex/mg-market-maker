use mm_common::types::{Price, Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

use crate::r#trait::{bps_to_frac, Strategy, StrategyContext};

/// Cross-Exchange Market Making strategy.
///
/// Make on venue A (post limit orders), hedge on venue B (take on fill).
///
/// How it works:
/// 1. Observe best bid/ask on the HEDGE venue (venue B)
/// 2. Quote on the MAKER venue (venue A) at prices that guarantee
///    profit after hedging: our bid < hedge_bid (can sell hedge profitably),
///    our ask > hedge_ask (can buy hedge profitably).
/// 3. When filled on venue A, immediately hedge on venue B.
///
/// Profit = (our_ask - hedge_ask) or (hedge_bid - our_bid) minus both venues' fees.
///
/// This strategy uses the local book (venue A) and a reference price
/// that represents venue B's mid. The reference price should be set
/// externally from the unified order book.
pub struct CrossExchangeStrategy {
    /// Reference mid price from the hedge venue.
    pub hedge_mid: Price,
    /// Hedge venue maker fee (we'll be taker on hedge venue).
    pub hedge_taker_fee: Decimal,
    /// Our venue maker fee (negative = rebate).
    pub maker_fee: Decimal,
    /// Minimum profit margin in bps after fees.
    pub min_profit_bps: Decimal,
    /// Investigate #15 — stand down entirely when the hedge
    /// book is older than this. Cross-venue feeds jitter under
    /// load and a hedge WS drop is a loud failure mode: better
    /// to skip a refresh than keep quoting primary against a
    /// stale reference — that path silently builds unhedged
    /// exposure. `None` = legacy behaviour (no gate); tests and
    /// paper runs often set it to `None` explicitly. Config
    /// default: 5000ms.
    pub max_hedge_staleness_ms: Option<i64>,
}

impl CrossExchangeStrategy {
    pub fn new(min_profit_bps: Decimal) -> Self {
        Self {
            hedge_mid: dec!(0),
            hedge_taker_fee: dec!(0.001), // Default 0.1% taker on hedge.
            maker_fee: dec!(-0.0005),     // Default -0.05% rebate on our venue.
            min_profit_bps,
            max_hedge_staleness_ms: Some(5_000),
        }
    }

    /// Update the hedge venue reference price.
    pub fn set_hedge_mid(&mut self, mid: Price) {
        self.hedge_mid = mid;
    }

    /// Set hedge venue fees.
    pub fn set_fees(&mut self, maker_fee: Decimal, hedge_taker_fee: Decimal) {
        self.maker_fee = maker_fee;
        self.hedge_taker_fee = hedge_taker_fee;
    }

    /// Calculate the effective price after fees for hedging.
    /// If we BUY on hedge venue: effective = hedge_ask * (1 + taker_fee).
    /// If we SELL on hedge venue: effective = hedge_bid * (1 - taker_fee).
    pub fn effective_hedge_buy(&self, price: Price) -> Price {
        price * (dec!(1) + self.hedge_taker_fee)
    }

    pub fn effective_hedge_sell(&self, price: Price) -> Price {
        price * (dec!(1) - self.hedge_taker_fee)
    }
}

impl Strategy for CrossExchangeStrategy {
    fn name(&self) -> &str {
        "cross-exchange"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        // Investigate #15 — hedge-book staleness gate. The
        // engine threads `hedge_book_age_ms` from the last
        // hedge-WS update; if the hedge venue dropped, the
        // cached mid is meaningless and continuing to quote
        // primary against it is how you build unhedged
        // exposure in a silent failure. Same pattern as
        // basis.rs. We only apply the gate when the engine
        // is actually driving ref_price from a live hedge
        // book — unit tests construct a strategy with a
        // static `hedge_mid` and `hedge_book_age_ms = None`;
        // that path keeps working because the gate only fires
        // when `ctx.ref_price` is Some (i.e. engine wired).
        if let (Some(gate), Some(_)) = (self.max_hedge_staleness_ms, ctx.ref_price) {
            let stale = match ctx.hedge_book_age_ms {
                Some(age) => age > gate,
                // Engine provided `ref_price` but not `age` —
                // inconsistent plumbing, safer to refuse.
                None => true,
            };
            if stale {
                debug!(
                    strategy = "cross-exchange",
                    age_ms = ?ctx.hedge_book_age_ms,
                    gate_ms = gate,
                    "hedge book stale — quoting disabled"
                );
                return vec![];
            }
        }
        // Prefer the engine-threaded hedge book mid (`ctx.ref_price`,
        // populated from `hedge_book.book.mid_price()` in
        // market_maker::refresh_quotes) so the strategy sees live
        // cross-venue prices without the engine having to mutate
        // our private `hedge_mid` field. Fall back to the manual
        // setter value for unit tests that construct a strategy
        // directly and call `set_hedge_mid`.
        let hedge_mid = match ctx.ref_price {
            Some(p) if !p.is_zero() => p,
            _ if !self.hedge_mid.is_zero() => self.hedge_mid,
            _ => return vec![],
        };
        let min_profit = bps_to_frac(self.min_profit_bps) * hedge_mid;
        // Fee accounting with rebate support (Epic 40.2). `maker_fee`
        // is signed: positive = we pay, negative = venue pays us.
        // A rebate must REDUCE total_fees, not increase it — the old
        // `.abs()` treated every rebate as a cost, which eroded the
        // profit floor by 2× the rebate magnitude. On Binance spot
        // MM-program (−0.5 bps rebate) that was a 1 bps/side error,
        // enough to flip a profitable quote into a loss on thin
        // spread targets.
        let total_fees = self.hedge_taker_fee + self.maker_fee;
        let fee_cost = total_fees * hedge_mid;

        // Our ask must be high enough that: our_ask - hedge_ask - fees > min_profit.
        // So: our_ask > hedge_mid + fee_cost + min_profit.
        let min_ask = hedge_mid + fee_cost + min_profit;

        // Our bid must be low enough that: hedge_bid - our_bid - fees > min_profit.
        // So: our_bid < hedge_mid - fee_cost - min_profit.
        let max_bid = hedge_mid - fee_cost - min_profit;

        // Epic D stage-3 — Cartea adverse-selection widening
        // applied as an additive shift on each side. The
        // cross-exchange ladder already starts from the
        // profit-floor edges (`max_bid` for buys,
        // `min_ask` for sells); AS widens the bid down and
        // the ask up by `(1 − 2·ρ_side) · σ · √(T − t)`,
        // safety-clamped at zero so informed flow can never
        // *narrow* the cross-exchange profit floor (that
        // would invite an adverse fill below the fee
        // threshold).
        let sigma = ctx.volatility;
        let sqrt_t = crate::volatility::decimal_sqrt(ctx.time_remaining);
        let (bid_as_widen, ask_as_widen) = match (ctx.as_prob_bid, ctx.as_prob_ask) {
            (Some(rho_b), Some(rho_a)) => {
                let bid_w = (dec!(1) - dec!(2) * rho_b) * sigma * sqrt_t;
                let ask_w = (dec!(1) - dec!(2) * rho_a) * sigma * sqrt_t;
                (bid_w.max(dec!(0)), ask_w.max(dec!(0)))
            }
            _ => {
                let widen = match ctx.as_prob {
                    None => dec!(0),
                    Some(rho) if rho == dec!(0.5) => dec!(0),
                    Some(rho) => {
                        let d = (dec!(1) - dec!(2) * rho) * sigma * sqrt_t;
                        d.max(dec!(0))
                    }
                };
                (widen, widen)
            }
        };
        let max_bid = max_bid - bid_as_widen;
        let min_ask = min_ask + ask_as_widen;

        let order_size = ctx.product.round_qty(ctx.config.order_size);

        let mut quotes = Vec::with_capacity(ctx.config.num_levels);
        let level_step = bps_to_frac(dec!(2)) * hedge_mid; // 2 bps per level.

        for i in 0..ctx.config.num_levels {
            let offset = Decimal::from(i as u64) * level_step;

            let bid_price = ctx.product.round_price(max_bid - offset);
            let ask_price = ctx.product.round_price(min_ask + offset);

            let bid =
                if bid_price > dec!(0) && ctx.product.meets_min_notional(bid_price, order_size) {
                    Some(Quote {
                        side: Side::Buy,
                        price: bid_price,
                        qty: order_size,
                    })
                } else {
                    None
                };

            let ask =
                if ask_price > dec!(0) && ctx.product.meets_min_notional(ask_price, order_size) {
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
            strategy = "cross-exchange",
            %hedge_mid,
            %min_ask,
            %max_bid,
            %fee_cost,
            levels = quotes.len(),
            "computed cross-exchange quotes"
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

    #[test]
    fn test_cross_exchange_profitable_quotes() {
        let product = ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.001),
            min_notional: dec!(10),
            maker_fee: dec!(-0.0005),
            taker_fee: dec!(0.001),
            trading_status: Default::default(),
        };
        let config = MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 2,
            order_size: dec!(0.001),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(5),
            max_distance_bps: dec!(100),
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
            var_guard_cvar_limit_95: None,
            var_guard_cvar_limit_99: None,
            cross_venue_basis_max_staleness_ms: 1500,
            strategy_capital_budget: std::collections::HashMap::new(),
            symbol_circulating_supply: std::collections::HashMap::new(),
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false,
            sor_dispatch_interval_secs: 5,
            sor_urgency: rust_decimal_macros::dec!(0.4),
            sor_target_qty_source: mm_common::config::SorTargetSource::InventoryExcess,
            sor_inventory_threshold: rust_decimal::Decimal::ZERO,
            sor_trade_rate_window_secs: 60,
            sor_queue_refresh_secs: 2,
            sor_extra_l1_poll_secs: 5,
        };
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(50000),
                qty: dec!(1),
            }],
            vec![PriceLevel {
                price: dec!(50001),
                qty: dec!(1),
            }],
            1,
        );

        let mut strategy = CrossExchangeStrategy::new(dec!(3)); // 3 bps min profit.
        strategy.set_hedge_mid(dec!(50000));
        strategy.set_fees(dec!(-0.0005), dec!(0.001));

        let ctx = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        };

        let quotes = strategy.compute_quotes(&ctx);
        assert!(!quotes.is_empty());

        // Ask must be above hedge mid (profitable to sell on hedge after buying here).
        let ask = quotes[0].ask.as_ref().unwrap();
        assert!(ask.price > dec!(50000), "ask should be above hedge mid");

        // Bid must be below hedge mid.
        let bid = quotes[0].bid.as_ref().unwrap();
        assert!(bid.price < dec!(50000), "bid should be below hedge mid");

        // Spread should cover fees + min profit.
        let spread = ask.price - bid.price;
        assert!(spread > dec!(50), "spread should cover fees");
    }

    /// Investigate #15 — when the engine threads a live hedge
    /// `ref_price` AND the hedge-book age exceeds the gate, the
    /// strategy must stand down entirely. Without this the
    /// strategy would quote primary against a stale reference
    /// and silently accumulate unhedged fills.
    #[test]
    fn xexch_stale_hedge_book_disables_quoting() {
        let (product, config, book, mut strategy) = xexch_setup();
        strategy.max_hedge_staleness_ms = Some(2000);
        let ctx = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
            // Engine IS driving ref_price — this is the live
            // path, and age > gate means hedge WS dropped.
            ref_price: Some(dec!(50000)),
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: Some(10_000),
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        };
        let quotes = strategy.compute_quotes(&ctx);
        assert!(
            quotes.is_empty(),
            "stale hedge book must disable cross-exchange quoting"
        );
    }

    /// Companion — same shape but fresh hedge book → quotes fire.
    #[test]
    fn xexch_fresh_hedge_book_allows_quoting() {
        let (product, config, book, mut strategy) = xexch_setup();
        strategy.max_hedge_staleness_ms = Some(2000);
        let ctx = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
            ref_price: Some(dec!(50000)),
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: Some(500),
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        };
        assert!(
            !strategy.compute_quotes(&ctx).is_empty(),
            "fresh hedge book must NOT disable quoting"
        );
    }

    // ---- Epic D stage-3 — Cartea AS + per-side ρ on CrossExchange ----

    fn xexch_setup() -> (
        ProductSpec,
        MarketMakerConfig,
        LocalOrderBook,
        CrossExchangeStrategy,
    ) {
        let product = ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.001),
            min_notional: dec!(10),
            maker_fee: dec!(-0.0005),
            taker_fee: dec!(0.001),
            trading_status: Default::default(),
        };
        let config = MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 1,
            order_size: dec!(0.001),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(5),
            max_distance_bps: dec!(100),
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
            var_guard_cvar_limit_95: None,
            var_guard_cvar_limit_99: None,
            cross_venue_basis_max_staleness_ms: 1500,
            strategy_capital_budget: std::collections::HashMap::new(),
            symbol_circulating_supply: std::collections::HashMap::new(),
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false,
            sor_dispatch_interval_secs: 5,
            sor_urgency: rust_decimal_macros::dec!(0.4),
            sor_target_qty_source: mm_common::config::SorTargetSource::InventoryExcess,
            sor_inventory_threshold: rust_decimal::Decimal::ZERO,
            sor_trade_rate_window_secs: 60,
            sor_queue_refresh_secs: 2,
            sor_extra_l1_poll_secs: 5,
        };
        let mut book = LocalOrderBook::new("BTCUSDT".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(50000),
                qty: dec!(1),
            }],
            vec![PriceLevel {
                price: dec!(50001),
                qty: dec!(1),
            }],
            1,
        );
        let mut strategy = CrossExchangeStrategy::new(dec!(3));
        strategy.set_hedge_mid(dec!(50_000));
        strategy.set_fees(dec!(-0.0005), dec!(0.001));
        (product, config, book, strategy)
    }

    fn xexch_ctx<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        config: &'a MarketMakerConfig,
        as_prob: Option<Decimal>,
        as_prob_bid: Option<Decimal>,
        as_prob_ask: Option<Decimal>,
    ) -> StrategyContext<'a> {
        StrategyContext {
            book,
            product,
            config,
            inventory: dec!(0),
            // Large sigma so AS perturbation visibly shifts
            // the cross-exchange profit floor edges.
            volatility: dec!(50),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob,
            as_prob_bid,
            as_prob_ask,
        }
    }

    #[test]
    fn xexch_as_prob_none_is_byte_identical_to_wave1() {
        let (product, config, book, strategy) = xexch_setup();
        let baseline = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
            volatility: dec!(50),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        };
        let q_base = strategy.compute_quotes(&baseline);
        // None path should produce identical output to a
        // hand-constructed identity context.
        let q_none =
            strategy.compute_quotes(&xexch_ctx(&book, &product, &config, None, None, None));
        assert_eq!(
            q_base[0].bid.as_ref().unwrap().price,
            q_none[0].bid.as_ref().unwrap().price
        );
        assert_eq!(
            q_base[0].ask.as_ref().unwrap().price,
            q_none[0].ask.as_ref().unwrap().price
        );
    }

    #[test]
    fn xexch_symmetric_low_rho_widens_profit_floor() {
        let (product, config, book, strategy) = xexch_setup();
        let neutral = xexch_ctx(&book, &product, &config, Some(dec!(0.5)), None, None);
        let widen = xexch_ctx(&book, &product, &config, Some(dec!(0)), None, None);
        let q_n = strategy.compute_quotes(&neutral);
        let q_w = strategy.compute_quotes(&widen);
        let bid_n = q_n[0].bid.as_ref().unwrap().price;
        let bid_w = q_w[0].bid.as_ref().unwrap().price;
        let ask_n = q_n[0].ask.as_ref().unwrap().price;
        let ask_w = q_w[0].ask.as_ref().unwrap().price;
        assert!(bid_w < bid_n, "low ρ should drop the bid");
        assert!(ask_w > ask_n, "low ρ should lift the ask");
    }

    #[test]
    fn xexch_high_rho_does_not_narrow_profit_floor() {
        // Per the safety clamp: informed flow (ρ > 0.5) on
        // cross-exchange should NEVER tighten the profit
        // floor. The widen-vs-neutral distance must be
        // exactly zero (clamped) when ρ = 1.
        let (product, config, book, strategy) = xexch_setup();
        let neutral = xexch_ctx(&book, &product, &config, Some(dec!(0.5)), None, None);
        let narrow = xexch_ctx(&book, &product, &config, Some(dec!(1)), None, None);
        let q_n = strategy.compute_quotes(&neutral);
        let q_t = strategy.compute_quotes(&narrow);
        assert_eq!(
            q_n[0].bid.as_ref().unwrap().price,
            q_t[0].bid.as_ref().unwrap().price
        );
        assert_eq!(
            q_n[0].ask.as_ref().unwrap().price,
            q_t[0].ask.as_ref().unwrap().price
        );
    }

    #[test]
    fn xexch_per_side_widens_one_side_independently() {
        let (product, config, book, strategy) = xexch_setup();
        let neutral = xexch_ctx(
            &book,
            &product,
            &config,
            None,
            Some(dec!(0.5)),
            Some(dec!(0.5)),
        );
        let widen_ask = xexch_ctx(
            &book,
            &product,
            &config,
            None,
            Some(dec!(0.5)),
            Some(dec!(0)),
        );
        let q_n = strategy.compute_quotes(&neutral);
        let q_w = strategy.compute_quotes(&widen_ask);
        let bid_n = q_n[0].bid.as_ref().unwrap().price;
        let bid_w = q_w[0].bid.as_ref().unwrap().price;
        let ask_n = q_n[0].ask.as_ref().unwrap().price;
        let ask_w = q_w[0].ask.as_ref().unwrap().price;
        // Ask widens (lifts); bid unchanged.
        assert!(ask_w > ask_n);
        assert_eq!(bid_w, bid_n);
    }
}
