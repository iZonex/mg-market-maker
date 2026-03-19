use mm_common::config::MarketMakerConfig;
use mm_common::orderbook::LocalOrderBook;
use mm_common::types::*;
use mm_risk::inventory::InventoryManager;
use mm_risk::pnl::PnlTracker;
use mm_strategy::r#trait::{Strategy, StrategyContext};
use mm_strategy::volatility::VolatilityEstimator;
use rust_decimal_macros::dec;

use crate::data::RecordedEvent;
use crate::report::BacktestReport;

/// Fill simulation mode.
#[derive(Debug, Clone, Copy)]
pub enum FillModel {
    /// Fill if price crosses our quote (optimistic, overestimates).
    PriceCross,
    /// Fill with a probability based on queue position (more realistic).
    QueuePosition { fill_probability: f64 },
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

        // Active quotes.
        let mut active_bids: Vec<Quote> = Vec::new();
        let mut active_asks: Vec<Quote> = Vec::new();

        for event in events {
            match event {
                RecordedEvent::BookSnapshot {
                    bids,
                    asks,
                    sequence,
                    ..
                } => {
                    book.apply_snapshot(bids.clone(), asks.clone(), *sequence);

                    if let Some(mid) = book.mid_price() {
                        vol_est.update(mid);
                    }

                    // Refresh quotes every snapshot.
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
                        };

                        let quotes = strategy.compute_quotes(&ctx);
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

                    // Check if any of our quotes would be filled.
                    match taker_side {
                        Side::Buy => {
                            // Buyer is lifting asks — check our asks.
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
                            // Seller is hitting bids — check our bids.
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
        }
    }
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
}
