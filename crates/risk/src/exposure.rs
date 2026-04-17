use mm_common::config::RiskConfig;
use mm_common::types::Price;
use rust_decimal::Decimal;
use tracing::warn;

/// Tracks total exposure and drawdown.
pub struct ExposureManager {
    /// Peak equity (for drawdown calculation).
    peak_equity: Decimal,
    /// Starting equity.
    _initial_equity: Decimal,
}

impl ExposureManager {
    pub fn new(initial_equity: Decimal) -> Self {
        Self {
            peak_equity: initial_equity,
            _initial_equity: initial_equity,
        }
    }

    /// Update peak equity after a PnL update.
    pub fn update_equity(&mut self, current_equity: Decimal) {
        if current_equity > self.peak_equity {
            self.peak_equity = current_equity;
        }
    }

    /// Current drawdown from peak.
    pub fn drawdown(&self, current_equity: Decimal) -> Decimal {
        self.peak_equity - current_equity
    }

    /// Check if we exceed max drawdown.
    pub fn is_drawdown_breached(&self, current_equity: Decimal, config: &RiskConfig) -> bool {
        let dd = self.drawdown(current_equity);
        if dd > config.max_drawdown_quote {
            warn!(
                drawdown = %dd,
                max = %config.max_drawdown_quote,
                "drawdown limit breached"
            );
            return true;
        }
        false
    }

    /// Check if position value exceeds max exposure.
    pub fn is_exposure_breached(
        &self,
        inventory: Decimal,
        mark_price: Price,
        config: &RiskConfig,
    ) -> bool {
        let exposure = inventory.abs() * mark_price;
        if exposure > config.max_exposure_quote {
            warn!(
                exposure = %exposure,
                max = %config.max_exposure_quote,
                "exposure limit breached"
            );
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    fn mk_risk(max_dd: Decimal, max_exp: Decimal) -> RiskConfig {
        RiskConfig {
            max_inventory: dec!(10),
            max_exposure_quote: max_exp,
            max_drawdown_quote: max_dd,
            inventory_skew_factor: dec!(1),
            max_spread_bps: dec!(500),
            max_spread_to_quote_bps: None,
            stale_book_timeout_secs: 10,
            max_order_size: dec!(0),
            max_daily_volume_quote: dec!(0),
            max_hourly_volume_quote: dec!(0),
        }
    }

    prop_compose! {
        fn equity_strat()(raw in -1_000_000_000i64..1_000_000_000i64) -> Decimal {
            Decimal::new(raw, 2)
        }
    }

    proptest! {
        /// peak_equity is monotonic — update_equity never
        /// lowers it. Catches a regression where the max path
        /// accidentally wrote a smaller value.
        #[test]
        fn peak_is_monotonic(
            initial in equity_strat(),
            updates in proptest::collection::vec(equity_strat(), 0..30),
        ) {
            let mut em = ExposureManager::new(initial);
            let mut prev = initial;
            for u in &updates {
                em.update_equity(*u);
                let dd = em.drawdown(*u);
                // peak is whatever was tracked so far
                let peak = dd + *u;
                prop_assert!(peak >= prev,
                    "peak regressed {} → {}", prev, peak);
                prev = peak;
            }
        }

        /// Drawdown at the current equity is always non-negative
        /// because peak ≥ current by construction. Catches a
        /// sign flip.
        #[test]
        fn drawdown_is_non_negative(
            initial in equity_strat(),
            updates in proptest::collection::vec(equity_strat(), 1..20),
        ) {
            let mut em = ExposureManager::new(initial);
            for u in &updates {
                em.update_equity(*u);
            }
            // Drawdown at any past value is bounded below by 0
            // only when queried at the peak. Query at the last
            // update (a real dashboard pattern).
            let last = *updates.last().unwrap();
            let dd = em.drawdown(last);
            prop_assert!(dd >= dec!(0), "drawdown {} < 0", dd);
        }

        /// is_exposure_breached matches the arithmetic
        /// |inventory|·mark > max. Independent of drawdown state.
        #[test]
        fn exposure_breach_matches_arithmetic(
            inventory in equity_strat(),
            mark_raw in 1i64..10_000_000i64,
            max_raw in 1i64..10_000_000_000i64,
        ) {
            let em = ExposureManager::new(dec!(0));
            let mark = Decimal::new(mark_raw, 2);
            let max_exp = Decimal::new(max_raw, 2);
            let cfg = mk_risk(dec!(1_000_000), max_exp);
            let expected = inventory.abs() * mark > max_exp;
            prop_assert_eq!(em.is_exposure_breached(inventory, mark, &cfg), expected);
        }
    }
}
