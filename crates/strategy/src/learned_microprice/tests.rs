use super::*;

fn single_bucket_config() -> LearnedMicropriceConfig {
    LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 1,
        min_bucket_samples: 5,
        ..Default::default()
    }
}

#[test]
fn empty_config_returns_zero_predictions() {
    let mut mp = LearnedMicroprice::empty(LearnedMicropriceConfig::default());
    mp.finalize();
    assert_eq!(mp.predict(dec!(0.3), dec!(0.01)), Decimal::ZERO);
    assert_eq!(mp.predict(dec!(-0.7), dec!(0.05)), Decimal::ZERO);
}

#[test]
fn predict_before_finalize_returns_zero() {
    let mp = LearnedMicroprice::empty(single_bucket_config());
    // No finalize call — predict should safely return zero.
    assert_eq!(mp.predict(dec!(0.5), dec!(0.01)), Decimal::ZERO);
}

#[test]
fn single_bucket_fit_recovers_mean_delta() {
    let mut mp = LearnedMicroprice::empty(single_bucket_config());
    // Feed 10 observations with Δmid = +0.5 in the high-
    // imbalance bucket. Fewer in negative buckets so they
    // clamp to zero.
    for _ in 0..10 {
        mp.accumulate(dec!(0.8), dec!(0.01), dec!(0.5));
    }
    mp.finalize();
    let pred = mp.predict(dec!(0.8), dec!(0.01));
    assert_eq!(pred, dec!(0.5));
}

#[test]
fn undersampled_bucket_clamps_to_zero() {
    let mut mp = LearnedMicroprice::empty(single_bucket_config());
    // Only 3 samples — below min_bucket_samples=5.
    for _ in 0..3 {
        mp.accumulate(dec!(0.5), dec!(0.01), dec!(1.0));
    }
    mp.finalize();
    // Bucket count below threshold → prediction clamps to 0.
    assert_eq!(mp.predict(dec!(0.5), dec!(0.01)), Decimal::ZERO);
}

#[test]
fn imbalance_bucket_boundaries_are_stable() {
    // With 4 buckets on [-1, 1]: edges at -0.5, 0, +0.5.
    assert_eq!(imbalance_bucket(dec!(-1), 4), 0);
    assert_eq!(imbalance_bucket(dec!(-0.75), 4), 0);
    assert_eq!(imbalance_bucket(dec!(-0.25), 4), 1);
    assert_eq!(imbalance_bucket(dec!(0), 4), 2);
    assert_eq!(imbalance_bucket(dec!(0.25), 4), 2);
    assert_eq!(imbalance_bucket(dec!(0.75), 4), 3);
    assert_eq!(imbalance_bucket(dec!(1), 4), 3);
}

#[test]
fn imbalance_bucket_clamps_out_of_range() {
    assert_eq!(imbalance_bucket(dec!(-5), 4), 0);
    assert_eq!(imbalance_bucket(dec!(2), 4), 3);
}

#[test]
fn spread_bucket_with_edges() {
    let edges = vec![dec!(0.01), dec!(0.05), dec!(0.1)];
    // 4 buckets total: (−∞, 0.01], (0.01, 0.05], (0.05, 0.1], (0.1, +∞)
    assert_eq!(spread_bucket(dec!(0.005), &edges), 0);
    assert_eq!(spread_bucket(dec!(0.01), &edges), 0);
    assert_eq!(spread_bucket(dec!(0.03), &edges), 1);
    assert_eq!(spread_bucket(dec!(0.05), &edges), 1);
    assert_eq!(spread_bucket(dec!(0.07), &edges), 2);
    assert_eq!(spread_bucket(dec!(0.5), &edges), 3);
}

#[test]
fn spread_bucket_no_edges_always_zero() {
    // Degenerate n_spread_buckets = 1 — empty edges slice.
    assert_eq!(spread_bucket(dec!(0), &[]), 0);
    assert_eq!(spread_bucket(dec!(100), &[]), 0);
}

#[test]
fn two_pass_fit_with_spread_edges() {
    // Demonstrates the two-pass path: operator computes
    // spread edges externally (e.g. quantiles over a
    // training corpus), seeds them, then accumulates.
    let config = LearnedMicropriceConfig {
        n_imbalance_buckets: 2,
        n_spread_buckets: 2,
        min_bucket_samples: 2,
        ..Default::default()
    };
    let mut mp = LearnedMicroprice::empty(config);
    mp.with_spread_edges(vec![dec!(0.05)]);

    // Low imbalance + tight spread → Δmid negative.
    mp.accumulate_with_edges(dec!(-0.8), dec!(0.01), dec!(-0.3));
    mp.accumulate_with_edges(dec!(-0.8), dec!(0.02), dec!(-0.5));

    // High imbalance + wide spread → Δmid positive.
    mp.accumulate_with_edges(dec!(0.8), dec!(0.1), dec!(0.4));
    mp.accumulate_with_edges(dec!(0.8), dec!(0.15), dec!(0.6));

    mp.finalize();

    let low_tight = mp.predict(dec!(-0.9), dec!(0.02));
    let high_wide = mp.predict(dec!(0.9), dec!(0.12));
    assert_eq!(low_tight, dec!(-0.4));
    assert_eq!(high_wide, dec!(0.5));
}

#[test]
fn finalize_is_idempotent() {
    let mut mp = LearnedMicroprice::empty(single_bucket_config());
    for _ in 0..10 {
        mp.accumulate(dec!(0.5), dec!(0.01), dec!(0.1));
    }
    mp.finalize();
    let p1 = mp.predict(dec!(0.5), dec!(0.01));
    mp.finalize();
    let p2 = mp.predict(dec!(0.5), dec!(0.01));
    assert_eq!(p1, p2);
}

#[test]
#[should_panic(expected = "cannot accumulate after finalize")]
fn accumulate_after_finalize_panics() {
    let mut mp = LearnedMicroprice::empty(single_bucket_config());
    mp.finalize();
    mp.accumulate(dec!(0.5), dec!(0.01), dec!(0.1));
}

#[test]
#[should_panic(expected = "n_imbalance_buckets must be >= 2")]
fn empty_panics_on_tiny_imbalance_buckets() {
    LearnedMicroprice::empty(LearnedMicropriceConfig {
        n_imbalance_buckets: 1,
        n_spread_buckets: 1,
        min_bucket_samples: 1,
        ..Default::default()
    });
}

#[test]
fn bucket_count_accessor_reports_totals() {
    let mut mp = LearnedMicroprice::empty(single_bucket_config());
    for _ in 0..7 {
        mp.accumulate(dec!(0.8), dec!(0.01), dec!(0.2));
    }
    // All 7 observations land in the last imbalance bucket
    // (i = 3) on spread bucket 0 since n_spread_buckets = 1.
    assert_eq!(mp.bucket_count(3, 0), 7);
    assert_eq!(mp.bucket_count(0, 0), 0);
}

// ------------------------- TOML persistence -------------------------

fn tmp_path(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    // Unique-ish suffix so parallel test threads don't stomp.
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("mm_lmp_{name}_{uniq}.toml"));
    p
}

#[test]
fn learned_microprice_toml_empty_roundtrip() {
    let mp = LearnedMicroprice::empty(single_bucket_config());
    let path = tmp_path("empty");
    mp.to_toml(&path).expect("write empty model");
    let reloaded = LearnedMicroprice::from_toml(&path).expect("read empty model");
    assert!(!reloaded.is_finalized());
    assert_eq!(reloaded.config.n_imbalance_buckets, 4);
    assert_eq!(reloaded.config.n_spread_buckets, 1);
    assert_eq!(reloaded.config.min_bucket_samples, 5);
    assert_eq!(reloaded.g_matrix().len(), 4);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn learned_microprice_toml_finalized_fit_roundtrip() {
    let mut mp = LearnedMicroprice::empty(single_bucket_config());
    for _ in 0..8 {
        mp.accumulate(dec!(0.8), dec!(0.01), dec!(0.5));
    }
    mp.finalize();
    let path = tmp_path("finalized");
    mp.to_toml(&path).expect("write");
    let reloaded = LearnedMicroprice::from_toml(&path).expect("read");
    assert!(reloaded.is_finalized());
    assert_eq!(reloaded.g_matrix(), mp.g_matrix());
    assert_eq!(reloaded.bucket_count(3, 0), mp.bucket_count(3, 0));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn learned_microprice_toml_spread_edges_roundtrip() {
    let config = LearnedMicropriceConfig {
        n_imbalance_buckets: 2,
        n_spread_buckets: 3,
        min_bucket_samples: 1,
        ..Default::default()
    };
    let mut mp = LearnedMicroprice::empty(config);
    mp.with_spread_edges(vec![dec!(0.02), dec!(0.08)]);
    // A few observations so the g_matrix is non-trivial.
    mp.accumulate_with_edges(dec!(-0.5), dec!(0.01), dec!(-0.1));
    mp.accumulate_with_edges(dec!(0.5), dec!(0.05), dec!(0.2));
    mp.accumulate_with_edges(dec!(0.5), dec!(0.2), dec!(0.4));
    mp.finalize();
    let path = tmp_path("edges");
    mp.to_toml(&path).expect("write");
    let reloaded = LearnedMicroprice::from_toml(&path).expect("read");
    assert_eq!(reloaded.spread_edges(), mp.spread_edges());
    assert_eq!(reloaded.spread_edges().len(), 2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn learned_microprice_toml_prediction_parity_post_roundtrip() {
    let config = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 2,
        min_bucket_samples: 2,
        ..Default::default()
    };
    let mut mp = LearnedMicroprice::empty(config);
    mp.with_spread_edges(vec![dec!(0.05)]);
    for _ in 0..4 {
        mp.accumulate_with_edges(dec!(-0.8), dec!(0.01), dec!(-0.2));
        mp.accumulate_with_edges(dec!(0.8), dec!(0.1), dec!(0.3));
    }
    mp.finalize();
    let path = tmp_path("parity");
    mp.to_toml(&path).expect("write");
    let reloaded = LearnedMicroprice::from_toml(&path).expect("read");
    // Exhaustive prediction parity over a grid of inputs.
    for im in [dec!(-0.9), dec!(-0.3), dec!(0.3), dec!(0.9)] {
        for sp in [dec!(0.001), dec!(0.03), dec!(0.06), dec!(0.2)] {
            assert_eq!(
                reloaded.predict(im, sp),
                mp.predict(im, sp),
                "prediction mismatch at im={im}, sp={sp}"
            );
        }
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn monotone_imbalance_produces_monotone_prediction_under_monotone_training() {
    // Training data: as imbalance rises, Δmid rises.
    // After fit, predictions should respect that ordering.
    let config = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 1,
        min_bucket_samples: 3,
        ..Default::default()
    };
    let mut mp = LearnedMicroprice::empty(config);
    // Bucket 0: Δmid = −0.4
    for _ in 0..4 {
        mp.accumulate(dec!(-0.75), dec!(0.01), dec!(-0.4));
    }
    // Bucket 1: Δmid = −0.1
    for _ in 0..4 {
        mp.accumulate(dec!(-0.25), dec!(0.01), dec!(-0.1));
    }
    // Bucket 2: Δmid = +0.1
    for _ in 0..4 {
        mp.accumulate(dec!(0.25), dec!(0.01), dec!(0.1));
    }
    // Bucket 3: Δmid = +0.4
    for _ in 0..4 {
        mp.accumulate(dec!(0.75), dec!(0.01), dec!(0.4));
    }
    mp.finalize();

    let p_neg = mp.predict(dec!(-0.75), dec!(0.01));
    let p_mid_lo = mp.predict(dec!(-0.25), dec!(0.01));
    let p_mid_hi = mp.predict(dec!(0.25), dec!(0.01));
    let p_pos = mp.predict(dec!(0.75), dec!(0.01));

    assert!(p_neg < p_mid_lo);
    assert!(p_mid_lo < p_mid_hi);
    assert!(p_mid_hi < p_pos);
}

// ── Iterative fixed-point tests ─────────────────────────

/// Iterative finalize fills sparse buckets from neighbors.
/// With 4 imbalance buckets and 1 spread bucket, train
/// only buckets 0 and 3 (extremes) and verify that the
/// interior buckets get non-zero predictions via neighbor
/// interpolation.
#[test]
fn iterative_fills_sparse_from_neighbors() {
    let cfg = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 1,
        min_bucket_samples: 5,
        ..Default::default()
    };
    let mut mp = LearnedMicroprice::empty(cfg);
    // Train only bucket 0 (imbalance ~ -0.75) and bucket 3
    // (imbalance ~ +0.75).
    for _ in 0..10 {
        mp.accumulate(dec!(-0.9), dec!(0.01), dec!(-0.5));
    }
    for _ in 0..10 {
        mp.accumulate(dec!(0.9), dec!(0.01), dec!(0.5));
    }
    mp.finalize_iterative(10);

    // Interior buckets should be non-zero (filled from neighbors).
    let p1 = mp.predict(dec!(-0.25), dec!(0.01));
    let p2 = mp.predict(dec!(0.25), dec!(0.01));
    assert!(
        p1 != Decimal::ZERO,
        "bucket 1 should be filled by neighbor, got 0"
    );
    assert!(
        p2 != Decimal::ZERO,
        "bucket 2 should be filled by neighbor, got 0"
    );
}

/// Standard finalize clamps sparse buckets to zero; iterative
/// does not. Verify the difference.
#[test]
fn iterative_differs_from_standard_on_sparse() {
    let cfg = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 1,
        min_bucket_samples: 5,
        ..Default::default()
    };

    // Standard finalize.
    let mut mp_std = LearnedMicroprice::empty(cfg.clone());
    for _ in 0..10 {
        mp_std.accumulate(dec!(-0.9), dec!(0.01), dec!(-0.5));
    }
    mp_std.finalize();
    let p_std = mp_std.predict(dec!(-0.25), dec!(0.01));
    assert_eq!(p_std, Decimal::ZERO, "standard should clamp sparse to 0");

    // Iterative finalize.
    let mut mp_iter = LearnedMicroprice::empty(cfg);
    for _ in 0..10 {
        mp_iter.accumulate(dec!(-0.9), dec!(0.01), dec!(-0.5));
    }
    mp_iter.finalize_iterative(10);
    let p_iter = mp_iter.predict(dec!(-0.25), dec!(0.01));
    assert!(
        p_iter != Decimal::ZERO,
        "iterative should fill sparse bucket"
    );
}

/// Iterative finalize with all well-sampled buckets matches
/// standard finalize exactly (no neighbor borrowing needed).
#[test]
fn iterative_matches_standard_when_all_sampled() {
    let cfg = single_bucket_config();
    let mut mp_std = LearnedMicroprice::empty(cfg.clone());
    let mut mp_iter = LearnedMicroprice::empty(cfg);

    for i in 0..40 {
        let imb = Decimal::from(i % 4) / dec!(2) - dec!(0.75);
        let dm = imb * dec!(0.1);
        mp_std.accumulate(imb, dec!(0.01), dm);
        mp_iter.accumulate(imb, dec!(0.01), dm);
    }
    mp_std.finalize();
    mp_iter.finalize_iterative(10);

    for imb_idx in 0..4 {
        let imb = Decimal::from(imb_idx) / dec!(2) - dec!(0.75);
        assert_eq!(
            mp_std.predict(imb, dec!(0.01)),
            mp_iter.predict(imb, dec!(0.01)),
            "bucket {} should match",
            imb_idx
        );
    }
}

// ---------------------------------------------------------
// Epic D stage-2 — online streaming fit
// ---------------------------------------------------------

fn seed_finalised_fit() -> LearnedMicroprice {
    let cfg = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 1,
        min_bucket_samples: 2,
        online_ring_capacity: 50,
        refit_every: 10,
    };
    let mut mp = LearnedMicroprice::empty(cfg);
    // Seed with a mild upward drift so the offline fit is
    // not identically zero, otherwise update_online parity
    // tests can't distinguish "no effect" from "reset to
    // zero".
    for _ in 0..4 {
        mp.accumulate(dec!(-0.75), dec!(0.01), dec!(-0.1));
    }
    for _ in 0..4 {
        mp.accumulate(dec!(0.75), dec!(0.01), dec!(0.1));
    }
    mp.finalize_iterative(5);
    mp
}

#[test]
fn update_online_on_unfinalised_model_is_noop() {
    let cfg = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 1,
        min_bucket_samples: 2,
        online_ring_capacity: 50,
        refit_every: 10,
    };
    let mut mp = LearnedMicroprice::empty(cfg);
    mp.update_online(dec!(0.5), dec!(0.01), dec!(1.0));
    // Silent no-op: counter stays zero, ring stays empty.
    assert_eq!(mp.online_ring_len(), 0);
    assert_eq!(mp.online_counter(), 0);
}

#[test]
fn update_online_appends_every_call_refits_on_boundary() {
    let mut mp = seed_finalised_fit();
    let initial_g = mp.g_matrix().to_vec();
    for i in 1..=9 {
        mp.update_online(dec!(0.5), dec!(0.01), dec!(0.2));
        // Ring grows; g-matrix should be unchanged until
        // refit_every=10 is reached.
        assert_eq!(mp.online_ring_len(), i);
        assert_eq!(
            mp.g_matrix(),
            initial_g.as_slice(),
            "g-matrix must not refit before the {i}th update"
        );
    }
    // 10th update triggers the rebuild.
    mp.update_online(dec!(0.5), dec!(0.01), dec!(0.2));
    assert_eq!(mp.online_counter(), 10);
    assert_ne!(
        mp.g_matrix(),
        initial_g.as_slice(),
        "refit at boundary must update g-matrix"
    );
}

#[test]
fn update_online_bounded_ring_does_not_grow_unbounded() {
    let mut mp = seed_finalised_fit();
    for _ in 0..200 {
        mp.update_online(dec!(0.0), dec!(0.01), dec!(0.0));
    }
    assert_eq!(
        mp.online_ring_len(),
        50,
        "ring must cap at online_ring_capacity=50"
    );
    assert_eq!(mp.online_counter(), 200);
}

#[test]
fn update_online_preserves_spread_edges() {
    // Build a multi-spread-bucket fit via the two-pass
    // path, then online-push some observations and verify
    // the edges stay byte-for-byte equal.
    let cfg = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 3,
        min_bucket_samples: 1,
        online_ring_capacity: 50,
        refit_every: 5,
    };
    let mut mp = LearnedMicroprice::empty(cfg);
    mp.with_spread_edges(vec![dec!(0.02), dec!(0.08)]);
    for _ in 0..3 {
        mp.accumulate_with_edges(dec!(-0.5), dec!(0.01), dec!(-0.2));
        mp.accumulate_with_edges(dec!(0.5), dec!(0.05), dec!(0.3));
    }
    mp.finalize();
    let edges_before: Vec<Decimal> = mp.spread_edges().to_vec();

    for _ in 0..20 {
        mp.update_online(dec!(0.7), dec!(0.2), dec!(0.5));
    }
    assert_eq!(
        mp.spread_edges(),
        edges_before.as_slice(),
        "online fit must not mutate spread edges"
    );
}

#[test]
fn update_online_shifts_prediction_toward_new_observations() {
    // Fit a model on mildly positive data, then flood the
    // online ring with strongly negative observations at
    // the +0.75 imbalance bucket. After the refit, the
    // prediction for that imbalance should move negative.
    let mut mp = seed_finalised_fit();
    let before = mp.predict(dec!(0.75), dec!(0.01));
    assert!(
        before > dec!(0),
        "seed should produce positive prediction at +0.75, got {before}"
    );
    // Push enough observations to fill and refit the ring
    // a few times over. Stream length > ring capacity so
    // the original seed observations are pushed out.
    for _ in 0..60 {
        mp.update_online(dec!(0.75), dec!(0.01), dec!(-0.3));
    }
    let after = mp.predict(dec!(0.75), dec!(0.01));
    assert!(
        after < dec!(0),
        "online fit should flip prediction negative after 60 negative updates, got {after}"
    );
}

#[test]
fn update_online_matches_offline_fit_on_same_observations() {
    // Parity: pushing exactly the same observations through
    // the online path should yield an identical g-matrix to
    // accumulating them through the offline single-bucket
    // path (both applied on top of the same seed).
    //
    // Build two models with identical config, seed both the
    // same way, then on model A push 10 observations
    // through update_online; on model B wipe + rebuild
    // using direct accumulate on a fresh fit sharing the
    // same spread_edges. Assert prediction parity.
    let mut mp_online = seed_finalised_fit();
    let obs: Vec<(Decimal, Decimal, Decimal)> = (0..10)
        .map(|_| (dec!(0.75), dec!(0.01), dec!(-0.3)))
        .collect();
    for (i, s, d) in &obs {
        mp_online.update_online(*i, *s, *d);
    }
    // Build the offline equivalent: same config, direct
    // accumulate of just the 10 online obs (no seed — the
    // online path throws out the seed once the refit
    // happens against the ring contents).
    let cfg = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 1,
        min_bucket_samples: 2,
        online_ring_capacity: 50,
        refit_every: 10,
    };
    let mut mp_offline = LearnedMicroprice::empty(cfg);
    for (i, s, d) in &obs {
        mp_offline.accumulate(*i, *s, *d);
    }
    mp_offline.finalize_iterative(5);
    // All 10 observations land in bucket 3 (+0.75).
    // Parity check on the imbalance bucket that got data.
    assert_eq!(
        mp_online.predict(dec!(0.75), dec!(0.01)),
        mp_offline.predict(dec!(0.75), dec!(0.01)),
        "online refit parity failed"
    );
}

#[test]
fn toml_roundtrip_default_online_fields_preserved() {
    let cfg = LearnedMicropriceConfig {
        n_imbalance_buckets: 4,
        n_spread_buckets: 1,
        min_bucket_samples: 2,
        online_ring_capacity: 500,
        refit_every: 42,
    };
    let mut mp = LearnedMicroprice::empty(cfg);
    mp.accumulate(dec!(-0.5), dec!(0.01), dec!(-0.1));
    mp.accumulate(dec!(0.5), dec!(0.01), dec!(0.1));
    mp.accumulate(dec!(-0.5), dec!(0.01), dec!(-0.2));
    mp.accumulate(dec!(0.5), dec!(0.01), dec!(0.2));
    mp.finalize();
    let path = tmp_path("online_fields");
    mp.to_toml(&path).expect("write");
    let reloaded = LearnedMicroprice::from_toml(&path).expect("read");
    // The online fields are `#[serde(skip)]` so they
    // default back on load — but the *config* fields
    // (ring_capacity + refit_every) must round-trip
    // exactly since they are serialised.
    assert_eq!(reloaded.config.online_ring_capacity, 500);
    assert_eq!(reloaded.config.refit_every, 42);
    // Online state resets to empty.
    assert_eq!(reloaded.online_ring_len(), 0);
    assert_eq!(reloaded.online_counter(), 0);
    let _ = std::fs::remove_file(&path);
}
