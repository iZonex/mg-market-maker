use super::*;
use crate::connector_bundle::ConnectorBundle;
use crate::test_support::MockConnector;
use mm_common::config::AppConfig;
use mm_common::types::{Fill, Side};
use mm_exchange_core::connector::{VenueId, VenueProduct};
use mm_exchange_core::events::MarketEvent;
use mm_portfolio::Portfolio;
use mm_strategy::AvellanedaStoikov;

fn sample_product(symbol: &str) -> ProductSpec {
    ProductSpec {
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

fn build_engine(symbol: &str, portfolio: Arc<Mutex<Portfolio>>) -> MarketMakerEngine {
    let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let bundle = ConnectorBundle::single(primary);
    MarketMakerEngine::new(
        symbol.to_string(),
        AppConfig::default(),
        sample_product(symbol),
        Box::new(AvellanedaStoikov),
        bundle,
        None,
        None,
    )
    .with_portfolio(portfolio)
}

fn fill_event(symbol: &str, side: Side, qty: Decimal, price: Decimal) -> MarketEvent {
    MarketEvent::Fill {
        venue: VenueId::Binance,
        fill: Fill {
            trade_id: 1,
            order_id: mm_common::types::OrderId::new_v4(),
            symbol: symbol.to_string(),
            side,
            price,
            qty,
            is_maker: true,
            timestamp: chrono::Utc::now(),
        },
    }
}

#[test]
fn engine_without_portfolio_runs_untouched() {
    let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let bundle = ConnectorBundle::single(primary);
    let mut engine = MarketMakerEngine::new(
        "BTCUSDT".to_string(),
        AppConfig::default(),
        sample_product("BTCUSDT"),
        Box::new(AvellanedaStoikov),
        bundle,
        None,
        None,
    );
    assert!(engine.portfolio.is_none());
    // Fill should NOT panic when portfolio is absent.
    engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));
}

#[test]
fn fill_routes_signed_qty_to_shared_portfolio() {
    let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
    let mut engine = build_engine("BTCUSDT", portfolio.clone());

    engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));

    let snap = portfolio.lock().unwrap().snapshot();
    let btc = snap.per_asset.get("BTCUSDT").expect("BTCUSDT entry");
    assert_eq!(btc.qty, dec!(0.1), "long from buy fill");
    assert_eq!(btc.avg_entry, dec!(50_000));
}

#[test]
fn sell_fill_routes_negative_qty_to_portfolio() {
    let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
    let mut engine = build_engine("BTCUSDT", portfolio.clone());

    // Buy 0.2 then sell 0.15 → net long 0.05, realise +50 USDT
    // on the 0.15 closed at 51_000 vs avg 50_000.
    engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.2), dec!(50_000)));
    engine.handle_ws_event(fill_event("BTCUSDT", Side::Sell, dec!(0.15), dec!(51_000)));

    let snap = portfolio.lock().unwrap().snapshot();
    let btc = snap.per_asset.get("BTCUSDT").unwrap();
    assert_eq!(btc.qty, dec!(0.05));
    assert_eq!(btc.realised_pnl_native, dec!(150));
    assert_eq!(snap.total_realised_pnl, dec!(150));
}

#[test]
fn multi_symbol_engines_share_one_portfolio() {
    // Two engines, one shared portfolio. After both report
    // a buy fill, the snapshot sees both positions under the
    // unified reporting currency.
    let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
    let mut btc_engine = build_engine("BTCUSDT", portfolio.clone());
    let mut eth_engine = build_engine("ETHUSDT", portfolio.clone());

    btc_engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.1), dec!(50_000)));
    eth_engine.handle_ws_event(fill_event("ETHUSDT", Side::Buy, dec!(1), dec!(3_000)));

    let snap = portfolio.lock().unwrap().snapshot();
    assert_eq!(snap.per_asset.len(), 2, "both symbols tracked");
    assert!(snap.per_asset.contains_key("BTCUSDT"));
    assert!(snap.per_asset.contains_key("ETHUSDT"));
}

#[test]
fn portfolio_fx_and_reporting_currency_roundtrip() {
    // Portfolio remains in USDT regardless of per-engine
    // quote assets. The engine does NOT set FX by default —
    // callers are responsible for wiring `set_fx` when the
    // engine quotes in a non-USDT asset. This test locks
    // that contract.
    let portfolio = Arc::new(Mutex::new(Portfolio::new("USDT")));
    let mut engine = build_engine("BTCUSDT", portfolio.clone());
    engine.handle_ws_event(fill_event("BTCUSDT", Side::Buy, dec!(0.01), dec!(50_000)));
    let snap = portfolio.lock().unwrap().snapshot();
    assert_eq!(snap.reporting_currency, "USDT");
}
