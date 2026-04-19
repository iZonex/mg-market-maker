//! Epic R — Spoofing exploit strategy. **PENTEST ONLY.**
//!
//! Generates the classic spoofing silhouette the corresponding
//! detector in `mm-risk::surveillance` flags:
//!
//!   · tick N:   place a large order on the "pressure" side
//!               (several levels away from mid — the fake book
//!               the strategy wants other traders to react to)
//!               + a small order on the "real" side at the top
//!   · tick N+1: drop the pressure order from the quote bundle
//!               → the engine's order diff cancels it; the real
//!               order stays
//!
//! Order lifetime ≈ `refresh_interval_ms` (usually 500 ms), which
//! is well inside the 100 ms / 5× size bands the
//! `SpoofingDetector` uses by default — so running this strategy
//! against our own venue triggers our own surveillance on the
//! same tape. That's the point: a pentest should be a visible
//! attack, not a silent one.
//!
//! **Never ship into a real operator's TOML.** The composite
//! `Strategy.Spoof` graph node is marked `restricted: true`, which
//! the deploy handler refuses without `MM_ALLOW_RESTRICTED=yes-pentest-mode`.

use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::r#trait::{bps_to_frac, Strategy, StrategyContext};

/// Config knobs for [`SpoofStrategy`]. Defaults chosen so the
/// silhouette matches the reference detector's hot profile at
/// typical BTC mid prices.
#[derive(Debug, Clone)]
pub struct SpoofConfig {
    /// Which side bears the pressure. `Buy` prints a large bid
    /// to drive price up, `Sell` a large ask to drive it down.
    pub pressure_side: Side,
    /// Qty of the fake (cancel-bound) order as a multiple of
    /// `ctx.config.order_size`. Default 10× — well above the
    /// detector's 5× bar.
    pub pressure_size_mult: Decimal,
    /// How far from mid the fake order sits, in bps. Far enough
    /// that market-taking flow can't fill it in the one tick
    /// before cancel, close enough that others believe it.
    pub pressure_distance_bps: Decimal,
    /// Qty of the genuine (post-reaction) order as a multiple
    /// of `ctx.config.order_size`. Smaller — the point of the
    /// exploit is that we get filled cheaply once the market
    /// reacts to the fake.
    pub real_size_mult: Decimal,
    /// How far from mid the genuine order sits, in bps.
    pub real_distance_bps: Decimal,
}

impl Default for SpoofConfig {
    fn default() -> Self {
        Self {
            pressure_side: Side::Buy,
            pressure_size_mult: dec!(10),
            pressure_distance_bps: dec!(15),
            real_size_mult: dec!(1),
            real_distance_bps: dec!(3),
        }
    }
}

/// **Pentest-only.** See module docs.
///
/// Internal state: an alternating tick counter so every second
/// tick drops the pressure order. `AtomicU64` because the
/// `Strategy` trait requires `Send + Sync` — the engine may share
/// strategies across its async runtime.
#[derive(Debug, Default)]
pub struct SpoofStrategy {
    pub config: SpoofConfig,
    tick: AtomicU64,
}

impl SpoofStrategy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_config(config: SpoofConfig) -> Self {
        Self { config, tick: AtomicU64::new(0) }
    }
}

impl Strategy for SpoofStrategy {
    fn name(&self) -> &str {
        "spoof"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let mid = ctx.mid_price;
        if mid <= Decimal::ZERO {
            return Vec::new();
        }
        let base_size = ctx.config.order_size;
        let pressure_px_shift = bps_to_frac(self.config.pressure_distance_bps) * mid;
        let real_px_shift = bps_to_frac(self.config.real_distance_bps) * mid;

        let tick_n = self.tick.fetch_add(1, Ordering::Relaxed);
        let show_pressure = tick_n.is_multiple_of(2);

        let pressure_qty = ctx.product.round_qty(base_size * self.config.pressure_size_mult);
        let real_qty = ctx.product.round_qty(base_size * self.config.real_size_mult);

        let mut pairs: Vec<QuotePair> = Vec::with_capacity(2);

        // "Real" quote — the one we actually want filled. Opposite
        // side from the pressure (we push price with a fake bid,
        // capture with a real ask that was sitting quietly above).
        let real_side = match self.config.pressure_side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let real_px = match real_side {
            Side::Sell => ctx.product.round_price(mid + real_px_shift),
            Side::Buy => ctx.product.round_price(mid - real_px_shift),
        };
        if real_px > Decimal::ZERO
            && ctx.product.meets_min_notional(real_px, real_qty)
        {
            let real = Quote {
                side: real_side,
                price: real_px,
                qty: real_qty,
            };
            pairs.push(match real_side {
                Side::Buy => QuotePair { bid: Some(real), ask: None },
                Side::Sell => QuotePair { bid: None, ask: Some(real) },
            });
        }

        // Pressure quote — large order on the other side, dropped
        // every other tick so the engine's diff cancels it.
        if show_pressure {
            let pressure_px = match self.config.pressure_side {
                Side::Buy => ctx.product.round_price(mid - pressure_px_shift),
                Side::Sell => ctx.product.round_price(mid + pressure_px_shift),
            };
            if pressure_px > Decimal::ZERO
                && ctx.product.meets_min_notional(pressure_px, pressure_qty)
            {
                let pressure = Quote {
                    side: self.config.pressure_side,
                    price: pressure_px,
                    qty: pressure_qty,
                };
                pairs.push(match self.config.pressure_side {
                    Side::Buy => QuotePair { bid: Some(pressure), ask: None },
                    Side::Sell => QuotePair { bid: None, ask: Some(pressure) },
                });
            }
        }

        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::config::MarketMakerConfig;
    use mm_common::orderbook::LocalOrderBook;
    use mm_common::types::ProductSpec;

    fn pt_product() -> ProductSpec {
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
    fn pt_cfg() -> MarketMakerConfig {
        // Spoof doesn't read most of these — we borrow the full
        // canonical shape so the StrategyContext compiles, then
        // only order_size is load-bearing for the assertions.
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
            margin_reduce_slice_pct: rust_decimal_macros::dec!(0.1),
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
            sor_inventory_threshold: rust_decimal::Decimal::ZERO,
            sor_trade_rate_window_secs: 60, sor_queue_refresh_secs: 2,
        }
    }
    fn ctx<'a>(
        book: &'a LocalOrderBook,
        product: &'a ProductSpec,
        cfg: &'a MarketMakerConfig,
        mid: Decimal,
    ) -> StrategyContext<'a> {
        StrategyContext {
            book, product, config: cfg,
            inventory: Decimal::ZERO,
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
        }
    }

    #[test]
    fn spoof_alternates_pressure_on_off() {
        let product = pt_product();
        let cfg = pt_cfg();
        let book = LocalOrderBook::new("BTCUSDT".into());
        let strat = SpoofStrategy::new();
        let mid = dec!(30000);

        // Even tick — pressure present (2 pairs: real + fake).
        let t0 = strat.compute_quotes(&ctx(&book, &product, &cfg, mid));
        assert_eq!(t0.len(), 2, "even tick shows pressure + real");

        // Odd tick — pressure dropped (only real).
        let t1 = strat.compute_quotes(&ctx(&book, &product, &cfg, mid));
        assert_eq!(t1.len(), 1, "odd tick drops pressure");
    }

    #[test]
    fn spoof_pressure_is_ten_x_real_size() {
        let product = pt_product();
        let cfg = pt_cfg();
        let book = LocalOrderBook::new("BTCUSDT".into());
        let strat = SpoofStrategy::new();
        let mid = dec!(30000);

        let pairs = strat.compute_quotes(&ctx(&book, &product, &cfg, mid));
        // Pressure side is Buy (default), so the fake is `bid`.
        let real = pairs.iter().find_map(|p| p.ask.as_ref()).unwrap();
        let fake = pairs.iter().find_map(|p| p.bid.as_ref()).unwrap();
        assert_eq!(fake.qty / real.qty, dec!(10));
    }
}
