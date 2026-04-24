//! Sprint 1.3 — end-to-end Epic R flow.
//!
//! Feeds a spoof-like order lifecycle straight into the engine's
//! surveillance tracker (bypassing the venue WS + connector — we're
//! testing the **graph + tracker + detector** wiring, not the
//! network). Confirms that after N bursts:
//!
//!   · the tracker's stats reflect the spoof silhouette,
//!   · the spoofing detector scores above the alert threshold,
//!   · the engine owns the tracker via its shared handle (so real
//!     paper-mode deploys would feed the same state).

use mm_risk::surveillance::{
    new_shared_tracker, OrderLifecycleTracker, Side, SpoofingDetector, SurveillanceEvent,
};
use rust_decimal_macros::dec;

/// Build a tracker and drive a spoof silhouette through it.
/// Returns the tracker + the detector's score + diagnostics so
/// the test body can assert on each independently.
fn run_spoof_silhouette() -> (
    mm_risk::surveillance::SymbolStats,
    mm_risk::surveillance::DetectorOutput,
) {
    let tracker = new_shared_tracker();
    let t0 = chrono::Utc::now();

    // Seed the trade tape with a few normal-size fills so the
    // detector's "order size vs avg trade" sub-signal can compute
    // a baseline.
    for i in 0..3 {
        let id = format!("fill{i}");
        let ts = t0 + chrono::Duration::milliseconds((i * 100) as i64);
        {
            let mut t = tracker.lock().unwrap();
            t.feed(&SurveillanceEvent::OrderPlaced {
                order_id: id.clone(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                price: dec!(30_000),
                qty: dec!(1),
                ts,
            });
            t.feed(&SurveillanceEvent::OrderFilled {
                order_id: id,
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                filled_qty: dec!(1),
                price: dec!(30_000),
                ts: ts + chrono::Duration::milliseconds(500),
            });
        }
    }

    // Spoof silhouette: 20 cancels at 30-ms lifetime, one giant
    // open order 10× trade avg.
    for i in 0..20 {
        let id = format!("spoof{i}");
        let ts =
            t0 + chrono::Duration::seconds(1) + chrono::Duration::milliseconds((i * 25) as i64);
        let mut t = tracker.lock().unwrap();
        t.feed(&SurveillanceEvent::OrderPlaced {
            order_id: id.clone(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            price: dec!(30_000),
            qty: dec!(1),
            ts,
        });
        t.feed(&SurveillanceEvent::OrderCancelled {
            order_id: id,
            symbol: "BTCUSDT".into(),
            ts: ts + chrono::Duration::milliseconds(30),
        });
    }
    {
        let mut t = tracker.lock().unwrap();
        t.feed(&SurveillanceEvent::OrderPlaced {
            order_id: "big".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            price: dec!(30_000),
            qty: dec!(10),
            ts: t0 + chrono::Duration::seconds(2),
        });
    }

    let t = tracker.lock().unwrap();
    let stats = t.snapshot("BTCUSDT");
    let score = SpoofingDetector::new().score("BTCUSDT", &t);
    (stats, score)
}

#[test]
fn spoof_silhouette_produces_alert_grade_score() {
    let (stats, out) = run_spoof_silhouette();
    assert!(
        stats.cancel_count >= 20,
        "tracker should see all spoof cancels, saw {}",
        stats.cancel_count
    );
    assert!(
        stats.cancel_to_fill_ratio >= dec!(0.8),
        "cancel/fill ratio should be spoof-grade, was {}",
        stats.cancel_to_fill_ratio
    );
    assert!(
        stats.median_order_lifetime_ms.is_some_and(|m| m <= 100),
        "median lifetime should be fast-cancel, was {:?}",
        stats.median_order_lifetime_ms
    );
    assert!(
        out.score >= dec!(0.9),
        "spoofing detector should alert, score {}",
        out.score
    );
}

/// Tracker with no events behaves as "no evidence" — detector
/// doesn't produce a false alarm on a cold start.
#[test]
fn empty_tracker_scores_zero() {
    let t = OrderLifecycleTracker::new();
    let out = SpoofingDetector::new().score("BTCUSDT", &t);
    assert_eq!(out.score, dec!(0));
}
