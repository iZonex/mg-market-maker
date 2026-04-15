use mm_common::config::MarketMakerConfig;
use mm_common::orderbook::LocalOrderBook;
use mm_common::types::*;
use mm_risk::inventory::InventoryManager;
use mm_risk::pnl::PnlTracker;
use mm_strategy::r#trait::{Strategy, StrategyContext};
use mm_strategy::volatility::VolatilityEstimator;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::data::RecordedEvent;
use crate::queue_model::{LogProbQueueFunc, PowerProbQueueFunc, Probability, QueuePos};
use crate::report::BacktestReport;

/// Which probability function drives the queue-aware fill model.
///
/// Two canonical choices from the upstream hftbacktest library.
/// See [`crate::queue_model`] for the full trait + math.
#[derive(Debug, Clone, Copy)]
pub enum QueueProbModel {
    /// `f(x) = ln(1 + x)` — good default. Log weighting pulls
    /// aggressive cancels toward the back of a deep queue.
    Log,
    /// `f(x) = x^n`. Larger `n` saturates the probability
    /// harder toward the side with more qty.
    Power { n: f64 },
}

impl QueueProbModel {
    fn prob(&self, front: Decimal, back: Decimal) -> f64 {
        match self {
            QueueProbModel::Log => LogProbQueueFunc::new().prob(front, back),
            QueueProbModel::Power { n } => PowerProbQueueFunc::new(*n).prob(front, back),
        }
    }
}

/// Fill simulation mode.
#[derive(Debug, Clone, Copy)]
pub enum FillModel {
    /// Fill if price crosses our quote (optimistic, overestimates).
    PriceCross,
    /// Legacy probability-of-fill coin flip. Overestimates MM PnL
    /// because it ignores queue FIFO semantics. Kept for
    /// backward-compat; prefer `QueueAware` for realistic backtests.
    QueuePosition { fill_probability: f64 },
    /// Queue-position-aware fill model. Tracks per-order queue
    /// state (`front_q_qty`, `cum_trade_qty`) from
    /// [`crate::queue_model::QueuePos`] and advances it on every
    /// trade / depth change at the order's price level. Fills fire
    /// only when the queue ahead actually clears. Closes the
    /// 10-30 % PnL over-estimate that `QueuePosition` /
    /// `PriceCross` leave in place.
    ///
    /// `entry_latency_ns` / `response_latency_ns` model the asymmetric
    /// local→exchange and exchange→local latencies using the constant
    /// model from [`crate::latency_model::ConstantLatency`]. Positive
    /// values shift fill timestamps forward; the current simulator
    /// records them on the `Fill` event for post-hoc analysis but
    /// does not currently delay fills (backtest clock is event-
    /// driven, not wall-clock).
    QueueAware {
        prob_model: QueueProbModel,
        entry_latency_ns: i64,
        response_latency_ns: i64,
    },
}

impl FillModel {
    /// Convenience constructor for the default `Log`
    /// probability with zero latency.
    pub fn queue_aware_log() -> Self {
        Self::QueueAware {
            prob_model: QueueProbModel::Log,
            entry_latency_ns: 0,
            response_latency_ns: 0,
        }
    }
}

/// Per-order queue tracking for the `QueueAware` fill model.
/// The simulator keeps one of these per live maker quote and
/// updates it on every event at the quote's price level.
#[derive(Debug, Clone)]
struct TrackedQuote {
    quote: Quote,
    queue: QueuePos,
    /// Book qty at `quote.price` as of the last snapshot. Used
    /// by `on_depth_change` to detect how much qty was cancelled
    /// vs traded between snapshots.
    last_book_qty: Decimal,
}

/// Backtesting simulator — replays recorded events through a strategy.
pub struct Simulator {
    config: MarketMakerConfig,
    product: ProductSpec,
    fill_model: FillModel,
}

impl Simulator {
    pub fn new(config: MarketMakerConfig, product: ProductSpec, fill_model: FillModel) -> Self {
        Self {
            config,
            product,
            fill_model,
        }
    }

    /// Run a backtest over recorded events.
    pub fn run(&self, strategy: &dyn Strategy, events: &[RecordedEvent]) -> BacktestReport {
        let mut book = LocalOrderBook::new(self.product.symbol.clone());
        let mut inventory_mgr = InventoryManager::new();
        let mut pnl_tracker = PnlTracker::new(self.product.maker_fee, self.product.taker_fee);
        let mut vol_est = VolatilityEstimator::new(dec!(0.94), dec!(0.5));

        let mut total_fills = 0u64;
        let mut total_quotes = 0u64;
        let mut ticks = 0u64;

        // Active quotes for the legacy fill models (PriceCross
        // / QueuePosition). Ignored when `QueueAware` is
        // selected — that path uses `tracked_bids` /
        // `tracked_asks` below.
        let mut active_bids: Vec<Quote> = Vec::new();
        let mut active_asks: Vec<Quote> = Vec::new();

        // Queue-aware state. Per-order queue trackers live here
        // so their state survives across snapshots.
        let mut tracked_bids: Vec<TrackedQuote> = Vec::new();
        let mut tracked_asks: Vec<TrackedQuote> = Vec::new();

        let queue_aware = matches!(self.fill_model, FillModel::QueueAware { .. });

        for event in events {
            match event {
                RecordedEvent::BookSnapshot {
                    bids,
                    asks,
                    sequence,
                    ..
                } => {
                    // Step 1: advance queue state for tracked
                    // orders using the DEPTH DELTA between the
                    // previous snapshot and this one. Trades
                    // that happened in between have already
                    // advanced `front_q_qty` via `on_trade`; the
                    // `on_depth_change` call subtracts
                    // `cum_trade_qty` internally to avoid
                    // double-counting.
                    if queue_aware {
                        let prob = match self.fill_model {
                            FillModel::QueueAware { prob_model, .. } => prob_model,
                            _ => unreachable!(),
                        };
                        // Build a lookup of new bid/ask qty at
                        // price for O(1) access.
                        let new_bid_qty = |p: &Decimal| -> Decimal {
                            bids.iter()
                                .find(|l| &l.price == p)
                                .map(|l| l.qty)
                                .unwrap_or(Decimal::ZERO)
                        };
                        let new_ask_qty = |p: &Decimal| -> Decimal {
                            asks.iter()
                                .find(|l| &l.price == p)
                                .map(|l| l.qty)
                                .unwrap_or(Decimal::ZERO)
                        };
                        for tq in tracked_bids.iter_mut() {
                            let new_qty = new_bid_qty(&tq.quote.price);
                            tq.queue.on_depth_change(
                                tq.last_book_qty,
                                new_qty,
                                &ProbModelDispatch(prob),
                            );
                            tq.last_book_qty = new_qty;
                        }
                        for tq in tracked_asks.iter_mut() {
                            let new_qty = new_ask_qty(&tq.quote.price);
                            tq.queue.on_depth_change(
                                tq.last_book_qty,
                                new_qty,
                                &ProbModelDispatch(prob),
                            );
                            tq.last_book_qty = new_qty;
                        }
                    }

                    book.apply_snapshot(bids.clone(), asks.clone(), *sequence);

                    if let Some(mid) = book.mid_price() {
                        vol_est.update(mid);
                    }

                    // Step 2: re-run the strategy to get the
                    // desired quote set for this tick.
                    if let Some(mid) = book.mid_price() {
                        let sigma = vol_est.volatility().unwrap_or(self.config.sigma);
                        let ctx = StrategyContext {
                            book: &book,
                            product: &self.product,
                            config: &self.config,
                            inventory: inventory_mgr.inventory(),
                            volatility: sigma,
                            time_remaining: dec!(1),
                            mid_price: mid,
                            ref_price: None,
                            hedge_book: None,
                            borrow_cost_bps: None,
                            hedge_book_age_ms: None,
                        };

                        let quotes = strategy.compute_quotes(&ctx);

                        if queue_aware {
                            // Merge the new desired quotes into
                            // the tracked state. Quotes at the
                            // same price preserve their
                            // `QueuePos` so the queue estimate
                            // survives across snapshots. Quotes
                            // at new prices get a fresh
                            // `QueuePos` initialised from the
                            // current book qty at the price.
                            tracked_bids = merge_tracked(
                                &tracked_bids,
                                quotes.iter().filter_map(|q| q.bid.clone()),
                                Side::Buy,
                                &book,
                            );
                            tracked_asks = merge_tracked(
                                &tracked_asks,
                                quotes.iter().filter_map(|q| q.ask.clone()),
                                Side::Sell,
                                &book,
                            );
                        } else {
                            active_bids.clear();
                            active_asks.clear();
                            for q in &quotes {
                                if let Some(bid) = &q.bid {
                                    active_bids.push(bid.clone());
                                }
                                if let Some(ask) = &q.ask {
                                    active_asks.push(ask.clone());
                                }
                            }
                        }
                        total_quotes += 1;
                    }
                    ticks += 1;
                }
                RecordedEvent::Trade {
                    price,
                    qty,
                    taker_side,
                    timestamp,
                    ..
                } => {
                    let mid = book.mid_price().unwrap_or(*price);

                    if queue_aware {
                        let (entry_latency_ns, response_latency_ns) = match self.fill_model {
                            FillModel::QueueAware {
                                entry_latency_ns,
                                response_latency_ns,
                                ..
                            } => (entry_latency_ns, response_latency_ns),
                            _ => unreachable!(),
                        };
                        match taker_side {
                            // Buyer lifting asks → route trade
                            // through our tracked asks at or
                            // below the trade price.
                            Side::Buy => {
                                queue_aware_fill(
                                    &mut tracked_asks,
                                    Side::Sell,
                                    *price,
                                    *qty,
                                    *timestamp,
                                    mid,
                                    entry_latency_ns,
                                    response_latency_ns,
                                    &self.product,
                                    &mut inventory_mgr,
                                    &mut pnl_tracker,
                                    &mut total_fills,
                                    /*is_taker_buy=*/ true,
                                );
                            }
                            Side::Sell => {
                                queue_aware_fill(
                                    &mut tracked_bids,
                                    Side::Buy,
                                    *price,
                                    *qty,
                                    *timestamp,
                                    mid,
                                    entry_latency_ns,
                                    response_latency_ns,
                                    &self.product,
                                    &mut inventory_mgr,
                                    &mut pnl_tracker,
                                    &mut total_fills,
                                    /*is_taker_buy=*/ false,
                                );
                            }
                        }
                    } else {
                        // Legacy fill path.
                        match taker_side {
                            Side::Buy => {
                                let filled: Vec<_> = active_asks
                                    .iter()
                                    .filter(|a| *price >= a.price && self.should_fill())
                                    .cloned()
                                    .collect();
                                for ask in &filled {
                                    let fill = Fill {
                                        trade_id: total_fills,
                                        order_id: uuid::Uuid::new_v4(),
                                        symbol: self.product.symbol.clone(),
                                        side: Side::Sell,
                                        price: ask.price,
                                        qty: ask.qty.min(*qty),
                                        is_maker: true,
                                        timestamp: *timestamp,
                                    };
                                    inventory_mgr.on_fill(&fill);
                                    pnl_tracker.on_fill(&fill, mid);
                                    total_fills += 1;
                                }
                                active_asks.retain(|a| *price < a.price);
                            }
                            Side::Sell => {
                                let filled: Vec<_> = active_bids
                                    .iter()
                                    .filter(|b| *price <= b.price && self.should_fill())
                                    .cloned()
                                    .collect();
                                for bid in &filled {
                                    let fill = Fill {
                                        trade_id: total_fills,
                                        order_id: uuid::Uuid::new_v4(),
                                        symbol: self.product.symbol.clone(),
                                        side: Side::Buy,
                                        price: bid.price,
                                        qty: bid.qty.min(*qty),
                                        is_maker: true,
                                        timestamp: *timestamp,
                                    };
                                    inventory_mgr.on_fill(&fill);
                                    pnl_tracker.on_fill(&fill, mid);
                                    total_fills += 1;
                                }
                                active_bids.retain(|b| *price > b.price);
                            }
                        }
                    }

                    pnl_tracker.mark_to_market(*price);
                }
            }
        }

        let final_mid = book.mid_price().unwrap_or(dec!(0));

        BacktestReport {
            strategy_name: strategy.name().to_string(),
            total_events: events.len() as u64,
            total_ticks: ticks,
            total_quotes,
            total_fills,
            final_inventory: inventory_mgr.inventory(),
            realized_pnl: inventory_mgr.realized_pnl(),
            unrealized_pnl: inventory_mgr.unrealized_pnl(final_mid),
            total_pnl: inventory_mgr.total_pnl(final_mid),
            pnl_attribution: pnl_tracker.attribution.clone(),
        }
    }

    fn should_fill(&self) -> bool {
        match self.fill_model {
            FillModel::PriceCross => true,
            FillModel::QueuePosition { fill_probability } => rand_simple(fill_probability),
            FillModel::QueueAware { .. } => true, // unused — queue-aware path branches earlier
        }
    }
}

/// Adapter so the enum-based `QueueProbModel` can be passed to
/// `QueuePos::on_depth_change`, which takes a `&P` where `P:
/// Probability`. Avoids boxing the probability model per call.
struct ProbModelDispatch(QueueProbModel);

impl Probability for ProbModelDispatch {
    fn prob(&self, front: Decimal, back: Decimal) -> f64 {
        self.0.prob(front, back)
    }
}

/// Reconcile a new set of desired `Quote`s with the existing
/// `TrackedQuote`s. Preserves `QueuePos` state for quotes at the
/// same price and initialises a fresh `QueuePos` from the current
/// book for quotes at brand-new prices.
fn merge_tracked<I: IntoIterator<Item = Quote>>(
    existing: &[TrackedQuote],
    desired: I,
    side: Side,
    book: &LocalOrderBook,
) -> Vec<TrackedQuote> {
    let mut out = Vec::new();
    for quote in desired {
        let book_qty = match side {
            Side::Buy => book
                .bids
                .get(&quote.price)
                .copied()
                .unwrap_or(Decimal::ZERO),
            Side::Sell => book
                .asks
                .get(&quote.price)
                .copied()
                .unwrap_or(Decimal::ZERO),
        };
        // Look for an existing tracker at the same price — the
        // quoted qty may have shifted but the queue state
        // carries over.
        if let Some(prev) = existing.iter().find(|tq| tq.quote.price == quote.price) {
            let mut q = prev.clone();
            q.quote = quote;
            q.last_book_qty = book_qty;
            out.push(q);
        } else {
            out.push(TrackedQuote {
                quote,
                queue: QueuePos::new(book_qty),
                last_book_qty: book_qty,
            });
        }
    }
    out
}

/// Queue-aware fill dispatch for one trade event.
///
/// Routes the trade through every tracked quote at or better
/// than the trade price on the matched side, advancing each
/// queue position and emitting a `Fill` for any overshoot. The
/// caller-supplied `maker_side` is the side of OUR quote (the
/// maker), which is always the opposite of the taker.
#[allow(clippy::too_many_arguments)]
fn queue_aware_fill(
    tracked: &mut Vec<TrackedQuote>,
    maker_side: Side,
    trade_price: Decimal,
    trade_qty: Decimal,
    timestamp: chrono::DateTime<chrono::Utc>,
    mid: Decimal,
    entry_latency_ns: i64,
    response_latency_ns: i64,
    product: &ProductSpec,
    inventory_mgr: &mut InventoryManager,
    pnl_tracker: &mut PnlTracker,
    total_fills: &mut u64,
    is_taker_buy: bool,
) {
    let mut remaining_trade = trade_qty;
    // Quotes that actually sit at the touch (price equal to the
    // trade) are the ones whose queue advances on this trade.
    // Orders resting deeper in the book are unaffected.
    for tq in tracked.iter_mut() {
        if remaining_trade <= Decimal::ZERO {
            break;
        }
        // The trade affects a maker quote only if the taker
        // price reaches the quote. For a taker buy at price P,
        // our asks with ask_price ≤ P are swept (best ask
        // first). For a taker sell at price P, our bids with
        // bid_price ≥ P are hit.
        let touched = if is_taker_buy {
            tq.quote.price <= trade_price
        } else {
            tq.quote.price >= trade_price
        };
        if !touched {
            continue;
        }
        // Advance the queue by the trade size (capped by the
        // remaining trade that the taker can still consume).
        let consume = remaining_trade.min(tq.queue.front_q_qty.max(Decimal::ZERO) + tq.quote.qty);
        tq.queue.on_trade(consume);
        remaining_trade -= consume;

        let filled_qty = tq.queue.consume_fill();
        if filled_qty > Decimal::ZERO {
            let eff_qty = filled_qty.min(tq.quote.qty);
            let fill_ts =
                timestamp + chrono::Duration::nanoseconds(entry_latency_ns + response_latency_ns);
            let fill = Fill {
                trade_id: *total_fills,
                order_id: uuid::Uuid::new_v4(),
                symbol: product.symbol.clone(),
                side: maker_side,
                price: tq.quote.price,
                qty: eff_qty,
                is_maker: true,
                timestamp: fill_ts,
            };
            inventory_mgr.on_fill(&fill);
            pnl_tracker.on_fill(&fill, mid);
            *total_fills += 1;
            // Decrement the quoted qty; once the quote is fully
            // consumed it should rotate out on the next snapshot
            // refresh.
            tq.quote.qty -= eff_qty;
            if tq.quote.qty < Decimal::ZERO {
                tq.quote.qty = Decimal::ZERO;
            }
        }
    }
    // Drop any fully-consumed quotes so they don't linger.
    tracked.retain(|tq| tq.quote.qty > Decimal::ZERO);
}

/// Very simple deterministic-ish random for backtesting.
/// In production, use proper rng.
fn rand_simple(prob: f64) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Simple hash-based approach.
    let hash = c
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let normalized = (hash as f64) / (u64::MAX as f64);
    normalized < prob
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mm_common::config::StrategyType;
    use mm_strategy::AvellanedaStoikov;

    #[test]
    fn test_simple_backtest() {
        let config = MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 1,
            order_size: dec!(0.01),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(10),
            max_distance_bps: dec!(100),
            strategy: StrategyType::AvellanedaStoikov,
            momentum_enabled: false,
            momentum_window: 200,
            basis_shift: dec!(0.5),
            market_resilience_enabled: true,
            otr_enabled: true,
            hma_enabled: true,
            hma_window: 9,
            user_stream_enabled: true,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: true,
            amend_max_ticks: 2,
            fee_tier_refresh_enabled: true,
            fee_tier_refresh_secs: 600,
            borrow_enabled: false,
            borrow_rate_refresh_secs: 1800,
            borrow_holding_secs: 3600,
            borrow_max_base: dec!(0),
            borrow_buffer_base: dec!(0),
            pair_lifecycle_enabled: true,
            pair_lifecycle_refresh_secs: 300,
            cross_venue_basis_max_staleness_ms: 1500,
        };
        let product = ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.001),
            min_notional: dec!(1),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
            trading_status: Default::default(),
        };

        let events = vec![
            RecordedEvent::BookSnapshot {
                timestamp: Utc::now(),
                bids: vec![PriceLevel {
                    price: dec!(50000),
                    qty: dec!(10),
                }],
                asks: vec![PriceLevel {
                    price: dec!(50010),
                    qty: dec!(10),
                }],
                sequence: 1,
            },
            RecordedEvent::Trade {
                timestamp: Utc::now(),
                price: dec!(50008),
                qty: dec!(0.5),
                taker_side: Side::Buy,
            },
            RecordedEvent::Trade {
                timestamp: Utc::now(),
                price: dec!(50002),
                qty: dec!(0.5),
                taker_side: Side::Sell,
            },
        ];

        let sim = Simulator::new(config, product, FillModel::PriceCross);
        let strategy = AvellanedaStoikov;
        let report = sim.run(&strategy, &events);

        assert!(report.total_events == 3);
        assert!(report.total_ticks == 1);
        report.print();
    }

    // ---- QueueAware fill model ----

    fn queue_test_config() -> MarketMakerConfig {
        MarketMakerConfig {
            gamma: dec!(0.1),
            kappa: dec!(1.5),
            sigma: dec!(0.02),
            time_horizon_secs: 300,
            num_levels: 1,
            order_size: dec!(1),
            refresh_interval_ms: 500,
            min_spread_bps: dec!(10),
            max_distance_bps: dec!(100),
            strategy: StrategyType::AvellanedaStoikov,
            momentum_enabled: false,
            momentum_window: 200,
            basis_shift: dec!(0.5),
            market_resilience_enabled: true,
            otr_enabled: true,
            hma_enabled: true,
            hma_window: 9,
            user_stream_enabled: true,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: true,
            amend_max_ticks: 2,
            fee_tier_refresh_enabled: true,
            fee_tier_refresh_secs: 600,
            borrow_enabled: false,
            borrow_rate_refresh_secs: 1800,
            borrow_holding_secs: 3600,
            borrow_max_base: dec!(0),
            borrow_buffer_base: dec!(0),
            pair_lifecycle_enabled: true,
            pair_lifecycle_refresh_secs: 300,
            cross_venue_basis_max_staleness_ms: 1500,
        }
    }

    fn queue_test_product() -> ProductSpec {
        ProductSpec {
            symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.001),
            min_notional: dec!(1),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
            trading_status: Default::default(),
        }
    }

    /// Grid strategy that posts one bid and one ask at fixed
    /// prices — useful for isolating the fill model from the
    /// strategy's own pricing logic in tests.
    struct FixedQuotesStrategy {
        bid_price: Decimal,
        ask_price: Decimal,
        qty: Decimal,
    }

    impl Strategy for FixedQuotesStrategy {
        fn name(&self) -> &str {
            "fixed"
        }
        fn compute_quotes(&self, _ctx: &StrategyContext) -> Vec<QuotePair> {
            vec![QuotePair {
                bid: Some(Quote {
                    side: Side::Buy,
                    price: self.bid_price,
                    qty: self.qty,
                }),
                ask: Some(Quote {
                    side: Side::Sell,
                    price: self.ask_price,
                    qty: self.qty,
                }),
            }]
        }
    }

    /// When there is a large resting queue ahead of our bid, a
    /// small taker sell should NOT fill us because the queue
    /// has to be exhausted first. The legacy `PriceCross` model
    /// would fill us anyway — this is the classic MM backtest
    /// accuracy bug that `QueueAware` closes.
    #[test]
    fn queue_aware_does_not_fill_when_queue_ahead_is_thick() {
        let events = vec![
            RecordedEvent::BookSnapshot {
                timestamp: Utc::now(),
                // Our bid will be placed at 49_995 (the
                // strategy's fixed price). The book already has
                // 100 BTC of resting qty at that level — we sit
                // at the back of the queue.
                bids: vec![
                    PriceLevel {
                        price: dec!(50000),
                        qty: dec!(10),
                    },
                    PriceLevel {
                        price: dec!(49995),
                        qty: dec!(100),
                    },
                ],
                asks: vec![PriceLevel {
                    price: dec!(50010),
                    qty: dec!(10),
                }],
                sequence: 1,
            },
            // One small sell trade of 0.5 at 49_995 — only
            // advances our front queue by 0.5, nowhere near the
            // 100 BTC ahead of us. No fill expected.
            RecordedEvent::Trade {
                timestamp: Utc::now(),
                price: dec!(49995),
                qty: dec!(0.5),
                taker_side: Side::Sell,
            },
        ];

        let strategy = FixedQuotesStrategy {
            bid_price: dec!(49995),
            ask_price: dec!(50015),
            qty: dec!(1),
        };
        let sim = Simulator::new(
            queue_test_config(),
            queue_test_product(),
            FillModel::queue_aware_log(),
        );
        let report = sim.run(&strategy, &events);
        assert_eq!(
            report.total_fills, 0,
            "queue-aware fill model must not fire on a 0.5 trade with 100 BTC ahead"
        );
    }

    /// When a large sweep trade at our price drains the queue
    /// entirely, our order fills for the overshoot. This pins
    /// the "eventually fills" half of the contract — without it
    /// the model would be too conservative.
    #[test]
    fn queue_aware_fills_when_large_sweep_drains_queue() {
        let events = vec![
            RecordedEvent::BookSnapshot {
                timestamp: Utc::now(),
                bids: vec![PriceLevel {
                    price: dec!(49995),
                    qty: dec!(5),
                }],
                asks: vec![PriceLevel {
                    price: dec!(50010),
                    qty: dec!(10),
                }],
                sequence: 1,
            },
            // Huge sell of 20 BTC at 49_995 — the queue ahead
            // (5 BTC) clears, and our 1 BTC bid fills.
            RecordedEvent::Trade {
                timestamp: Utc::now(),
                price: dec!(49995),
                qty: dec!(20),
                taker_side: Side::Sell,
            },
        ];
        let strategy = FixedQuotesStrategy {
            bid_price: dec!(49995),
            ask_price: dec!(50010),
            qty: dec!(1),
        };
        let sim = Simulator::new(
            queue_test_config(),
            queue_test_product(),
            FillModel::queue_aware_log(),
        );
        let report = sim.run(&strategy, &events);
        assert!(
            report.total_fills >= 1,
            "expected at least one fill after the sweep"
        );
    }

    /// PriceCross vs QueueAware on the same input: PriceCross
    /// fills every touch, QueueAware filters fills through the
    /// queue. A small trade series that touches our quote
    /// repeatedly must produce STRICTLY MORE fills under
    /// PriceCross than under QueueAware. This is the concrete
    /// over-reporting that the queue-aware model corrects.
    #[test]
    fn queue_aware_produces_fewer_fills_than_price_cross_on_thin_trades() {
        // Build a stream of small sells at our bid. Each touches
        // 0.1 BTC; with 50 BTC queue ahead PriceCross reports
        // every touch as a fill, QueueAware reports none.
        let mut events: Vec<RecordedEvent> = Vec::new();
        events.push(RecordedEvent::BookSnapshot {
            timestamp: Utc::now(),
            bids: vec![PriceLevel {
                price: dec!(49995),
                qty: dec!(50),
            }],
            asks: vec![PriceLevel {
                price: dec!(50010),
                qty: dec!(10),
            }],
            sequence: 1,
        });
        for _ in 0..30 {
            events.push(RecordedEvent::Trade {
                timestamp: Utc::now(),
                price: dec!(49995),
                qty: dec!(0.1),
                taker_side: Side::Sell,
            });
        }

        let strategy = FixedQuotesStrategy {
            bid_price: dec!(49995),
            ask_price: dec!(50015),
            qty: dec!(1),
        };

        let sim_cross = Simulator::new(
            queue_test_config(),
            queue_test_product(),
            FillModel::PriceCross,
        );
        let report_cross = sim_cross.run(&strategy, &events);

        let sim_queue = Simulator::new(
            queue_test_config(),
            queue_test_product(),
            FillModel::queue_aware_log(),
        );
        let report_queue = sim_queue.run(&strategy, &events);

        assert!(
            report_cross.total_fills > report_queue.total_fills,
            "PriceCross reported {} fills, QueueAware reported {} — \
             queue model must be stricter",
            report_cross.total_fills,
            report_queue.total_fills
        );
    }

    #[test]
    fn queue_aware_advances_through_cancels_via_depth_change() {
        // Set up a queue of 10 BTC ahead. Nothing trades. Then
        // the next snapshot shows only 2 BTC at the level —
        // 8 BTC were cancelled. The queue model should
        // partition those cancels between the front and back
        // via the LogProb model. After that, a 3 BTC sell must
        // fill at least some of our order.
        let events = vec![
            RecordedEvent::BookSnapshot {
                timestamp: Utc::now(),
                bids: vec![PriceLevel {
                    price: dec!(49995),
                    qty: dec!(10),
                }],
                asks: vec![PriceLevel {
                    price: dec!(50010),
                    qty: dec!(10),
                }],
                sequence: 1,
            },
            RecordedEvent::BookSnapshot {
                timestamp: Utc::now(),
                bids: vec![PriceLevel {
                    price: dec!(49995),
                    qty: dec!(2),
                }],
                asks: vec![PriceLevel {
                    price: dec!(50010),
                    qty: dec!(10),
                }],
                sequence: 2,
            },
            // Large sell that clearly overshoots the tiny
            // front queue remaining after the cancel wave.
            RecordedEvent::Trade {
                timestamp: Utc::now(),
                price: dec!(49995),
                qty: dec!(10),
                taker_side: Side::Sell,
            },
        ];
        let strategy = FixedQuotesStrategy {
            bid_price: dec!(49995),
            ask_price: dec!(50015),
            qty: dec!(1),
        };
        let sim = Simulator::new(
            queue_test_config(),
            queue_test_product(),
            FillModel::queue_aware_log(),
        );
        let report = sim.run(&strategy, &events);
        assert!(
            report.total_fills >= 1,
            "cancels should have advanced the queue; subsequent sweep must fill"
        );
    }
}
