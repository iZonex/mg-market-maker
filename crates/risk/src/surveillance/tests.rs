use super::*;

fn ev_place(id: &str, sym: &str, side: Side, qty: Decimal, ts: DateTime<Utc>) -> SurveillanceEvent {
    SurveillanceEvent::OrderPlaced {
        order_id: id.into(),
        symbol: sym.into(),
        side,
        price: dec!(100),
        qty,
        ts,
    }
}
fn ev_cancel(id: &str, sym: &str, ts: DateTime<Utc>) -> SurveillanceEvent {
    SurveillanceEvent::OrderCancelled {
        order_id: id.into(),
        symbol: sym.into(),
        ts,
    }
}
fn ev_fill(id: &str, sym: &str, qty: Decimal, ts: DateTime<Utc>) -> SurveillanceEvent {
    SurveillanceEvent::OrderFilled {
        order_id: id.into(),
        symbol: sym.into(),
        side: Side::Buy,
        filled_qty: qty,
        price: dec!(100),
        ts,
    }
}

#[test]
fn tracker_pairs_place_and_cancel_into_lifetime() {
    let mut t = OrderLifecycleTracker::new();
    let t0 = Utc::now();
    t.feed(&ev_place("o1", "BTCUSDT", Side::Buy, dec!(1), t0));
    t.feed(&ev_cancel(
        "o1",
        "BTCUSDT",
        t0 + chrono::Duration::milliseconds(50),
    ));
    let s = t.snapshot("BTCUSDT");
    assert_eq!(s.cancel_count, 1);
    assert_eq!(s.median_order_lifetime_ms, Some(50));
}

#[test]
fn tracker_cancel_to_fill_ratio() {
    let mut t = OrderLifecycleTracker::new();
    let t0 = Utc::now();
    // 9 cancels + 1 fill → ratio 0.9.
    for i in 0..9 {
        let id = format!("c{i}");
        t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
        t.feed(&ev_cancel(
            &id,
            "BTCUSDT",
            t0 + chrono::Duration::milliseconds(30),
        ));
    }
    t.feed(&ev_place("f1", "BTCUSDT", Side::Buy, dec!(1), t0));
    t.feed(&ev_fill(
        "f1",
        "BTCUSDT",
        dec!(1),
        t0 + chrono::Duration::milliseconds(200),
    ));
    let s = t.snapshot("BTCUSDT");
    assert_eq!(s.cancel_to_fill_ratio, dec!(0.9));
}

#[test]
fn spoofing_hot_profile_scores_high() {
    let mut t = OrderLifecycleTracker::new();
    let t0 = Utc::now();
    // Feed the trade tape so avg_trade_size is known and small.
    for i in 0..3 {
        let id = format!("trade{i}");
        t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
        t.feed(&ev_fill(
            &id,
            "BTCUSDT",
            dec!(1),
            t0 + chrono::Duration::milliseconds(500),
        ));
    }
    // Spoofing profile: 20 cancels with 30ms lifetime, no fills,
    // plus one huge open order 10x the trade avg.
    for i in 0..20 {
        let id = format!("spoof{i}");
        t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
        t.feed(&ev_cancel(
            &id,
            "BTCUSDT",
            t0 + chrono::Duration::milliseconds(30),
        ));
    }
    t.feed(&ev_place("big", "BTCUSDT", Side::Buy, dec!(10), t0));
    let det = SpoofingDetector::new();
    let out = det.score("BTCUSDT", &t);
    assert!(out.score >= dec!(0.9), "score was {}", out.score);
}

#[test]
fn layering_cluster_of_five_bids_scores_high() {
    let mut t = OrderLifecycleTracker::new();
    let t0 = Utc::now();
    // 6 buy orders clustered within 3 bps of each other.
    for (i, px) in [100.00, 100.01, 100.02, 100.01, 100.03, 100.02]
        .iter()
        .enumerate()
    {
        let id = format!("L{i}");
        t.feed(&SurveillanceEvent::OrderPlaced {
            order_id: id.clone(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            price: Decimal::from_f64_retain(*px).unwrap(),
            qty: dec!(1),
            ts: t0,
        });
    }
    let d = LayeringDetector::new();
    let out = d.score("BTCUSDT", &t);
    assert!(out.score >= dec!(0.5), "layering score was {}", out.score);
}

#[test]
fn quote_stuffing_high_rate_low_fill_scores_high() {
    let mut t = OrderLifecycleTracker::new();
    let t0 = Utc::now();
    // Feed enough cancels to clear the 50 orders/sec × 60s bar
    // (3000 total). All fast-cancelled, zero fills — classic
    // stuffing silhouette.
    for i in 0..3100 {
        let id = format!("S{i}");
        t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
        t.feed(&ev_cancel(
            &id,
            "BTCUSDT",
            t0 + chrono::Duration::milliseconds(20),
        ));
    }
    let d = QuoteStuffingDetector::new();
    let out = d.score("BTCUSDT", &t);
    assert!(out.score >= dec!(0.9), "stuffing score was {}", out.score);
}

#[test]
fn wash_pairs_own_buy_and_sell_at_same_price() {
    let t0 = Utc::now();
    let fills = vec![
        WashFillView {
            ts: t0,
            side: Side::Buy,
            price: dec!(100),
        },
        WashFillView {
            ts: t0 + chrono::Duration::milliseconds(100),
            side: Side::Sell,
            price: dec!(100),
        },
        WashFillView {
            ts: t0 + chrono::Duration::milliseconds(200),
            side: Side::Buy,
            price: dec!(100),
        },
        WashFillView {
            ts: t0 + chrono::Duration::milliseconds(300),
            side: Side::Sell,
            price: dec!(100),
        },
    ];
    let d = WashDetector::new();
    let out = d.score_from_fills(&fills);
    // Four fills → pairs = 4 (every opposite-side within 500ms
    // same price). With pair_count_hot=3, score clamps to 1.
    assert!(out.score >= dec!(0.9), "wash score was {}", out.score);
}

#[test]
fn wash_ignores_distant_prices() {
    let t0 = Utc::now();
    let fills = vec![
        WashFillView {
            ts: t0,
            side: Side::Buy,
            price: dec!(100),
        },
        WashFillView {
            ts: t0 + chrono::Duration::milliseconds(100),
            side: Side::Sell,
            price: dec!(105),
        },
    ];
    let d = WashDetector::new();
    assert_eq!(d.score_from_fills(&fills).score, dec!(0));
}

#[test]
fn momentum_burst_dominant_side_scores_high() {
    let t0 = Utc::now();
    let mut trades: Vec<PublicTradeSample> = Vec::new();
    // 40 trades over 1.5 s all aggressor-buy + price drifts up 50 bps.
    for i in 0..40 {
        let ts = t0 + chrono::Duration::milliseconds((i * 30) as i64);
        let px = dec!(100) + Decimal::from(i) / dec!(20); // 100 → 101.95
        trades.push(PublicTradeSample {
            ts,
            price: px,
            qty: dec!(1),
            aggressor: Some(Side::Buy),
        });
    }
    let d = MomentumIgnitionDetector::new();
    let out = d.score(&trades);
    assert!(out.score >= dec!(0.9), "mi score was {}", out.score);
}

#[test]
fn momentum_balanced_flow_scores_low() {
    let t0 = Utc::now();
    let mut trades = Vec::new();
    for i in 0u32..6 {
        trades.push(PublicTradeSample {
            ts: t0 + chrono::Duration::milliseconds((i as i64) * 200),
            price: dec!(100),
            qty: dec!(1),
            aggressor: Some(if i.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            }),
        });
    }
    let d = MomentumIgnitionDetector::new();
    let out = d.score(&trades);
    assert!(out.score <= dec!(0.5), "mi score was {}", out.score);
}

#[test]
fn fake_liquidity_detects_pulled_level() {
    let t0 = Utc::now();
    let then = L2Snapshot {
        bids: vec![
            L2Level {
                price: dec!(99.90),
                qty: dec!(10),
            },
            L2Level {
                price: dec!(99.80),
                qty: dec!(8),
            },
        ],
        asks: vec![
            L2Level {
                price: dec!(100.10),
                qty: dec!(10),
            },
            L2Level {
                price: dec!(100.20),
                qty: dec!(8),
            },
        ],
        ts: t0,
    };
    // 300 ms later: bid at 99.90 shrinks from 10 → 1 (pulled),
    // ask at 100.10 vanishes entirely. Two pulled levels.
    let now = L2Snapshot {
        bids: vec![
            L2Level {
                price: dec!(99.90),
                qty: dec!(1),
            },
            L2Level {
                price: dec!(99.80),
                qty: dec!(8),
            },
        ],
        asks: vec![L2Level {
            price: dec!(100.20),
            qty: dec!(8),
        }],
        ts: t0 + chrono::Duration::milliseconds(300),
    };
    let d = FakeLiquidityDetector::new();
    let out = d.score(&then, &now);
    assert!(
        out.score >= dec!(0.6),
        "fake-liquidity score was {}",
        out.score
    );
}

#[test]
fn cross_market_scores_ratio_and_move() {
    let d = CrossMarketDetector::new();
    // 5× baseline vol + 20 bps correlated move → both sigs full.
    assert_eq!(d.score(dec!(5), dec!(20)).score, dec!(1));
    // Small ratio + flat move → 0.
    assert_eq!(d.score(dec!(0), dec!(0)).score, dec!(0));
}

#[test]
fn latency_exploit_counts_fills_in_window() {
    let d = LatencyExploitDetector::new();
    let out = d.score(&[20, 30, 40, 500]);
    assert_eq!(out.score, dec!(1)); // 3 hits of 3 = 1.0
}

#[test]
fn rebate_abuse_needs_losing_trade_pnl() {
    let d = RebateAbuseDetector::new();
    // Losing -100, rebate 250 → ratio 2.5 > 2 → 1.0 (clamped).
    assert_eq!(d.score(dec!(-100), dec!(250)).score, dec!(1));
    // Positive trade PnL → 0.
    assert_eq!(d.score(dec!(10), dec!(50)).score, dec!(0));
}

#[test]
fn imbalance_manipulation_detects_flip() {
    let t0 = Utc::now();
    let samples = vec![
        ImbalanceSample {
            ts: t0,
            imbalance: dec!(0.7),
        },
        ImbalanceSample {
            ts: t0 + chrono::Duration::milliseconds(200),
            imbalance: dec!(-0.7),
        },
    ];
    let d = ImbalanceManipulationDetector::new();
    assert_eq!(d.score(&samples).score, dec!(1));
}

#[test]
fn cancel_on_reaction_counts_reflex_cancels() {
    let d = CancelOnReactionDetector::new();
    let out = d.score(&[Some(20), Some(30), Some(500), None]);
    assert_eq!(out.score, dec!(1.0).min(dec!(2) / dec!(3))); // 2 hits / 3 hot
}

#[test]
fn one_sided_quoting_ratio_ramp() {
    let d = OneSidedQuotingDetector::new();
    // 9 / 10 = 0.9 → right at threshold → 1.0.
    assert_eq!(d.score(9, 10).score, dec!(1));
    // 1 / 10 = 0.1 → 0.1/0.9 ≈ 0.111.
    assert!(d.score(1, 10).score < dec!(0.2));
}

#[test]
fn inventory_pushing_correlation() {
    let d = InventoryPushingDetector::new();
    // Strong positive correlation → hot.
    assert_eq!(d.score(dec!(0.8), dec!(0.8)).score, dec!(1));
    // Zero or negative correlation → 0.
    assert_eq!(d.score(dec!(0.8), dec!(-0.8)).score, dec!(0));
}

#[test]
fn strategic_non_filling_needs_min_placements() {
    let d = StrategicNonFillingDetector::new();
    // Only 10 placements → below threshold → 0.
    assert_eq!(d.score(10, 0).score, dec!(0));
    // 100 placements, 0 fills → fill_rate 0 → fully cold → 1.0.
    assert_eq!(d.score(100, 0).score, dec!(1));
}

#[test]
fn marking_close_scores_high_inside_window_with_spike() {
    let d = MarkingCloseDetector::new();
    // 20 s to boundary (inside 60-s window), triple baseline vol.
    let out = d.score(20, dec!(300), dec!(100));
    assert_eq!(out.score, dec!(1));
}

#[test]
fn marking_close_ignores_outside_window() {
    let d = MarkingCloseDetector::new();
    // 300 s to boundary → outside window → 0.
    assert_eq!(d.score(300, dec!(999), dec!(100)).score, dec!(0));
}

#[test]
fn fake_liquidity_ignores_stable_book() {
    let t0 = Utc::now();
    let snap = L2Snapshot {
        bids: vec![L2Level {
            price: dec!(99.90),
            qty: dec!(10),
        }],
        asks: vec![L2Level {
            price: dec!(100.10),
            qty: dec!(10),
        }],
        ts: t0,
    };
    let mut later = snap.clone();
    later.ts = t0 + chrono::Duration::milliseconds(300);
    let d = FakeLiquidityDetector::new();
    assert_eq!(d.score(&snap, &later).score, dec!(0));
}

#[test]
fn spoofing_clean_profile_scores_low() {
    let mut t = OrderLifecycleTracker::new();
    let t0 = Utc::now();
    // Balanced fills + cancels, long lifetimes, similar sizes.
    for i in 0..10 {
        let id = format!("fill{i}");
        t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
        t.feed(&ev_fill(
            &id,
            "BTCUSDT",
            dec!(1),
            t0 + chrono::Duration::seconds(5),
        ));
    }
    for i in 0..2 {
        let id = format!("late{i}");
        t.feed(&ev_place(&id, "BTCUSDT", Side::Buy, dec!(1), t0));
        t.feed(&ev_cancel(
            &id,
            "BTCUSDT",
            t0 + chrono::Duration::seconds(10),
        ));
    }
    let det = SpoofingDetector::new();
    let out = det.score("BTCUSDT", &t);
    assert!(out.score <= dec!(0.3), "score was {}", out.score);
}
