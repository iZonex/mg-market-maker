    use super::*;
    use chrono::Utc;
    use mm_common::types::PriceLevel;

    #[test]
    fn test_book_imbalance() {
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(10),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let imb = MomentumSignals::book_imbalance(&book, 5);
        // (10 - 5) / (10 + 5) = 0.333
        assert!(imb > dec!(0.3));
    }

    #[test]
    fn test_trade_flow() {
        let mut signals = MomentumSignals::new(100);
        for _ in 0..10 {
            signals.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        let flow = signals.trade_flow_imbalance();
        assert_eq!(flow, dec!(1)); // All buys.
    }

    #[test]
    fn test_alpha_neutral() {
        let signals = MomentumSignals::new(100);
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(5),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();
        let alpha = signals.alpha(&book, mid);
        // Balanced book, no trades → alpha ≈ 0.
        assert!(alpha.abs() < dec!(0.0001));
    }

    // ----- HMA wiring tests -----

    /// Without `with_hma` the HMA accessors return `None` and
    /// `on_mid` is a no-op.
    #[test]
    fn hma_is_none_by_default() {
        let mut s = MomentumSignals::new(10);
        s.on_mid(dec!(100));
        s.on_mid(dec!(101));
        assert!(s.hma_value().is_none());
        assert!(s.hma_slope(dec!(100)).is_none());
    }

    /// After `with_hma` the HMA warms up and produces a value
    /// on enough mid-price samples.
    #[test]
    fn hma_warms_up_after_enough_samples() {
        let mut s = MomentumSignals::new(10).with_hma(DEFAULT_HMA_WINDOW);
        for i in 0..40 {
            s.on_mid(dec!(100) + Decimal::from(i));
        }
        assert!(s.hma_value().is_some());
        // Slope must be positive on a rising mid stream.
        let slope = s.hma_slope(dec!(130)).unwrap();
        assert!(slope > dec!(0));
    }

    /// A warmed-up HMA on a rising stream should drive the
    /// alpha positive — i.e. produce a larger output than the
    /// same `MomentumSignals` without the HMA attached.
    #[test]
    fn hma_attached_tilts_alpha_positive_on_rising_mid() {
        let mut book = LocalOrderBook::new("TEST".into());
        book.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(5),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(5),
            }],
            1,
        );
        let mid = book.mid_price().unwrap();

        // Baseline signals — no HMA, same trade stream.
        let mut baseline = MomentumSignals::new(10);
        for _ in 0..20 {
            baseline.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        let base_alpha = baseline.alpha(&book, mid);

        // With HMA on a rising mid stream — slope positive,
        // alpha should be biased up compared to baseline.
        let mut withhma = MomentumSignals::new(10).with_hma(DEFAULT_HMA_WINDOW);
        for _ in 0..20 {
            withhma.on_trade(&Trade {
                trade_id: 1,
                symbol: "TEST".into(),
                price: dec!(100),
                qty: dec!(1),
                taker_side: Side::Buy,
                timestamp: Utc::now(),
            });
        }
        for i in 0..40 {
            withhma.on_mid(dec!(100) + Decimal::from(i));
        }
        let hma_alpha = withhma.alpha(&book, mid);

        assert!(
            hma_alpha > dec!(0),
            "HMA alpha must stay positive on a rising stream: {hma_alpha}"
        );
        // The two alphas use different weight splits, so the
        // direct comparison is a sanity check: neither should
        // be zero, and neither should be NaN-like.
        assert!(base_alpha > dec!(0));
    }

    // ------ Epic D sub-component #1 + #2 — OFI + learned MP ------

    fn balanced_book() -> LocalOrderBook {
        let mut b = LocalOrderBook::new("BTCUSDT".into());
        b.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(10),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(10),
            }],
            1,
        );
        b
    }

    #[test]
    fn ofi_is_none_by_default() {
        let m = MomentumSignals::new(20);
        assert!(m.ofi_ewma().is_none());
    }

    #[test]
    fn with_ofi_then_l1_snapshots_populate_ewma() {
        let mut m = MomentumSignals::new(20).with_ofi();
        // Seed.
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        assert!(m.ofi_ewma().is_none(), "first snapshot only seeds");
        // Aggressive bid arrival → positive OFI.
        m.on_l1_snapshot(dec!(100), dec!(10), dec!(101), dec!(10));
        let ewma = m.ofi_ewma().expect("EWMA populated");
        assert!(ewma > dec!(0), "positive OFI expected, got {ewma}");
    }

    #[test]
    fn ofi_stream_emits_positive_ewma_on_growing_bid_depth() {
        // Run a stream of monotonically growing bid depth at
        // a fixed touch — every event contributes a strictly
        // positive bid delta, so the EWMA accumulates upward.
        let mut m = MomentumSignals::new(20).with_ofi();
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        for n in 1..=20 {
            let bid_qty = dec!(10) + Decimal::from(n);
            m.on_l1_snapshot(dec!(99), bid_qty, dec!(101), dec!(10));
        }
        let ewma = m.ofi_ewma().expect("EWMA populated");
        assert!(ewma > dec!(0), "expected positive smoothed OFI, got {ewma}");
    }

    #[test]
    fn ofi_z_saturates_near_one_on_one_sided_stream() {
        // Same one-sided bid-growth stream as above — the z
        // score should settle above 0.5 since every
        // observation contributes the same sign.
        let mut m = MomentumSignals::new(20).with_ofi();
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        for n in 1..=30 {
            let bid_qty = dec!(10) + Decimal::from(n);
            m.on_l1_snapshot(dec!(99), bid_qty, dec!(101), dec!(10));
        }
        let z = m.ofi_z().expect("z populated");
        assert!(z > dec!(0.5), "expected strong positive z, got {z}");
        assert!(z <= dec!(1.5), "z should be bounded near signal/RMS, got {z}");
    }

    #[test]
    fn ofi_z_near_zero_on_balanced_tape() {
        // Alternate aggressive bids + aggressive asks —
        // mean near zero, RMS positive → z ≈ 0.
        let mut m = MomentumSignals::new(20).with_ofi();
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        for i in 0..30 {
            let (bq, aq) = if i % 2 == 0 {
                (dec!(15), dec!(10))
            } else {
                (dec!(10), dec!(15))
            };
            m.on_l1_snapshot(dec!(99), bq, dec!(101), aq);
        }
        let z = m.ofi_z().expect("z populated");
        assert!(z.abs() < dec!(0.5), "balanced tape z should be near 0, got {z}");
    }

    #[test]
    fn ofi_z_none_before_any_snapshot() {
        let m = MomentumSignals::new(20).with_ofi();
        assert!(m.ofi_z().is_none());
    }

    #[test]
    fn ofi_alpha_tilts_versus_baseline() {
        // Direct alpha comparison: balanced book → baseline = 0.
        // Attach OFI + feed positive depth growth → alpha tilts up.
        let book = balanced_book();
        let mid = book.mid_price().unwrap();
        let base = MomentumSignals::new(20).alpha(&book, mid);
        assert_eq!(base, dec!(0));

        let mut m = MomentumSignals::new(20).with_ofi();
        m.on_l1_snapshot(dec!(99), dec!(10), dec!(101), dec!(10));
        for n in 1..=20 {
            m.on_l1_snapshot(dec!(99), dec!(10) + Decimal::from(n), dec!(101), dec!(10));
        }
        let ofi_alpha = m.alpha(&book, mid);
        assert!(
            ofi_alpha > dec!(0),
            "OFI-attached alpha should be positive, got {ofi_alpha}"
        );
    }

    #[test]
    fn learned_mp_is_none_until_attached_and_finalized() {
        let book = balanced_book();
        let mid = book.mid_price().unwrap();
        let m = MomentumSignals::new(20);
        // No model attached → drift is None.
        assert!(m.learned_microprice_drift(&book, mid).is_none());

        // Attach an unfinalized model → still None.
        let model = crate::learned_microprice::LearnedMicroprice::empty(
            crate::learned_microprice::LearnedMicropriceConfig::default(),
        );
        let m2 = MomentumSignals::new(20).with_learned_microprice(model);
        assert!(m2.learned_microprice_drift(&book, mid).is_none());
    }

    #[test]
    fn learned_mp_finalized_with_zero_buckets_returns_none() {
        let book = balanced_book();
        let mid = book.mid_price().unwrap();
        // A fresh `empty` + `finalize` model has zero in
        // every bucket → predict returns 0 → drift is None.
        let mut model = crate::learned_microprice::LearnedMicroprice::empty(
            crate::learned_microprice::LearnedMicropriceConfig::default(),
        );
        model.finalize();
        let m = MomentumSignals::new(20).with_learned_microprice(model);
        assert!(m.learned_microprice_drift(&book, mid).is_none());
    }

    #[test]
    fn learned_mp_negative_prediction_pulls_alpha_below_baseline() {
        // Train a model so the high-imbalance bucket predicts
        // a *negative* Δmid (mean-reversion). On a bid-heavy
        // book, the wave-1 components want to push alpha up;
        // the LMP component pushes it back down. Net: the
        // LMP-attached alpha should be strictly less than the
        // baseline.
        let cfg = crate::learned_microprice::LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 2,
            ..Default::default()
        };
        let mut model = crate::learned_microprice::LearnedMicroprice::empty(cfg);
        for _ in 0..5 {
            // Big magnitude → enough drift to overcome the
            // wave-1-weight reduction from attaching one
            // optional signal.
            model.accumulate(dec!(0.9), dec!(1), dec!(-50));
        }
        model.finalize();

        let mut tilted = LocalOrderBook::new("BTCUSDT".into());
        tilted.apply_snapshot(
            vec![PriceLevel {
                price: dec!(100),
                qty: dec!(50),
            }],
            vec![PriceLevel {
                price: dec!(101),
                qty: dec!(2),
            }],
            1,
        );
        let mid = tilted.mid_price().unwrap();

        let base = MomentumSignals::new(20).alpha(&tilted, mid);
        let withlmp = MomentumSignals::new(20).with_learned_microprice(model);
        let lmp_alpha = withlmp.alpha(&tilted, mid);
        assert!(
            base > dec!(0),
            "baseline should be positive on bid-heavy book"
        );
        assert!(
            lmp_alpha < base,
            "LMP-attached alpha should be pulled below baseline by negative prediction: \
             base={base}, lmp={lmp_alpha}"
        );
    }

    // ---------------------------------------------------------
    // Epic D stage-2 — online lMP fit via on_l1_snapshot
    // ---------------------------------------------------------

    /// Feeding `on_l1_snapshot` with a steady stream of
    /// bid-heavy books AND rising mids should make the online
    /// fit attribute a positive `Δmid` to positive-imbalance
    /// buckets. Within refit_every counts the g-matrix
    /// shouldn't change; past the boundary it should.
    #[test]
    fn online_lmp_refits_g_matrix_after_horizon_and_refit_cadence() {
        use crate::learned_microprice::{LearnedMicroprice, LearnedMicropriceConfig};

        // Build + finalise a model with a neutral seed so the
        // initial g-matrix is non-zero; refit_every=5 so the
        // test runs quickly.
        let cfg = LearnedMicropriceConfig {
            n_imbalance_buckets: 4,
            n_spread_buckets: 1,
            min_bucket_samples: 2,
            online_ring_capacity: 30,
            refit_every: 5,
        };
        let mut model = LearnedMicroprice::empty(cfg);
        for _ in 0..3 {
            model.accumulate(dec!(-0.75), dec!(0.01), dec!(-0.1));
            model.accumulate(dec!(0.75), dec!(0.01), dec!(0.1));
        }
        model.finalize_iterative(5);
        let initial_g = model.g_matrix().to_vec();

        // Horizon of 2 snapshots — very short so we emit
        // observations quickly.
        let mut m = MomentumSignals::new(10).with_learned_microprice_online(model, 2);

        // Push 20 bid-heavy snapshots with monotonically rising
        // mid. Horizon=2 → first two snapshots buffer, third
        // emits the first `update_online` call with a positive
        // Δmid on the +0.75 imbalance bucket. After 5 emits
        // the refit triggers. We need >= horizon + refit_every
        // + epsilon = 2 + 5 + buffer = ~10 ticks.
        for t in 0..20 {
            let mid = dec!(100) + Decimal::from(t);
            let bid = mid - dec!(0.005);
            let ask = mid + dec!(0.005);
            // Bid-heavy book.
            m.on_l1_snapshot(bid, dec!(10), ask, dec!(1));
        }

        // After 20 ticks the model should have fired at least
        // one refit. The g-matrix at imbalance +0.75 should now
        // skew STRONGER positive than the seed (more recent
        // data has larger Δmid = +1 per tick × horizon 2 = +2,
        // vs. seed +0.1).
        let new_g = m
            .learned_mp
            .as_ref()
            .map(|mp| mp.g_matrix().to_vec())
            .expect("model attached");
        assert_ne!(new_g, initial_g, "online fit should have mutated g-matrix");
    }

    #[test]
    fn online_lmp_builder_panics_on_zero_horizon() {
        use crate::learned_microprice::{LearnedMicroprice, LearnedMicropriceConfig};
        let model = LearnedMicroprice::empty(LearnedMicropriceConfig::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = MomentumSignals::new(10).with_learned_microprice_online(model, 0);
        }));
        assert!(result.is_err(), "horizon=0 must panic");
    }

    /// 22B-6 — basic snapshot_state returns None on empty tracker.
    #[test]
    fn empty_tracker_returns_none() {
        let m = MomentumSignals::new(10);
        assert!(m.snapshot_state().is_none());
    }

    fn mk_trade(qty: Decimal, taker_side: Side, id: u64) -> Trade {
        Trade {
            trade_id: id,
            symbol: "BTCUSDT".into(),
            price: dec!(100),
            qty,
            taker_side,
            timestamp: Utc::now(),
        }
    }

    /// 22B-6 — signed_volumes round-trips through snapshot/restore.
    #[test]
    fn signed_volumes_round_trip() {
        let mut src = MomentumSignals::new(10);
        // Drive a few trades to populate signed_volumes.
        src.on_trade(&mk_trade(dec!(1), Side::Buy, 1));
        src.on_trade(&mk_trade(dec!(2), Side::Sell, 2));
        let snap = src.snapshot_state().expect("has data");

        let mut dst = MomentumSignals::new(10);
        dst.restore_state(&snap).unwrap();
        assert_eq!(dst.signed_volumes.len(), 2);
        // on_trade signs notional (price * qty): +100 buy, -200 sell.
        assert_eq!(dst.signed_volumes[0], dec!(100));
        assert_eq!(dst.signed_volumes[1], dec!(-200));
    }

    /// 22B-6 — window cap truncates oversize signed_volumes
    /// buffer during restore.
    #[test]
    fn restore_truncates_oversize_window() {
        let mut src = MomentumSignals::new(100);
        for i in 0..80 {
            src.on_trade(&mk_trade(Decimal::from(i + 1), Side::Buy, i as u64));
        }
        let snap = src.snapshot_state().expect("has data");

        let mut dst = MomentumSignals::new(10); // smaller cap
        dst.restore_state(&snap).unwrap();
        assert_eq!(dst.signed_volumes.len(), 10);
    }

    /// 22W-6 — ema-weighted trade-flow imbalance uses
    /// `mm_indicators::ema_weights` to emphasise recent trades
    /// over the oldest ones. With 5 buy trades followed by 1
    /// sell, uniform-weighted is positive; EMA-weighted should
    /// tilt less positive because the most recent trade (sell)
    /// gets the biggest weight.
    #[test]
    fn ema_weighted_flow_emphasises_recent_trades() {
        let mut m = MomentumSignals::new(10);
        for i in 0..5 {
            m.on_trade(&mk_trade(dec!(1), Side::Buy, i));
        }
        m.on_trade(&mk_trade(dec!(1), Side::Sell, 5));

        let uniform = m.trade_flow_imbalance();
        let weighted = m.trade_flow_imbalance_ema_weighted(None);
        assert!(uniform > dec!(0), "uniform = {uniform}");
        assert!(
            weighted < uniform,
            "weighted ({weighted}) must be < uniform ({uniform}) when the most recent trade flipped"
        );
    }

    /// 22W-6 — fewer than 2 samples falls through to the uniform
    /// path so the ema_weights call never panics on `window < 2`.
    #[test]
    fn ema_weighted_flow_short_window_matches_uniform() {
        let mut m = MomentumSignals::new(10);
        m.on_trade(&mk_trade(dec!(1), Side::Buy, 1));
        let u = m.trade_flow_imbalance();
        let w = m.trade_flow_imbalance_ema_weighted(None);
        assert_eq!(u, w);
    }

    /// 22B-4 — learned_mp round-trips through snapshot/restore
    /// when both sides have the subsystem attached.
    #[test]
    fn learned_mp_round_trip() {
        use crate::learned_microprice::{LearnedMicroprice, LearnedMicropriceConfig};
        let cfg = LearnedMicropriceConfig::default();
        let src_model = LearnedMicroprice::empty(cfg.clone());
        let dst_model = LearnedMicroprice::empty(cfg);
        let src = MomentumSignals::new(10)
            .with_learned_microprice(src_model);
        let mut dst = MomentumSignals::new(10)
            .with_learned_microprice(dst_model);
        let snap = src.snapshot_state().expect("has data");
        dst.restore_state(&snap).unwrap();
        assert!(dst.learned_mp.is_some());
    }

    #[test]
    fn restore_rejects_wrong_schema() {
        let mut m = MomentumSignals::new(10);
        let bogus = serde_json::json!({
            "schema_version": 999,
            "window": 10,
            "signed_volumes": [],
            "ofi_ewma": null,
            "ofi_ewma_sq": null,
            "learned_mp": null,
            "online_mp_ring": null,
        });
        assert!(m.restore_state(&bogus).is_err());
    }
