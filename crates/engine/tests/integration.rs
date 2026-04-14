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
