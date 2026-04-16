//! A/B split testing engine (Epic 6 item 6.2).
//!
//! Runs two parameter variants side-by-side and tracks
//! performance separately. The engine alternates between
//! variants based on a configurable split mode.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Strategy parameters for one A/B variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantParams {
    pub name: String,
    pub gamma_mult: Decimal,
    pub spread_mult: Decimal,
    pub size_mult: Decimal,
}

impl Default for VariantParams {
    fn default() -> Self {
        Self {
            name: "default".into(),
            gamma_mult: dec!(1),
            spread_mult: dec!(1),
            size_mult: dec!(1),
        }
    }
}

/// How to split traffic between variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitMode {
    /// Alternate by time: variant A for N ticks, then B for N ticks.
    TimeBased,
    /// Split by symbol: odd symbols → A, even → B.
    SymbolBased,
}

/// Per-variant performance snapshot.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VariantPerformance {
    pub name: String,
    pub ticks: u64,
    pub total_pnl: Decimal,
    pub sharpe_estimate: Decimal,
    pub fill_rate: Decimal,
}

/// A/B split engine.
pub struct AbSplitEngine {
    pub variant_a: VariantParams,
    pub variant_b: VariantParams,
    pub mode: SplitMode,
    /// Ticks per variant in time-based mode.
    pub period_ticks: u64,
    /// Current tick counter.
    tick_count: u64,
    /// Performance trackers.
    perf_a: VariantPerformance,
    perf_b: VariantPerformance,
}

impl AbSplitEngine {
    pub fn new(
        variant_a: VariantParams,
        variant_b: VariantParams,
        mode: SplitMode,
        period_ticks: u64,
    ) -> Self {
        let perf_a = VariantPerformance {
            name: variant_a.name.clone(),
            ..Default::default()
        };
        let perf_b = VariantPerformance {
            name: variant_b.name.clone(),
            ..Default::default()
        };
        Self {
            variant_a,
            variant_b,
            mode,
            period_ticks: period_ticks.max(1),
            tick_count: 0,
            perf_a,
            perf_b,
        }
    }

    /// Get the active variant for this tick. In time-based mode,
    /// alternates every `period_ticks`. In symbol-based mode,
    /// uses the symbol hash to determine the variant.
    pub fn active_variant(&self, symbol: &str) -> &VariantParams {
        match self.mode {
            SplitMode::TimeBased => {
                let cycle = self.tick_count / self.period_ticks;
                if cycle.is_multiple_of(2) {
                    &self.variant_a
                } else {
                    &self.variant_b
                }
            }
            SplitMode::SymbolBased => {
                let hash: u64 = symbol.bytes().map(|b| b as u64).sum();
                if hash.is_multiple_of(2) {
                    &self.variant_a
                } else {
                    &self.variant_b
                }
            }
        }
    }

    /// Advance the tick counter.
    pub fn tick(&mut self) {
        self.tick_count += 1;
    }

    /// Record a PnL observation for the active variant.
    pub fn record_pnl(&mut self, symbol: &str, pnl_delta: Decimal) {
        let is_a = std::ptr::eq(self.active_variant(symbol), &self.variant_a);
        let perf = if is_a {
            &mut self.perf_a
        } else {
            &mut self.perf_b
        };
        perf.ticks += 1;
        perf.total_pnl += pnl_delta;
    }

    /// Compare A vs B performance.
    pub fn compare(&self) -> (VariantPerformance, VariantPerformance) {
        (self.perf_a.clone(), self.perf_b.clone())
    }

    /// Returns the winning variant (higher PnL).
    pub fn winner(&self) -> &VariantParams {
        if self.perf_a.total_pnl >= self.perf_b.total_pnl {
            &self.variant_a
        } else {
            &self.variant_b
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(name: &str, gamma: Decimal) -> VariantParams {
        VariantParams {
            name: name.into(),
            gamma_mult: gamma,
            spread_mult: dec!(1),
            size_mult: dec!(1),
        }
    }

    #[test]
    fn time_based_alternates() {
        let engine = AbSplitEngine::new(
            params("A", dec!(0.5)),
            params("B", dec!(1.5)),
            SplitMode::TimeBased,
            10,
        );
        assert_eq!(engine.active_variant("BTCUSDT").name, "A");
    }

    #[test]
    fn time_based_switches_after_period() {
        let mut engine = AbSplitEngine::new(
            params("A", dec!(0.5)),
            params("B", dec!(1.5)),
            SplitMode::TimeBased,
            5,
        );
        for _ in 0..5 {
            engine.tick();
        }
        assert_eq!(engine.active_variant("BTCUSDT").name, "B");
    }

    #[test]
    fn symbol_based_deterministic() {
        let engine = AbSplitEngine::new(
            params("A", dec!(0.5)),
            params("B", dec!(1.5)),
            SplitMode::SymbolBased,
            10,
        );
        let v1 = engine.active_variant("BTCUSDT").name.clone();
        let v2 = engine.active_variant("BTCUSDT").name.clone();
        assert_eq!(v1, v2);
    }

    #[test]
    fn winner_is_higher_pnl() {
        let mut engine = AbSplitEngine::new(
            params("A", dec!(0.5)),
            params("B", dec!(1.5)),
            SplitMode::TimeBased,
            1,
        );
        engine.record_pnl("BTCUSDT", dec!(100));
        engine.tick();
        engine.record_pnl("BTCUSDT", dec!(50));
        assert_eq!(engine.winner().name, "A");
    }
}
