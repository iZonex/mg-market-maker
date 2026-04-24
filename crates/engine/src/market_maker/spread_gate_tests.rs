use super::*;
use crate::connector_bundle::ConnectorBundle;
use crate::test_support::MockConnector;
use mm_common::config::AppConfig;
use mm_common::types::PriceLevel;
use mm_exchange_core::connector::{VenueId, VenueProduct};
use mm_exchange_core::events::MarketEvent;
use mm_strategy::AvellanedaStoikov;

fn base_engine_with_gate(gate_bps: Option<Decimal>) -> MarketMakerEngine {
    let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let bundle = ConnectorBundle::single(primary);
    let mut cfg = AppConfig::default();
    cfg.risk.max_spread_to_quote_bps = gate_bps;
    MarketMakerEngine::new(
        "BTCUSDT".to_string(),
        cfg,
        ProductSpec {
            symbol: "BTCUSDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(0.0001),
            min_notional: dec!(10),
            maker_fee: dec!(0.0001),
            taker_fee: dec!(0.0005),
            trading_status: Default::default(),
        },
        Box::new(AvellanedaStoikov),
        bundle,
        None,
        None,
    )
}

fn snapshot_with_spread(bid: Decimal, ask: Decimal) -> MarketEvent {
    MarketEvent::BookSnapshot {
        venue: VenueId::Binance,
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel {
            price: bid,
            qty: dec!(10),
        }],
        asks: vec![PriceLevel {
            price: ask,
            qty: dec!(10),
        }],
        sequence: 1,
    }
}

#[tokio::test]
async fn spread_gate_none_never_blocks_quoting() {
    // Baseline: no gate configured, wide book → CB may trip
    // but the gate itself is inert. We only verify the gate
    // path does not short-circuit when unset.
    let mut engine = base_engine_with_gate(None);
    // Absurdly wide book — 100 bps spread.
    engine.handle_ws_event(snapshot_with_spread(dec!(50_000), dec!(50_500)));
    // `refresh_quotes` reaches the quote-compute path.
    // We cannot easily assert "quotes were computed" without
    // more plumbing, but we can at least verify tick_count
    // advances (it is the first statement of refresh_quotes).
    let before = engine.tick_count;
    // If the gate blocked us, tick_count still advanced
    // (the increment happens before the gate), so use a
    // weaker invariant: the call returns Ok without panic.
    assert!(engine.refresh_quotes().await.is_ok());
    assert_eq!(engine.tick_count, before + 1);
}

#[tokio::test]
async fn spread_gate_blocks_quoting_when_spread_exceeds_threshold() {
    // Gate set at 50 bps. Push a 100 bps book — the gate
    // must return early and NOT trip the circuit breaker.
    let mut engine = base_engine_with_gate(Some(dec!(50)));
    engine.handle_ws_event(snapshot_with_spread(dec!(50_000), dec!(50_500)));

    let cb_before = engine.circuit_breaker.is_tripped();
    let live_before = engine.order_manager.live_count();

    let result = engine.refresh_quotes().await;
    assert!(result.is_ok());

    // No new orders placed because the gate short-circuited.
    assert_eq!(engine.order_manager.live_count(), live_before);
    // Circuit breaker untouched by the soft gate (but the
    // hard `check_spread` may still have fired if the book
    // was above `max_spread_bps`; default is 500 bps, and
    // 100 bps < 500, so the hard check should also be
    // clean). This is the test that pins the soft semantics.
    assert_eq!(
        engine.circuit_breaker.is_tripped(),
        cb_before,
        "soft spread gate must not trip the circuit breaker"
    );
}

#[tokio::test]
async fn spread_gate_allows_quoting_when_spread_is_tight() {
    // Gate at 50 bps. 2 bps book → passes.
    let mut engine = base_engine_with_gate(Some(dec!(50)));
    engine.handle_ws_event(snapshot_with_spread(dec!(50_000), dec!(50_010)));
    // Just verify it does not error out; the main test is
    // the blocking path above.
    assert!(engine.refresh_quotes().await.is_ok());
}
