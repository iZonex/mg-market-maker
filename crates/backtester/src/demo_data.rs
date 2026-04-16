//! Demo data generator (Epic 8 item 8.6).
//!
//! Generates synthetic market data for paper trading demos.
//! Produces a stream of book/trade events with configurable
//! volatility and mean-reversion characteristics.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;

/// A synthetic market event for demo purposes.
#[derive(Debug, Clone, Serialize)]
pub struct DemoEvent {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub event_type: DemoEventType,
}

/// Type of demo event.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DemoEventType {
    /// Book update with new mid price and spread.
    BookUpdate {
        mid_price: Decimal,
        spread_bps: Decimal,
        bid_depth: Decimal,
        ask_depth: Decimal,
    },
    /// Simulated trade.
    Trade {
        price: Decimal,
        qty: Decimal,
        is_buy: bool,
    },
}

/// Configuration for demo data generation.
pub struct DemoConfig {
    /// Starting mid price.
    pub initial_price: Decimal,
    /// Annualized volatility (e.g., 0.5 for 50%).
    pub volatility: Decimal,
    /// Mean-reversion speed (0 = random walk, 1 = strong reversion).
    pub mean_reversion: Decimal,
    /// Average spread in bps.
    pub avg_spread_bps: Decimal,
    /// Average depth per side in quote asset.
    pub avg_depth: Decimal,
    /// Trade frequency: average trades per second.
    pub trades_per_second: Decimal,
}

impl Default for DemoConfig {
    fn default() -> Self {
        Self {
            initial_price: dec!(50000),
            volatility: dec!(0.5),
            mean_reversion: dec!(0.1),
            avg_spread_bps: dec!(5),
            avg_depth: dec!(5000),
            trades_per_second: dec!(2),
        }
    }
}

/// Generate demo book events for a symbol over a duration.
///
/// Uses a simple geometric random walk with mean-reversion
/// and deterministic pseudo-random numbers (no external RNG
/// dependency — uses a simple LCG for reproducibility).
pub fn generate_demo_events(
    symbol: &str,
    duration_secs: u64,
    config: &DemoConfig,
    seed: u64,
) -> Vec<DemoEvent> {
    let mut events = Vec::new();
    let mut price = config.initial_price;
    let mut rng_state = seed;
    let start = Utc::now();

    // Convert annual vol to per-second vol.
    // σ_sec = σ_annual / sqrt(365 * 24 * 3600)
    let seconds_per_year = dec!(31536000);
    let vol_per_sec = config.volatility / decimal_sqrt(seconds_per_year);

    let anchor_price = config.initial_price;

    for sec in 0..duration_secs {
        let ts = start + Duration::seconds(sec as i64);

        // Random walk step with mean-reversion.
        let (r, new_state) = lcg_normal(rng_state);
        rng_state = new_state;
        let return_pct = vol_per_sec * r;

        // Mean-reversion pull toward anchor.
        let deviation = (price - anchor_price) / anchor_price;
        let mr_pull = -config.mean_reversion * deviation * vol_per_sec;

        price *= dec!(1) + return_pct + mr_pull;
        if price < dec!(0.01) {
            price = dec!(0.01);
        }

        // Book update every second.
        events.push(DemoEvent {
            timestamp: ts,
            symbol: symbol.to_string(),
            event_type: DemoEventType::BookUpdate {
                mid_price: price,
                spread_bps: config.avg_spread_bps,
                bid_depth: config.avg_depth,
                ask_depth: config.avg_depth,
            },
        });

        // Trades: deterministic based on seed.
        let (trade_r, new_state2) = lcg_uniform(rng_state);
        rng_state = new_state2;
        if trade_r < decimal_to_f64(config.trades_per_second) {
            let is_buy = trade_r < decimal_to_f64(config.trades_per_second) / 2.0;
            let slip = if is_buy { dec!(1) } else { dec!(-1) };
            let trade_price = price + slip * price * config.avg_spread_bps / dec!(20000);
            events.push(DemoEvent {
                timestamp: ts,
                symbol: symbol.to_string(),
                event_type: DemoEventType::Trade {
                    price: trade_price,
                    qty: dec!(0.01),
                    is_buy,
                },
            });
        }
    }

    events
}

// Simple LCG for deterministic pseudo-random numbers.
fn lcg_uniform(state: u64) -> (f64, u64) {
    let next = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let val = (next >> 33) as f64 / (1u64 << 31) as f64;
    (val, next)
}

fn lcg_normal(state: u64) -> (Decimal, u64) {
    // Box-Muller approximation using two uniform samples.
    let (u1, s1) = lcg_uniform(state);
    let (u2, s2) = lcg_uniform(s1);
    let u1 = u1.max(1e-10);
    let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
    let d = Decimal::try_from(z).unwrap_or(Decimal::ZERO);
    (d, s2)
}

fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let mut guess = x / dec!(2);
    for _ in 0..20 {
        let next = (guess + x / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000000001) {
            return next;
        }
        guess = next;
    }
    guess
}

fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_events_for_duration() {
        let events = generate_demo_events("BTCUSDT", 60, &DemoConfig::default(), 42);
        assert!(events.len() >= 60); // at least one book update per second
    }

    #[test]
    fn prices_stay_positive() {
        let config = DemoConfig {
            volatility: dec!(2.0), // extreme volatility
            ..Default::default()
        };
        let events = generate_demo_events("BTCUSDT", 300, &config, 123);
        for e in &events {
            if let DemoEventType::BookUpdate { mid_price, .. } = &e.event_type {
                assert!(*mid_price > Decimal::ZERO);
            }
        }
    }

    #[test]
    fn deterministic_with_same_seed() {
        let e1 = generate_demo_events("BTCUSDT", 10, &DemoConfig::default(), 42);
        let e2 = generate_demo_events("BTCUSDT", 10, &DemoConfig::default(), 42);
        assert_eq!(e1.len(), e2.len());
        // Check first book update has same price.
        if let (
            DemoEventType::BookUpdate { mid_price: p1, .. },
            DemoEventType::BookUpdate { mid_price: p2, .. },
        ) = (&e1[0].event_type, &e2[0].event_type)
        {
            assert_eq!(p1, p2);
        }
    }
}
