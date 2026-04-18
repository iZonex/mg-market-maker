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
    /// Minimum funding rate to enter (annualized %). Baseline
    /// threshold applied when a symbol has no explicit override.
    pub min_rate_annual_pct: Decimal,
    /// Maximum position size in base asset.
    pub max_position: Decimal,
    /// Maximum basis deviation before exit (in bps).
    pub max_basis_bps: Decimal,
    /// Enable the strategy.
    pub enabled: bool,
    /// Epic 40.5 — per-pair threshold overrides. Key is the
    /// primary symbol (`"BTCUSDT"`, `"ETHUSDT"`, …); value is the
    /// minimum annualised funding rate (%) required to enter on
    /// that specific pair. Rationale: majors (BTC/ETH) run tight
    /// funding; 10% APR is unrealistic and would never fire.
    /// Alts funding can spike to 100%+ APR, so a flat 10% floor
    /// under-filters them. Operators calibrate per pair from
    /// historical distributions.
    ///
    /// Example config:
    /// ```toml
    /// [funding_arb.per_pair_min_rate_annual_pct]
    /// BTCUSDT = 5
    /// ETHUSDT = 5
    /// SOLUSDT = 15
    /// DOGEUSDT = 25
    /// ```
    #[serde(default)]
    pub per_pair_min_rate_annual_pct:
        std::collections::HashMap<String, Decimal>,
    /// Epic 40.5 — estimated cross-venue transfer latency cost,
    /// in bps. Subtracted from the expected carry when the
    /// strategy evaluates entry profitability on a symbol that
    /// requires moving collateral between venues. Conservative
    /// default is 8 bps (≈ 3-day transfer × 10% APR borrow cost,
    /// per Amberdata funding-arb guide).
    #[serde(default = "default_transfer_latency_cost_bps")]
    pub transfer_latency_cost_bps: Decimal,
}

fn default_transfer_latency_cost_bps() -> Decimal {
    dec!(8)
}

impl FundingArbConfig {
    /// Resolve the entry threshold for a specific symbol. Falls
    /// back to the baseline `min_rate_annual_pct` when no
    /// per-pair override is configured.
    pub fn threshold_for(&self, symbol: &str) -> Decimal {
        self.per_pair_min_rate_annual_pct
            .get(symbol)
            .copied()
            .unwrap_or(self.min_rate_annual_pct)
    }
}

impl Default for FundingArbConfig {
    fn default() -> Self {
        Self {
            min_rate_annual_pct: dec!(10), // 10% APR baseline
            max_position: dec!(0.1),       // 0.1 BTC max
            max_basis_bps: dec!(50),       // 50 bps max basis
            enabled: false,                // Off by default
            per_pair_min_rate_annual_pct: std::collections::HashMap::new(),
            transfer_latency_cost_bps: default_transfer_latency_cost_bps(),
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

    /// Incrementally adjust the spot leg's position from a real
    /// exchange fill. Keeps `FundingArbState` in sync with the
    /// live OMS rather than relying on the ideal `on_entry`
    /// amount written at dispatch time. Caller passes a signed
    /// qty (positive = buy, negative = sell).
    pub fn apply_spot_fill(&mut self, signed_qty: Decimal) {
        self.state.spot_position += signed_qty;
        self.state.net_delta = self.state.spot_position + self.state.perp_position;
    }

    /// Same as [`apply_spot_fill`] for the perp leg.
    pub fn apply_perp_fill(&mut self, signed_qty: Decimal) {
        self.state.perp_position += signed_qty;
        self.state.net_delta = self.state.spot_position + self.state.perp_position;
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
            ..Default::default()
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
            ..Default::default()
        };
        let mut engine = FundingArbEngine::new("BTCUSDT", config);

        // 0.001% per 8h ≈ 1.095% APR → too low.
        let signal = engine.evaluate(dec!(0.00001), dec!(50000), dec!(50001));
        assert!(matches!(signal, FundingSignal::Hold));
    }

    // ── Property-based tests (Epic 20) ────────────────────────

    use proptest::prelude::*;

    fn mk_config(min_pct: Decimal) -> FundingArbConfig {
        FundingArbConfig {
            min_rate_annual_pct: min_pct,
            max_position: dec!(0.1),
            max_basis_bps: dec!(50),
            enabled: true,
            ..Default::default()
        }
    }

    proptest! {
        /// When disabled, every evaluation returns Hold regardless
        /// of inputs. Catches a regression where enable-flag fails
        /// to short-circuit.
        #[test]
        fn disabled_engine_always_holds(
            rate_raw in -100i64..100,
            spot_raw in 10_000i64..100_000,
            perp_raw in 10_000i64..100_000,
        ) {
            let cfg = FundingArbConfig { enabled: false, ..FundingArbConfig::default() };
            let mut e = FundingArbEngine::new("BTCUSDT", cfg);
            let rate = Decimal::new(rate_raw, 5);
            let spot = Decimal::new(spot_raw, 0);
            let perp = Decimal::new(perp_raw, 0);
            let sig = e.evaluate(rate, spot, perp);
            prop_assert!(matches!(sig, FundingSignal::Hold));
        }

        /// Entry side selection: positive funding rate → long spot +
        /// short perp. Negative → sell spot + long perp. Invariant
        /// the strategy's P&L hinges on.
        #[test]
        fn entry_side_matches_funding_sign(
            rate_raw in 1i64..1000,  // non-zero rate magnitude
            positive in any::<bool>(),
        ) {
            let cfg = mk_config(dec!(5));
            let mut e = FundingArbEngine::new("BTCUSDT", cfg);
            // Build rate that annualizes above threshold.
            let magnitude = Decimal::new(rate_raw, 4); // 0.0001..=0.1
            let rate = if positive { magnitude } else { -magnitude };
            let sig = e.evaluate(rate, dec!(50_000), dec!(50_010));
            match sig {
                FundingSignal::Enter { spot_side, perp_side, .. } => {
                    if positive {
                        prop_assert!(matches!(spot_side, SpotAction::Buy));
                        prop_assert!(matches!(perp_side, PerpAction::Short));
                    } else {
                        prop_assert!(matches!(spot_side, SpotAction::Sell));
                        prop_assert!(matches!(perp_side, PerpAction::Long));
                    }
                }
                FundingSignal::Hold => {
                    // Below threshold — acceptable.
                }
                FundingSignal::Exit { .. } => prop_assert!(false, "fresh engine should not exit"),
            }
        }

        /// Fills accumulate: apply_spot_fill(a) + apply_spot_fill(b)
        /// equals one fill of (a+b). net_delta stays the sum of legs.
        #[test]
        fn fills_accumulate_additively(
            spot_fills in proptest::collection::vec(-100i64..100, 1..20),
            perp_fills in proptest::collection::vec(-100i64..100, 1..20),
        ) {
            let mut e = FundingArbEngine::new("BTCUSDT", FundingArbConfig::default());
            let mut spot_sum = dec!(0);
            let mut perp_sum = dec!(0);
            for s in &spot_fills {
                let q = Decimal::new(*s, 2);
                e.apply_spot_fill(q);
                spot_sum += q;
            }
            for p in &perp_fills {
                let q = Decimal::new(*p, 2);
                e.apply_perp_fill(q);
                perp_sum += q;
            }
            prop_assert_eq!(e.state().spot_position, spot_sum);
            prop_assert_eq!(e.state().perp_position, perp_sum);
            prop_assert_eq!(e.state().net_delta, spot_sum + perp_sum);
        }

        /// total_pnl = accumulated_funding + basis_pnl. Simple
        /// algebraic invariant; catches a refactor that drops a term.
        #[test]
        fn total_pnl_sum_invariant(
            funding_raw in -1_000_000i64..1_000_000,
            basis_raw in -1_000_000i64..1_000_000,
        ) {
            let mut s = FundingArbState::new("BTCUSDT");
            s.accumulated_funding = Decimal::new(funding_raw, 2);
            s.basis_pnl = Decimal::new(basis_raw, 2);
            prop_assert_eq!(s.total_pnl(), s.accumulated_funding + s.basis_pnl);
        }

        /// on_funding_payment: cumulative sum equals individual
        /// payments; period count equals call count.
        #[test]
        fn funding_payments_accumulate(
            payments in proptest::collection::vec(-1_000_000i64..1_000_000, 0..30),
        ) {
            let mut e = FundingArbEngine::new("BTCUSDT", FundingArbConfig::default());
            let mut expected_total = dec!(0);
            for p in &payments {
                let d = Decimal::new(*p, 2);
                e.on_funding_payment(d);
                expected_total += d;
            }
            prop_assert_eq!(e.state().accumulated_funding, expected_total);
            prop_assert_eq!(e.state().funding_periods, payments.len() as u64);
        }

        /// on_exit flattens all positions. Regardless of prior state
        /// spot/perp/net_delta land at zero.
        #[test]
        fn on_exit_flattens_positions(
            spot in -1_000_000i64..1_000_000,
            perp in -1_000_000i64..1_000_000,
        ) {
            let mut e = FundingArbEngine::new("BTCUSDT", FundingArbConfig::default());
            e.apply_spot_fill(Decimal::new(spot, 2));
            e.apply_perp_fill(Decimal::new(perp, 2));
            e.on_exit();
            prop_assert_eq!(e.state().spot_position, dec!(0));
            prop_assert_eq!(e.state().perp_position, dec!(0));
            prop_assert_eq!(e.state().net_delta, dec!(0));
        }

        /// is_open() iff at least one leg is non-zero.
        #[test]
        fn is_open_matches_any_nonzero_leg(
            spot in -100i64..100,
            perp in -100i64..100,
        ) {
            let mut s = FundingArbState::new("BTCUSDT");
            s.spot_position = Decimal::new(spot, 2);
            s.perp_position = Decimal::new(perp, 2);
            let expected = !s.spot_position.is_zero() || !s.perp_position.is_zero();
            prop_assert_eq!(s.is_open(), expected);
        }
    }
}
