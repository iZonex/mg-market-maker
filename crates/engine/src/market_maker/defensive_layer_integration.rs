use super::*;
use crate::connector_bundle::ConnectorBundle;
use crate::test_support::MockConnector;
use mm_common::config::AppConfig;
use mm_exchange_core::connector::{VenueId, VenueProduct};
use mm_risk::lead_lag_guard::{LeadLagGuard, LeadLagGuardConfig};
use mm_risk::news_retreat::{NewsRetreatConfig, NewsRetreatStateMachine};
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

fn make_engine() -> MarketMakerEngine {
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

fn fixture_news_config() -> NewsRetreatConfig {
    NewsRetreatConfig {
        critical_keywords: vec!["hack".to_string(), "exploit".to_string()],
        high_keywords: vec!["FOMC".to_string(), "CPI".to_string()],
        low_keywords: vec!["partnership".to_string()],
        critical_cooldown_ms: 30 * 60_000,
        high_cooldown_ms: 5 * 60_000,
        low_cooldown_ms: 0,
        high_multiplier: dec!(2),
        critical_multiplier: dec!(3),
    }
}

/// Builder smoke test: both defensive controls plug onto
/// the engine via the new builder methods.
#[test]
fn builders_install_both_defensive_controls() {
    let guard = LeadLagGuard::new(LeadLagGuardConfig::default());
    let news = NewsRetreatStateMachine::new(fixture_news_config()).expect("valid news config");
    let engine = make_engine()
        .with_lead_lag_guard(guard)
        .with_news_retreat(news);
    assert!(engine.lead_lag_guard.is_some());
    assert!(engine.news_retreat.is_some());
}

/// End-to-end #1: a synthetic leader-mid stream with a
/// sharp shock pushes the lead-lag guard into ramp
/// territory; the autotuner's `lead_lag_mult` updates;
/// `effective_spread_mult` widens.
#[test]
fn lead_lag_pipeline_widens_autotuner_on_shock() {
    let guard = LeadLagGuard::new(LeadLagGuardConfig {
        half_life_events: 10,
        z_min: dec!(2),
        z_max: dec!(4),
        max_mult: dec!(3),
    });
    let mut engine = make_engine().with_lead_lag_guard(guard);

    let baseline = engine.auto_tuner.effective_spread_mult();
    // Build up some non-zero variance with small wiggles.
    let mid = dec!(50000);
    for i in 0..30 {
        let delta = if i % 2 == 0 { dec!(1) } else { dec!(-1) };
        engine.update_lead_lag_from_mid(mid + delta);
    }
    // Sharp 5% jump → vastly larger than EWMA std.
    engine.update_lead_lag_from_mid(dec!(52500));
    let after = engine.auto_tuner.effective_spread_mult();
    assert!(
        after > baseline,
        "lead-lag shock should widen the autotuner spread mult: baseline={baseline}, after={after}"
    );
    assert_eq!(
        engine.auto_tuner.lead_lag_mult(),
        dec!(3),
        "guard should saturate at max_mult on a 5% shock"
    );
}

/// End-to-end #2: a Critical-class news headline drives
/// `on_news_headline` → `NewsRetreatStateMachine` → kill
/// switch L2 escalation. The autotuner's news-retreat
/// multiplier also fires.
#[test]
fn critical_headline_escalates_kill_switch_to_l2() {
    let news = NewsRetreatStateMachine::new(fixture_news_config()).expect("valid news config");
    let mut engine = make_engine().with_news_retreat(news);
    let starting = engine.kill_switch.level();
    assert_eq!(starting, mm_risk::kill_switch::KillLevel::Normal);

    engine.on_news_headline("Major exchange hack reported");

    assert_eq!(
        engine.kill_switch.level(),
        mm_risk::kill_switch::KillLevel::StopNewOrders,
        "Critical news should escalate kill switch to L2"
    );
    assert_eq!(
        engine.auto_tuner.news_retreat_mult(),
        dec!(3),
        "autotuner news-retreat multiplier should saturate"
    );
}

/// High-class headline activates the autotuner widening
/// but does NOT escalate the kill switch (the engine still
/// quotes, just wider).
#[test]
fn high_headline_widens_but_does_not_stop_orders() {
    let news = NewsRetreatStateMachine::new(fixture_news_config()).expect("valid news config");
    let mut engine = make_engine().with_news_retreat(news);
    let starting = engine.kill_switch.level();

    engine.on_news_headline("FOMC presser at 2pm");

    assert_eq!(engine.kill_switch.level(), starting);
    assert_eq!(engine.auto_tuner.news_retreat_mult(), dec!(2));
}

/// No-match headlines are silent — no audit, no
/// multiplier change, no kill switch escalation.
#[test]
fn unmatched_headline_is_silent_noop() {
    let news = NewsRetreatStateMachine::new(fixture_news_config()).expect("valid news config");
    let mut engine = make_engine().with_news_retreat(news);
    let starting = engine.kill_switch.level();
    let baseline = engine.auto_tuner.effective_spread_mult();

    engine.on_news_headline("Dogecoin price stable amid market chop");

    assert_eq!(engine.kill_switch.level(), starting);
    assert_eq!(engine.auto_tuner.effective_spread_mult(), baseline);
    assert_eq!(engine.auto_tuner.news_retreat_mult(), dec!(1));
}

/// Engine without any defensive controls attached: both
/// public push APIs are no-ops and never panic.
#[test]
fn push_apis_are_noop_without_attached_controls() {
    let mut engine = make_engine();
    engine.update_lead_lag_from_mid(dec!(50000));
    engine.on_news_headline("hack");
    // Baseline state preserved.
    assert_eq!(engine.auto_tuner.lead_lag_mult(), dec!(1));
    assert_eq!(engine.auto_tuner.news_retreat_mult(), dec!(1));
}
