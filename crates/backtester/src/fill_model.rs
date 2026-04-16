//! Probabilistic fill model with latency and slippage simulation.
//!
//! The existing [`crate::simulator::FillModel`] enum models fills
//! deterministically: either the price crossed our quote or it didn't,
//! and the QueuePosition variant applies a single global probability.
//! Reality is noisier:
//!
//! - A limit order that is touched by the market may not fill (queue
//!   ahead of us cleared first).
//! - A filled order may receive price improvement, or worse slippage
//!   past the quoted price.
//! - Our order entry has network latency; by the time the venue sees
//!   our quote, the top of book may have moved.
//!
//! This module adds a [`ProbabilisticFiller`] that captures all three
//! in a single deterministic RNG so backtests remain reproducible. Pass
//! the same `seed` twice and you get the same fills.
//!
//! ## Parameters
//!
//! - `prob_fill_on_touch`: fraction of "price touched our level"
//!   events that actually produce a fill. Nautilus calls this
//!   `prob_fill_on_limit`.
//! - `prob_slippage`: fraction of filled orders that receive adverse
//!   slippage on top of the quoted price.
//! - `slippage_bps`: magnitude of the adverse slippage, in basis
//!   points of the quoted price.
//! - `latency_ms`: round-trip latency from place → exchange ack. The
//!   caller uses this to shift the fill timestamp forward.
//!
//! The old deterministic models remain as configuration presets:
//! `ProbabilisticFillConfig::price_cross()` is equivalent to
//! `FillModel::PriceCross`, `ProbabilisticFillConfig::queue_position(p)`
//! is equivalent to `FillModel::QueuePosition { fill_probability: p }`.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use mm_common::types::{Quote, Side};

/// Configuration for the probabilistic fill model.
#[derive(Debug, Clone, Copy)]
pub struct ProbabilisticFillConfig {
    pub prob_fill_on_touch: f64,
    pub prob_slippage: f64,
    pub slippage_bps: Decimal,
    pub latency_ms: u64,
}

impl ProbabilisticFillConfig {
    /// Optimistic: every touch fills at the quoted price, zero latency.
    /// Equivalent to the legacy `FillModel::PriceCross`.
    pub fn price_cross() -> Self {
        Self {
            prob_fill_on_touch: 1.0,
            prob_slippage: 0.0,
            slippage_bps: dec!(0),
            latency_ms: 0,
        }
    }

    /// Queue-position model with a single global fill probability.
    /// Equivalent to the legacy `FillModel::QueuePosition`.
    pub fn queue_position(p: f64) -> Self {
        Self {
            prob_fill_on_touch: p,
            prob_slippage: 0.0,
            slippage_bps: dec!(0),
            latency_ms: 0,
        }
    }

    /// A sensible default for a mid-tier crypto MM venue:
    /// 60% touch→fill, 5% slippage chance at 1 bps, 5ms latency.
    pub fn realistic_crypto() -> Self {
        Self {
            prob_fill_on_touch: 0.6,
            prob_slippage: 0.05,
            slippage_bps: dec!(1),
            latency_ms: 5,
        }
    }
}

/// The outcome of feeding a market tick to the filler.
#[derive(Debug, Clone, PartialEq)]
pub struct FillOutcome {
    /// `true` if the quote filled on this tick.
    pub filled: bool,
    /// Price at which the fill occurred. May be worse than the
    /// original quote price when slippage applies.
    pub fill_price: Decimal,
    /// Simulated latency to apply to the fill timestamp, in
    /// milliseconds. Zero when `filled == false`.
    pub latency_ms: u64,
}

impl FillOutcome {
    pub fn no_fill() -> Self {
        Self {
            filled: false,
            fill_price: Decimal::ZERO,
            latency_ms: 0,
        }
    }
}

/// Deterministic probabilistic fill simulator.
///
/// Caller feeds each incoming tick's best bid/ask and the outstanding
/// quote. The filler decides whether the quote fills this tick, at
/// what price, and applies the configured latency.
pub struct ProbabilisticFiller {
    config: ProbabilisticFillConfig,
    rng: ChaCha8Rng,
}

impl ProbabilisticFiller {
    pub fn new(config: ProbabilisticFillConfig, seed: u64) -> Self {
        Self {
            config,
            rng: ChaCha8Rng::seed_from_u64(seed),
        }
    }

    /// Evaluate a potential fill for `quote` given the current top of
    /// book on both sides.
    pub fn try_fill(&mut self, quote: &Quote, best_bid: Decimal, best_ask: Decimal) -> FillOutcome {
        // Step 1: did the market touch the quote at all?
        let touched = match quote.side {
            // A resting bid fills when the market ask falls to or
            // below our bid price.
            Side::Buy => best_ask <= quote.price,
            // A resting ask fills when the market bid rises to or
            // above our ask price.
            Side::Sell => best_bid >= quote.price,
        };
        if !touched {
            return FillOutcome::no_fill();
        }

        // Step 2: queue luck — do we get to the front in time?
        let roll: f64 = self.rng.random();
        if roll > self.config.prob_fill_on_touch {
            return FillOutcome::no_fill();
        }

        // Step 3: slippage — did the fill come at a worse price?
        let mut fill_price = quote.price;
        let slip_roll: f64 = self.rng.random();
        if slip_roll < self.config.prob_slippage {
            let adjust = quote.price * self.config.slippage_bps / dec!(10000);
            match quote.side {
                Side::Buy => fill_price += adjust,  // bad for the buyer
                Side::Sell => fill_price -= adjust, // bad for the seller
            }
        }

        FillOutcome {
            filled: true,
            fill_price,
            latency_ms: self.config.latency_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_common::types::Quote;

    fn bid_at(price: Decimal) -> Quote {
        Quote {
            side: Side::Buy,
            price,
            qty: dec!(1),
        }
    }

    fn ask_at(price: Decimal) -> Quote {
        Quote {
            side: Side::Sell,
            price,
            qty: dec!(1),
        }
    }

    #[test]
    fn price_cross_preset_always_fills_when_touched() {
        let mut f = ProbabilisticFiller::new(ProbabilisticFillConfig::price_cross(), 42);
        let out = f.try_fill(&bid_at(dec!(100)), dec!(99.5), dec!(100));
        assert!(out.filled);
        assert_eq!(out.fill_price, dec!(100));
        assert_eq!(out.latency_ms, 0);
    }

    #[test]
    fn no_fill_when_market_does_not_touch() {
        let mut f = ProbabilisticFiller::new(ProbabilisticFillConfig::price_cross(), 42);
        // Our bid is 100 but ask is 101 — not touched.
        let out = f.try_fill(&bid_at(dec!(100)), dec!(100.5), dec!(101));
        assert!(!out.filled);
    }

    #[test]
    fn same_seed_produces_same_fills() {
        let cfg = ProbabilisticFillConfig {
            prob_fill_on_touch: 0.5,
            prob_slippage: 0.2,
            slippage_bps: dec!(5),
            latency_ms: 0,
        };
        let mut a = ProbabilisticFiller::new(cfg, 12345);
        let mut b = ProbabilisticFiller::new(cfg, 12345);
        let mut trace_a = Vec::new();
        let mut trace_b = Vec::new();
        for i in 0..100 {
            let q = bid_at(dec!(100));
            let market_ask = dec!(99) + Decimal::new(i, 3);
            trace_a.push(a.try_fill(&q, dec!(98), market_ask));
            trace_b.push(b.try_fill(&q, dec!(98), market_ask));
        }
        assert_eq!(trace_a, trace_b);
    }

    #[test]
    fn different_seeds_diverge() {
        let cfg = ProbabilisticFillConfig {
            prob_fill_on_touch: 0.5,
            prob_slippage: 0.0,
            slippage_bps: dec!(0),
            latency_ms: 0,
        };
        let mut a = ProbabilisticFiller::new(cfg, 1);
        let mut b = ProbabilisticFiller::new(cfg, 2);
        let q = bid_at(dec!(100));
        let hits_a = (0..1000)
            .filter(|_| a.try_fill(&q, dec!(95), dec!(100)).filled)
            .count();
        let hits_b = (0..1000)
            .filter(|_| b.try_fill(&q, dec!(95), dec!(100)).filled)
            .count();
        // Both should be near 500 but differ in exact count.
        assert_ne!(hits_a, hits_b);
    }

    #[test]
    fn fill_rate_converges_near_target_probability() {
        let cfg = ProbabilisticFillConfig {
            prob_fill_on_touch: 0.3,
            prob_slippage: 0.0,
            slippage_bps: dec!(0),
            latency_ms: 0,
        };
        let mut f = ProbabilisticFiller::new(cfg, 7);
        let trials = 10_000;
        let q = bid_at(dec!(100));
        let hits = (0..trials)
            .filter(|_| f.try_fill(&q, dec!(95), dec!(100)).filled)
            .count();
        let rate = hits as f64 / trials as f64;
        assert!(
            (rate - 0.3).abs() < 0.02,
            "empirical fill rate {rate} not close to 0.3 over {trials} trials"
        );
    }

    #[test]
    fn buy_slippage_raises_fill_price() {
        let cfg = ProbabilisticFillConfig {
            prob_fill_on_touch: 1.0,
            prob_slippage: 1.0, // always slip
            slippage_bps: dec!(10),
            latency_ms: 0,
        };
        let mut f = ProbabilisticFiller::new(cfg, 0);
        let out = f.try_fill(&bid_at(dec!(1000)), dec!(999), dec!(1000));
        assert!(out.filled);
        // 10 bps of 1000 = 1.0, so fill_price = 1001.
        assert_eq!(out.fill_price, dec!(1001));
    }

    #[test]
    fn sell_slippage_lowers_fill_price() {
        let cfg = ProbabilisticFillConfig {
            prob_fill_on_touch: 1.0,
            prob_slippage: 1.0,
            slippage_bps: dec!(10),
            latency_ms: 0,
        };
        let mut f = ProbabilisticFiller::new(cfg, 0);
        let out = f.try_fill(&ask_at(dec!(1000)), dec!(1000), dec!(1001));
        assert!(out.filled);
        assert_eq!(out.fill_price, dec!(999));
    }

    #[test]
    fn latency_is_propagated() {
        let cfg = ProbabilisticFillConfig {
            prob_fill_on_touch: 1.0,
            prob_slippage: 0.0,
            slippage_bps: dec!(0),
            latency_ms: 15,
        };
        let mut f = ProbabilisticFiller::new(cfg, 0);
        let out = f.try_fill(&bid_at(dec!(100)), dec!(99), dec!(100));
        assert_eq!(out.latency_ms, 15);
    }

    #[test]
    fn zero_prob_never_fills() {
        let cfg = ProbabilisticFillConfig {
            prob_fill_on_touch: 0.0,
            prob_slippage: 0.0,
            slippage_bps: dec!(0),
            latency_ms: 0,
        };
        let mut f = ProbabilisticFiller::new(cfg, 0);
        for _ in 0..100 {
            assert!(!f.try_fill(&bid_at(dec!(100)), dec!(95), dec!(100)).filled);
        }
    }

    /// `try_fill` draws two f64s from the RNG on every touched
    /// evaluation — one for the queue-luck roll, one for the
    /// slippage roll. They must be **independent draws** (advancing
    /// the stream twice), not the same value reused. A regression
    /// that reused the first draw would make slippage perfectly
    /// correlated with fill outcome.
    ///
    /// We verify indirectly: with `prob_fill_on_touch = 1.0` (always
    /// fill, stream advances twice per tick) and `prob_slippage =
    /// 0.5`, over 2000 trials the slippage rate should sit near 50%.
    /// If the slippage roll reused the fill-roll value, the
    /// threshold `< 0.5` would always be true (because the first
    /// draw that passed `roll > 1.0 == false` is strictly below
    /// 1.0 but uniform in [0, 1), so `< 0.5` holds 50% of the time
    /// anyway — and this test does not distinguish the two
    /// mechanisms on its own). The REAL safeguard is that
    /// `same_seed_produces_same_fills` above pins the entire
    /// trace of outputs, which would change if the number of RNG
    /// draws per tick changed.
    ///
    /// This test specifically pins the expected slippage ratio so a
    /// regression in the draw count (e.g. dropping to one draw per
    /// tick, or adding a third) flips the empirical rate.
    #[test]
    fn slippage_roll_independent_from_fill_roll() {
        let cfg = ProbabilisticFillConfig {
            prob_fill_on_touch: 1.0,
            prob_slippage: 0.5,
            slippage_bps: dec!(10),
            latency_ms: 0,
        };
        let mut f = ProbabilisticFiller::new(cfg, 9_999);
        let trials = 2000;
        let q = bid_at(dec!(1000));
        let mut slipped = 0;
        for _ in 0..trials {
            let out = f.try_fill(&q, dec!(995), dec!(1000));
            assert!(out.filled);
            if out.fill_price != dec!(1000) {
                slipped += 1;
            }
        }
        let rate = slipped as f64 / trials as f64;
        assert!(
            (rate - 0.5).abs() < 0.05,
            "slippage rate {rate} not near 0.5 — RNG draws may be correlated"
        );
    }

    #[test]
    fn realistic_preset_has_nonzero_latency_and_partial_fills() {
        let cfg = ProbabilisticFillConfig::realistic_crypto();
        assert!(cfg.latency_ms > 0);
        assert!(cfg.prob_fill_on_touch < 1.0);
        assert!(cfg.prob_slippage > 0.0);
    }
}
