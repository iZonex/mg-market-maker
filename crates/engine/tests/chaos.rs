//! Chaos tests — exercise cascading failure paths across risk
//! components that the proptest unit tests cover in isolation.
//! Each scenario drives a realistic deterioration sequence:
//!
//! - **PnL cascade**: losing PnL walks through WidenSpreads →
//!   CancelAll escalation boundaries and cannot de-escalate.
//! - **Venue-error cascade**: consecutive errors on the `HealthManager`
//!   walk Normal → Degraded → Critical, recovering on success.
//! - **Mixed cascade**: a combined PnL + error storm lands the
//!   kill switch at CancelAll and health at Critical simultaneously,
//!   the engine's two signals agreeing the desk must stop quoting.
//! - **Reset recovery**: after any escalation, `reset()` restores
//!   the clean initial state so a human override can resume quoting.

use mm_engine::health::{HealthManager, HealthMode};
use mm_risk::kill_switch::{KillLevel, KillSwitch, KillSwitchConfig};
use rust_decimal_macros::dec;

fn test_config() -> KillSwitchConfig {
    KillSwitchConfig {
        daily_loss_limit: dec!(1000),
        daily_loss_warning: dec!(500),
        max_position_value: dec!(50_000),
        max_message_rate: 100,
        max_consecutive_errors: 10,
        no_fill_timeout_secs: 300,
    }
}

/// A PnL deterioration curve walks the switch past the warning
/// and loss thresholds in order and never rolls back.
#[test]
fn pnl_cascade_escalates_monotonically() {
    let mut ks = KillSwitch::new(test_config());

    // Below any threshold: stays Normal.
    ks.update_pnl(dec!(-100));
    assert_eq!(ks.level(), KillLevel::Normal);

    // Past the warning threshold: WidenSpreads.
    ks.update_pnl(dec!(-600));
    assert_eq!(ks.level(), KillLevel::WidenSpreads);

    // PnL recovers: escalate() cannot step back by design.
    ks.update_pnl(dec!(-200));
    assert_eq!(ks.level(), KillLevel::WidenSpreads);

    // PnL blows through the hard limit: CancelAll.
    ks.update_pnl(dec!(-1_500));
    assert_eq!(ks.level(), KillLevel::CancelAll);

    // Still cannot walk back — only manual reset clears it.
    ks.update_pnl(dec!(0));
    assert_eq!(ks.level(), KillLevel::CancelAll);
}

/// Position value alone can drive the switch to `StopNewOrders`
/// without any PnL input, and again only forward.
#[test]
fn position_value_triggers_stop_new_orders() {
    let mut ks = KillSwitch::new(test_config());
    ks.update_position_value(dec!(30_000));
    assert_eq!(ks.level(), KillLevel::Normal);

    ks.update_position_value(dec!(60_000));
    assert_eq!(ks.level(), KillLevel::StopNewOrders);

    // Position shrinks — level does not drop.
    ks.update_position_value(dec!(10_000));
    assert_eq!(ks.level(), KillLevel::StopNewOrders);
}

/// A PnL hit above the hard limit on a fresh switch jumps
/// straight to CancelAll — the intermediate warning state isn't
/// required for the hard-limit branch to fire.
#[test]
fn hard_loss_jumps_past_warning() {
    let mut ks = KillSwitch::new(test_config());
    ks.update_pnl(dec!(-2_000));
    assert_eq!(ks.level(), KillLevel::CancelAll);
}

/// Venue-error accrual walks the HealthManager from Normal
/// through Degraded to Critical, recovering to Normal only on
/// an explicit success signal.
#[test]
fn health_escalation_cascade() {
    let mut hm = HealthManager::new();

    // Below 5 errors — Normal.
    for _ in 0..3 {
        hm.record_error();
    }
    hm.evaluate();
    assert_eq!(hm.mode(), &HealthMode::Normal);

    // 5 consecutive errors — Degraded.
    for _ in 0..5 {
        hm.record_error();
    }
    hm.evaluate();
    assert!(matches!(hm.mode(), HealthMode::Degraded { .. }));
    assert_eq!(hm.spread_multiplier(), dec!(2));

    // 20 consecutive errors — Critical. `should_cancel_all()`
    // matches the Critical branch only.
    for _ in 0..15 {
        hm.record_error();
    }
    hm.evaluate();
    assert!(matches!(hm.mode(), HealthMode::Critical { .. }));
    assert!(hm.should_cancel_all());
    assert_eq!(hm.spread_multiplier(), dec!(0));

    // A single success resets the error counter and the next
    // evaluate drops us back to Normal.
    hm.record_success();
    hm.evaluate();
    assert_eq!(hm.mode(), &HealthMode::Normal);
    assert!(!hm.should_cancel_all());
}

/// Mixed cascade: simultaneous PnL cliff + sustained venue
/// errors drive both subsystems to their halt state in the same
/// sequence a real incident would.
#[test]
fn combined_pnl_and_error_cascade_halts_quoting() {
    let mut ks = KillSwitch::new(test_config());
    let mut hm = HealthManager::new();

    // Accumulate errors.
    for _ in 0..25 {
        hm.record_error();
    }
    hm.evaluate();
    // Position value breach first, then PnL cliff.
    ks.update_position_value(dec!(60_000));
    ks.update_pnl(dec!(-1_500));

    // Both subsystems independently say "stop".
    assert!(matches!(hm.mode(), HealthMode::Critical { .. }));
    assert!(hm.should_cancel_all());
    assert_eq!(ks.level(), KillLevel::CancelAll);
    assert_eq!(ks.spread_multiplier(), dec!(1));
    assert_eq!(ks.size_multiplier(), dec!(0));
}

/// After an escalation on either side, `reset()` returns the
/// switch to Normal and a success event restores health. This
/// is the happy-path a human operator takes after triaging an
/// incident.
#[test]
fn reset_restores_clean_state() {
    let mut ks = KillSwitch::new(test_config());
    let mut hm = HealthManager::new();

    // Escalate both.
    ks.manual_trigger(KillLevel::FlattenAll, "test");
    for _ in 0..30 {
        hm.record_error();
    }
    hm.evaluate();

    assert_eq!(ks.level(), KillLevel::FlattenAll);
    assert!(matches!(hm.mode(), HealthMode::Critical { .. }));

    // Recovery path.
    ks.reset();
    hm.record_success();
    hm.evaluate();

    assert_eq!(ks.level(), KillLevel::Normal);
    assert_eq!(hm.mode(), &HealthMode::Normal);
    assert!(!hm.should_cancel_all());
}

/// `allow_new_orders` short-circuits everything from
/// `StopNewOrders` up. Verifies the guard tracks the level.
#[test]
fn allow_new_orders_gate_matches_level() {
    let mut ks = KillSwitch::new(test_config());
    assert!(ks.allow_new_orders());

    ks.manual_trigger(KillLevel::WidenSpreads, "x");
    assert!(ks.allow_new_orders());

    ks.manual_trigger(KillLevel::StopNewOrders, "x");
    assert!(!ks.allow_new_orders());

    ks.manual_trigger(KillLevel::CancelAll, "x");
    assert!(!ks.allow_new_orders());

    ks.reset();
    assert!(ks.allow_new_orders());
}
