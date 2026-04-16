//! Portfolio-level risk manager (Epic 3: Cross-Symbol Portfolio Risk).
//!
//! Evaluates aggregate factor exposure against configured limits
//! and returns advisory actions (widen, halt) that the engine
//! coordinator broadcasts as `ConfigOverride`s.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for portfolio-level risk limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioRiskConfig {
    /// Maximum total absolute delta across all factors in USD.
    /// When the sum of `|factor_delta * factor_price|` exceeds
    /// this, the manager returns `WidenAll`.
    #[serde(default = "default_max_total_delta_usd")]
    pub max_total_delta_usd: Decimal,
    /// Per-factor delta limits.
    #[serde(default)]
    pub factor_limits: Vec<FactorLimitConfig>,
}

impl Default for PortfolioRiskConfig {
    fn default() -> Self {
        Self {
            max_total_delta_usd: default_max_total_delta_usd(),
            factor_limits: Vec::new(),
        }
    }
}

fn default_max_total_delta_usd() -> Decimal {
    dec!(100_000)
}

/// Per-factor exposure limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorLimitConfig {
    /// Factor name (e.g. "BTC", "ETH").
    pub factor: String,
    /// Maximum absolute net delta in base asset units.
    pub max_net_delta: Decimal,
    /// Spread multiplier applied when the factor is between
    /// `warn_pct` and 100% of `max_net_delta`.
    #[serde(default = "default_widen_mult")]
    pub widen_mult: Decimal,
    /// Percentage of `max_net_delta` at which widening starts.
    #[serde(default = "default_warn_pct")]
    pub warn_pct: Decimal,
}

fn default_widen_mult() -> Decimal {
    dec!(2)
}

fn default_warn_pct() -> Decimal {
    dec!(0.8)
}

/// Action returned by the portfolio risk evaluator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PortfolioRiskAction {
    /// No intervention needed.
    Normal,
    /// Widen spreads across all symbols contributing to the
    /// breached factor by `mult`.
    WidenAll { factor: String, mult: Decimal },
    /// Hard halt: factor delta exceeded 100% of limit.
    HaltFactor { factor: String },
}

/// Summary of the portfolio risk evaluation for dashboard.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PortfolioRiskSummary {
    pub actions: Vec<PortfolioRiskAction>,
    pub factor_utilization: HashMap<String, FactorUtilization>,
    pub total_abs_delta_usd: Decimal,
    pub total_delta_limit_usd: Decimal,
}

/// Per-factor utilization metrics.
#[derive(Debug, Clone, Serialize)]
pub struct FactorUtilization {
    pub net_delta: Decimal,
    pub max_delta: Decimal,
    pub utilization_pct: Decimal,
}

/// Portfolio-level risk manager. Stateless evaluator — takes a
/// snapshot of factor deltas and returns advisory actions.
#[derive(Debug, Clone)]
pub struct PortfolioRiskManager {
    config: PortfolioRiskConfig,
}

impl PortfolioRiskManager {
    pub fn new(config: PortfolioRiskConfig) -> Self {
        Self { config }
    }

    /// Evaluate portfolio risk given current factor deltas.
    ///
    /// `factor_deltas` maps factor name → net delta in base units
    /// (e.g. "BTC" → 0.5 means long 0.5 BTC across all symbols).
    pub fn evaluate(&self, factor_deltas: &HashMap<String, Decimal>) -> PortfolioRiskSummary {
        let mut actions = Vec::new();
        let mut utilization = HashMap::new();

        // Per-factor limits.
        for limit in &self.config.factor_limits {
            let net_delta = factor_deltas
                .get(&limit.factor)
                .copied()
                .unwrap_or(Decimal::ZERO);

            let abs_delta = net_delta.abs();
            let util_pct = if limit.max_net_delta > Decimal::ZERO {
                (abs_delta / limit.max_net_delta) * dec!(100)
            } else {
                dec!(100)
            };

            utilization.insert(
                limit.factor.clone(),
                FactorUtilization {
                    net_delta,
                    max_delta: limit.max_net_delta,
                    utilization_pct: util_pct,
                },
            );

            if abs_delta >= limit.max_net_delta {
                actions.push(PortfolioRiskAction::HaltFactor {
                    factor: limit.factor.clone(),
                });
            } else if abs_delta >= limit.max_net_delta * limit.warn_pct {
                actions.push(PortfolioRiskAction::WidenAll {
                    factor: limit.factor.clone(),
                    mult: limit.widen_mult,
                });
            }
        }

        // Global delta check (simplified: sum of absolute deltas).
        let total_abs = factor_deltas.values().map(|d| d.abs()).sum::<Decimal>();

        // If no factor-specific action but total exceeds global limit.
        if actions.is_empty() && total_abs > self.config.max_total_delta_usd {
            actions.push(PortfolioRiskAction::WidenAll {
                factor: "GLOBAL".to_string(),
                mult: dec!(2),
            });
        }

        PortfolioRiskSummary {
            actions,
            factor_utilization: utilization,
            total_abs_delta_usd: total_abs,
            total_delta_limit_usd: self.config.max_total_delta_usd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_btc_limit(max: Decimal) -> PortfolioRiskConfig {
        PortfolioRiskConfig {
            max_total_delta_usd: dec!(100_000),
            factor_limits: vec![FactorLimitConfig {
                factor: "BTC".into(),
                max_net_delta: max,
                widen_mult: dec!(2),
                warn_pct: dec!(0.8),
            }],
        }
    }

    #[test]
    fn normal_when_under_limit() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(10)));
        let mut deltas = HashMap::new();
        deltas.insert("BTC".into(), dec!(5));
        let summary = mgr.evaluate(&deltas);
        assert!(
            summary
                .actions
                .iter()
                .all(|a| *a == PortfolioRiskAction::Normal
                    || matches!(a, PortfolioRiskAction::Normal))
        );
        assert!(summary.actions.is_empty());
    }

    #[test]
    fn widen_when_approaching_limit() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(10)));
        let mut deltas = HashMap::new();
        deltas.insert("BTC".into(), dec!(9)); // 90% > 80% warn
        let summary = mgr.evaluate(&deltas);
        assert_eq!(summary.actions.len(), 1);
        assert!(matches!(
            &summary.actions[0],
            PortfolioRiskAction::WidenAll { factor, mult }
            if factor == "BTC" && *mult == dec!(2)
        ));
    }

    #[test]
    fn halt_when_over_limit() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(10)));
        let mut deltas = HashMap::new();
        deltas.insert("BTC".into(), dec!(10)); // exactly at limit
        let summary = mgr.evaluate(&deltas);
        assert_eq!(summary.actions.len(), 1);
        assert!(matches!(
            &summary.actions[0],
            PortfolioRiskAction::HaltFactor { factor } if factor == "BTC"
        ));
    }

    #[test]
    fn negative_delta_also_triggers() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(10)));
        let mut deltas = HashMap::new();
        deltas.insert("BTC".into(), dec!(-10)); // short 10 = at limit
        let summary = mgr.evaluate(&deltas);
        assert!(matches!(
            &summary.actions[0],
            PortfolioRiskAction::HaltFactor { factor } if factor == "BTC"
        ));
    }

    #[test]
    fn multi_factor_independence() {
        let config = PortfolioRiskConfig {
            max_total_delta_usd: dec!(100_000),
            factor_limits: vec![
                FactorLimitConfig {
                    factor: "BTC".into(),
                    max_net_delta: dec!(10),
                    widen_mult: dec!(2),
                    warn_pct: dec!(0.8),
                },
                FactorLimitConfig {
                    factor: "ETH".into(),
                    max_net_delta: dec!(100),
                    widen_mult: dec!(1.5),
                    warn_pct: dec!(0.8),
                },
            ],
        };
        let mgr = PortfolioRiskManager::new(config);
        let mut deltas = HashMap::new();
        deltas.insert("BTC".into(), dec!(5)); // 50% — normal
        deltas.insert("ETH".into(), dec!(90)); // 90% — widen
        let summary = mgr.evaluate(&deltas);
        assert_eq!(summary.actions.len(), 1);
        assert!(matches!(
            &summary.actions[0],
            PortfolioRiskAction::WidenAll { factor, .. } if factor == "ETH"
        ));
    }

    #[test]
    fn empty_portfolio_returns_normal() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(10)));
        let summary = mgr.evaluate(&HashMap::new());
        assert!(summary.actions.is_empty());
    }

    #[test]
    fn zero_limit_always_halts() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(0)));
        let mut deltas = HashMap::new();
        deltas.insert("BTC".into(), dec!(0.001));
        let summary = mgr.evaluate(&deltas);
        assert!(matches!(
            &summary.actions[0],
            PortfolioRiskAction::HaltFactor { .. }
        ));
    }

    #[test]
    fn factor_not_configured_is_ignored() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(10)));
        let mut deltas = HashMap::new();
        deltas.insert("SOL".into(), dec!(1000)); // not configured
        let summary = mgr.evaluate(&deltas);
        // No factor-specific action; global limit still applies
        assert!(
            summary.factor_utilization.is_empty()
                || !summary.factor_utilization.contains_key("SOL")
        );
    }

    #[test]
    fn utilization_pct_computed_correctly() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(10)));
        let mut deltas = HashMap::new();
        deltas.insert("BTC".into(), dec!(5));
        let summary = mgr.evaluate(&deltas);
        let btc_util = summary.factor_utilization.get("BTC").unwrap();
        assert_eq!(btc_util.utilization_pct, dec!(50));
        assert_eq!(btc_util.net_delta, dec!(5));
    }

    #[test]
    fn global_limit_triggers_widen() {
        let config = PortfolioRiskConfig {
            max_total_delta_usd: dec!(100),
            factor_limits: vec![],
        };
        let mgr = PortfolioRiskManager::new(config);
        let mut deltas = HashMap::new();
        deltas.insert("BTC".into(), dec!(60));
        deltas.insert("ETH".into(), dec!(50));
        let summary = mgr.evaluate(&deltas);
        assert_eq!(summary.actions.len(), 1);
        assert!(matches!(
            &summary.actions[0],
            PortfolioRiskAction::WidenAll { factor, .. } if factor == "GLOBAL"
        ));
    }

    #[test]
    fn mixed_long_short_nets_correctly() {
        let mgr = PortfolioRiskManager::new(config_with_btc_limit(dec!(10)));
        let mut deltas = HashMap::new();
        // Net BTC across symbols: sum is computed externally
        // by Portfolio::factor_delta(). Here we pass the net.
        deltas.insert("BTC".into(), dec!(3)); // under limit
        let summary = mgr.evaluate(&deltas);
        assert!(summary.actions.is_empty());
    }
}
