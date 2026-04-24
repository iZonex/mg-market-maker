use super::*;
use crate::connector_bundle::ConnectorBundle;
use crate::test_support::MockConnector;
use mm_common::config::AppConfig;
use mm_common::PriceLevel;
use mm_exchange_core::connector::{ExchangeConnector, VenueId, VenueProduct};
use mm_strategy::avellaneda::AvellanedaStoikov;

fn sample_product(symbol: &str) -> mm_common::types::ProductSpec {
    mm_common::types::ProductSpec {
        symbol: symbol.to_string(),
        base_asset: "BTC".to_string(),
        quote_asset: "USDT".to_string(),
        tick_size: dec!(0.01),
        lot_size: dec!(0.0001),
        min_notional: dec!(10),
        maker_fee: dec!(0.0001),
        taker_fee: dec!(0.0005),
        trading_status: Default::default(),
    }
}

/// End-to-end: build a `MarketMakerEngine` whose primary
/// connector is a `MockConnector` with `max_batch_size=20`,
/// apply a book snapshot, call `refresh_quotes()`, and
/// assert the connector saw exactly one
/// `place_orders_batch` call (carrying all `num_levels × 2`
/// quotes) and zero per-order `place_order` calls.
///
/// This is the pin for the entire Epic E sub-component #1
/// wiring: the strategy → diff → batch path is byte-
/// connected through the existing `refresh_quotes` flow,
/// no engine field changes required.
#[tokio::test]
async fn refresh_quotes_routes_through_batch_on_first_diff() {
    // Hold a typed Arc<MockConnector> alongside the
    // dyn-typed Arc that ConnectorBundle wants — both
    // share the same allocation, so the test can read
    // batch counters after refresh_quotes.
    let mock =
        Arc::new(MockConnector::new(VenueId::Bybit, VenueProduct::Spot).with_max_batch_size(20));
    let dyn_conn: Arc<dyn ExchangeConnector> = mock.clone();
    let bundle = ConnectorBundle::single(dyn_conn);
    let mut engine = MarketMakerEngine::new(
        "BTCUSDT".to_string(),
        AppConfig::default(),
        sample_product("BTCUSDT"),
        Box::new(AvellanedaStoikov),
        bundle,
        None,
        None,
    );

    // Apply a tight book snapshot so the strategy
    // produces non-zero quotes.
    // Populate the balance cache directly with synthetic
    // balances. We deliberately bypass `refresh_balances`
    // because that rebuilds `exposure_manager` with the
    // wallet's starting equity, after which a fresh-engine
    // refresh_quotes sees a "current equity = 0" vs
    // "starting equity = 100k" delta, trips the drawdown
    // circuit breaker, and returns early before reaching
    // execute_diff. The synthetic-balance path keeps
    // exposure_manager at its default zero baseline.
    engine.balance_cache.update_from_exchange(&[
        mm_common::types::Balance {
            asset: "USDT".to_string(),
            wallet: mm_common::types::WalletType::Spot,
            total: dec!(100_000),
            locked: dec!(0),
            available: dec!(100_000),
        },
        mm_common::types::Balance {
            asset: "BTC".to_string(),
            wallet: mm_common::types::WalletType::Spot,
            total: dec!(10),
            locked: dec!(0),
            available: dec!(10),
        },
    ]);

    engine.handle_ws_event(MarketEvent::BookSnapshot {
        venue: VenueId::Bybit,
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel {
            price: dec!(50_000),
            qty: dec!(10),
        }],
        asks: vec![PriceLevel {
            price: dec!(50_001),
            qty: dec!(10),
        }],
        sequence: 1,
    });

    let result = engine.refresh_quotes().await;
    assert!(result.is_ok(), "refresh_quotes errored: {result:?}");

    // Default `num_levels = 3` × (1 bid + 1 ask) = 6
    // raw quotes, but the diff layer dedupes by
    // `(side, price)` after tick rounding — at the
    // default `order_size = 0.001` and `tick = 0.01`,
    // adjacent levels collide on the same tick, so the
    // engine ends up with 1 unique bid + 1 unique ask
    // = 2 placements. That's still ≥ MIN_BATCH_SIZE=2,
    // so the batch path fires exactly once.
    let batch_calls = mock.place_batch_calls();
    let single_calls = mock.place_single_calls();
    assert_eq!(
        batch_calls, 1,
        "expected exactly one batch place call, got {batch_calls}"
    );
    assert_eq!(
        single_calls, 0,
        "expected zero per-order place calls, got {single_calls}"
    );
    assert_eq!(engine.order_manager.live_count(), 2);
}

/// Sanity test: a venue with `max_batch_size=1` (the
/// pathological floor) keeps the engine on the per-order
/// path even on a multi-quote first diff.
#[tokio::test]
async fn refresh_quotes_stays_per_order_when_max_batch_size_is_one() {
    let mock =
        Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot).with_max_batch_size(1));
    let dyn_conn: Arc<dyn ExchangeConnector> = mock.clone();
    let bundle = ConnectorBundle::single(dyn_conn);
    let mut engine = MarketMakerEngine::new(
        "BTCUSDT".to_string(),
        AppConfig::default(),
        sample_product("BTCUSDT"),
        Box::new(AvellanedaStoikov),
        bundle,
        None,
        None,
    );

    // Direct balance-cache populate (not refresh_balances)
    // — see the routes_through_batch test for the
    // exposure-manager rationale.
    engine.balance_cache.update_from_exchange(&[
        mm_common::types::Balance {
            asset: "USDT".to_string(),
            wallet: mm_common::types::WalletType::Spot,
            total: dec!(100_000),
            locked: dec!(0),
            available: dec!(100_000),
        },
        mm_common::types::Balance {
            asset: "BTC".to_string(),
            wallet: mm_common::types::WalletType::Spot,
            total: dec!(10),
            locked: dec!(0),
            available: dec!(10),
        },
    ]);

    engine.handle_ws_event(MarketEvent::BookSnapshot {
        venue: VenueId::Binance,
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel {
            price: dec!(50_000),
            qty: dec!(10),
        }],
        asks: vec![PriceLevel {
            price: dec!(50_001),
            qty: dec!(10),
        }],
        sequence: 1,
    });

    let result = engine.refresh_quotes().await;
    assert!(result.is_ok(), "refresh_quotes errored: {result:?}");

    // max_batch=1 forces per-order path. Diff produces
    // 2 unique quotes (see comment in the sibling test
    // about tick-rounding dedupe), so we expect 2 single
    // calls and zero batch calls.
    assert_eq!(mock.place_batch_calls(), 0);
    assert_eq!(mock.place_single_calls(), 2);
    assert_eq!(engine.order_manager.live_count(), 2);
}
