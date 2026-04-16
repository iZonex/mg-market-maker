//! Parameter calibration from recorded market data.
//!
//! Takes a JSONL file of recorded events, runs a parameter sweep,
//! and produces a calibration report with the best parameters
//! and a GO / NEEDS_MORE_DATA / UNPROFITABLE recommendation.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;

/// Calibrated strategy parameters.
#[derive(Debug, Clone, Serialize)]
pub struct CalibratedParams {
    pub gamma: Decimal,
    pub kappa: Decimal,
    pub sigma: Decimal,
    pub min_spread_bps: Decimal,
    pub num_levels: usize,
    pub order_size: Decimal,
}

/// Calibration report — summary of parameter sweep results.
#[derive(Debug, Clone, Serialize)]
pub struct CalibrationReport {
    pub symbol: String,
    pub data_points: usize,
    pub data_duration_hours: f64,
    pub best_params: CalibratedParams,
    pub sharpe: Decimal,
    pub max_drawdown: Decimal,
    pub total_pnl: Decimal,
    pub num_fills: u64,
    /// "GO" / "NEEDS_MORE_DATA" / "UNPROFITABLE"
    pub recommendation: String,
}

impl CalibrationReport {
    /// Generate a TOML config snippet for the best parameters.
    pub fn to_toml_snippet(&self) -> String {
        format!(
            r#"[market_maker]
gamma = {}
kappa = {}
sigma = {}
min_spread_bps = {}
num_levels = {}
order_size = {}
# Calibrated from {} data points ({:.1}h)
# Sharpe: {}, Max Drawdown: {}, PnL: {}
# Recommendation: {}"#,
            self.best_params.gamma,
            self.best_params.kappa,
            self.best_params.sigma,
            self.best_params.min_spread_bps,
            self.best_params.num_levels,
            self.best_params.order_size,
            self.data_points,
            self.data_duration_hours,
            self.sharpe,
            self.max_drawdown,
            self.total_pnl,
            self.recommendation,
        )
    }

    /// Determine recommendation from metrics.
    pub fn compute_recommendation(
        data_points: usize,
        data_duration_hours: f64,
        sharpe: Decimal,
        total_pnl: Decimal,
    ) -> String {
        if data_points < 1000 || data_duration_hours < 24.0 {
            return "NEEDS_MORE_DATA".into();
        }
        if sharpe < dec!(0.5) || total_pnl < Decimal::ZERO {
            return "UNPROFITABLE".into();
        }
        "GO".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> CalibrationReport {
        CalibrationReport {
            symbol: "BTCUSDT".into(),
            data_points: 50000,
            data_duration_hours: 168.0,
            best_params: CalibratedParams {
                gamma: dec!(0.15),
                kappa: dec!(1.2),
                sigma: dec!(0.018),
                min_spread_bps: dec!(3),
                num_levels: 3,
                order_size: dec!(0.001),
            },
            sharpe: dec!(1.2),
            max_drawdown: dec!(150),
            total_pnl: dec!(420),
            num_fills: 1200,
            recommendation: "GO".into(),
        }
    }

    #[test]
    fn toml_snippet_contains_params() {
        let report = sample_report();
        let toml = report.to_toml_snippet();
        assert!(toml.contains("gamma = 0.15"));
        assert!(toml.contains("kappa = 1.2"));
        assert!(toml.contains("Recommendation: GO"));
    }

    #[test]
    fn recommendation_needs_more_data() {
        let rec = CalibrationReport::compute_recommendation(500, 12.0, dec!(2.0), dec!(100));
        assert_eq!(rec, "NEEDS_MORE_DATA");
    }

    #[test]
    fn recommendation_unprofitable() {
        let rec = CalibrationReport::compute_recommendation(5000, 48.0, dec!(0.3), dec!(-50));
        assert_eq!(rec, "UNPROFITABLE");
    }

    #[test]
    fn recommendation_go() {
        let rec = CalibrationReport::compute_recommendation(5000, 48.0, dec!(1.5), dec!(200));
        assert_eq!(rec, "GO");
    }
}
