use super::*;
use chrono::Utc;

fn trade(price: &str, qty: &str, side: Side) -> Trade {
    Trade {
        trade_id: 1,
        symbol: "BTCUSDT".into(),
        price: price.parse().unwrap(),
        qty: qty.parse().unwrap(),
        taker_side: side,
        timestamp: Utc::now(),
    }
}

#[test]
fn test_vpin_balanced_flow() {
    let mut vpin = VpinEstimator::new(dec!(1000), 10);
    // Equal buy and sell volume — should be low VPIN.
    for _ in 0..50 {
        vpin.on_trade(&trade("100", "5", Side::Buy));
        vpin.on_trade(&trade("100", "5", Side::Sell));
    }
    let v = vpin.vpin().unwrap();
    assert!(v < dec!(0.1), "balanced flow should have low VPIN, got {v}");
}

#[test]
fn test_vpin_toxic_flow() {
    let mut vpin = VpinEstimator::new(dec!(1000), 10);
    // All buy volume — completely toxic.
    for _ in 0..100 {
        vpin.on_trade(&trade("100", "5", Side::Buy));
    }
    let v = vpin.vpin().unwrap();
    assert!(
        v > dec!(0.8),
        "one-sided flow should have high VPIN, got {v}"
    );
}

#[test]
fn test_kyle_lambda() {
    let mut kl = KyleLambda::new(50);
    // Simulate: price goes up when buy volume is positive.
    for i in 0..30 {
        let signed_vol = if i % 2 == 0 { dec!(100) } else { dec!(-100) };
        let dp = signed_vol * dec!(0.001); // Lambda should be ~0.001.
        kl.update(dp, signed_vol);
    }
    let lambda = kl.lambda().unwrap();
    assert!(lambda > dec!(0), "lambda should be positive");
}

// ---------------------------------------------------------------
// Epic D sub-component #3 — BVC classifier + VPIN on_bvc_bar
// ---------------------------------------------------------------

fn approx(a: Decimal, b: Decimal, eps: Decimal) -> bool {
    (a - b).abs() < eps
}

#[test]
fn bvc_warmup_returns_none() {
    let mut b = BvcClassifier::new(dec!(0.25), 50);
    // Fewer than 10 observations → None.
    for i in 0..5 {
        let res = b.classify(Decimal::from(i), dec!(100));
        assert!(res.is_none(), "warmup i={i}");
    }
}

#[test]
fn bvc_zero_variance_window_returns_none() {
    let mut b = BvcClassifier::new(dec!(0.25), 50);
    // All bars identical → zero std → None.
    for _ in 0..15 {
        let res = b.classify(dec!(1), dec!(100));
        assert!(res.is_none());
    }
}

#[test]
fn bvc_positive_price_change_classifies_majority_buy() {
    let mut b = BvcClassifier::new(dec!(0.25), 50);
    // Warm up the window with mean-zero dp.
    for i in 0..20 {
        let dp = if i % 2 == 0 { dec!(-1) } else { dec!(1) };
        let _ = b.classify(dp, dec!(100));
    }
    // Now feed a strongly positive dp — should land in
    // the right tail of the Student-t, producing
    // majority buy.
    let (buy, sell) = b.classify(dec!(5), dec!(100)).expect("warmup done");
    assert!(buy > sell, "buy={buy} sell={sell}");
    assert!(approx(buy + sell, dec!(100), dec!(0.0001)));
}

#[test]
fn bvc_negative_price_change_classifies_majority_sell() {
    let mut b = BvcClassifier::new(dec!(0.25), 50);
    for i in 0..20 {
        let dp = if i % 2 == 0 { dec!(-1) } else { dec!(1) };
        let _ = b.classify(dp, dec!(100));
    }
    let (buy, sell) = b.classify(dec!(-5), dec!(100)).expect("warmup done");
    assert!(sell > buy, "buy={buy} sell={sell}");
    assert!(approx(buy + sell, dec!(100), dec!(0.0001)));
}

#[test]
fn bvc_total_volume_invariant() {
    let mut b = BvcClassifier::new(dec!(0.25), 50);
    for i in 0..15 {
        let dp = Decimal::from(i as i64 % 5 - 2);
        if let Some((buy, sell)) = b.classify(dp, dec!(200)) {
            assert!(
                approx(buy + sell, dec!(200), dec!(0.0001)),
                "buy+sell != 200 (buy={buy}, sell={sell})"
            );
        }
    }
}

#[test]
fn student_t_cdf_at_zero_is_half_for_any_nu() {
    assert!(approx(
        student_t_cdf(dec!(0), dec!(0.25)),
        dec!(0.5),
        dec!(0.0001)
    ));
    assert!(approx(
        student_t_cdf(dec!(0), dec!(3)),
        dec!(0.5),
        dec!(0.0001)
    ));
    assert!(approx(
        student_t_cdf(dec!(0), dec!(30)),
        dec!(0.5),
        dec!(0.0001)
    ));
}

#[test]
fn student_t_cdf_saturates_in_tails() {
    // Very large positive z → ~1, very large negative → ~0.
    let high = student_t_cdf(dec!(1000), dec!(5));
    let low = student_t_cdf(dec!(-1000), dec!(5));
    assert!(high > dec!(0.99));
    assert!(low < dec!(0.01));
}

#[test]
fn student_t_cdf_large_nu_matches_normal() {
    // At ν = 30+, the Student-t CDF collapses to Normal.
    // CDF(1.0) for Normal ≈ 0.8413.
    let v = student_t_cdf(dec!(1), dec!(50));
    assert!(
        approx(v, dec!(0.8413), dec!(0.001)),
        "Φ(1) ≈ 0.8413, got {v}"
    );
}

#[test]
fn on_bvc_bar_produces_same_vpin_shape_as_on_trade() {
    // Feed two VpinEstimators the same underlying
    // buy/sell split — one via the tick-rule path, one
    // via the BVC bar path. VPIN outputs should be
    // byte-identical.
    let mut vpin_tick = VpinEstimator::new(dec!(1000), 10);
    let mut vpin_bvc = VpinEstimator::new(dec!(1000), 10);

    for _ in 0..100 {
        // 60 qty @ 100 buy + 40 qty @ 100 sell per iteration
        // → 60/100 buy-share.
        vpin_tick.on_trade(&trade("100", "6", Side::Buy));
        vpin_tick.on_trade(&trade("100", "4", Side::Sell));

        // Same split via BVC path: 600 quote buy + 400 quote sell.
        vpin_bvc.on_bvc_bar(dec!(600), dec!(400));
    }

    let v_tick = vpin_tick.vpin();
    let v_bvc = vpin_bvc.vpin();
    assert_eq!(v_tick, v_bvc);
}

#[test]
fn bvc_classifier_rolling_mean_and_std_accessors() {
    let mut b = BvcClassifier::new(dec!(0.25), 10);
    for i in 1..=5 {
        b.classify(Decimal::from(i), dec!(100));
    }
    let mean = b.rolling_mean().unwrap();
    // Mean of 1..=5 is 3.
    assert_eq!(mean, dec!(3));
    let std = b.rolling_std().unwrap();
    assert!(std > Decimal::ZERO);
}

#[test]
#[should_panic(expected = "window_size must be >= 2")]
fn bvc_panics_on_tiny_window() {
    BvcClassifier::new(dec!(0.25), 1);
}

#[test]
#[should_panic(expected = "nu must be positive")]
fn bvc_panics_on_nonpositive_nu() {
    BvcClassifier::new(Decimal::ZERO, 10);
}

// ---------------------------------------------------------
// Epic D stage-2 — BvcBarAggregator tests
// ---------------------------------------------------------

const BAR_NS: i64 = 1_000_000_000;

#[test]
fn aggregator_first_trade_seeds_bar_without_emitting() {
    let mut agg = BvcBarAggregator::new(1);
    let out = agg.push(100, dec!(100), dec!(1));
    assert!(out.is_none());
}

#[test]
fn aggregator_same_bar_folds_trades() {
    let mut agg = BvcBarAggregator::new(1);
    agg.push(100, dec!(100), dec!(1));
    agg.push(200, dec!(101), dec!(2));
    // Still inside the 1s bar — no emission, last_px tracks
    // rolling latest.
    let out = agg.push(500, dec!(102), dec!(3));
    assert!(out.is_none());
}

#[test]
fn aggregator_emits_on_boundary_crossing() {
    let mut agg = BvcBarAggregator::new(1);
    agg.push(0, dec!(100), dec!(1));
    agg.push(500_000_000, dec!(102), dec!(2));
    // Crosses 1s boundary.
    let emitted = agg.push(BAR_NS, dec!(105), dec!(1));
    assert_eq!(emitted, Some((dec!(2), dec!(100) + dec!(204)))); // dp = 102-100, vol = 100+204
}

#[test]
fn aggregator_flush_if_due_emits_when_bar_closed_and_quiet() {
    let mut agg = BvcBarAggregator::new(1);
    agg.push(0, dec!(100), dec!(1));
    agg.push(100_000_000, dec!(100), dec!(2));
    // Well past boundary but no trade to carry it over —
    // flush_if_due surfaces the closed bar.
    let emitted = agg.flush_if_due(2 * BAR_NS);
    assert_eq!(emitted, Some((dec!(0), dec!(300))));
    // Subsequent flush with no new push is a no-op.
    assert_eq!(agg.flush_if_due(10 * BAR_NS), None);
}

#[test]
fn aggregator_flush_before_boundary_is_noop() {
    let mut agg = BvcBarAggregator::new(1);
    agg.push(0, dec!(100), dec!(1));
    assert_eq!(agg.flush_if_due(BAR_NS / 2), None);
}

#[test]
fn aggregator_flush_without_any_trade_is_noop() {
    let mut agg = BvcBarAggregator::new(1);
    assert_eq!(agg.flush_if_due(BAR_NS * 10), None);
}

#[test]
fn aggregator_bar_secs_zero_gets_clamped_to_one() {
    let agg = BvcBarAggregator::new(0);
    assert_eq!(agg.bar_len_ns(), BAR_NS);
}

// ---------------------------------------------------------
// Epic D stage-3 — per-side adverse-selection bps
// ---------------------------------------------------------

fn seed_completed_fill(
    tracker: &mut AdverseSelectionTracker,
    side: Side,
    fill_price: Decimal,
    mid_at_fill: Decimal,
    mid_after: Decimal,
) {
    tracker.fills.push_back(FillOutcome {
        fill_price,
        side,
        mid_at_fill,
        mid_after: Some(mid_after),
        timestamp_ms: 0,
    });
}

#[test]
fn per_side_bps_returns_none_below_threshold() {
    // Fewer than 5 completed fills on a side → None.
    let mut t = AdverseSelectionTracker::new(50);
    for _ in 0..3 {
        seed_completed_fill(&mut t, Side::Buy, dec!(100), dec!(100), dec!(99));
    }
    assert!(t.adverse_selection_bps_bid().is_none());
    assert!(t.adverse_selection_bps_ask().is_none());
    // Symmetric path also requires 5 — only 3 total.
    assert!(t.adverse_selection_bps().is_none());
}

#[test]
fn per_side_bps_filters_buy_only() {
    let mut t = AdverseSelectionTracker::new(50);
    // 6 buy fills — adverse 100 bps each (bought at 100, mid dropped to 99).
    for _ in 0..6 {
        seed_completed_fill(&mut t, Side::Buy, dec!(100), dec!(100), dec!(99));
    }
    // 5 sell fills — neutral (sold at 100, mid stayed at 100).
    for _ in 0..5 {
        seed_completed_fill(&mut t, Side::Sell, dec!(100), dec!(100), dec!(100));
    }
    // Bid path sees only the buys → ~+100 bps adverse.
    let bid = t.adverse_selection_bps_bid().unwrap();
    assert!((bid - dec!(100)).abs() < dec!(0.001));
    // Ask path sees only the sells → 0 bps.
    let ask = t.adverse_selection_bps_ask().unwrap();
    assert_eq!(ask, dec!(0));
}

#[test]
fn per_side_bps_filters_sell_only() {
    let mut t = AdverseSelectionTracker::new(50);
    // 5 sell fills — adverse 50 bps each (sold at 100, mid rose to 100.5).
    for _ in 0..5 {
        seed_completed_fill(&mut t, Side::Sell, dec!(100), dec!(100), dec!(100.5));
    }
    // 6 buy fills — neutral.
    for _ in 0..6 {
        seed_completed_fill(&mut t, Side::Buy, dec!(100), dec!(100), dec!(100));
    }
    let ask = t.adverse_selection_bps_ask().unwrap();
    assert!((ask - dec!(50)).abs() < dec!(0.001));
    let bid = t.adverse_selection_bps_bid().unwrap();
    assert_eq!(bid, dec!(0));
}

#[test]
fn per_side_average_matches_symmetric_when_one_sided() {
    // When all fills are on one side, that side's per-side
    // average equals the symmetric average.
    let mut t = AdverseSelectionTracker::new(50);
    for _ in 0..7 {
        seed_completed_fill(&mut t, Side::Buy, dec!(50_000), dec!(50_000), dec!(49_995));
    }
    let symmetric = t.adverse_selection_bps().unwrap();
    let bid = t.adverse_selection_bps_bid().unwrap();
    assert_eq!(symmetric, bid);
}

// ── Property-based tests (Epic 12) ───────────────────────

use proptest::prelude::*;
use proptest::sample::select;

fn side_strat() -> impl Strategy<Value = Side> {
    select(vec![Side::Buy, Side::Sell])
}
prop_compose! {
    fn price_strat_tox()(cents in 100i64..100_000_000i64) -> Decimal {
        Decimal::new(cents, 2)
    }
}
prop_compose! {
    fn qty_strat_tox()(units in 1i64..1_000_000i64) -> Decimal {
        Decimal::new(units, 4)
    }
}

fn make_trade(side: Side, price: Decimal, qty: Decimal) -> Trade {
    Trade {
        trade_id: 1,
        symbol: "TEST".into(),
        price,
        qty,
        taker_side: side,
        timestamp: chrono::Utc::now(),
    }
}

// ── VPIN ─────────────────────────────────────────────────

proptest! {
    // Heavier than the default — proptest runs 256 cases of
    // up-to-60-trade sequences. 32 cases keep CI under
    // 10 seconds while still covering enough random shapes
    // to exercise the overflow math.
    #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]

    /// VPIN is bounded in [0, 1] for any trade sequence. A
    /// value > 1 would be mathematically impossible (the
    /// imbalance can't exceed the volume). This property
    /// caught a real bug in the bucket-overflow path where
    /// imbalance was computed against current_total_vol
    /// instead of bucket_size, letting VPIN exceed 1 on
    /// one-sided flow.
    #[test]
    fn vpin_is_bounded_in_0_1(
        trades in proptest::collection::vec(
            (side_strat(), price_strat_tox(), qty_strat_tox()),
            10..60,
        ),
    ) {
        let mut v = VpinEstimator::new(dec!(10_000), 20);
        for (side, p, q) in &trades {
            v.on_trade(&make_trade(*side, *p, *q));
        }
        if let Some(vpin) = v.vpin() {
            prop_assert!(vpin >= dec!(0), "VPIN {} < 0", vpin);
            prop_assert!(vpin <= dec!(1), "VPIN {} > 1", vpin);
        }
    }

    /// A completely one-sided flow (all buys) saturates
    /// toward VPIN = 1. Verifies the imbalance is picked
    /// up as toxic.
    #[test]
    fn one_sided_flow_has_high_vpin(
        qty in qty_strat_tox(),
        price in price_strat_tox(),
        n_trades in 30usize..60usize,
    ) {
        // Bucket size = one-trade notional so each trade
        // finalises its own bucket.
        let bucket_size = price * qty;
        let mut v = VpinEstimator::new(bucket_size, 20);
        for _ in 0..n_trades {
            v.on_trade(&make_trade(Side::Buy, price, qty));
        }
        if let Some(vpin) = v.vpin() {
            prop_assert!(vpin >= dec!(0.5),
                "all-buy flow produced VPIN {} < 0.5", vpin);
        }
    }
}

// ── Kyle Lambda ──────────────────────────────────────────

proptest! {
    /// Fewer than 10 observations always returns None — the
    /// estimator refuses to produce λ without enough samples
    /// for a meaningful regression.
    #[test]
    fn kyle_requires_10_samples(n in 0usize..10usize) {
        let mut kl = KyleLambda::new(100);
        for i in 0..n {
            kl.update(Decimal::new(i as i64, 2), Decimal::new(i as i64, 0));
        }
        prop_assert!(kl.lambda().is_none());
    }

    /// Zero OFI variance always returns None — the linear
    /// regression has no slope to compute. Catches a
    /// regression where divide-by-zero would surface as a
    /// spurious λ = 0.
    #[test]
    fn zero_variance_ofi_returns_none(
        n in 10usize..50usize,
        constant_ofi in -1000i64..1000i64,
        price_changes in proptest::collection::vec(
            -100_000i64..100_000i64,
            10..50,
        ),
    ) {
        let mut kl = KyleLambda::new(100);
        let ofi = Decimal::from(constant_ofi);
        for dp in price_changes.iter().take(n).copied() {
            kl.update(Decimal::new(dp, 4), ofi);
        }
        prop_assert!(kl.lambda().is_none(),
            "constant OFI should yield no λ");
    }

    /// With a perfect linear relationship ΔP = α·OFI (α > 0),
    /// the estimator recovers λ ≈ α. Verifies the covariance
    /// / variance arithmetic is correctly assembled.
    #[test]
    fn kyle_recovers_linear_coefficient(
        alpha_raw in 1i64..100i64,
        ofis in proptest::collection::vec(-100i64..100i64, 10..30),
    ) {
        // Require OFI variance > 0 — otherwise lambda() returns None
        // (handled in a separate property above).
        prop_assume!(ofis.iter().collect::<std::collections::HashSet<_>>().len() >= 2);
        let alpha = Decimal::new(alpha_raw, 4);  // 0.0001 .. 0.0100
        let mut kl = KyleLambda::new(100);
        for ofi_raw in &ofis {
            let ofi = Decimal::from(*ofi_raw);
            let dp = alpha * ofi;
            kl.update(dp, ofi);
        }
        let lambda = kl.lambda().expect("lambda should be defined for varied OFI");
        let diff = (lambda - alpha).abs();
        prop_assert!(diff < dec!(0.0001),
            "recovered λ={} far from α={} (diff={})", lambda, alpha, diff);
    }

    /// Window is bounded — feeding more than window_size
    /// observations does not overflow or break the estimator.
    /// Returns a finite Decimal when window is full.
    #[test]
    fn window_bounded(
        n in 20usize..300usize,
    ) {
        let mut kl = KyleLambda::new(50);
        for i in 0..n {
            let ofi = Decimal::from(((i as i64) % 7) - 3);  // -3..3 cycle
            let dp = ofi * Decimal::new(2, 3);  // 0.002·ofi
            kl.update(dp, ofi);
        }
        // Window keeps only the last 50.
        let lambda = kl.lambda();
        prop_assert!(lambda.is_some(), "full window should give λ");
    }
}

// ── AdverseSelection ─────────────────────────────────────

proptest! {
    /// Fewer than 5 completed fills on a side always returns
    /// None — the estimator refuses to report until the
    /// window has enough samples for a meaningful average.
    #[test]
    fn per_side_requires_5_fills(
        n in 0usize..5usize,
        side in side_strat(),
        fill_price in price_strat_tox(),
    ) {
        let mut t = AdverseSelectionTracker::new(50);
        for _ in 0..n {
            seed_completed_fill(&mut t, side, fill_price, fill_price, fill_price);
        }
        prop_assert!(t.adverse_selection_bps_for_side(side).is_none());
    }

    /// Zero adverse selection — when mid_after equals the
    /// fill's benchmark, bps should be zero. Exact equality
    /// after averaging — catches rounding drift.
    #[test]
    fn no_price_move_yields_zero_bps(
        side in side_strat(),
        price in price_strat_tox(),
        n in 5usize..20usize,
    ) {
        let mut t = AdverseSelectionTracker::new(50);
        for _ in 0..n {
            seed_completed_fill(&mut t, side, price, price, price);
        }
        let bps = t.adverse_selection_bps_for_side(side).unwrap();
        prop_assert_eq!(bps, dec!(0));
    }

    /// The symmetric average is bounded by the per-side
    /// averages — i.e., the overall figure cannot exceed the
    /// worse of the two sides when both are populated.
    /// Catches weighting errors across the bid/ask split.
    #[test]
    fn symmetric_between_per_side(
        price in price_strat_tox(),
        n in 5usize..15usize,
    ) {
        let mut t = AdverseSelectionTracker::new(50);
        for _ in 0..n {
            seed_completed_fill(&mut t, Side::Buy, price, price, price - dec!(1));
            seed_completed_fill(&mut t, Side::Sell, price, price, price + dec!(2));
        }
        let bid = t.adverse_selection_bps_bid().unwrap();
        let ask = t.adverse_selection_bps_ask().unwrap();
        let sym = t.adverse_selection_bps().unwrap();
        let lo = bid.min(ask);
        let hi = bid.max(ask);
        prop_assert!(sym >= lo - dec!(0.01) && sym <= hi + dec!(0.01),
            "symmetric {} outside [{}, {}]", sym, lo, hi);
    }
}
