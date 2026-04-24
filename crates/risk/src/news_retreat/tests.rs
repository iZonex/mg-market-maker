use super::*;

fn fixture_config() -> NewsRetreatConfig {
    NewsRetreatConfig {
        critical_keywords: vec!["hack".to_string(), "exploit".to_string(), "SEC".to_string()],
        high_keywords: vec!["FOMC".to_string(), "CPI".to_string()],
        low_keywords: vec!["partnership".to_string(), "listing".to_string()],
        critical_cooldown_ms: 30 * 60_000,
        high_cooldown_ms: 5 * 60_000,
        low_cooldown_ms: 0,
        high_multiplier: dec!(2),
        critical_multiplier: dec!(3),
    }
}

#[test]
fn new_starts_in_normal() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    assert_eq!(sm.current_state(0), NewsRetreatState::Normal);
    assert_eq!(sm.current_multiplier(0), Decimal::ONE);
    assert!(!sm.should_stop_new_orders(0));
}

#[test]
fn empty_config_classifies_everything_as_no_match() {
    let mut sm = NewsRetreatStateMachine::new(NewsRetreatConfig::default()).unwrap();
    let result = sm.on_headline("SEC charges Coinbase with fraud", 0);
    assert_eq!(result, NewsRetreatTransition::NoMatch);
    assert_eq!(sm.current_state(0), NewsRetreatState::Normal);
}

#[test]
fn critical_keyword_promotes_to_critical() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    let result = sm.on_headline("Major exchange hack reported", 1000);
    assert_eq!(
        result,
        NewsRetreatTransition::Promoted {
            from: NewsRetreatState::Normal,
            to: NewsRetreatState::Critical,
        }
    );
    assert_eq!(sm.current_state(1000), NewsRetreatState::Critical);
    assert_eq!(sm.current_multiplier(1000), dec!(3));
    assert!(sm.should_stop_new_orders(1000));
}

#[test]
fn high_keyword_promotes_to_high() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    let result = sm.on_headline("FOMC raises rates 25bp", 500);
    assert!(matches!(
        result,
        NewsRetreatTransition::Promoted {
            from: NewsRetreatState::Normal,
            to: NewsRetreatState::High,
        }
    ));
    assert_eq!(sm.current_multiplier(500), dec!(2));
    assert!(!sm.should_stop_new_orders(500));
}

#[test]
fn low_keyword_does_not_widen_quotes() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    let result = sm.on_headline("New listing announcement", 0);
    assert!(matches!(
        result,
        NewsRetreatTransition::Promoted {
            from: NewsRetreatState::Normal,
            to: NewsRetreatState::Low,
        }
    ));
    // Low is alert-only — multiplier stays at 1.
    assert_eq!(sm.current_multiplier(0), Decimal::ONE);
    assert!(!sm.should_stop_new_orders(0));
}

#[test]
fn classification_is_case_insensitive() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    // "sec" lowercase still hits "SEC" keyword.
    let result = sm.on_headline("sec investigation underway", 0);
    assert!(matches!(
        result,
        NewsRetreatTransition::Promoted {
            to: NewsRetreatState::Critical,
            ..
        }
    ));
}

#[test]
fn priority_ladder_critical_beats_high_keyword_match() {
    // A headline containing both "FOMC" (High) and
    // "hack" (Critical) classifies as Critical because
    // Critical is checked first.
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    let result = sm.on_headline("FOMC member's twitter hack", 0);
    assert!(matches!(
        result,
        NewsRetreatTransition::Promoted {
            to: NewsRetreatState::Critical,
            ..
        }
    ));
}

#[test]
fn lower_class_in_higher_state_is_suppressed() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    sm.on_headline("Critical hack just landed", 1000);
    // While in Critical, a fresh High headline does NOT
    // demote — it's a no-op suppression.
    let result = sm.on_headline("FOMC speaks at 2pm", 2000);
    assert_eq!(
        result,
        NewsRetreatTransition::Suppressed {
            class: NewsClass::High,
            current: NewsRetreatState::Critical,
        }
    );
    assert_eq!(sm.current_state(2000), NewsRetreatState::Critical);
}

#[test]
fn refresh_resets_cooldown_clock() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    sm.on_headline("First hack alert", 1000);
    // Half the cooldown elapses.
    let halfway = 1000 + 15 * 60_000;
    // Fresh same-class headline at halfway → Refreshed.
    let result = sm.on_headline("Second hack alert", halfway);
    assert_eq!(
        result,
        NewsRetreatTransition::Refreshed(NewsRetreatState::Critical)
    );
    // 30 min after halfway → state still Critical
    // because the timer reset.
    let later = halfway + 29 * 60_000;
    assert_eq!(sm.current_state(later), NewsRetreatState::Critical);
}

#[test]
fn cooldown_expiry_returns_to_normal() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    sm.on_headline("Critical hack now", 1000);
    assert_eq!(sm.current_state(1000), NewsRetreatState::Critical);
    // 30 min + 1 ms later → cooldown expired.
    let after = 1000 + 30 * 60_000 + 1;
    assert_eq!(sm.current_state(after), NewsRetreatState::Normal);
    assert_eq!(sm.current_multiplier(after), Decimal::ONE);
    assert!(!sm.should_stop_new_orders(after));
}

#[test]
fn high_state_uses_5min_cooldown() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    sm.on_headline("FOMC press conference", 0);
    assert_eq!(sm.current_state(0), NewsRetreatState::High);
    // 5 min - 1 ms → still High.
    assert_eq!(sm.current_state(5 * 60_000 - 1), NewsRetreatState::High);
    // 5 min exactly → Normal.
    assert_eq!(sm.current_state(5 * 60_000), NewsRetreatState::Normal);
}

#[test]
fn low_state_zero_cooldown_reverts_immediately() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    sm.on_headline("New listing announcement", 1000);
    // low_cooldown_ms is 0, so the very next read at the
    // SAME time-instant already expires the state.
    assert_eq!(sm.current_state(1000), NewsRetreatState::Normal);
}

#[test]
fn force_clear_overrides_active_state() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    sm.on_headline("Major hack", 1000);
    assert_eq!(sm.current_state(1000), NewsRetreatState::Critical);
    sm.force_clear();
    assert_eq!(sm.current_state(2000), NewsRetreatState::Normal);
}

// ---------------- Stage-2 regex tests ----------------

/// Word-boundary anchor: `\bhack\b` should match
/// "exchange hack reported" but NOT "hackathon news".
/// Stage-1 substring matcher would false-positive on
/// "hackathon"; stage-2 fixes this.
#[test]
fn word_boundary_excludes_hackathon() {
    let config = NewsRetreatConfig {
        critical_keywords: vec![r"\bhack\b".to_string()],
        high_keywords: vec![],
        low_keywords: vec![],
        ..fixture_config()
    };
    let mut sm = NewsRetreatStateMachine::new(config).unwrap();
    let r1 = sm.on_headline("Major exchange hack reported", 1000);
    assert!(matches!(
        r1,
        NewsRetreatTransition::Promoted {
            to: NewsRetreatState::Critical,
            ..
        }
    ));
    sm.force_clear();
    let r2 = sm.on_headline("ETHGlobal hackathon news roundup", 2000);
    assert_eq!(r2, NewsRetreatTransition::NoMatch);
}

/// Alternation: a single pattern can cover multiple
/// literal keywords via `|`.
#[test]
fn alternation_pattern_matches_any_branch() {
    let config = NewsRetreatConfig {
        critical_keywords: vec![r"SEC|fraud|hack".to_string()],
        high_keywords: vec![],
        low_keywords: vec![],
        ..fixture_config()
    };
    let mut sm = NewsRetreatStateMachine::new(config).unwrap();
    for headline in [
        "SEC opens probe into major exchange",
        "fraud allegations surface",
        "another hack reported",
    ] {
        sm.force_clear();
        let r = sm.on_headline(headline, 0);
        assert!(
            matches!(
                r,
                NewsRetreatTransition::Promoted {
                    to: NewsRetreatState::Critical,
                    ..
                }
            ),
            "headline {headline:?} should promote to Critical, got {r:?}"
        );
    }
}

/// Wildcard: `crypto.*hack` matches "crypto exchange
/// hack" (text between the two literals is arbitrary).
#[test]
fn wildcard_pattern_matches_across_words() {
    let config = NewsRetreatConfig {
        critical_keywords: vec![r"crypto.*hack".to_string()],
        high_keywords: vec![],
        low_keywords: vec![],
        ..fixture_config()
    };
    let mut sm = NewsRetreatStateMachine::new(config).unwrap();
    let r = sm.on_headline("crypto exchange hack confirmed", 0);
    assert!(matches!(
        r,
        NewsRetreatTransition::Promoted {
            to: NewsRetreatState::Critical,
            ..
        }
    ));
    sm.force_clear();
    assert_eq!(
        sm.on_headline("exchange hack confirmed", 0),
        NewsRetreatTransition::NoMatch
    );
}

/// Malformed regex must surface as an error from `new`
/// rather than panicking or silently dropping the
/// pattern.
#[test]
fn malformed_pattern_returns_error_from_new() {
    let config = NewsRetreatConfig {
        critical_keywords: vec!["[unclosed".to_string()],
        ..fixture_config()
    };
    let result = NewsRetreatStateMachine::new(config);
    assert!(result.is_err(), "malformed pattern should return Err");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("critical_keywords"),
        "error should mention list name, got: {err}"
    );
}

/// Case-insensitivity is preserved via the baked-in
/// `(?i)` prefix — a mixed-case pattern still matches
/// headlines in any case.
#[test]
fn regex_case_insensitive_by_default() {
    let config = NewsRetreatConfig {
        critical_keywords: vec!["HACK".to_string()],
        high_keywords: vec![],
        low_keywords: vec![],
        ..fixture_config()
    };
    let mut sm = NewsRetreatStateMachine::new(config).unwrap();
    let r = sm.on_headline("small-hack-reported overnight", 0);
    assert!(matches!(
        r,
        NewsRetreatTransition::Promoted {
            to: NewsRetreatState::Critical,
            ..
        }
    ));
}

/// Legacy substring keywords are still valid regex and
/// behave unchanged — operators who upgrade see no
/// behaviour change on their v1 config.
#[test]
fn legacy_substring_keywords_still_work() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    let r = sm.on_headline("Major exchange hack reported", 1000);
    assert!(matches!(
        r,
        NewsRetreatTransition::Promoted {
            to: NewsRetreatState::Critical,
            ..
        }
    ));
}

#[test]
fn promotion_ladder_low_to_high_to_critical() {
    let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
    sm.on_headline("Listing news", 1000);
    // Low has 0 cooldown so we re-enter via classify on
    // each call — but at the exact same moment the state
    // is Normal (cooldown=0 expires instantly). For the
    // promotion ladder test, use non-zero cooldowns or
    // call current_state at the same instant.
    let _ = sm.current_state(1000);
    // Promote to High at a fresh timestamp.
    let r1 = sm.on_headline("FOMC at 2pm", 2000);
    assert!(matches!(
        r1,
        NewsRetreatTransition::Promoted {
            to: NewsRetreatState::High,
            ..
        }
    ));
    let r2 = sm.on_headline("Big hack", 3000);
    assert!(matches!(
        r2,
        NewsRetreatTransition::Promoted {
            from: NewsRetreatState::High,
            to: NewsRetreatState::Critical,
        }
    ));
}

// ── Poisson jump intensity tests ────────────────────────

#[test]
fn jump_baseline_multiplier_is_one() {
    let nj = NewsJumpIntensity::new(NewsJumpConfig::default());
    assert_eq!(nj.multiplier_at(0), dec!(1));
}

#[test]
fn jump_critical_event_raises_multiplier() {
    let mut nj = NewsJumpIntensity::new(NewsJumpConfig::default());
    let mult = nj.on_event(NewsClass::Critical, 0);
    assert!(
        mult > dec!(1),
        "critical event should raise multiplier, got {}",
        mult
    );
}

#[test]
fn jump_multiplier_decays_over_time() {
    let mut nj = NewsJumpIntensity::new(NewsJumpConfig::default());
    nj.on_event(NewsClass::Critical, 0);
    let mult_soon = nj.multiplier_at(1000); // 1 second later
    let mult_later = nj.multiplier_at(600_000); // 10 minutes later
    assert!(
        mult_soon > mult_later,
        "multiplier should decay: soon={} > later={}",
        mult_soon,
        mult_later
    );
}

#[test]
fn jump_multiple_events_accumulate() {
    let mut nj = NewsJumpIntensity::new(NewsJumpConfig::default());
    nj.on_event(NewsClass::High, 0);
    let mult_one = nj.multiplier_at(100);
    nj.on_event(NewsClass::High, 100);
    let mult_two = nj.multiplier_at(200);
    assert!(
        mult_two > mult_one,
        "two events should produce higher multiplier: {} > {}",
        mult_two,
        mult_one
    );
}

#[test]
fn jump_saturates_at_max_mult() {
    let cfg = NewsJumpConfig {
        lambda_sat: dec!(1),
        max_mult: dec!(3),
        weight_critical: dec!(10),
        ..Default::default()
    };
    let mut nj = NewsJumpIntensity::new(cfg);
    // Flood with critical events to saturate.
    for i in 0..20 {
        nj.on_event(NewsClass::Critical, i * 100);
    }
    let mult = nj.multiplier_at(2000);
    assert!(
        mult <= dec!(3),
        "multiplier should be capped at max_mult=3, got {}",
        mult
    );
}

#[test]
fn jump_is_critical_threshold() {
    let mut nj = NewsJumpIntensity::new(NewsJumpConfig {
        lambda_sat: dec!(5),
        weight_critical: dec!(5),
        ..Default::default()
    });
    assert!(!nj.is_critical(0));
    nj.on_event(NewsClass::Critical, 0);
    // intensity = 5, 80% of sat = 4 → 5 > 4 → critical
    assert!(nj.is_critical(0));
}

#[test]
fn jump_reset_clears_state() {
    let mut nj = NewsJumpIntensity::new(NewsJumpConfig::default());
    nj.on_event(NewsClass::Critical, 0);
    nj.reset();
    assert_eq!(nj.multiplier_at(0), dec!(1));
}

#[test]
fn jump_low_event_has_small_impact() {
    let mut nj = NewsJumpIntensity::new(NewsJumpConfig::default());
    let mult_low = nj.on_event(NewsClass::Low, 0);
    nj.reset();
    let mult_crit = nj.on_event(NewsClass::Critical, 0);
    assert!(
        mult_low < mult_crit,
        "low should have smaller impact: low={} < critical={}",
        mult_low,
        mult_crit
    );
}

// ── Property-based tests (Epic 13) ───────────────────────

use proptest::prelude::*;
use proptest::sample::select;

fn news_class_strat() -> impl Strategy<Value = NewsClass> {
    select(vec![NewsClass::Low, NewsClass::High, NewsClass::Critical])
}

fn news_retreat_cfg() -> NewsRetreatConfig {
    NewsRetreatConfig {
        critical_keywords: vec!["hack".into(), "fraud".into()],
        high_keywords: vec!["delist".into(), "halt".into()],
        low_keywords: vec!["rumor".into()],
        critical_cooldown_ms: 1_800_000,
        high_cooldown_ms: 300_000,
        low_cooldown_ms: 0,
        high_multiplier: dec!(2),
        critical_multiplier: dec!(3),
    }
}

proptest! {
    /// Every transition type respects its invariant:
    /// Promoted → strictly higher rank;
    /// Refreshed → same state;
    /// Suppressed → incoming class strictly below current.
    /// Evaluated against a small sequence so cooldown
    /// expiries + state auto-reversions are exercised too.
    #[test]
    fn transition_invariants_hold(
        headlines in proptest::collection::vec(news_class_strat(), 1..8),
    ) {
        let mut sm = NewsRetreatStateMachine::new(news_retreat_cfg())
            .expect("config compiles");
        for (i, c) in headlines.iter().enumerate() {
            let kw = match c {
                NewsClass::Low => "rumor",
                NewsClass::High => "delist",
                NewsClass::Critical => "hack",
            };
            let t = sm.on_headline(kw, i as i64);
            match t {
                NewsRetreatTransition::Promoted { from, to } => {
                    prop_assert!(to.rank() > from.rank(),
                        "Promoted with to<=from: {:?}→{:?}", from, to);
                }
                NewsRetreatTransition::Refreshed(_) => { /* state preserved */ }
                NewsRetreatTransition::Suppressed { class, current } => {
                    prop_assert!(
                        NewsRetreatState::from(class).rank() < current.rank(),
                        "Suppressed with class {:?} >= current {:?}", class, current);
                }
                NewsRetreatTransition::NoMatch => unreachable!(),
            }
        }
    }

    /// Multiplier is always >= 1.0 — the retreat only widens
    /// spreads, never tightens them. A value < 1 would narrow
    /// quotes under stress, the opposite of risk reduction.
    #[test]
    fn multiplier_is_never_below_one(
        headlines in proptest::collection::vec(news_class_strat(), 0..10),
    ) {
        let mut sm = NewsRetreatStateMachine::new(news_retreat_cfg())
            .expect("config compiles");
        for (i, c) in headlines.iter().enumerate() {
            let kw = match c {
                NewsClass::Low => "rumor",
                NewsClass::High => "delist",
                NewsClass::Critical => "hack",
            };
            sm.on_headline(kw, i as i64);
        }
        let mult = sm.current_multiplier(1000);
        prop_assert!(mult >= dec!(1),
            "multiplier {} < 1 under headline mix {:?}", mult, headlines);
    }

    /// Force-clear always returns to Normal — operator
    /// override must be unconditional.
    #[test]
    fn force_clear_always_returns_to_normal(
        class in news_class_strat(),
    ) {
        let mut sm = NewsRetreatStateMachine::new(news_retreat_cfg())
            .expect("config compiles");
        let kw = match class {
            NewsClass::Low => "rumor",
            NewsClass::High => "delist",
            NewsClass::Critical => "hack",
        };
        sm.on_headline(kw, 0);
        sm.force_clear();
        prop_assert_eq!(sm.current_state(1000), NewsRetreatState::Normal);
        prop_assert_eq!(sm.current_multiplier(1000), dec!(1));
    }

    /// After full cooldown elapsed the state returns to
    /// Normal. Critical cooldown is the longest — if the
    /// timer logic is wrong the state sticks in Critical
    /// forever.
    #[test]
    fn cooldown_expiry_returns_to_normal_prop(
        class in news_class_strat(),
    ) {
        let mut sm = NewsRetreatStateMachine::new(news_retreat_cfg())
            .expect("config compiles");
        let kw = match class {
            NewsClass::Low => "rumor",
            NewsClass::High => "delist",
            NewsClass::Critical => "hack",
        };
        sm.on_headline(kw, 0);
        // Jump past the critical cooldown (longest of the
        // three). Any lower-class state has expired too.
        prop_assert_eq!(sm.current_state(2_000_000), NewsRetreatState::Normal);
    }
}
