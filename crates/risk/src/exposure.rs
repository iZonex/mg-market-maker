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
