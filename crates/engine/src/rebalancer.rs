//! Cross-venue balance rebalancer.
//!
//! Monitors per-venue available balances and recommends (or
//! executes) transfers when one venue's balance drops below a
//! configurable threshold. Critical for multi-venue MM
//! operations where inventory must flow dynamically.
//!
//! # v1 scope
//!
//! Advisory-only: the rebalancer computes recommendations but
//! does NOT execute transfers automatically. Operators review
//! recommendations via the dashboard and trigger manually.
//! Auto-execution is a stage-2 feature gated behind an
//! operator config flag.

use mm_exchange_core::connector::VenueId;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use std::collections::HashMap;

/// Per-venue balance snapshot for rebalancing decisions.
#[derive(Debug, Clone)]
pub struct VenueBalance {
    pub venue: VenueId,
    pub asset: String,
    pub available: Decimal,
    pub locked: Decimal,
}

/// A recommended transfer between two venues.
#[derive(Debug, Clone, Serialize)]
pub struct RebalanceRecommendation {
    pub from_venue: String,
    pub to_venue: String,
    pub asset: String,
    pub qty: Decimal,
    pub reason: String,
}

/// Configuration for the rebalancer.
#[derive(Debug, Clone)]
pub struct RebalancerConfig {
    /// Minimum balance per venue per asset before a rebalance
    /// recommendation fires. Default: 0 (disabled).
    pub min_balance_per_venue: Decimal,
    /// Target balance after rebalancing. The recommendation
    /// will suggest transferring enough to reach this level
    /// on the deficit venue. Default: 0 (equal split).
    pub target_balance_per_venue: Decimal,
}

impl Default for RebalancerConfig {
    fn default() -> Self {
        Self {
            min_balance_per_venue: Decimal::ZERO,
            target_balance_per_venue: Decimal::ZERO,
        }
    }
}

/// Cross-venue balance rebalancer.
#[derive(Debug, Clone)]
pub struct Rebalancer {
    config: RebalancerConfig,
}

impl Rebalancer {
    pub fn new(config: RebalancerConfig) -> Self {
        Self { config }
    }

    /// Compute rebalancing recommendations from a set of
    /// per-venue balance snapshots. Groups by asset and
    /// recommends transfers from surplus venues to deficit
    /// venues.
    pub fn recommend(&self, balances: &[VenueBalance]) -> Vec<RebalanceRecommendation> {
        // Group by asset.
        let mut by_asset: HashMap<String, Vec<&VenueBalance>> = HashMap::new();
        for b in balances {
            by_asset
                .entry(b.asset.clone())
                .or_default()
                .push(b);
        }

        let mut recs = Vec::new();
        for (asset, venues) in &by_asset {
            if venues.len() < 2 {
                continue;
            }
            let total: Decimal = venues.iter().map(|v| v.available).sum();
            let n = Decimal::from(venues.len() as u64);
            let target = if self.config.target_balance_per_venue > Decimal::ZERO {
                self.config.target_balance_per_venue
            } else {
                total / n // Equal split.
            };

            // Find deficit and surplus venues.
            let min_thresh = self.config.min_balance_per_venue;
            for deficit in venues.iter().filter(|v| v.available < min_thresh) {
                let need = target - deficit.available;
                if need <= Decimal::ZERO {
                    continue;
                }
                // Find the best surplus venue.
                if let Some(surplus) = venues
                    .iter()
                    .filter(|v| v.available > target + need / dec!(2))
                    .max_by_key(|v| v.available)
                {
                    let transfer_qty = need.min(surplus.available - target);
                    if transfer_qty > Decimal::ZERO {
                        recs.push(RebalanceRecommendation {
                            from_venue: format!("{:?}", surplus.venue),
                            to_venue: format!("{:?}", deficit.venue),
                            asset: asset.clone(),
                            qty: transfer_qty,
                            reason: format!(
                                "{:?} has {} available (below threshold {})",
                                deficit.venue, deficit.available, min_thresh
                            ),
                        });
                    }
                }
            }
        }
        recs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_rebalance_when_balanced() {
        let r = Rebalancer::new(RebalancerConfig {
            min_balance_per_venue: dec!(100),
            target_balance_per_venue: dec!(500),
        });
        let balances = vec![
            VenueBalance {
                venue: VenueId::Binance,
                asset: "USDT".into(),
                available: dec!(500),
                locked: dec!(0),
            },
            VenueBalance {
                venue: VenueId::Bybit,
                asset: "USDT".into(),
                available: dec!(500),
                locked: dec!(0),
            },
        ];
        let recs = r.recommend(&balances);
        assert!(recs.is_empty());
    }

    #[test]
    fn recommends_transfer_on_deficit() {
        let r = Rebalancer::new(RebalancerConfig {
            min_balance_per_venue: dec!(100),
            target_balance_per_venue: dec!(500),
        });
        let balances = vec![
            VenueBalance {
                venue: VenueId::Binance,
                asset: "USDT".into(),
                available: dec!(50), // Below threshold.
                locked: dec!(0),
            },
            VenueBalance {
                venue: VenueId::Bybit,
                asset: "USDT".into(),
                available: dec!(1000), // Surplus.
                locked: dec!(0),
            },
        ];
        let recs = r.recommend(&balances);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].asset, "USDT");
        assert!(recs[0].qty > Decimal::ZERO);
    }

    #[test]
    fn single_venue_no_recommendation() {
        let r = Rebalancer::new(RebalancerConfig {
            min_balance_per_venue: dec!(100),
            ..Default::default()
        });
        let balances = vec![VenueBalance {
            venue: VenueId::Binance,
            asset: "USDT".into(),
            available: dec!(10),
            locked: dec!(0),
        }];
        let recs = r.recommend(&balances);
        assert!(recs.is_empty());
    }

    #[test]
    fn equal_split_when_no_target_set() {
        let r = Rebalancer::new(RebalancerConfig {
            min_balance_per_venue: dec!(100),
            target_balance_per_venue: Decimal::ZERO, // Equal split.
        });
        let balances = vec![
            VenueBalance {
                venue: VenueId::Binance,
                asset: "BTC".into(),
                available: dec!(10), // Below threshold.
                locked: dec!(0),
            },
            VenueBalance {
                venue: VenueId::Bybit,
                asset: "BTC".into(),
                available: dec!(990), // Surplus.
                locked: dec!(0),
            },
        ];
        let recs = r.recommend(&balances);
        assert!(!recs.is_empty());
        // Target = (10 + 990) / 2 = 500.
        // Need = 500 - 10 = 490.
        assert!(recs[0].qty > dec!(0));
    }
}
