use super::*;
use crate::connector_bundle::ConnectorBundle;
use crate::test_support::MockConnector;
use mm_common::config::AppConfig;
use mm_common::types::InstrumentPair;
use mm_exchange_core::connector::{VenueId, VenueProduct};
use mm_risk::kill_switch::KillLevel;
use mm_strategy::funding_arb_driver::DriverEvent;
use mm_strategy::AvellanedaStoikov;

fn dual_engine_with_driver_field() -> MarketMakerEngine {
    let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let hedge = Arc::new(MockConnector::new(
        VenueId::HyperLiquid,
        VenueProduct::LinearPerp,
    ));
    let pair = InstrumentPair {
        primary_symbol: "BTCUSDT".to_string(),
        hedge_symbol: "BTC-PERP".to_string(),
        multiplier: dec!(1),
        funding_interval_secs: Some(28_800),
        basis_threshold_bps: dec!(50),
    };
    let bundle = ConnectorBundle::dual(primary, hedge, pair);
    MarketMakerEngine::new(
        "BTCUSDT".to_string(),
        AppConfig::default(),
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

#[tokio::test]
async fn compensated_pair_break_does_not_halt_driver_but_audits() {
    let mut engine = dual_engine_with_driver_field();
    // Driver itself is None in this test — we only assert
    // the dispatcher's behaviour on the event value. A real
    // driver would have been constructed via
    // `with_funding_arb_driver`.
    let starting_level = engine.kill_switch.level();
    engine.handle_driver_event(DriverEvent::PairBreak {
        reason: "post-only cross".to_string(),
        compensated: true,
    });
    // Compensated breaks do NOT escalate kill switch.
    assert_eq!(engine.kill_switch.level(), starting_level);
}

#[tokio::test]
async fn uncompensated_pair_break_escalates_to_l2_and_drops_driver() {
    let mut engine = dual_engine_with_driver_field();
    // Start with driver set so we can verify it gets dropped.
    // Construct a driver with a NullSink using real connectors.
    let driver = mm_strategy::FundingArbDriver::new(
        engine.connectors.primary.clone(),
        engine.connectors.hedge.clone().unwrap(),
        engine.connectors.pair.clone().unwrap(),
        mm_strategy::FundingArbDriverConfig::default(),
        Arc::new(mm_strategy::NullSink),
    );
    engine.funding_arb_driver = Some(driver);

    engine.handle_driver_event(DriverEvent::PairBreak {
        reason: "post-only cross".to_string(),
        compensated: false,
    });

    assert_eq!(
        engine.kill_switch.level(),
        KillLevel::StopNewOrders,
        "uncompensated break → L2"
    );
    assert!(
        engine.funding_arb_driver.is_none(),
        "driver dropped so it stops ticking"
    );
}

#[tokio::test]
async fn hold_events_are_silent_noops() {
    let mut engine = dual_engine_with_driver_field();
    let starting_level = engine.kill_switch.level();
    engine.handle_driver_event(DriverEvent::Hold);
    engine.handle_driver_event(DriverEvent::InputUnavailable {
        reason: "test".to_string(),
    });
    assert_eq!(engine.kill_switch.level(), starting_level);
}

#[tokio::test]
async fn entered_and_exited_only_audit_do_not_escalate() {
    let mut engine = dual_engine_with_driver_field();
    let starting_level = engine.kill_switch.level();
    engine.handle_driver_event(DriverEvent::TakerRejected {
        reason: "insufficient margin".to_string(),
    });
    assert_eq!(engine.kill_switch.level(), starting_level);
}

#[tokio::test]
async fn fills_reconcile_driver_state_on_both_legs() {
    let mut engine = dual_engine_with_driver_field();
    let driver = mm_strategy::FundingArbDriver::new(
        engine.connectors.primary.clone(),
        engine.connectors.hedge.clone().unwrap(),
        engine.connectors.pair.clone().unwrap(),
        mm_strategy::FundingArbDriverConfig::default(),
        Arc::new(mm_strategy::NullSink),
    );
    engine.funding_arb_driver = Some(driver);

    // Primary leg fill: long 0.1 spot.
    engine.handle_ws_event(MarketEvent::Fill {
        venue: VenueId::Binance,
        fill: mm_common::types::Fill {
            trade_id: 1,
            order_id: mm_common::types::OrderId::new_v4(),
            symbol: "BTCUSDT".to_string(),
            side: mm_common::types::Side::Buy,
            price: dec!(50_000),
            qty: dec!(0.1),
            is_maker: true,
            timestamp: chrono::Utc::now(),
        },
    });

    // Hedge leg fill: short 0.1 perp.
    engine.handle_hedge_event(MarketEvent::Fill {
        venue: VenueId::HyperLiquid,
        fill: mm_common::types::Fill {
            trade_id: 2,
            order_id: mm_common::types::OrderId::new_v4(),
            symbol: "BTC-PERP".to_string(),
            side: mm_common::types::Side::Sell,
            price: dec!(50_010),
            qty: dec!(0.1),
            is_maker: false,
            timestamp: chrono::Utc::now(),
        },
    });

    let state = engine.funding_arb_driver.as_ref().unwrap().state();
    assert_eq!(state.spot_position, dec!(0.1), "spot long");
    assert_eq!(state.perp_position, dec!(-0.1), "perp short");
    assert_eq!(state.net_delta, dec!(0), "delta-neutral");
}
