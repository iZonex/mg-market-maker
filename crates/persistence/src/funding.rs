use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Funding rate arbitrage tracker.
///
/// Strategy: Long spot + Short perpetual = delta-neutral.
/// Profit from funding payments (every 8h on most exchanges).
///
/// When funding rate > threshold:
///   - Buy spot (or already holding)
///   - Short perp (perpetual futures)
///   - Collect funding every 8 hours
///
/// When funding rate < -threshold:
///   - Sell spot / Short spot
///   - Long perp
///   - Collect negative funding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingArbState {
    pub symbol: String,
    /// Current spot position (positive = long).
    pub spot_position: Decimal,
    /// Current perp position (negative = short).
    pub perp_position: Decimal,
    /// Net delta (should be ~0 for delta-neutral).
    pub net_delta: Decimal,
    /// Entry basis (perp_price - spot_price at entry).
    pub entry_basis: Decimal,
    /// Accumulated funding payments (positive = received).
    pub accumulated_funding: Decimal,
    /// Total basis PnL from entry.
    pub basis_pnl: Decimal,
    /// Number of funding periods collected.
    pub funding_periods: u64,
    /// Timestamp of last funding collection.
    pub last_funding_at: Option<DateTime<Utc>>,
}

impl FundingArbState {
    pub fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            spot_position: dec!(0),
            perp_position: dec!(0),
            net_delta: dec!(0),
            entry_basis: dec!(0),
            accumulated_funding: dec!(0),
            basis_pnl: dec!(0),
            funding_periods: 0,
            last_funding_at: None,
        }
    }

    /// Total PnL = funding collected + basis PnL.
    pub fn total_pnl(&self) -> Decimal {
        self.accumulated_funding + self.basis_pnl
    }

    /// Is the position currently open?
    pub fn is_open(&self) -> bool {
        !self.spot_position.is_zero() || !self.perp_position.is_zero()
    }
}

/// Funding rate arbitrage engine.
pub struct FundingArbEngine {
    state: FundingArbState,
    config: FundingArbConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingArbConfig {
    /// Minimum funding rate to enter (annualized %).
    pub min_rate_annual_pct: Decimal,
    /// Maximum position size in base asset.
    pub max_position: Decimal,
    /// Maximum basis deviation before exit (in bps).
    pub max_basis_bps: Decimal,
    /// Enable the strategy.
    pub enabled: bool,
}

impl Default for FundingArbConfig {
    fn default() -> Self {
        Self {
            min_rate_annual_pct: dec!(10), // 10% APR minimum.
            max_position: dec!(0.1),       // 0.1 BTC max.
            max_basis_bps: dec!(50),       // 50 bps max basis deviation.
            enabled: false,                // Off by default.
        }
    }
}

/// Signals from the funding arb engine.
#[derive(Debug, Clone)]
pub enum FundingSignal {
    /// Enter position: buy spot, sell perp.
    Enter {
        spot_side: SpotAction,
        perp_side: PerpAction,
        size: Decimal,
    },
    /// Exit position: close both legs.
    Exit { reason: String },
    /// Do nothing.
    Hold,
}

#[derive(Debug, Clone)]
pub enum SpotAction {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub enum PerpAction {
    Long,
    Short,
}

impl FundingArbEngine {
    pub fn new(symbol: &str, config: FundingArbConfig) -> Self {
        Self {
            state: FundingArbState::new(symbol),
            config,
        }
    }

    pub fn state(&self) -> &FundingArbState {
        &self.state
    }

    /// Evaluate current funding rate and decide action.
    ///
    /// `funding_rate`: the current 8h funding rate (e.g., 0.0001 = 0.01%).
    /// `spot_price`: current spot price.
    /// `perp_price`: current perpetual price.
    pub fn evaluate(
        &mut self,
        funding_rate: Decimal,
        spot_price: Decimal,
        perp_price: Decimal,
    ) -> FundingSignal {
        if !self.config.enabled {
            return FundingSignal::Hold;
        }

        // Annualize: 8h rate * 3 * 365.
        let annual_rate = funding_rate * dec!(1095) * dec!(100); // as percentage.
        let basis_bps = (perp_price - spot_price).abs() / spot_price * dec!(10_000);

        debug!(
            %funding_rate,
            %annual_rate,
            %basis_bps,
            position_open = self.state.is_open(),
            "funding arb evaluation"
        );

        if self.state.is_open() {
            // Check exit conditions.
            if basis_bps > self.config.max_basis_bps {
                return FundingSignal::Exit {
                    reason: format!("basis too wide: {basis_bps} bps"),
                };
            }
            if annual_rate.abs() < dec!(2) {
                return FundingSignal::Exit {
                    reason: "funding rate too low to hold".to_string(),
                };
            }
            FundingSignal::Hold
        } else {
            // Check entry conditions.
            if annual_rate.abs() >= self.config.min_rate_annual_pct {
                let size = self.config.max_position;
                if funding_rate > dec!(0) {
                    // Positive funding: longs pay shorts.
                    // Strategy: buy spot + short perp → collect funding.
                    info!(%annual_rate, %size, "entering funding arb: long spot + short perp");
                    FundingSignal::Enter {
                        spot_side: SpotAction::Buy,
                        perp_side: PerpAction::Short,
                        size,
                    }
                } else {
                    // Negative funding: shorts pay longs.
                    // Strategy: sell spot + long perp → collect funding.
                    info!(%annual_rate, %size, "entering funding arb: short spot + long perp");
                    FundingSignal::Enter {
                        spot_side: SpotAction::Sell,
                        perp_side: PerpAction::Long,
                        size,
                    }
                }
            } else {
                FundingSignal::Hold
            }
        }
    }

    /// Record a funding payment received.
    pub fn on_funding_payment(&mut self, amount: Decimal) {
        self.state.accumulated_funding += amount;
        self.state.funding_periods += 1;
        self.state.last_funding_at = Some(Utc::now());
        info!(
            amount = %amount,
            total = %self.state.accumulated_funding,
            periods = self.state.funding_periods,
            "funding payment received"
        );
    }

    /// Update basis PnL.
    pub fn update_basis(&mut self, spot_price: Decimal, perp_price: Decimal) {
        if self.state.is_open() {
            let current_basis = perp_price - spot_price;
            self.state.basis_pnl =
                (self.state.entry_basis - current_basis) * self.state.spot_position.abs();
            self.state.net_delta = self.state.spot_position + self.state.perp_position;
        }
    }

    /// Record position entry.
    pub fn on_entry(
        &mut self,
        spot_qty: Decimal,
        perp_qty: Decimal,
        spot_price: Decimal,
        perp_price: Decimal,
    ) {
        self.state.spot_position = spot_qty;
        self.state.perp_position = perp_qty;
        self.state.entry_basis = perp_price - spot_price;
        self.state.net_delta = spot_qty + perp_qty;
    }

    /// Record position exit.
    pub fn on_exit(&mut self) {
        info!(
            total_pnl = %self.state.total_pnl(),
            funding = %self.state.accumulated_funding,
            basis = %self.state.basis_pnl,
            periods = self.state.funding_periods,
            "funding arb position closed"
        );
        self.state.spot_position = dec!(0);
        self.state.perp_position = dec!(0);
        self.state.net_delta = dec!(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enter_positive_funding() {
        let config = FundingArbConfig {
            min_rate_annual_pct: dec!(10),
            max_position: dec!(0.1),
            max_basis_bps: dec!(50),
            enabled: true,
        };
        let mut engine = FundingArbEngine::new("BTCUSDT", config);

        // 0.01% per 8h ≈ 10.95% APR → should enter.
        let signal = engine.evaluate(dec!(0.0001), dec!(50000), dec!(50010));
        match signal {
            FundingSignal::Enter { size, .. } => assert_eq!(size, dec!(0.1)),
            _ => panic!("expected Enter signal"),
        }
    }

    #[test]
    fn test_hold_when_rate_too_low() {
        let config = FundingArbConfig {
            min_rate_annual_pct: dec!(10),
            max_position: dec!(0.1),
            max_basis_bps: dec!(50),
            enabled: true,
        };
        let mut engine = FundingArbEngine::new("BTCUSDT", config);

        // 0.001% per 8h ≈ 1.095% APR → too low.
        let signal = engine.evaluate(dec!(0.00001), dec!(50000), dec!(50001));
        assert!(matches!(signal, FundingSignal::Hold));
    }
}
