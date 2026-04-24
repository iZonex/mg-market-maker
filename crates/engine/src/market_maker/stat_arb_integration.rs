use super::*;
use crate::connector_bundle::ConnectorBundle;
use crate::test_support::MockConnector;
use mm_common::config::AppConfig;
use mm_exchange_core::connector::{VenueId, VenueProduct};
use mm_strategy::avellaneda::AvellanedaStoikov;
use mm_strategy::stat_arb::{
    NullStatArbSink, SpreadDirection, StatArbDriver, StatArbDriverConfig, StatArbEvent,
    StatArbPair, ZScoreConfig,
};
use std::time::Duration;

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

fn single_engine() -> MarketMakerEngine {
    let primary = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let bundle = ConnectorBundle::single(primary);
    MarketMakerEngine::new(
        "BTCUSDT".to_string(),
        AppConfig::default(),
        sample_product("BTCUSDT"),
        Box::new(AvellanedaStoikov),
        bundle,
        None,
        None,
    )
}

fn stat_arb_pair() -> StatArbPair {
    StatArbPair {
        y_symbol: "BTCUSDT".to_string(),
        x_symbol: "ETHUSDT".to_string(),
        strategy_class: "stat_arb_BTCUSDT_ETHUSDT".to_string(),
    }
}

fn small_stat_arb_config() -> StatArbDriverConfig {
    StatArbDriverConfig {
        tick_interval: Duration::from_millis(10),
        zscore: ZScoreConfig {
            window: 20,
            entry_threshold: dec!(1.5),
            exit_threshold: dec!(0.3),
        },
        kalman_transition_var: dec!(0.000001),
        kalman_observation_var: dec!(0.001),
        leg_notional_usd: dec!(1000),
    }
}

/// Seed a synthetic `Y = 2 · X` cointegrated history.
fn seed_cointegrated(driver: &mut StatArbDriver) {
    let x: Vec<Decimal> = (0..60)
        .map(|i| dec!(100) + Decimal::from(i as i64 % 5 - 2))
        .collect();
    let y: Vec<Decimal> = x
        .iter()
        .enumerate()
        .map(|(i, xi)| {
            let jitter = Decimal::from(i as i64 % 3 - 1) / dec!(10);
            dec!(2) * xi + jitter
        })
        .collect();
    driver.recheck_cointegration(&y, &x);
}

/// Silent routing: none of the benign variants should
/// escalate the kill switch or mutate engine state.
#[tokio::test]
async fn silent_variants_do_not_escalate() {
    let mut engine = single_engine();
    let starting = engine.kill_switch.level();
    engine.handle_stat_arb_event(StatArbEvent::Hold { z: dec!(0.1) }, None);
    engine.handle_stat_arb_event(
        StatArbEvent::Warmup {
            samples: 3,
            required: 20,
        },
        None,
    );
    engine.handle_stat_arb_event(StatArbEvent::NotCointegrated { adf_stat: None }, None);
    engine.handle_stat_arb_event(
        StatArbEvent::InputUnavailable {
            reason: "empty book".to_string(),
        },
        None,
    );
    assert_eq!(engine.kill_switch.level(), starting);
}

/// Entered / Exited events flow through `handle_stat_arb_event`
/// without panic — the handler emits audit records but
/// does NOT dispatch orders in stage-1 (advisory only).
#[tokio::test]
async fn entered_and_exited_routed_to_audit_without_panic() {
    let mut engine = single_engine();
    let starting = engine.kill_switch.level();

    engine.handle_stat_arb_event(
        StatArbEvent::Entered {
            direction: SpreadDirection::SellY,
            y_qty: dec!(5),
            x_qty: dec!(10),
            z: dec!(2.5),
            spread: dec!(1.5),
        },
        None,
    );
    engine.handle_stat_arb_event(
        StatArbEvent::Exited {
            z: dec!(0.2),
            spread: dec!(0.1),
            realised_pnl_estimate: dec!(42),
        },
        None,
    );

    // Stage-1 advisory-only: kill switch untouched.
    assert_eq!(engine.kill_switch.level(), starting);
}

/// Builder smoke test: `with_stat_arb_driver` plumbs a
/// driver onto the engine and sets the tick interval.
#[tokio::test]
async fn with_stat_arb_driver_installs_driver() {
    let y = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let x = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    y.set_mid(dec!(200));
    x.set_mid(dec!(100));
    let driver = StatArbDriver::new(
        y,
        x,
        stat_arb_pair(),
        small_stat_arb_config(),
        Arc::new(NullStatArbSink),
    );
    let engine = single_engine().with_stat_arb_driver(driver, Duration::from_millis(50));
    assert!(engine.stat_arb_driver.is_some());
    assert_eq!(engine.stat_arb_tick, Duration::from_millis(50));
}

/// End-to-end pipeline: synthetic cointegrated pair drives
/// the full `kalman → signal → driver → engine event`
/// chain. Asserts that a spread shock produces an
/// `Entered` and a revert produces an `Exited` event —
/// and that both route through `handle_stat_arb_event`
/// without tripping the engine's kill switch in
/// advisory-only stage-1.
#[tokio::test]
async fn full_pipeline_entered_then_exited_through_engine_handler() {
    let mut engine = single_engine();
    let starting = engine.kill_switch.level();

    let y = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let x = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    y.set_mid(dec!(200));
    x.set_mid(dec!(100));
    let mut driver = StatArbDriver::new(
        y.clone(),
        x.clone(),
        stat_arb_pair(),
        small_stat_arb_config(),
        Arc::new(NullStatArbSink),
    );
    seed_cointegrated(&mut driver);

    // Warmup: steady book, z stays near zero.
    for _ in 0..20 {
        y.set_mid(dec!(200));
        x.set_mid(dec!(100));
        let e = driver.tick_once().await;
        engine.handle_stat_arb_event(e, None);
    }

    // Shock: Y +5 pushes spread far above its rolling mean.
    y.set_mid(dec!(205));
    let shock_event = driver.tick_once().await;
    let got_entered = matches!(shock_event, StatArbEvent::Entered { .. });
    engine.handle_stat_arb_event(shock_event, None);
    assert!(got_entered, "expected Entered on spread shock");

    // Revert: Y back to 200. Spread shrinks, z returns to
    // the exit band. Drive enough ticks for the rolling
    // mean to catch up.
    y.set_mid(dec!(200));
    let mut saw_exited = false;
    for _ in 0..60 {
        let e = driver.tick_once().await;
        if matches!(e, StatArbEvent::Exited { .. }) {
            engine.handle_stat_arb_event(e, None);
            saw_exited = true;
            break;
        }
        engine.handle_stat_arb_event(e, None);
    }
    assert!(saw_exited, "expected Exited after revert");

    // Stage-1 advisory-only: no kill-switch escalation
    // regardless of the event sequence.
    assert_eq!(engine.kill_switch.level(), starting);
}

/// MV-4 — a partial dispatch (one leg filled, the other
/// errored) escalates kill switch to StopNewOrders and
/// drops the stat-arb driver. This is the naked-leg
/// safety we were missing while the driver was labelled
/// "advisory only".
#[tokio::test]
async fn partial_dispatch_failure_escalates_and_drops_driver() {
    use mm_strategy::stat_arb::{LegDispatchReport, LegOutcome};

    let mut engine = single_engine();
    let y = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let x = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let driver = StatArbDriver::new(
        y,
        x,
        stat_arb_pair(),
        small_stat_arb_config(),
        Arc::new(NullStatArbSink),
    );
    engine.stat_arb_driver = Some(driver);
    let starting = engine.kill_switch.level();
    assert!(engine.stat_arb_driver.is_some());

    let partial = LegDispatchReport {
        y: Some(LegOutcome {
            symbol: "Y".into(),
            side: mm_common::types::Side::Sell,
            target_qty: dec!(5),
            dispatched_qty: dec!(5),
            error: None,
        }),
        x: Some(LegOutcome {
            symbol: "X".into(),
            side: mm_common::types::Side::Buy,
            target_qty: dec!(10),
            dispatched_qty: dec!(0),
            error: Some("place_order: venue rate limited".into()),
        }),
    };
    engine.handle_stat_arb_event(
        StatArbEvent::Entered {
            direction: SpreadDirection::SellY,
            y_qty: dec!(5),
            x_qty: dec!(10),
            z: dec!(2.5),
            spread: dec!(1.5),
        },
        Some(partial),
    );

    assert_ne!(engine.kill_switch.level(), starting);
    assert_eq!(
        engine.kill_switch.level(),
        mm_risk::kill_switch::KillLevel::StopNewOrders
    );
    assert!(
        engine.stat_arb_driver.is_none(),
        "driver must drop after naked-leg incident"
    );
}

/// MV-4 — full success (both legs placed) must NOT
/// escalate and must leave the driver in place.
#[tokio::test]
async fn full_dispatch_success_does_not_escalate() {
    use mm_strategy::stat_arb::{LegDispatchReport, LegOutcome};

    let mut engine = single_engine();
    let y = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let x = Arc::new(MockConnector::new(VenueId::Binance, VenueProduct::Spot));
    let driver = StatArbDriver::new(
        y,
        x,
        stat_arb_pair(),
        small_stat_arb_config(),
        Arc::new(NullStatArbSink),
    );
    engine.stat_arb_driver = Some(driver);
    let starting = engine.kill_switch.level();

    let both_ok = LegDispatchReport {
        y: Some(LegOutcome {
            symbol: "Y".into(),
            side: mm_common::types::Side::Sell,
            target_qty: dec!(5),
            dispatched_qty: dec!(5),
            error: None,
        }),
        x: Some(LegOutcome {
            symbol: "X".into(),
            side: mm_common::types::Side::Buy,
            target_qty: dec!(10),
            dispatched_qty: dec!(10),
            error: None,
        }),
    };
    engine.handle_stat_arb_event(
        StatArbEvent::Entered {
            direction: SpreadDirection::SellY,
            y_qty: dec!(5),
            x_qty: dec!(10),
            z: dec!(2.5),
            spread: dec!(1.5),
        },
        Some(both_ok),
    );

    assert_eq!(engine.kill_switch.level(), starting);
    assert!(engine.stat_arb_driver.is_some());
}
