use super::*;

// ---- regimes_agree partition ----

#[test]
fn regimes_agree_on_identity() {
    for r in [
        MarketRegime::Quiet,
        MarketRegime::Trending,
        MarketRegime::Volatile,
        MarketRegime::MeanReverting,
    ] {
        assert!(regimes_agree(r, r));
    }
}

#[test]
fn regimes_agree_tolerates_quiet_pairings() {
    // Quiet is compatible with every other label in our
    // "disagreement = downgrade" policy.
    use MarketRegime::*;
    assert!(regimes_agree(Volatile, Quiet));
    assert!(regimes_agree(Trending, Quiet));
    assert!(regimes_agree(MeanReverting, Quiet));
}

#[test]
fn regimes_disagree_on_trending_vs_mean_reverting() {
    // Trending vs MeanReverting is the sharpest
    // contradiction — heuristic says the series is going
    // one way, Hurst says the other. Must reject.
    use MarketRegime::*;
    assert!(!regimes_agree(Trending, MeanReverting));
    assert!(!regimes_agree(MeanReverting, Trending));
}

#[test]
fn regimes_disagree_on_volatile_vs_mean_reverting() {
    use MarketRegime::*;
    assert!(!regimes_agree(Volatile, MeanReverting));
    assert!(!regimes_agree(MeanReverting, Volatile));
}

// ---- Hurst-driven downgrade in RegimeDetector::detect ----

// ---- hurst_label direct unit tests ----

fn push(det: &mut RegimeDetector, r: Decimal, n: usize) {
    for _ in 0..n {
        det.update(r);
    }
}

#[test]
fn hurst_label_none_on_tiny_window() {
    let mut det = RegimeDetector::new(200);
    // Fewer than 20 samples — the Hurst helper returns
    // `None` by construction (the inner `hurst_exponent`
    // guard rejects short series).
    for i in 0..15 {
        det.update(Decimal::from(i) / dec!(10000));
    }
    assert!(det.hurst_label_for_test().is_none());
}

#[test]
fn hurst_label_trending_on_monotonic_returns() {
    // A monotonically increasing return series yields
    // `H ≈ 1` under R/S analysis — the label must be
    // Trending when the 95 % CI's lower bound sits
    // above 0.55.
    let mut det = RegimeDetector::new(500);
    for i in 0..500 {
        det.update(Decimal::from(i));
    }
    let label = det.hurst_label_for_test();
    assert!(
        matches!(label, Some(MarketRegime::Trending)),
        "expected Trending on monotone returns, got {label:?}"
    );
}

#[test]
fn hurst_label_iid_white_noise_is_usually_quiet_or_none() {
    // iid ±1 via xorshift popcount parity. Hurst lands
    // near 0.5 and the CI either tightly brackets 0.5
    // (→ Quiet) or is wide enough that the classifier
    // returns `None`. Either is acceptable — the contract
    // is "do NOT label iid white noise as Trending or
    // MeanReverting".
    let mut det = RegimeDetector::new(2000);
    let mut state: u64 = 0x1234_5678_9abc_def0;
    for _ in 0..2000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let parity = state.count_ones() & 1;
        det.update(if parity == 0 { dec!(-1) } else { dec!(1) });
    }
    let label = det.hurst_label_for_test();
    assert!(
        !matches!(
            label,
            Some(MarketRegime::Trending) | Some(MarketRegime::MeanReverting)
        ),
        "iid white noise should not classify as trending / mean-reverting, got {label:?}"
    );
}

/// End-to-end detect(): a mean-reverting alternating
/// stream with high variance. Heuristic sees MeanReverting
/// directly (autocorr < -0.1). Hurst on the same stream
/// either agrees → final is MeanReverting, or is noisy
/// and downgrades → final is Quiet. Either way the result
/// must NOT be Trending or Volatile — that's the safety
/// contract the Hurst cross-check enforces.
#[test]
fn detect_never_labels_mean_reverting_returns_as_trending() {
    let mut det = RegimeDetector::new(200);
    for i in 0..200 {
        let r = if i % 2 == 0 { dec!(0.03) } else { dec!(-0.03) };
        det.update(r);
    }
    let regime = det.regime();
    assert!(
        !matches!(regime, MarketRegime::Trending | MarketRegime::Volatile),
        "detector must not label an alternating stream as Trending/Volatile, got {regime:?}"
    );
}

/// Feed an obviously quiet series (all returns very
/// small). Heuristic lands in `Quiet` via the low-variance
/// branch. The detector must not promote this to a more
/// aggressive regime.
#[test]
fn quiet_returns_stay_quiet_after_hurst_check() {
    let mut det = RegimeDetector::new(200);
    push(&mut det, dec!(0.0000001), 200);
    assert_eq!(det.regime(), MarketRegime::Quiet);
}

/// Iid white-noise returns → heuristic likely lands on
/// Quiet or Volatile. Hurst on white noise sits near 0.5
/// with a narrow CI → label is `Quiet`. Agreement path —
/// no downgrade. Simply verifies the detector does not
/// break under the common "nothing to see" input.
#[test]
fn white_noise_returns_do_not_produce_panic() {
    let mut det = RegimeDetector::new(200);
    // Deterministic ±0.00001 iid via xorshift + popcount.
    let mut state: u64 = 0x5555_aaaa_1234_5678;
    for _ in 0..200 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let parity = state.count_ones() & 1;
        let r = if parity == 0 {
            dec!(-0.00001)
        } else {
            dec!(0.00001)
        };
        det.update(r);
    }
    // Just assert `regime()` returns something; the
    // specific label depends on the variance threshold.
    let _ = det.regime();
}

// ----- InventoryGammaPolicy tests -----

#[test]
fn policy_returns_min_mult_at_zero_state() {
    let p = InventoryGammaPolicy::new(dec!(0.1));
    // q = 0, time_remaining = 1 (start of session). Both
    // terms vanish → multiplier = 1, clamped at min_mult.
    assert_eq!(p.multiplier(Decimal::ZERO, Decimal::ONE), Decimal::ONE);
}

#[test]
fn policy_scales_up_with_inventory() {
    let p = InventoryGammaPolicy::new(dec!(0.1));
    let at_zero = p.multiplier(Decimal::ZERO, Decimal::ONE);
    let at_half = p.multiplier(dec!(0.05), Decimal::ONE);
    let at_max = p.multiplier(dec!(0.1), Decimal::ONE);
    assert!(at_half > at_zero, "inventory term must push mult up");
    assert!(at_max > at_half, "mult should be monotone in |q|");
}

#[test]
fn policy_scales_up_as_time_runs_out() {
    let p = InventoryGammaPolicy::new(dec!(0.1));
    let early = p.multiplier(Decimal::ZERO, Decimal::ONE);
    let late = p.multiplier(Decimal::ZERO, dec!(0.1));
    let end = p.multiplier(Decimal::ZERO, Decimal::ZERO);
    assert!(late > early, "time term must push mult up as t→0");
    assert!(end > late, "session close hits max time term");
}

#[test]
fn policy_clamps_to_max_mult() {
    // Custom policy with low cap so we can force clamping.
    let p = InventoryGammaPolicy {
        max_inventory: dec!(0.1),
        q_weight: dec!(10),
        q_exp: dec!(1),
        t_weight: dec!(10),
        t_exp: dec!(1),
        min_mult: dec!(1),
        max_mult: dec!(2),
    };
    let m = p.multiplier(dec!(0.1), Decimal::ZERO);
    assert_eq!(m, dec!(2), "must clamp at max_mult");
}

#[test]
fn policy_clamps_abs_inventory_above_max() {
    let p = InventoryGammaPolicy::new(dec!(0.1));
    let at_max = p.multiplier(dec!(0.1), Decimal::ONE);
    let above_max = p.multiplier(dec!(0.5), Decimal::ONE);
    assert_eq!(at_max, above_max, "q_norm must saturate at 1");
}

#[test]
fn policy_negative_inventory_uses_abs() {
    let p = InventoryGammaPolicy::new(dec!(0.1));
    let long = p.multiplier(dec!(0.05), Decimal::ONE);
    let short = p.multiplier(dec!(-0.05), Decimal::ONE);
    assert_eq!(long, short, "policy must be symmetric in q sign");
}

#[test]
fn autotune_applies_policy_via_effective_gamma_mult() {
    let mut t =
        AutoTuner::new(32).with_inventory_gamma_policy(InventoryGammaPolicy::new(dec!(0.1)));
    let base = t.effective_gamma_mult();
    t.update_policy_state(dec!(0.1), Decimal::ZERO);
    let loaded = t.effective_gamma_mult();
    assert!(
        loaded > base,
        "loaded/end-of-session state must widen γ above the unloaded baseline"
    );
}

#[test]
fn autotune_none_policy_leaves_gamma_untouched() {
    let mut t = AutoTuner::new(32);
    let before = t.effective_gamma_mult();
    t.update_policy_state(dec!(0.1), Decimal::ZERO);
    let after = t.effective_gamma_mult();
    assert_eq!(
        before, after,
        "no policy attached → state updates must be ignored"
    );
}

// ----- inventory_risk_penalty tests -----

#[test]
fn risk_penalty_zero_for_zero_inventory() {
    let r = inventory_risk_penalty(Decimal::ZERO, dec!(0.01), dec!(1));
    assert_eq!(r, Decimal::ZERO);
}

#[test]
fn risk_penalty_zero_for_non_positive_sigma_or_dt() {
    assert_eq!(
        inventory_risk_penalty(dec!(1), Decimal::ZERO, dec!(1)),
        Decimal::ZERO
    );
    assert_eq!(
        inventory_risk_penalty(dec!(1), dec!(0.01), Decimal::ZERO),
        Decimal::ZERO
    );
    assert_eq!(
        inventory_risk_penalty(dec!(1), dec!(-0.01), dec!(1)),
        Decimal::ZERO
    );
}

#[test]
fn risk_penalty_scales_linearly_with_absolute_inventory() {
    let a = inventory_risk_penalty(dec!(1), dec!(0.02), dec!(4));
    let b = inventory_risk_penalty(dec!(2), dec!(0.02), dec!(4));
    assert_eq!(b, a * dec!(2));
}

#[test]
fn risk_penalty_is_symmetric_in_inventory_sign() {
    let long = inventory_risk_penalty(dec!(1.5), dec!(0.02), dec!(4));
    let short = inventory_risk_penalty(dec!(-1.5), dec!(0.02), dec!(4));
    assert_eq!(long, short);
}

#[test]
fn risk_penalty_canonical_hand_computed_value() {
    // 0.5 * |2| * 0.02 * sqrt(4) = 0.5 * 2 * 0.02 * 2 = 0.04
    let r = inventory_risk_penalty(dec!(2), dec!(0.02), dec!(4));
    assert_eq!(r, dec!(0.04));
}

// ----- Market Resilience wiring tests -----

/// Without an MR reading the spread multiplier falls back
/// to the regime+toxicity product.
#[test]
fn effective_spread_mult_ignores_unset_market_resilience() {
    let t = AutoTuner::new(32);
    let before = t.effective_spread_mult();
    // No reading attached → unchanged.
    assert!(before > Decimal::ZERO);
}

/// A resilient MR score of 1.0 must leave the spread
/// multiplier unchanged (1 / 1 = 1).
#[test]
fn mr_of_one_is_a_noop_on_spread_mult() {
    let mut t = AutoTuner::new(32);
    let before = t.effective_spread_mult();
    t.set_market_resilience(Decimal::ONE);
    let after = t.effective_spread_mult();
    assert_eq!(before, after);
}

/// A depressed MR score must widen the effective spread
/// multiplier compared to the default (no reading).
#[test]
fn low_mr_widens_effective_spread_mult() {
    let mut t = AutoTuner::new(32);
    let base = t.effective_spread_mult();
    t.set_market_resilience(dec!(0.3));
    let after = t.effective_spread_mult();
    assert!(
        after > base,
        "MR=0.3 must widen the book: base={base}, after={after}"
    );
}

/// The spread multiplier is capped by the MR floor at 0.2 —
/// even an MR of 0 cannot push the widen factor past 5×.
#[test]
fn mr_floor_caps_the_widen_factor() {
    let mut t = AutoTuner::new(32);
    let base = t.effective_spread_mult();
    t.set_market_resilience(Decimal::ZERO);
    let after = t.effective_spread_mult();
    // base / 0.2 = base * 5.
    assert_eq!(after, base * dec!(5));
}

/// Out-of-range MR inputs are clamped into `[0, 1]` before
/// they influence the multiplier.
#[test]
fn out_of_range_mr_is_clamped() {
    let mut t = AutoTuner::new(32);
    let base = t.effective_spread_mult();
    t.set_market_resilience(dec!(-3));
    let after_low = t.effective_spread_mult();
    assert_eq!(after_low, base * dec!(5), "negative MR clamps to 0");
    t.set_market_resilience(dec!(5));
    let after_high = t.effective_spread_mult();
    assert_eq!(after_high, base, "MR above 1 clamps to 1");
}

/// `clear_market_resilience` removes the reading so the
/// baseline is restored.
#[test]
fn clearing_mr_restores_baseline() {
    let mut t = AutoTuner::new(32);
    let base = t.effective_spread_mult();
    t.set_market_resilience(dec!(0.3));
    t.clear_market_resilience();
    let after = t.effective_spread_mult();
    assert_eq!(after, base);
}

// ---- Epic F — defensive layer multipliers ----

#[test]
fn lead_lag_default_is_one_and_byte_identical_to_pre_epic_f() {
    let t = AutoTuner::new(32);
    assert_eq!(t.lead_lag_mult(), dec!(1));
    // Spread is the wave-1 baseline since both new
    // multipliers default to 1.0.
    let before = t.effective_spread_mult();
    let mut t2 = AutoTuner::new(32);
    t2.set_lead_lag_mult(dec!(1));
    t2.set_news_retreat_mult(dec!(1));
    assert_eq!(t2.effective_spread_mult(), before);
}

#[test]
fn lead_lag_mult_widens_effective_spread() {
    let mut t = AutoTuner::new(32);
    let base = t.effective_spread_mult();
    t.set_lead_lag_mult(dec!(2.5));
    assert_eq!(t.lead_lag_mult(), dec!(2.5));
    assert_eq!(t.effective_spread_mult(), base * dec!(2.5));
}

#[test]
fn lead_lag_mult_clamps_below_one() {
    // Defensive: a sub-1.0 multiplier would mean the
    // guard is *narrowing* the spread. Lead-lag should
    // only widen, never narrow — clamp at 1.0.
    let mut t = AutoTuner::new(32);
    t.set_lead_lag_mult(dec!(0.4));
    assert_eq!(t.lead_lag_mult(), dec!(1));
}

#[test]
fn news_retreat_mult_widens_effective_spread() {
    let mut t = AutoTuner::new(32);
    let base = t.effective_spread_mult();
    t.set_news_retreat_mult(dec!(3));
    assert_eq!(t.news_retreat_mult(), dec!(3));
    assert_eq!(t.effective_spread_mult(), base * dec!(3));
}

#[test]
fn news_retreat_mult_clamps_below_one() {
    let mut t = AutoTuner::new(32);
    t.set_news_retreat_mult(dec!(0.5));
    assert_eq!(t.news_retreat_mult(), dec!(1));
}

#[test]
fn lead_lag_and_news_retreat_compose_multiplicatively() {
    // Both signals firing at the same time produce a
    // product widening, not a max() — matches the
    // existing toxicity × MR composition shape.
    let mut t = AutoTuner::new(32);
    let base = t.effective_spread_mult();
    t.set_lead_lag_mult(dec!(2));
    t.set_news_retreat_mult(dec!(3));
    assert_eq!(t.effective_spread_mult(), base * dec!(6));
}

/// 22B-3 — RegimeDetector snapshot/restore preserves the
/// returns window + current_regime across a process boundary.
#[test]
fn regime_detector_snapshot_restore_round_trip() {
    let mut src = RegimeDetector::new(50);
    // Feed volatile returns to push the detector off Quiet.
    for i in 0..60 {
        let r = if i % 2 == 0 { dec!(0.01) } else { dec!(-0.01) };
        src.update(r);
    }
    let before_regime = src.regime();
    let before_samples = src.returns.len();
    let snap = src.snapshot_state();

    let mut dst = RegimeDetector::new(50);
    dst.restore_state(&snap).unwrap();
    assert_eq!(dst.regime(), before_regime);
    assert_eq!(dst.returns.len(), before_samples);
}

#[test]
fn regime_detector_restore_rejects_wrong_schema() {
    let mut d = RegimeDetector::new(10);
    let bogus = serde_json::json!({
        "schema_version": 99,
        "window": 10,
        "current_regime": "quiet",
        "returns": [],
    });
    assert!(d.restore_state(&bogus).is_err());
}

#[test]
fn regime_detector_restore_truncates_oversize_window() {
    // Source has 200 samples; destination has a 50-sample cap.
    let mut src = RegimeDetector::new(200);
    for i in 0..200 {
        src.update(Decimal::new(i as i64, 4));
    }
    let snap = src.snapshot_state();
    let mut dst = RegimeDetector::new(50);
    dst.restore_state(&snap).unwrap();
    assert_eq!(dst.returns.len(), 50);
}
