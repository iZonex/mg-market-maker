//! Loss functions. Each reduces a [`Metrics`] snapshot to a scalar
//! "badness" score — lower is better.
//!
//! Port of Freqtrade's hyperopt loss library, shaped for MM workflows.

use crate::metrics::Metrics;

pub trait LossFn: Send + Sync {
    fn evaluate(&self, metrics: &Metrics) -> f64;
    fn name(&self) -> &'static str;
}

/// Maximise Sharpe. Loss = `-sharpe`.
pub struct SharpeLoss;

impl LossFn for SharpeLoss {
    fn evaluate(&self, m: &Metrics) -> f64 {
        -m.sharpe
    }
    fn name(&self) -> &'static str {
        "sharpe"
    }
}

/// Maximise Sortino. Loss = `-sortino`.
pub struct SortinoLoss;

impl LossFn for SortinoLoss {
    fn evaluate(&self, m: &Metrics) -> f64 {
        -m.sortino
    }
    fn name(&self) -> &'static str {
        "sortino"
    }
}

/// Maximise Calmar (annualised-return / max-drawdown). Loss = `-calmar`.
pub struct CalmarLoss;

impl LossFn for CalmarLoss {
    fn evaluate(&self, m: &Metrics) -> f64 {
        -m.calmar
    }
    fn name(&self) -> &'static str {
        "calmar"
    }
}

/// Minimise max drawdown directly. Loss = `max_drawdown`.
pub struct MaxDrawdownLoss;

impl LossFn for MaxDrawdownLoss {
    fn evaluate(&self, m: &Metrics) -> f64 {
        m.max_drawdown
    }
    fn name(&self) -> &'static str {
        "max_drawdown"
    }
}

/// Linear combination of normalised metrics. Use it to balance
/// profitability against risk with explicit weights.
///
/// Loss = `w_dd * max_drawdown - w_sharpe * sharpe - w_pnl * total_pnl`
///
/// All weights default to zero; set the ones you care about to any
/// non-zero value. Units mix (drawdown is quote currency; sharpe is
/// dimensionless) — normalise at the caller if needed.
#[derive(Debug, Clone)]
pub struct MultiMetricLoss {
    pub w_drawdown: f64,
    pub w_sharpe: f64,
    pub w_sortino: f64,
    pub w_calmar: f64,
    pub w_pnl: f64,
}

impl Default for MultiMetricLoss {
    fn default() -> Self {
        Self {
            w_drawdown: 1.0,
            w_sharpe: 1.0,
            w_sortino: 0.0,
            w_calmar: 0.0,
            w_pnl: 0.0,
        }
    }
}

impl LossFn for MultiMetricLoss {
    fn evaluate(&self, m: &Metrics) -> f64 {
        self.w_drawdown * m.max_drawdown
            - self.w_sharpe * m.sharpe
            - self.w_sortino * m.sortino
            - self.w_calmar * m.calmar
            - self.w_pnl * m.total_pnl
    }
    fn name(&self) -> &'static str {
        "multi_metric"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(sharpe: f64, sortino: f64, calmar: f64, dd: f64, pnl: f64) -> Metrics {
        Metrics {
            sharpe,
            sortino,
            calmar,
            max_drawdown: dd,
            total_pnl: pnl,
            num_trades: 0,
            fill_rate: 0.0,
        }
    }

    #[test]
    fn sharpe_loss_is_negated() {
        assert_eq!(SharpeLoss.evaluate(&m(2.5, 0.0, 0.0, 0.0, 0.0)), -2.5);
    }

    #[test]
    fn sortino_loss_is_negated() {
        assert_eq!(SortinoLoss.evaluate(&m(0.0, 3.0, 0.0, 0.0, 0.0)), -3.0);
    }

    #[test]
    fn calmar_loss_is_negated() {
        assert_eq!(CalmarLoss.evaluate(&m(0.0, 0.0, 1.7, 0.0, 0.0)), -1.7);
    }

    #[test]
    fn max_drawdown_loss_is_positive_number() {
        assert_eq!(
            MaxDrawdownLoss.evaluate(&m(0.0, 0.0, 0.0, 200.0, 0.0)),
            200.0
        );
    }

    #[test]
    fn multi_metric_default_penalises_drawdown_and_rewards_sharpe() {
        let l = MultiMetricLoss::default();
        // drawdown=100, sharpe=2 → 1*100 - 1*2 - 0 - 0 - 0 = 98
        assert_eq!(l.evaluate(&m(2.0, 0.0, 0.0, 100.0, 0.0)), 98.0);
    }

    #[test]
    fn better_metrics_give_lower_loss() {
        let worse = m(0.5, 0.5, 0.1, 500.0, 100.0);
        let better = m(2.0, 2.0, 1.5, 100.0, 500.0);
        assert!(SharpeLoss.evaluate(&better) < SharpeLoss.evaluate(&worse));
        assert!(MaxDrawdownLoss.evaluate(&better) < MaxDrawdownLoss.evaluate(&worse));
        assert!(
            MultiMetricLoss::default().evaluate(&better)
                < MultiMetricLoss::default().evaluate(&worse)
        );
    }

    /// Property: sorting trials by `loss` (ascending) must match the
    /// ordering of the metric the loss maximises (descending for
    /// Sharpe/Sortino/Calmar, ascending for MaxDrawdown). This
    /// catches sign errors or sort-direction mistakes that trivial
    /// "lower is better" assertions miss.
    #[test]
    fn loss_functions_are_monotonic_in_the_metric_they_optimise() {
        // Construct a small spread of Sharpe values and check that
        // sorting by SharpeLoss ascending matches sorting by Sharpe
        // descending.
        let sharpes = [0.1, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0];
        let mut by_sharpe: Vec<(f64, f64)> = sharpes
            .iter()
            .map(|&s| (s, SharpeLoss.evaluate(&m(s, 0.0, 0.0, 0.0, 0.0))))
            .collect();
        by_sharpe.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let sharpe_order: Vec<f64> = by_sharpe.iter().map(|t| t.0).collect();
        let mut expected_desc = sharpes.to_vec();
        expected_desc.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert_eq!(sharpe_order, expected_desc);

        // Same for Sortino.
        let sortinos = [0.0, 0.5, 1.0, 2.0, 3.5];
        let mut by_sortino: Vec<(f64, f64)> = sortinos
            .iter()
            .map(|&s| (s, SortinoLoss.evaluate(&m(0.0, s, 0.0, 0.0, 0.0))))
            .collect();
        by_sortino.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let sortino_order: Vec<f64> = by_sortino.iter().map(|t| t.0).collect();
        let mut expected_desc_sortino = sortinos.to_vec();
        expected_desc_sortino.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert_eq!(sortino_order, expected_desc_sortino);

        // MaxDrawdown is a minimise-the-metric loss, so ascending
        // loss matches ascending drawdown.
        let dds = [50.0, 100.0, 250.0, 500.0, 1000.0];
        let mut by_dd: Vec<(f64, f64)> = dds
            .iter()
            .map(|&d| (d, MaxDrawdownLoss.evaluate(&m(0.0, 0.0, 0.0, d, 0.0))))
            .collect();
        by_dd.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let dd_order: Vec<f64> = by_dd.iter().map(|t| t.0).collect();
        assert_eq!(dd_order, dds.to_vec());
    }

    #[test]
    fn loss_names_are_stable_strings() {
        assert_eq!(SharpeLoss.name(), "sharpe");
        assert_eq!(SortinoLoss.name(), "sortino");
        assert_eq!(CalmarLoss.name(), "calmar");
        assert_eq!(MaxDrawdownLoss.name(), "max_drawdown");
        assert_eq!(MultiMetricLoss::default().name(), "multi_metric");
    }
}
