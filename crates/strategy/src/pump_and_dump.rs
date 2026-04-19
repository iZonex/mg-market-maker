//! Epic R Phase 2 — pump-and-dump exploit orchestrator.
//! **PENTEST ONLY.**
//!
//! Four-phase finite-state machine that reproduces the
//! RAVE / SIREN / MYX pattern on a test venue so the defensive
//! [`mm_risk::manipulation`] detectors can be validated against
//! the offensive side under controlled conditions. Never ship
//! with `MM_ALLOW_RESTRICTED` enabled on a real production
//! deployment — the same gate that blocks the bundled
//! `pentest-spoof-classic` template gates this strategy.
//!
//! # Phase FSM (tick-driven)
//!
//! ```text
//!   Accumulate ─┐
//!       │       │  build inventory quietly at / just below mid
//!       ▼       │  (passive post-only bids, size = accumulate_size)
//!   Pump ──────┼─ aggressive cross-through buy pressure
//!       │       │  price pushed up by `pump_depth_bps` per burst
//!       ▼       │
//!   Distribute ┼─ post ascending ask ladder above the new mid
//!       │       │  to dump acquired inventory into the FOMO flow
//!       ▼       │
//!   Dump ──────┘  last-mile exit: aggressive cross-through sells
//! ```
//!
//! Phases advance by tick counts. The caller configures
//! `accumulate_ticks`, `pump_ticks`, `distribute_ticks`,
//! `dump_ticks`; the FSM wraps after the last one so the same
//! strategy instance can run successive cycles in a smoke test.
//!
//! # Intentional limitations
//!
//! - The FSM doesn't look at realised inventory — once
//!   `accumulate_ticks` fires it moves on, regardless of what
//!   actually filled. This keeps the exploit test-reproducible.
//! - `Accumulate` emits passive bids only (no asks). A real
//!   MM-like PostOnly invariant is preserved — the engine's
//!   own risk controls catch abuse the same way they would for
//!   a honest quote.
//! - The offensive cross-through orders use the same "deep
//!   limit into the opposite book" trick as
//!   [`crate::ignite::IgniteStrategy`] — we don't add a new
//!   market-order code path just for pentest.

use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::r#trait::{Strategy, StrategyContext};

/// Which phase the FSM is currently in. Reported via
/// [`PumpAndDumpStrategy::current_phase`] for test assertions
/// and panel visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpDumpPhase {
    Accumulate,
    Pump,
    Distribute,
    Dump,
}

#[derive(Debug, Clone)]
pub struct PumpAndDumpConfig {
    /// Ticks the strategy spends accumulating inventory.
    pub accumulate_ticks: u64,
    /// Per-tick passive bid size in the accumulate phase.
    pub accumulate_size: Decimal,
    /// Offset below mid (bps) for the accumulate bid. `0` =
    /// post at mid; negative not allowed.
    pub accumulate_offset_bps: Decimal,

    /// Ticks the strategy spends aggressively pumping.
    pub pump_ticks: u64,
    /// Per-tick aggressive-buy qty during pump.
    pub pump_size: Decimal,
    /// How deep across the ask touch the pump limit goes, in
    /// bps. Higher = more aggressive, consumes more levels.
    pub pump_depth_bps: Decimal,

    /// Ticks the strategy spends distributing (laddered asks).
    pub distribute_ticks: u64,
    /// Qty per ladder rung during distribute.
    pub distribute_size: Decimal,
    /// Bps above mid for the lowest rung. Each subsequent rung
    /// sits one rung higher by the same step.
    pub distribute_offset_bps: Decimal,
    /// Number of ladder rungs (simultaneous ask quotes).
    pub distribute_rungs: u32,
    /// Spacing between ladder rungs, in bps.
    pub distribute_step_bps: Decimal,

    /// Ticks the strategy spends dumping the remainder
    /// aggressively.
    pub dump_ticks: u64,
    /// Per-tick aggressive-sell qty during dump.
    pub dump_size: Decimal,
    /// How deep across the bid touch the dump limit goes, in
    /// bps.
    pub dump_depth_bps: Decimal,
}

impl Default for PumpAndDumpConfig {
    fn default() -> Self {
        Self {
            accumulate_ticks: 20,
            accumulate_size: dec!(0.002),
            accumulate_offset_bps: dec!(5),
            pump_ticks: 10,
            pump_size: dec!(0.002),
            pump_depth_bps: dec!(50),
            distribute_ticks: 20,
            distribute_size: dec!(0.001),
            distribute_offset_bps: dec!(20),
            distribute_rungs: 4,
            distribute_step_bps: dec!(15),
            dump_ticks: 10,
            dump_size: dec!(0.002),
            dump_depth_bps: dec!(60),
        }
    }
}

#[derive(Debug, Default)]
pub struct PumpAndDumpStrategy {
    pub config: PumpAndDumpConfig,
    /// Monotone tick counter. `compute_quotes` reads
    /// fetch-add so the trait's `&self` receiver (required by
    /// `Send + Sync`) stays honest.
    tick: AtomicU64,
}

impl PumpAndDumpStrategy {
    pub fn new() -> Self {
        Self {
            config: PumpAndDumpConfig::default(),
            tick: AtomicU64::new(0),
        }
    }

    pub fn with_config(config: PumpAndDumpConfig) -> Self {
        Self {
            config,
            tick: AtomicU64::new(0),
        }
    }

    fn cycle_len(&self) -> u64 {
        self.config.accumulate_ticks
            + self.config.pump_ticks
            + self.config.distribute_ticks
            + self.config.dump_ticks
    }

    /// Return the phase for a given tick index. `None` when
    /// the cycle length is zero (misconfigured).
    pub fn phase_at(&self, tick: u64) -> Option<PumpDumpPhase> {
        let cycle = self.cycle_len();
        if cycle == 0 {
            return None;
        }
        let t = tick % cycle;
        let a = self.config.accumulate_ticks;
        let p = a + self.config.pump_ticks;
        let d = p + self.config.distribute_ticks;
        Some(if t < a {
            PumpDumpPhase::Accumulate
        } else if t < p {
            PumpDumpPhase::Pump
        } else if t < d {
            PumpDumpPhase::Distribute
        } else {
            PumpDumpPhase::Dump
        })
    }

    /// Phase at the current tick counter (read-only).
    pub fn current_phase(&self) -> Option<PumpDumpPhase> {
        self.phase_at(self.tick.load(Ordering::Relaxed))
    }
}

impl Strategy for PumpAndDumpStrategy {
    fn name(&self) -> &str {
        "pump_and_dump"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let mid = ctx.mid_price;
        if mid <= Decimal::ZERO {
            return Vec::new();
        }
        let tick = self.tick.fetch_add(1, Ordering::Relaxed);
        let Some(phase) = self.phase_at(tick) else {
            return Vec::new();
        };
        match phase {
            PumpDumpPhase::Accumulate => {
                let offset = mid * self.config.accumulate_offset_bps / dec!(10_000);
                let price = ctx.product.round_price((mid - offset).max(Decimal::ZERO));
                let qty = ctx.product.round_qty(self.config.accumulate_size);
                if price.is_zero() || !ctx.product.meets_min_notional(price, qty) {
                    return Vec::new();
                }
                vec![QuotePair {
                    bid: Some(Quote { side: Side::Buy, price, qty }),
                    ask: None,
                }]
            }
            PumpDumpPhase::Pump => {
                // Cross the ask touch by `pump_depth_bps`.
                let cross = mid * self.config.pump_depth_bps / dec!(10_000);
                let price = ctx.product.round_price(mid + cross);
                let qty = ctx.product.round_qty(self.config.pump_size);
                if !ctx.product.meets_min_notional(price, qty) {
                    return Vec::new();
                }
                vec![QuotePair {
                    bid: Some(Quote { side: Side::Buy, price, qty }),
                    ask: None,
                }]
            }
            PumpDumpPhase::Distribute => {
                let rungs = self.config.distribute_rungs.max(1) as i64;
                let base_offset =
                    mid * self.config.distribute_offset_bps / dec!(10_000);
                let step = mid * self.config.distribute_step_bps / dec!(10_000);
                let qty = ctx.product.round_qty(self.config.distribute_size);
                let mut out = Vec::with_capacity(rungs as usize);
                for r in 0..rungs {
                    let price = ctx.product.round_price(
                        mid + base_offset + step * Decimal::from(r),
                    );
                    if price <= mid {
                        continue;
                    }
                    if !ctx.product.meets_min_notional(price, qty) {
                        continue;
                    }
                    out.push(QuotePair {
                        bid: None,
                        ask: Some(Quote { side: Side::Sell, price, qty }),
                    });
                }
                out
            }
            PumpDumpPhase::Dump => {
                let cross = mid * self.config.dump_depth_bps / dec!(10_000);
                let price = ctx.product.round_price((mid - cross).max(Decimal::ZERO));
                let qty = ctx.product.round_qty(self.config.dump_size);
                if price.is_zero() || !ctx.product.meets_min_notional(price, qty) {
                    return Vec::new();
                }
                vec![QuotePair {
                    bid: None,
                    ask: Some(Quote { side: Side::Sell, price, qty }),
                }]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::MarketMakerConfig;
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::{PriceLevel, ProductSpec};

    fn test_product() -> ProductSpec {
        ProductSpec {
            symbol: "RAVEUSDT".into(),
            base_asset: "RAVE".into(),
            quote_asset: "USDT".into(),
            tick_size: dec!(0.0001),
            lot_size: dec!(0.00001),
            // Sized so 0.001 RAVE @ $9-11 ≈ $0.01 clears the
            // min-notional check in the test config.
            min_notional: dec!(0.001),
            maker_fee: dec!(0.001),
            taker_fee: dec!(0.002),
            trading_status: Default::default(),
        }
    }

    fn test_cfg() -> MarketMakerConfig {
        // Minimal working config — the pump-and-dump FSM never
        // reads any of these knobs, but StrategyContext insists
        // on a `&MarketMakerConfig`. Copy the layout from
        // `ignite.rs::pt_cfg` so additions to the config struct
        // surface here too.
        MarketMakerConfig {
            gamma: dec!(0.1), kappa: dec!(1.5), sigma: dec!(0.02),
            time_horizon_secs: 300, num_levels: 1,
            order_size: dec!(0.01), refresh_interval_ms: 500,
            min_spread_bps: dec!(5), max_distance_bps: dec!(500),
            strategy: mm_common::config::StrategyType::Grid,
            momentum_enabled: false, momentum_window: 200,
            basis_shift: dec!(0.5), market_resilience_enabled: false,
            otr_enabled: false, hma_enabled: false,
            adaptive_enabled: false, apply_pair_class_template: false,
            hma_window: 9, momentum_ofi_enabled: false,
            momentum_learned_microprice_path: None,
            momentum_learned_microprice_pair_paths: std::collections::HashMap::new(),
            momentum_learned_microprice_online: false,
            momentum_learned_microprice_horizon: 10,
            user_stream_enabled: false,
            inventory_drift_tolerance: dec!(0.0001),
            inventory_drift_auto_correct: false,
            amend_enabled: false, amend_max_ticks: 2,
            margin_reduce_slice_pct: dec!(0.1),
            fee_tier_refresh_enabled: false, fee_tier_refresh_secs: 600,
            borrow_enabled: false, borrow_rate_refresh_secs: 1800,
            borrow_holding_secs: 3600, borrow_max_base: dec!(0),
            borrow_buffer_base: dec!(0),
            pair_lifecycle_enabled: false, pair_lifecycle_refresh_secs: 300,
            var_guard_enabled: false, var_guard_limit_95: None,
            var_guard_limit_99: None, var_guard_ewma_lambda: None,
            var_guard_cvar_limit_95: None, var_guard_cvar_limit_99: None,
            cross_venue_basis_max_staleness_ms: 1500,
            strategy_capital_budget: std::collections::HashMap::new(),
            symbol_circulating_supply: std::collections::HashMap::new(),
            cross_exchange_min_profit_bps: dec!(5),
            max_cross_venue_divergence_pct: None,
            sor_inline_enabled: false, sor_dispatch_interval_secs: 5,
            sor_urgency: dec!(0.4),
            sor_target_qty_source: mm_common::config::SorTargetSource::InventoryExcess,
            sor_inventory_threshold: Decimal::ZERO,
            sor_trade_rate_window_secs: 60, sor_queue_refresh_secs: 2,
        }
    }

    fn test_book() -> LocalOrderBook {
        let mut book = LocalOrderBook::new("RAVEUSDT".to_string());
        book.apply_snapshot(
            vec![PriceLevel { price: dec!(9.99), qty: dec!(100) }],
            vec![PriceLevel { price: dec!(10.01), qty: dec!(100) }],
            1,
        );
        book
    }

    fn tick_ctx<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        cfg: &'a MarketMakerConfig,
    ) -> StrategyContext<'a> {
        StrategyContext {
            book,
            product,
            config: cfg,
            inventory: dec!(0),
            volatility: dec!(0.02),
            time_remaining: dec!(1),
            mid_price: book.mid_price().unwrap(),
            ref_price: None,
            hedge_book: None,
            borrow_cost_bps: None,
            hedge_book_age_ms: None,
            as_prob: None,
            as_prob_bid: None,
            as_prob_ask: None,
        }
    }

    /// Phase FSM walks Accumulate → Pump → Distribute → Dump
    /// based on tick count.
    #[test]
    fn phases_advance_by_tick() {
        let s = PumpAndDumpStrategy::with_config(PumpAndDumpConfig {
            accumulate_ticks: 3,
            pump_ticks: 2,
            distribute_ticks: 4,
            dump_ticks: 2,
            ..PumpAndDumpConfig::default()
        });
        let product = test_product();
        let cfg = test_cfg();
        let book = test_book();
        let ctx = tick_ctx(&book, &product, &cfg);
        // First 3 ticks → Accumulate, passive bid only.
        for _ in 0..3 {
            let q = s.compute_quotes(&ctx);
            assert_eq!(q.len(), 1);
            assert!(q[0].bid.is_some() && q[0].ask.is_none());
            assert!(q[0].bid.as_ref().unwrap().price < ctx.mid_price);
        }
        // Next 2 ticks → Pump, bid priced ABOVE mid (cross).
        for _ in 0..2 {
            let q = s.compute_quotes(&ctx);
            assert_eq!(q.len(), 1);
            let bid = q[0].bid.as_ref().expect("pump emits a bid");
            assert!(bid.price > ctx.mid_price, "pump should cross the ask");
        }
        // Next 4 ticks → Distribute, 4 ask rungs per tick.
        for _ in 0..4 {
            let q = s.compute_quotes(&ctx);
            assert_eq!(q.len(), 4);
            for rung in &q {
                assert!(rung.ask.is_some() && rung.bid.is_none());
                assert!(rung.ask.as_ref().unwrap().price > ctx.mid_price);
            }
        }
        // Final 2 ticks → Dump, ask BELOW mid (cross).
        for _ in 0..2 {
            let q = s.compute_quotes(&ctx);
            assert_eq!(q.len(), 1);
            let ask = q[0].ask.as_ref().expect("dump emits an ask");
            assert!(ask.price < ctx.mid_price, "dump should cross the bid");
        }
    }

    /// After a full cycle the FSM wraps — useful so smoke-test
    /// runs can span multiple cycles without restart.
    #[test]
    fn cycle_wraps_after_full_pass() {
        let s = PumpAndDumpStrategy::with_config(PumpAndDumpConfig {
            accumulate_ticks: 1,
            pump_ticks: 1,
            distribute_ticks: 1,
            dump_ticks: 1,
            ..PumpAndDumpConfig::default()
        });
        assert_eq!(s.phase_at(0), Some(PumpDumpPhase::Accumulate));
        assert_eq!(s.phase_at(1), Some(PumpDumpPhase::Pump));
        assert_eq!(s.phase_at(2), Some(PumpDumpPhase::Distribute));
        assert_eq!(s.phase_at(3), Some(PumpDumpPhase::Dump));
        // Wraps.
        assert_eq!(s.phase_at(4), Some(PumpDumpPhase::Accumulate));
        assert_eq!(s.phase_at(9), Some(PumpDumpPhase::Pump));
    }

    /// Degenerate config (all-zero phases) → strategy is a
    /// no-op, never panics.
    #[test]
    fn zero_cycle_is_noop() {
        let s = PumpAndDumpStrategy::with_config(PumpAndDumpConfig {
            accumulate_ticks: 0,
            pump_ticks: 0,
            distribute_ticks: 0,
            dump_ticks: 0,
            ..PumpAndDumpConfig::default()
        });
        let product = test_product();
        let cfg = test_cfg();
        let book = test_book();
        let ctx = tick_ctx(&book, &product, &cfg);
        assert!(s.compute_quotes(&ctx).is_empty());
        assert_eq!(s.current_phase(), None);
    }
}
