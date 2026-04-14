use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::loss::LossFn;
use crate::metrics::Metrics;
use crate::space::SearchSpace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trial {
    pub id: u64,
    pub params: HashMap<String, f64>,
    pub metrics: Metrics,
    pub loss: f64,
    pub loss_fn: String,
}

/// Random-search hyperparameter optimiser. Suggests parameter sets
/// drawn uniformly from the search space, records each trial with
/// its loss, tracks the best seen, and can persist the full trial
/// log to JSONL for offline analysis (e.g. in a notebook).
pub struct RandomSearch<L: LossFn> {
    space: SearchSpace,
    loss_fn: L,
    rng: ChaCha8Rng,
    trials: Vec<Trial>,
    /// Candidate parameter set returned by the last `suggest` call
    /// that has not yet been `report`-ed.
    pending: Option<HashMap<String, f64>>,
    next_id: u64,
}

impl<L: LossFn> RandomSearch<L> {
    pub fn new(space: SearchSpace, loss_fn: L, seed: u64) -> Self {
        Self {
            space,
            loss_fn,
            rng: ChaCha8Rng::seed_from_u64(seed),
            trials: Vec::new(),
            pending: None,
            next_id: 0,
        }
    }

    /// Return a fresh candidate. The caller runs a backtest with
    /// these parameters and then calls [`report`](Self::report).
    pub fn suggest(&mut self) -> HashMap<String, f64> {
        let params = self.space.sample(&mut self.rng);
        self.pending = Some(params.clone());
        params
    }

    /// Record the result of a trial.
    pub fn report(&mut self, params: HashMap<String, f64>, metrics: Metrics) {
        let loss = self.loss_fn.evaluate(&metrics);
        let trial = Trial {
            id: self.next_id,
            params,
            metrics,
            loss,
            loss_fn: self.loss_fn.name().to_string(),
        };
        self.next_id += 1;
        self.trials.push(trial);
        self.pending = None;
    }

    pub fn trials(&self) -> &[Trial] {
        &self.trials
    }

    /// Lowest-loss trial seen so far, or `None` before any reports.
    pub fn best_trial(&self) -> Option<&Trial> {
        self.trials.iter().min_by(|a, b| {
            a.loss
                .partial_cmp(&b.loss)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// Save the full trial log as newline-delimited JSON. One trial
    /// per line; safe to `tail -f` or stream-load into pandas /
    /// DataFrames.
    pub fn save_jsonl(&self, path: &Path) -> Result<()> {
        let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut w = BufWriter::new(file);
        for trial in &self.trials {
            serde_json::to_writer(&mut w, trial).context("serialise trial")?;
            w.write_all(b"\n").context("write newline")?;
        }
        w.flush().context("flush jsonl")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loss::{MaxDrawdownLoss, SharpeLoss};
    use crate::space::Param;

    fn empty_metrics() -> Metrics {
        Metrics::default()
    }

    #[test]
    fn suggest_returns_values_for_every_param() {
        let space = SearchSpace::new()
            .add(Param::uniform("a", 0.0, 1.0))
            .add(Param::uniform("b", 10.0, 20.0));
        let mut s = RandomSearch::new(space, SharpeLoss, 0);
        let params = s.suggest();
        assert_eq!(params.len(), 2);
        assert!(params.contains_key("a"));
        assert!(params.contains_key("b"));
    }

    #[test]
    fn report_records_trial_with_computed_loss() {
        let space = SearchSpace::new().add(Param::uniform("x", 0.0, 1.0));
        let mut s = RandomSearch::new(space, SharpeLoss, 0);
        let params = s.suggest();
        let mut m = empty_metrics();
        m.sharpe = 2.5;
        s.report(params, m);
        assert_eq!(s.trials().len(), 1);
        assert_eq!(s.trials()[0].loss, -2.5);
        assert_eq!(s.trials()[0].loss_fn, "sharpe");
        assert_eq!(s.trials()[0].id, 0);
    }

    #[test]
    fn best_trial_is_the_lowest_loss() {
        let space = SearchSpace::new().add(Param::uniform("x", 0.0, 1.0));
        let mut s = RandomSearch::new(space, SharpeLoss, 0);
        for sharpe in &[0.5, 2.0, 1.0, 3.5, 0.1] {
            let p = s.suggest();
            let mut m = empty_metrics();
            m.sharpe = *sharpe;
            s.report(p, m);
        }
        let best = s.best_trial().unwrap();
        assert_eq!(best.metrics.sharpe, 3.5);
    }

    #[test]
    fn seeded_search_is_reproducible() {
        let space = SearchSpace::new()
            .add(Param::uniform("a", 0.0, 1.0))
            .add(Param::log_uniform("b", 0.01, 10.0));
        let mut s1 = RandomSearch::new(space.clone(), SharpeLoss, 42);
        let mut s2 = RandomSearch::new(space, SharpeLoss, 42);
        for _ in 0..20 {
            assert_eq!(s1.suggest(), s2.suggest());
            // Don't report — we're just comparing the sample stream.
        }
    }

    #[test]
    fn random_search_finds_a_good_sample_in_reasonable_trials() {
        // Known quadratic objective peaked at x = 0.5.
        // Loss = (x - 0.5)^2; we use MaxDrawdownLoss on `max_drawdown`
        // field as a proxy for a minimisation objective.
        let space = SearchSpace::new().add(Param::uniform("x", 0.0, 1.0));
        let mut s = RandomSearch::new(space, MaxDrawdownLoss, 42);
        for _ in 0..200 {
            let p = s.suggest();
            let x = p["x"];
            let mut m = empty_metrics();
            m.max_drawdown = (x - 0.5).powi(2);
            s.report(p, m);
        }
        let best = s.best_trial().unwrap();
        // 200 uniform samples within [0, 1] should land within 0.05
        // of the optimum with overwhelming probability.
        assert!((best.params["x"] - 0.5).abs() < 0.05);
    }

    #[test]
    fn save_jsonl_roundtrips_every_trial() {
        let space = SearchSpace::new().add(Param::uniform("x", 0.0, 1.0));
        let mut s = RandomSearch::new(space, SharpeLoss, 0);
        for sharpe in [0.1, 0.2, 0.3] {
            let p = s.suggest();
            let mut m = empty_metrics();
            m.sharpe = sharpe;
            s.report(p, m);
        }
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mm-hyperopt-test-{}.jsonl", std::process::id()));
        s.save_jsonl(&path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let t: Trial = serde_json::from_str(line).unwrap();
            assert_eq!(t.id as usize, i);
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn best_trial_returns_none_before_any_reports() {
        let space = SearchSpace::new().add(Param::uniform("x", 0.0, 1.0));
        let s = RandomSearch::new(space, SharpeLoss, 0);
        assert!(s.best_trial().is_none());
    }
}
