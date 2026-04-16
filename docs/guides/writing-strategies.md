# Writing Custom Strategies

## Architecture Overview

Every strategy implements one trait:

```rust
pub trait Strategy: Send {
    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair>;
}
```

The engine calls `compute_quotes()` on every refresh tick (default 500ms). You return bid/ask pairs — the engine handles everything else: order diffing, placement, cancellation, amend, PnL tracking, risk limits.

## StrategyContext — What You Get

```rust
pub struct StrategyContext {
    pub mid: Decimal,              // current mid price
    pub best_bid: Decimal,         // best bid from order book
    pub best_ask: Decimal,         // best ask from order book
    pub volatility: Decimal,       // EWMA realized vol (annualized)
    pub inventory: Decimal,        // current net position (base asset)
    pub time_remaining: Decimal,   // fraction of time horizon remaining [0,1]
    pub alpha: Decimal,            // momentum signal [-1, 1]
    pub borrow_cost_bps: Decimal,  // loan carry cost (basis points)
    pub ref_price: Option<Decimal>, // hedge-leg mid (for cross-product)
}
```

## QuotePair — What You Return

```rust
pub struct QuotePair {
    pub bid_price: Decimal,
    pub ask_price: Decimal,
    pub bid_qty: Decimal,
    pub ask_qty: Decimal,
}
```

Return multiple pairs for multi-level quoting. The engine places one order per pair.

## Minimal Example

```rust
// crates/strategy/src/my_strategy.rs

use mm_common::types::Decimal;
use rust_decimal_macros::dec;
use crate::r#trait::{Strategy, StrategyContext, QuotePair};

pub struct SimpleSpread {
    pub spread_bps: Decimal,
    pub size: Decimal,
    pub levels: usize,
}

impl Strategy for SimpleSpread {
    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let half_spread = ctx.mid * self.spread_bps / dec!(20000);

        // Skew toward reducing inventory
        let skew = ctx.inventory * dec!(0.0001);

        (0..self.levels)
            .map(|i| {
                let offset = half_spread * Decimal::from(i as u64 + 1);
                QuotePair {
                    bid_price: ctx.mid - offset - skew,
                    ask_price: ctx.mid + offset - skew,
                    bid_qty: self.size,
                    ask_qty: self.size,
                }
            })
            .collect()
    }
}
```

## Registering Your Strategy

1. Add your file to `crates/strategy/src/lib.rs`:

```rust
pub mod my_strategy;
```

2. Add a variant to `StrategyType` in `crates/common/src/config.rs`:

```rust
pub enum StrategyType {
    // ... existing
    MyStrategy,
}
```

3. Wire it in `crates/server/src/main.rs` in the strategy match:

```rust
StrategyType::MyStrategy => {
    Box::new(my_strategy::SimpleSpread {
        spread_bps: config.market_maker.min_spread_bps,
        size: config.market_maker.order_size,
        levels: config.market_maker.num_levels,
    })
}
```

4. Set in config:

```toml
[market_maker]
strategy = "my_strategy"
```

## Using Alpha Signals

The engine pre-computes momentum alpha from multiple sources:

| Signal | Source | Weight |
|--------|--------|--------|
| Book imbalance | L1 bid/ask qty ratio | 0.25 |
| Trade flow | Recent trade direction bias | 0.25 |
| Microprice | Weighted mid-price | 0.2 |
| HMA slope | Hull Moving Average trend | 0.15 |
| OFI | Cont-Kukanov-Stoikov order flow | 0.15 |

Access via `ctx.alpha` — a value in `[-1, 1]` where positive = bullish.

**Using alpha to shift reservation price:**

```rust
fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
    // Shift mid toward predicted direction
    let alpha_shift = ctx.mid * ctx.alpha * dec!(0.0001);
    let adjusted_mid = ctx.mid + alpha_shift;

    // Now quote around adjusted_mid instead of ctx.mid
    // ...
}
```

## Using Regime Detection

The engine's `AutoTuner` detects four market regimes:
- **Quiet** — low volatility, mean-reverting (tighten spread)
- **Trending** — directional move (widen spread, reduce size)
- **Volatile** — high variance (widen spread significantly)
- **MeanReverting** — detected via Hurst exponent (tighten spread)

Regime multipliers are applied automatically to `gamma`, `spread`, and `size` before your strategy sees them. You don't need to handle regimes manually unless you want to override.

## Risk Overlay

The engine applies these risk limits **after** your strategy returns quotes:

1. **Inventory skew** — shifts quotes to reduce position
2. **Kill switch** — reduces size / widens spread / cancels all
3. **Circuit breaker** — cancels all if drawdown/exposure/stale book
4. **VaR guard** — throttles size based on rolling tail risk
5. **Portfolio risk** — widens spread if factor delta exceeds limit
6. **Balance pre-check** — rejects orders exceeding available balance
7. **VPIN / toxicity** — widens spread when informed flow detected

You don't need to implement any of this — it happens transparently.

## Cross-Product Strategies

For strategies that need a reference price from another venue (basis trading, funding arb):

1. Set `[hedge]` section in config with the hedge exchange + pair
2. Access `ctx.ref_price` — the hedge-leg mid price
3. The engine manages a separate `BookKeeper` and `OrderManager` for the hedge leg

See `BasisStrategy` and `CrossExchangeStrategy` for examples.

## Testing Your Strategy

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(mid: Decimal, inventory: Decimal) -> StrategyContext {
        StrategyContext {
            mid,
            best_bid: mid - dec!(1),
            best_ask: mid + dec!(1),
            volatility: dec!(0.02),
            inventory,
            time_remaining: dec!(0.5),
            alpha: dec!(0),
            borrow_cost_bps: dec!(0),
            ref_price: None,
        }
    }

    #[test]
    fn quotes_are_symmetric_at_zero_inventory() {
        let s = SimpleSpread {
            spread_bps: dec!(10),
            size: dec!(0.01),
            levels: 1,
        };
        let quotes = s.compute_quotes(&test_ctx(dec!(50000), dec!(0)));
        assert_eq!(quotes.len(), 1);
        let mid_distance_bid = dec!(50000) - quotes[0].bid_price;
        let mid_distance_ask = quotes[0].ask_price - dec!(50000);
        assert!((mid_distance_bid - mid_distance_ask).abs() < dec!(0.01));
    }

    #[test]
    fn quotes_skew_with_inventory() {
        let s = SimpleSpread {
            spread_bps: dec!(10),
            size: dec!(0.01),
            levels: 1,
        };
        let quotes = s.compute_quotes(&test_ctx(dec!(50000), dec!(0.1)));
        // Long inventory → ask should be closer to mid (want to sell)
        let mid_distance_bid = dec!(50000) - quotes[0].bid_price;
        let mid_distance_ask = quotes[0].ask_price - dec!(50000);
        assert!(mid_distance_ask < mid_distance_bid);
    }
}
```

Run: `cargo test -p mm-strategy -- my_strategy`
