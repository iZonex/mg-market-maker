//! Integration tests: test the full pipeline from market data → strategy → quotes.

use mm_common::config::*;
use mm_common::orderbook::LocalOrderBook;
use mm_common::types::*;
use mm_risk::kill_switch::{KillLevel, KillSwitch, KillSwitchConfig};
use mm_risk::pnl::PnlTracker;
use mm_risk::sla::{SlaConfig, SlaTracker};
use mm_risk::toxicity::VpinEstimator;
use mm_strategy::autotune::AutoTuner;
use mm_strategy::inventory_skew::AdvancedInventoryManager;
use mm_strategy::momentum::MomentumSignals;
use mm_strategy::r#trait::{Strategy, StrategyContext};
use mm_strategy::volatility::VolatilityEstimator;
use mm_strategy::{AvellanedaStoikov, GlftStrategy, GridStrategy};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn test_product() -> ProductSpec {
    ProductSpec {
        symbol: "BTCUSDT".into(),
        base_asset: "BTC".into(),
        quote_asset: "USDT".into(),
        tick_size: dec!(0.01),
        lot_size: dec!(0.00001),
        min_notional: dec!(10),
        maker_fee: dec!(0.001),
        taker_fee: dec!(0.002),
        trading_status: Default::default(),
    }
}

fn test_config() -> MarketMakerConfig {
    MarketMakerConfig {
        gamma: dec!(0.1),
        kappa: dec!(1.5),
        sigma: dec!(0.02),
        time_horizon_secs: 300,
        num_levels: 3,
        order_size: dec!(0.001),
        refresh_interval_ms: 500,
        min_spread_bps: dec!(5),
        max_distance_bps: dec!(100),
        strategy: StrategyType::AvellanedaStoikov,
        momentum_enabled: true,
        momentum_window: 200,
        basis_shift: dec!(0.5),
        market_resilience_enabled: true,
        otr_enabled: true,
        hma_enabled: true,
        hma_window: 9,
        momentum_ofi_enabled: false,
        momentum_learned_microprice_path: None,
        momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
        user_stream_enabled: true,
        inventory_drift_tolerance: dec!(0.0001),
        inventory_drift_auto_correct: false,
        amend_enabled: true,
        amend_max_ticks: 2,
        fee_tier_refresh_enabled: true,
        fee_tier_refresh_secs: 600,
        borrow_enabled: false,
        borrow_rate_refresh_secs: 1800,
        borrow_holding_secs: 3600,
        borrow_max_base: dec!(0),
        borrow_buffer_base: dec!(0),
        pair_lifecycle_enabled: true,
        pair_lifecycle_refresh_secs: 300,
        var_guard_enabled: false,
        var_guard_limit_95: None,
        var_guard_limit_99: None,
        var_guard_ewma_lambda: None,
        cross_venue_basis_max_staleness_ms: 1500, sor_inline_enabled: false,
    }
}

fn test_book() -> LocalOrderBook {
    let mut book = LocalOrderBook::new("BTCUSDT".into());
    book.apply_snapshot(
        vec![
            PriceLevel {
                price: dec!(50000),
                qty: dec!(5),
            },
            PriceLevel {
                price: dec!(49999),
                qty: dec!(3),
            },
            PriceLevel {
                price: dec!(49998),
                qty: dec!(2),
            },
        ],
        vec![
            PriceLevel {
                price: dec!(50001),
                qty: dec!(5),
            },
            PriceLevel {
                price: dec!(50002),
                qty: dec!(3),
            },
            PriceLevel {
                price: dec!(50003),
                qty: dec!(2),
            },
        ],
        1,
    );
    book
}

/// Test: full pipeline book → strategy → quotes for all 3 strategies.
#[test]
fn test_all_strategies_produce_valid_quotes() {
    let book = test_book();
    let product = test_product();
    let config = test_config();
    let mid = book.mid_price().unwrap();

    let strategies: Vec<Box<dyn Strategy>> = vec![
        Box::new(AvellanedaStoikov),
        Box::new(GlftStrategy::new()),
        Box::new(GridStrategy),
    ];

    for strategy in &strategies {
        let ctx = StrategyContext {
            book: &book,
            product: &product,
            config: &config,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: mid,
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        };

        let quotes = strategy.compute_quotes(&ctx);
        assert!(!quotes.is_empty(), "{} produced no quotes", strategy.name());

        for (i, q) in quotes.iter().enumerate() {
            if let Some(bid) = &q.bid {
                assert!(
                    bid.price > dec!(0),
                    "{} bid {} price <= 0",
                    strategy.name(),
                    i
                );
                assert!(
                    bid.price < mid,
                    "{} bid {} price >= mid",
                    strategy.name(),
                    i
                );
                assert!(bid.qty > dec!(0), "{} bid {} qty <= 0", strategy.name(), i);
            }
            if let Some(ask) = &q.ask {
                assert!(
                    ask.price > dec!(0),
                    "{} ask {} price <= 0",
                    strategy.name(),
                    i
                );
                assert!(
                    ask.price > mid,
                    "{} ask {} price <= mid",
                    strategy.name(),
                    i
                );
                assert!(ask.qty > dec!(0), "{} ask {} qty <= 0", strategy.name(), i);
            }
        }
    }
}

/// Test: inventory skew actually skews quotes in the right direction.
#[test]
fn test_inventory_skew_direction() {
    let book = test_book();
    let product = test_product();
    let config = test_config();
    let mid = book.mid_price().unwrap();

    let strategy = AvellanedaStoikov;

    // No inventory — symmetric.
    let ctx_neutral = StrategyContext {
        book: &book,
        product: &product,
        config: &config,
        inventory: dec!(0),
        volatility: dec!(0.02),
        time_remaining: dec!(1),
        mid_price: mid,
        ref_price: None,
        hedge_book: None,
        borrow_cost_bps: None,
        hedge_book_age_ms: None,
        as_prob: None,
        as_prob_bid: None,
        as_prob_ask: None,
    };
    let q_neutral = strategy.compute_quotes(&ctx_neutral);
    let neutral_ask = q_neutral[0].ask.as_ref().unwrap().price;
    let neutral_bid = q_neutral[0].bid.as_ref().unwrap().price;

    // Long inventory — should push quotes DOWN (reservation price lower).
    // Use large inventory + high gamma to ensure measurable skew.
    let ctx_long = StrategyContext {
        book: &book,
        product: &product,
        config: &config,
        inventory: dec!(5.0),
        volatility: dec!(0.1),
        time_remaining: dec!(1),
        mid_price: mid,
        ref_price: None,
        hedge_book: None,
        borrow_cost_bps: None,
        hedge_book_age_ms: None,
        as_prob: None,
        as_prob_bid: None,
        as_prob_ask: None,
    };
    let q_long = strategy.compute_quotes(&ctx_long);
    let long_ask = q_long[0].ask.as_ref().unwrap().price;
    let long_bid = q_long[0].bid.as_ref().unwrap().price;

    assert!(
        long_ask <= neutral_ask,
        "long inventory should lower or equal ask: long={long_ask} neutral={neutral_ask}"
    );
    // Bid may hit the max_distance floor, so check mid-point instead.
    let long_mid_quote = (long_bid + long_ask) / dec!(2);
    let neutral_mid_quote = (neutral_bid + neutral_ask) / dec!(2);
    assert!(
        long_mid_quote < neutral_mid_quote,
        "long inventory should shift quote center down"
    );

    // Short inventory — should push quotes UP.
    let ctx_short = StrategyContext {
        book: &book,
        product: &product,
        config: &config,
        inventory: dec!(-5.0),
        volatility: dec!(0.1),
        time_remaining: dec!(1),
        mid_price: mid,
        ref_price: None,
        hedge_book: None,
        borrow_cost_bps: None,
        hedge_book_age_ms: None,
        as_prob: None,
        as_prob_bid: None,
        as_prob_ask: None,
    };
    let q_short = strategy.compute_quotes(&ctx_short);
    let short_ask = q_short[0].ask.as_ref().unwrap().price;
    let short_bid = q_short[0].bid.as_ref().unwrap().price;

    assert!(
        short_ask >= neutral_ask,
        "short inventory should raise or equal ask"
    );
    let short_mid_quote = (short_bid + short_ask) / dec!(2);
    assert!(
        short_mid_quote > neutral_mid_quote,
        "short inventory should shift quote center up"
    );
}

/// Test: kill switch escalation + spread/size multipliers.
#[test]
fn test_kill_switch_affects_quotes() {
    let config = KillSwitchConfig {
        daily_loss_limit: dec!(100),
        daily_loss_warning: dec!(50),
        ..Default::default()
    };
    let mut ks = KillSwitch::new(config);

    // Normal — multipliers are 1.
    assert_eq!(ks.spread_multiplier(), dec!(1));
    assert_eq!(ks.size_multiplier(), dec!(1));
    assert!(ks.allow_new_orders());

    // Warning level — spread 2x, size 0.5x.
    ks.update_pnl(dec!(-60));
    assert_eq!(ks.level(), KillLevel::WidenSpreads);
    assert_eq!(ks.spread_multiplier(), dec!(2));
    assert_eq!(ks.size_multiplier(), dec!(0.5));
    assert!(ks.allow_new_orders());

    // Cancel all — no new orders.
    ks.update_pnl(dec!(-110));
    assert_eq!(ks.level(), KillLevel::CancelAll);
    assert!(!ks.allow_new_orders());
}

/// Test: VPIN detects toxic flow and auto-tuner reacts.
#[test]
fn test_toxicity_widens_spread() {
    let mut vpin = VpinEstimator::new(dec!(1000), 10);
    let mut auto_tuner = AutoTuner::new(50);

    // Feed completely one-sided flow.
    for _ in 0..100 {
        vpin.on_trade(&Trade {
            trade_id: 1,
            symbol: "BTCUSDT".into(),
            price: dec!(50000),
            qty: dec!(0.1),
            taker_side: Side::Buy,
            timestamp: chrono::Utc::now(),
        });
    }

    let v = vpin.vpin().unwrap();
    assert!(v > dec!(0.8), "VPIN should be high for one-sided flow");

    auto_tuner.set_toxicity(v);
    let spread_mult = auto_tuner.effective_spread_mult();
    assert!(
        spread_mult > dec!(1.5),
        "toxic flow should widen spread multiplier"
    );
}

/// Test: momentum signals shift mid price.
#[test]
fn test_momentum_shifts_mid() {
    let book = test_book();
    let mid = book.mid_price().unwrap();
    let mut momentum = MomentumSignals::new(100);

    // Feed heavy buy pressure.
    for _ in 0..50 {
        momentum.on_trade(&Trade {
            trade_id: 1,
            symbol: "BTCUSDT".into(),
            price: dec!(50001),
            qty: dec!(1),
            taker_side: Side::Buy,
            timestamp: chrono::Utc::now(),
        });
    }

    let alpha = momentum.alpha(&book, mid);
    // Heavy buy flow + more bids → positive alpha.
    // Alpha shifts mid upward.
    let adjusted = mid + alpha * mid;
    // With balanced book imbalance but all-buy trade flow,
    // alpha should be positive.
    assert!(
        alpha > dec!(0),
        "heavy buy flow should produce positive alpha"
    );
    assert!(adjusted > mid, "adjusted mid should be above raw mid");
}

/// Test: SLA tracker records compliance correctly.
#[test]
fn test_sla_tracking() {
    let config = SlaConfig {
        max_spread_bps: dec!(100),
        min_depth_quote: dec!(100),
        min_uptime_pct: dec!(90),
        two_sided_required: true,
        max_requote_secs: 5,
        min_order_rest_secs: 3,
    };
    let mut sla = SlaTracker::new(config);

    // Compliant tick.
    sla.update_quotes(true, true, Some(dec!(50)), dec!(200), dec!(200));
    sla.tick();
    assert_eq!(sla.uptime_pct(), dec!(100));

    // Non-compliant: one-sided.
    sla.update_quotes(true, false, Some(dec!(50)), dec!(200), dec!(0));
    sla.tick();
    // 1 compliant out of 2 = 50%.
    assert_eq!(sla.uptime_pct(), dec!(50));
}

/// Test: PnL attribution after buy then sell.
#[test]
fn test_pnl_round_trip() {
    let mut pnl = PnlTracker::new(dec!(-0.001), dec!(0.002)); // Maker rebate.
    let mid = dec!(50000);

    let buy_fill = Fill {
        trade_id: 1,
        order_id: uuid::Uuid::new_v4(),
        symbol: "BTCUSDT".into(),
        side: Side::Buy,
        price: dec!(49990),
        qty: dec!(0.01),
        is_maker: true,
        timestamp: chrono::Utc::now(),
    };

    let sell_fill = Fill {
        trade_id: 2,
        order_id: uuid::Uuid::new_v4(),
        symbol: "BTCUSDT".into(),
        side: Side::Sell,
        price: dec!(50010),
        qty: dec!(0.01),
        is_maker: true,
        timestamp: chrono::Utc::now(),
    };

    pnl.on_fill(&buy_fill, mid);
    pnl.on_fill(&sell_fill, mid);

    // Spread capture: bought 10 below mid + sold 10 above mid = 0.2 total.
    assert!(
        pnl.attribution.spread_pnl > dec!(0),
        "spread capture should be positive"
    );
    // Rebate: two fills with maker rebate.
    assert!(
        pnl.attribution.rebate_income > dec!(0),
        "rebates should be positive"
    );
    // Round trip completed.
    assert_eq!(pnl.attribution.round_trips, 1);
}

/// Test: advanced inventory dynamic sizing.
#[test]
fn test_adv_inventory_sizing() {
    let adv = AdvancedInventoryManager::new(dec!(1.0));
    let base = dec!(0.01);

    // No inventory — full size both sides.
    let buy = adv.dynamic_size(base, dec!(0), Side::Buy);
    let sell = adv.dynamic_size(base, dec!(0), Side::Sell);
    assert_eq!(buy, base);
    assert_eq!(sell, base);

    // Long 80% of max — buy size heavily reduced, sell increased.
    let buy = adv.dynamic_size(base, dec!(0.8), Side::Buy);
    let sell = adv.dynamic_size(base, dec!(0.8), Side::Sell);
    assert!(buy < base * dec!(0.3), "buy size should be heavily reduced");
    assert!(sell > base, "sell size should be increased");
}

/// Test: volatility estimator converges.
#[test]
fn test_volatility_estimation() {
    let mut vol = VolatilityEstimator::new(dec!(0.94), dec!(1));

    // Feed 100 prices with small random-ish fluctuations.
    for i in 0..100 {
        let offset = dec!(0.1) * rust_decimal::Decimal::from(i % 7) - dec!(0.3);
        vol.update(dec!(50000) + offset);
    }

    let v = vol.volatility();
    assert!(v.is_some(), "should have volatility estimate");
    assert!(v.unwrap() > dec!(0), "volatility should be positive");
}

// ---------------------------------------------------------------------------
// End-to-end Market Resilience → AutoTuner → effective_spread_mult wiring.
//
// Proves that a just-happened large trade flows through the full signal
// chain and actually widens the book for the strategy. Unit tests in the
// individual modules cover correctness of each stage; this test is the
// glue check that we didn't break the plumbing between them.
// ---------------------------------------------------------------------------

/// Feed a synthetic trade + book stream through a
/// `MarketResilienceCalculator` paired with an `AutoTuner`.
/// After a large trade the effective spread multiplier must
/// widen relative to the no-shock baseline, and after the MR
/// decay window elapses it must return to the baseline.
#[test]
fn large_trade_widens_autotuner_spread_mult_and_recovers() {
    use mm_common::types::PriceLevel;
    use mm_strategy::market_resilience::{MarketResilienceCalculator, MrConfig};

    // Small config so warmup isn't 200 samples.
    let mr_config = MrConfig {
        warmup_samples: 5,
        shock_timeout_ns: 1_000_000_000,
        // 2 seconds decay so the test can assert both widening
        // and recovery on a compressed timeline.
        decay_window_ns: 2_000_000_000,
        ..MrConfig::default()
    };
    let mut mr = MarketResilienceCalculator::new(mr_config);
    let mut tuner = AutoTuner::new(32);

    // Baseline spread multiplier with no MR attached.
    let baseline = tuner.effective_spread_mult();

    // Warmup the detector: 50 small trades + book snapshots at
    // t=0..50ms, tight spread (0.1 units), deep levels.
    let deep_bids = vec![
        PriceLevel {
            price: dec!(100.0),
            qty: dec!(50),
        },
        PriceLevel {
            price: dec!(99.9),
            qty: dec!(40),
        },
        PriceLevel {
            price: dec!(99.8),
            qty: dec!(30),
        },
    ];
    let deep_asks = vec![
        PriceLevel {
            price: dec!(100.1),
            qty: dec!(50),
        },
        PriceLevel {
            price: dec!(100.2),
            qty: dec!(40),
        },
        PriceLevel {
            price: dec!(100.3),
            qty: dec!(30),
        },
    ];
    for i in 0..50_i64 {
        mr.on_trade(dec!(1), i * 100_000);
        mr.on_book(&deep_bids, &deep_asks, i * 100_000);
    }

    // Shock: huge trade + widened ask side at t = 60ms.
    mr.on_trade(dec!(100), 60_000_000);
    let wide_asks = vec![
        PriceLevel {
            price: dec!(105.0),
            qty: dec!(50),
        },
        PriceLevel {
            price: dec!(106.0),
            qty: dec!(40),
        },
    ];
    mr.on_book(&deep_bids, &wide_asks, 60_000_000);
    // Spread recovery: ask side returns to normal at t = 80ms.
    mr.on_book(&deep_bids, &deep_asks, 80_000_000);

    // At this point MR should have finalised a score below 1.0.
    let shock_ns = 80_000_000;
    let shock_score = mr.score(shock_ns);
    assert!(
        shock_score < Decimal::ONE,
        "MR must drop below 1.0 after shock: got {shock_score}"
    );

    // Push the score into the autotuner and check the spread
    // multiplier widens.
    tuner.set_market_resilience(shock_score);
    let widened = tuner.effective_spread_mult();
    assert!(
        widened > baseline,
        "autotuner spread mult must widen after shock: baseline={baseline}, widened={widened}"
    );

    // After the decay window, the score recovers to 1.0 and
    // the multiplier returns to the baseline.
    let recovered_ns = shock_ns + 3_000_000_000; // 3 s later, past decay window (2 s)
    let recovered_score = mr.score(recovered_ns);
    assert_eq!(
        recovered_score,
        Decimal::ONE,
        "MR score must decay back to 1.0 past the decay window"
    );
    tuner.set_market_resilience(recovered_score);
    let restored = tuner.effective_spread_mult();
    assert_eq!(
        restored, baseline,
        "autotuner spread mult must return to baseline after MR recovery"
    );
}

// ---------------------------------------------------------------------------
// End-to-end Binance listen-key user-data stream parsing → BalanceCache.
//
// Proves the invariant the engine's `handle_ws_event::BalanceUpdate`
// branch relies on: a WS frame from the listen-key stream parses into
// a `MarketEvent::BalanceUpdate`, and plugging that event into a
// fresh `BalanceCache::update_from_exchange` produces a cache whose
// `available_in(asset, wallet)` reads back the frame's free balance.
//
// This is the first-class test for P0.1 (ROADMAP): without it, the
// out-of-band fill path we just wired through `spawn_event_merger`
// in `mm-server::main` has no regression anchor.
// ---------------------------------------------------------------------------

/// Binance spot `outboundAccountPosition` frame → engine-visible
/// balance. The frame carries two assets (BTC + USDT) with distinct
/// free/locked splits; the cache must reflect them independently.
#[test]
fn binance_user_stream_frame_feeds_balance_cache() {
    use mm_common::types::WalletType;
    use mm_engine::balance_cache::BalanceCache;
    use mm_exchange_binance::user_stream::{parse_user_event_for_test, UserStreamProduct};
    use mm_exchange_core::events::MarketEvent;

    // Hand-crafted snapshot matching the Binance docs' example
    // shape for `outboundAccountPosition`. Two assets, one free,
    // one with a locked portion.
    let frame = serde_json::json!({
        "e": "outboundAccountPosition",
        "E": 1_710_000_000_000_u64,
        "u": 1_710_000_000_001_u64,
        "B": [
            { "a": "BTC", "f": "0.5", "l": "0.0" },
            { "a": "USDT", "f": "1000.0", "l": "250.0" }
        ]
    });

    let events = parse_user_event_for_test(&frame, UserStreamProduct::Spot);
    assert_eq!(events.len(), 2, "expected one BalanceUpdate per asset");

    // Plug both events into a fresh cache via the same code path
    // `market_maker.rs::handle_ws_event` uses — `update_from_exchange`
    // with a single-element `Balance` slice per event.
    let mut cache = BalanceCache::new_for(WalletType::Spot);
    for ev in &events {
        if let MarketEvent::BalanceUpdate {
            asset,
            wallet,
            total,
            locked,
            available,
            ..
        } = ev
        {
            cache.update_from_exchange(&[mm_common::types::Balance {
                asset: asset.clone(),
                wallet: *wallet,
                total: *total,
                locked: *locked,
                available: *available,
            }]);
        }
    }

    assert_eq!(
        cache.available_in("BTC", WalletType::Spot),
        dec!(0.5),
        "BTC free balance must be surfaced into the spot wallet slot"
    );
    assert_eq!(
        cache.available_in("USDT", WalletType::Spot),
        dec!(1000),
        "USDT free balance must carry over from the WS frame"
    );
}

/// Bybit V5 private WS `wallet` topic frame → engine-visible
/// balance. Mirrors `binance_user_stream_frame_feeds_balance_cache`
/// for the second venue the listen-key wiring now covers (P0.1).
/// Asserts that the parser, the `MarketEvent::BalanceUpdate` event
/// type, and the `BalanceCache::update_from_exchange` chain all
/// agree on the wallet bucket Bybit V5 UTA reports under.
#[test]
fn bybit_private_wallet_frame_feeds_balance_cache() {
    use mm_common::types::WalletType;
    use mm_engine::balance_cache::BalanceCache;
    use mm_exchange_bybit::user_stream::parse_user_event_for_test;
    use mm_exchange_core::events::MarketEvent;

    // V5 UTA wallet snapshot — `walletBalance` is the total,
    // `availableToWithdraw` is the post-IM available, and
    // `totalOrderIM` is the locked collateral. Two coins so we
    // catch any per-asset crosstalk in the cache.
    let frame = serde_json::json!({
        "topic": "wallet",
        "data": [{
            "accountType": "UNIFIED",
            "coin": [
                {
                    "coin": "USDT",
                    "walletBalance": "1000",
                    "availableToWithdraw": "750",
                    "totalOrderIM": "250"
                },
                {
                    "coin": "BTC",
                    "walletBalance": "0.5",
                    "availableToWithdraw": "0.5",
                    "totalOrderIM": "0"
                }
            ]
        }]
    });

    let events = parse_user_event_for_test(&frame, WalletType::Unified);
    assert_eq!(events.len(), 2, "expected one BalanceUpdate per coin");

    let mut cache = BalanceCache::new_for(WalletType::Unified);
    for ev in &events {
        if let MarketEvent::BalanceUpdate {
            asset,
            wallet,
            total,
            locked,
            available,
            ..
        } = ev
        {
            cache.update_from_exchange(&[mm_common::types::Balance {
                asset: asset.clone(),
                wallet: *wallet,
                total: *total,
                locked: *locked,
                available: *available,
            }]);
        }
    }

    assert_eq!(
        cache.available_in("USDT", WalletType::Unified),
        dec!(750),
        "USDT availableToWithdraw must surface as the unified-wallet available balance",
    );
    assert_eq!(
        cache.available_in("BTC", WalletType::Unified),
        dec!(0.5),
        "BTC availableToWithdraw must carry over from the wallet frame",
    );
}

/// HyperLiquid `webData2` perp frame → engine-visible USDC balance.
/// Third venue in the P0.1 cluster: HL multiplexes its private
/// stream onto the same WS connection as the public feed, so the
/// gap was the missing `webData2` subscription rather than an
/// entire missing user-stream module. This test pins the
/// `webData2` → `BalanceUpdate` → `BalanceCache::available_in`
/// pipe so a future schema drift fails the build before it
/// silently desyncs an HL bot.
#[test]
fn hl_webdata2_frame_feeds_balance_cache() {
    use mm_common::types::WalletType;
    use mm_engine::balance_cache::BalanceCache;
    use mm_exchange_core::events::MarketEvent;
    use mm_exchange_hyperliquid::parse_hl_event_for_test;

    let frame = serde_json::json!({
        "channel": "webData2",
        "data": {
            "user": "0xdeadbeef",
            "clearinghouseState": {
                "withdrawable": "750.50",
                "marginSummary": { "accountValue": "1000.00" }
            }
        }
    });

    let events = parse_hl_event_for_test(&frame, false);
    assert_eq!(events.len(), 1, "expected a single USDC BalanceUpdate");

    let mut cache = BalanceCache::new_for(WalletType::UsdMarginedFutures);
    for ev in &events {
        if let MarketEvent::BalanceUpdate {
            asset,
            wallet,
            total,
            locked,
            available,
            ..
        } = ev
        {
            cache.update_from_exchange(&[mm_common::types::Balance {
                asset: asset.clone(),
                wallet: *wallet,
                total: *total,
                locked: *locked,
                available: *available,
            }]);
        }
    }

    assert_eq!(
        cache.available_in("USDC", WalletType::UsdMarginedFutures),
        dec!(750.50),
        "withdrawable must surface as the perp-collateral available balance",
    );
    assert_eq!(
        cache.total_in("USDC", WalletType::UsdMarginedFutures),
        dec!(1000.00),
        "accountValue must surface as the perp-collateral total balance",
    );
}

// ---------------------------------------------------------------------------
// End-to-end inventory-vs-wallet drift detection.
//
// Proves the loop that ties `BalanceCache.total_in(base, Spot)` into the
// `InventoryDriftReconciler` and fires a `DriftReport` when the tracked
// inventory diverges from the wallet delta since the baseline reconcile.
// This is the regression anchor for P0.2 — without it the silent-drift
// failure mode from the ROADMAP audit has nothing to pin on.
// ---------------------------------------------------------------------------

/// Baseline reconcile captures the starting wallet balance.
/// A subsequent reconcile with a mismatched tracker (because a
/// BUY fill was dropped by a listen-key gap) must surface a
/// drift report with the correct signed value.
#[test]
fn baseline_then_missed_buy_surfaces_drift_report() {
    use mm_common::types::{Balance, WalletType};
    use mm_engine::balance_cache::BalanceCache;
    use mm_risk::inventory_drift::InventoryDriftReconciler;

    let mut cache = BalanceCache::new_for(WalletType::Spot);
    let mut reconciler = InventoryDriftReconciler::new("BTC", dec!(0.0001), false);

    // Baseline: wallet starts at exactly 1 BTC. Tracker = 0
    // (engine just started, no fills yet).
    cache.update_from_exchange(&[Balance {
        asset: "BTC".into(),
        wallet: WalletType::Spot,
        total: dec!(1),
        locked: dec!(0),
        available: dec!(1),
    }]);
    let first = reconciler.check(cache.total_in("BTC", WalletType::Spot), Decimal::ZERO);
    assert!(first.is_none(), "baseline reconcile must return None");

    // The venue processes a 0.2 BTC BUY fill but the listen-key
    // stream dropped the executionReport frame. Wallet refresh
    // on the next reconcile picks up the real state; tracker
    // never heard about the fill.
    cache.update_from_exchange(&[Balance {
        asset: "BTC".into(),
        wallet: WalletType::Spot,
        total: dec!(1.2),
        locked: dec!(0),
        available: dec!(1.2),
    }]);
    let report = reconciler
        .check(cache.total_in("BTC", WalletType::Spot), Decimal::ZERO)
        .expect("missed BUY fill must produce a drift report");
    assert_eq!(report.asset, "BTC");
    assert_eq!(report.baseline_wallet, dec!(1));
    assert_eq!(report.current_wallet, dec!(1.2));
    assert_eq!(report.expected_inventory, dec!(0.2));
    assert_eq!(report.tracked_inventory, dec!(0));
    assert_eq!(report.drift, dec!(0.2));
    assert!(!report.corrected, "alert-only mode keeps corrected = false");
}

/// With `auto_correct = true`, the report's `corrected` flag
/// flips on and `InventoryManager::force_reset_inventory_to`
/// is a single-shot self-heal the caller invokes.
#[test]
fn auto_correct_drift_force_resets_inventory_manager() {
    use mm_common::types::{Balance, WalletType};
    use mm_engine::balance_cache::BalanceCache;
    use mm_risk::inventory::InventoryManager;
    use mm_risk::inventory_drift::InventoryDriftReconciler;

    let mut cache = BalanceCache::new_for(WalletType::Spot);
    let mut reconciler = InventoryDriftReconciler::new("BTC", dec!(0.0001), true);
    let mut inventory = InventoryManager::new();

    cache.update_from_exchange(&[Balance {
        asset: "BTC".into(),
        wallet: WalletType::Spot,
        total: dec!(2),
        locked: dec!(0),
        available: dec!(2),
    }]);
    // Establish baseline.
    reconciler.check(
        cache.total_in("BTC", WalletType::Spot),
        inventory.inventory(),
    );

    // Wallet went to 1.75 (a 0.25 BTC sell actually happened
    // on the venue) but tracker never saw the fill.
    cache.update_from_exchange(&[Balance {
        asset: "BTC".into(),
        wallet: WalletType::Spot,
        total: dec!(1.75),
        locked: dec!(0),
        available: dec!(1.75),
    }]);
    let report = reconciler
        .check(
            cache.total_in("BTC", WalletType::Spot),
            inventory.inventory(),
        )
        .expect("missed SELL fill must produce a drift report");
    assert_eq!(report.drift, dec!(-0.25));
    assert!(report.corrected);

    // Apply the correction — the engine's `check_inventory_drift`
    // helper does exactly this when `corrected = true`.
    inventory.force_reset_inventory_to(report.expected_inventory);
    assert_eq!(inventory.inventory(), dec!(-0.25));
}
