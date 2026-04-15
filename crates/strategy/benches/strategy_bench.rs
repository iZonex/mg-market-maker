use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mm_common::config::{MarketMakerConfig, StrategyType};
use mm_common::orderbook::LocalOrderBook;
use mm_common::types::{PriceLevel, ProductSpec};
use mm_strategy::r#trait::{Strategy, StrategyContext};
use mm_strategy::{AvellanedaStoikov, GlftStrategy, GridStrategy};
use rust_decimal_macros::dec;

fn make_book() -> LocalOrderBook {
    let mut book = LocalOrderBook::new("BTCUSDT".into());
    let mut bids = Vec::new();
    let mut asks = Vec::new();
    for i in 0..25 {
        let offset = rust_decimal::Decimal::from(i) * dec!(0.01);
        bids.push(PriceLevel {
            price: dec!(50000) - offset,
            qty: dec!(1) + rust_decimal::Decimal::from(i) * dec!(0.1),
        });
        asks.push(PriceLevel {
            price: dec!(50001) + offset,
            qty: dec!(1) + rust_decimal::Decimal::from(i) * dec!(0.1),
        });
    }
    book.apply_snapshot(bids, asks, 1);
    book
}

fn make_product() -> ProductSpec {
    ProductSpec {
        symbol: "BTCUSDT".into(),
        base_asset: "BTC".into(),
        quote_asset: "USDT".into(),
        tick_size: dec!(0.01),
        lot_size: dec!(0.00001),
        min_notional: dec!(10),
        maker_fee: dec!(0.001),
        taker_fee: dec!(0.002),
    }
}

fn make_config() -> MarketMakerConfig {
    MarketMakerConfig {
        gamma: dec!(0.1),
        kappa: dec!(1.5),
        sigma: dec!(0.02),
        time_horizon_secs: 300,
        num_levels: 5,
        order_size: dec!(0.001),
        refresh_interval_ms: 500,
        min_spread_bps: dec!(5),
        max_distance_bps: dec!(100),
        strategy: StrategyType::AvellanedaStoikov,
        momentum_enabled: false,
        momentum_window: 200,
        basis_shift: dec!(0.5),
    }
}

fn bench_avellaneda(c: &mut Criterion) {
    let book = make_book();
    let product = make_product();
    let config = make_config();
    let mid = book.mid_price().unwrap();
    let strategy = AvellanedaStoikov;

    c.bench_function("avellaneda_stoikov_5_levels", |b| {
        b.iter(|| {
            let ctx = StrategyContext {
                book: &book,
                product: &product,
                config: &config,
                inventory: dec!(0.01),
                volatility: dec!(0.02),
                time_remaining: dec!(0.8),
                mid_price: mid,
                ref_price: None,
                hedge_book: None,
            };
            black_box(strategy.compute_quotes(&ctx))
        })
    });
}

fn bench_glft(c: &mut Criterion) {
    let book = make_book();
    let product = make_product();
    let config = make_config();
    let mid = book.mid_price().unwrap();
    let strategy = GlftStrategy::new();

    c.bench_function("glft_5_levels", |b| {
        b.iter(|| {
            let ctx = StrategyContext {
                book: &book,
                product: &product,
                config: &config,
                inventory: dec!(0.01),
                volatility: dec!(0.02),
                time_remaining: dec!(0.8),
                mid_price: mid,
                ref_price: None,
                hedge_book: None,
            };
            black_box(strategy.compute_quotes(&ctx))
        })
    });
}

fn bench_grid(c: &mut Criterion) {
    let book = make_book();
    let product = make_product();
    let config = make_config();
    let mid = book.mid_price().unwrap();
    let strategy = GridStrategy;

    c.bench_function("grid_5_levels", |b| {
        b.iter(|| {
            let ctx = StrategyContext {
                book: &book,
                product: &product,
                config: &config,
                inventory: dec!(0.01),
                volatility: dec!(0.02),
                time_remaining: dec!(0.8),
                mid_price: mid,
                ref_price: None,
                hedge_book: None,
            };
            black_box(strategy.compute_quotes(&ctx))
        })
    });
}

fn bench_orderbook_update(c: &mut Criterion) {
    let mut book = make_book();

    c.bench_function("orderbook_delta_update", |b| {
        let mut seq = 2u64;
        b.iter(|| {
            book.apply_delta(
                vec![PriceLevel {
                    price: dec!(49999.50),
                    qty: dec!(2.5),
                }],
                vec![PriceLevel {
                    price: dec!(50001.50),
                    qty: dec!(2.5),
                }],
                seq,
            );
            seq += 1;
            black_box(book.mid_price())
        })
    });
}

criterion_group!(
    benches,
    bench_avellaneda,
    bench_glft,
    bench_grid,
    bench_orderbook_update,
);
criterion_main!(benches);
