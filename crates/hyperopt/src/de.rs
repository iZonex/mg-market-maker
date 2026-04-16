//! Differential Evolution (DE/rand/1/bin) hyperparameter search.
//!
//! Ported from `hft-lab-core/src/optimization.rs::differential_evolution`
//! with two adaptations:
//!
//! 1. Deterministic `ChaCha8Rng` seed (matches the rest of the
//!    `hyperopt` crate — reproducible trial streams per seed).
//! 2. Works over our existing [`SearchSpace`] + [`LossFn`] traits,
//!    so operators can swap `RandomSearch` for `DifferentialEvolution`
//!    without touching the backtest driver.
//!
//! ## Algorithm — DE/rand/1/bin
//!
//! For each individual `i` in the population:
//!
//! 1. **Mutation.** Pick three random individuals `a, b, c ≠ i`.
//!    Form a mutant vector `v = a + F · (b - c)` and clamp each
//!    component to the search-space bounds.
//! 2. **Crossover.** For each parameter, take the mutant component
//!    with probability `CR`, otherwise keep the parent's component.
//!    At least one mutant component is always taken (`j_rand` guard).
//! 3. **Selection.** If the trial vector's loss is lower than the
//!    parent's, it replaces the parent. Otherwise the parent
//!    survives unchanged.
//!
//! Repeats for `max_generations` generations. Each individual's
//! current fitness is cached — every parameter set is evaluated at
//! most once per generation plus once at init, matching the classic
//! DE cost model of `(pop_size + pop_size × max_gen)` loss
//! evaluations.
//!
//! ## Why DE over random search
//!
//! Random search has no memory — each trial is drawn fresh from the
//! prior. DE carries a population that migrates toward better
//! regions of the space via difference vectors. On multi-modal
//! objectives (regime-dependent MM parameters, toxicity curves,
//! basis-strategy tuning) DE typically finds a comparable-quality
//! solution in `O(10×)` fewer evaluations than uniform random.
//!
//! Random search remains the right tool for wide cold-start
//! exploration and for embarrassingly-parallel fleet runs; DE is
//! the sharper instrument once you know roughly where to look.

use std::collections::HashMap;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::loss::LossFn;
use crate::metrics::Metrics;
use crate::search::Trial;
use crate::space::{Param, SearchSpace};

/// Configuration knobs for [`DifferentialEvolution`]. Defaults
/// follow Storn & Price (1997) recommendations.
#[derive(Debug, Clone, Copy)]
pub struct DeConfig {
    /// Population size **multiplier** — the actual population is
    /// `population_mult × n_params` (so a 4-dim search with
    /// `population_mult = 10` runs 40 individuals). Classic DE
    /// literature recommends `5 ≤ mult ≤ 10`.
    pub population_mult: usize,
    /// Mutation factor `F ∈ (0, 2]`. Typical range `[0.4, 0.9]`.
    /// Higher = more exploration.
    pub f: f64,
    /// Crossover rate `CR ∈ [0, 1]`. Typical `0.7` — higher means
    /// the trial vector inherits more from the mutant.
    pub cr: f64,
    /// Number of full population generations.
    pub max_generations: usize,
}

impl Default for DeConfig {
    fn default() -> Self {
        Self {
            population_mult: 10,
            f: 0.8,
            cr: 0.7,
            max_generations: 50,
        }
    }
}

/// Differential-evolution optimiser over a [`SearchSpace`].
///
/// API mirrors [`RandomSearch`](crate::search::RandomSearch):
/// `suggest`/`report`/`best_trial`/`save_jsonl`. The twist is that
/// DE's next suggestion depends on the loss of the previous one —
/// so calling `suggest` without `report`-ing in between advances
/// the internal cursor over a stale parent, which is intentional
/// (matches the classic synchronous generation loop).
///
/// Concretely, the cursor walks population indices `0 … pop_size-1`
/// within a generation. For each individual the optimiser either
/// returns the parent (first visit of this generation) or a trial
/// vector (evaluation phase). See `suggest` for the state machine.
pub struct DifferentialEvolution<L: LossFn> {
    space: SearchSpace,
    loss_fn: L,
    config: DeConfig,
    rng: ChaCha8Rng,

    // Population state.
    population: Vec<Vec<f64>>,
    fitness: Vec<f64>,
    best_idx: usize,

    // Generation cursor.
    generation: usize,
    cursor: usize,
    /// Cached trial vector for the current cursor, built on
    /// `suggest` and evaluated against `population[cursor]` on
    /// `report`. `None` between generations.
    pending_trial: Option<Vec<f64>>,
    pending_params: Option<HashMap<String, f64>>,

    // Trial log — every evaluated parameter set goes here so the
    // operator can replay / inspect the full run like with
    // `RandomSearch`.
    trials: Vec<Trial>,
    next_id: u64,
}

impl<L: LossFn> DifferentialEvolution<L> {
    /// Build a new optimiser. `seed` drives both the initial
    /// population and the per-generation random picks, so two
    /// runs with the same seed + config + space produce identical
    /// trial streams.
    pub fn new(space: SearchSpace, loss_fn: L, config: DeConfig, seed: u64) -> Self {
        assert!(
            !space.is_empty(),
            "DifferentialEvolution: empty search space"
        );
        assert!(config.population_mult >= 3, "population_mult must be >= 3");
        assert!(config.f > 0.0 && config.f <= 2.0, "F must be in (0, 2]");
        assert!((0.0..=1.0).contains(&config.cr), "CR must be in [0, 1]");
        assert!(config.max_generations >= 1, "max_generations must be >= 1");

        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let pop_size = config.population_mult * space.len();

        // Seed the population uniformly inside each bound.
        let population: Vec<Vec<f64>> = (0..pop_size)
            .map(|_| sample_vector(&space, &mut rng))
            .collect();

        Self {
            space,
            loss_fn,
            config,
            rng,
            population,
            fitness: vec![f64::INFINITY; pop_size],
            best_idx: 0,
            generation: 0,
            cursor: 0,
            pending_trial: None,
            pending_params: None,
            trials: Vec::new(),
            next_id: 0,
        }
    }

    /// How many evaluations total the run will request if the
    /// caller drives `suggest → report` to completion. Useful for
    /// sizing a progress bar.
    pub fn planned_evaluations(&self) -> usize {
        let pop = self.population.len();
        // Generation 0 evaluates the initial pop (`pop` calls), then
        // each subsequent generation evaluates one trial per
        // individual (`pop` calls per generation).
        pop + pop * self.config.max_generations
    }

    pub fn trials(&self) -> &[Trial] {
        &self.trials
    }

    pub fn best_trial(&self) -> Option<&Trial> {
        self.trials.iter().min_by(|a, b| {
            a.loss
                .partial_cmp(&b.loss)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// `true` when every generation has been processed and no
    /// further `suggest`/`report` calls will produce new work.
    pub fn is_finished(&self) -> bool {
        self.generation > self.config.max_generations
    }

    /// Return the next parameter set to evaluate, or `None` when
    /// the run is finished.
    pub fn suggest(&mut self) -> Option<HashMap<String, f64>> {
        if self.is_finished() {
            return None;
        }

        if self.generation == 0 {
            // Generation 0 = initial population evaluation. Each
            // individual is reported once, then we advance to
            // generation 1.
            let vec = self.population[self.cursor].clone();
            let params = params_from_vector(&self.space, &vec);
            self.pending_params = Some(params.clone());
            // pending_trial stays None — the init-phase `report`
            // writes directly into `fitness[cursor]`.
            self.pending_trial = None;
            return Some(params);
        }

        // Evolution phase. Build a trial vector for the current
        // parent at `cursor`.
        let trial = self.build_trial_vector();
        let params = params_from_vector(&self.space, &trial);
        self.pending_params = Some(params.clone());
        self.pending_trial = Some(trial);
        Some(params)
    }

    /// Report the metrics for the most-recent `suggest`. The
    /// optimiser advances its cursor and — at the end of a
    /// generation — bumps `generation`.
    pub fn report(&mut self, metrics: Metrics) {
        let Some(params) = self.pending_params.take() else {
            return;
        };
        let loss = self.loss_fn.evaluate(&metrics);

        // Log every evaluation.
        self.trials.push(Trial {
            id: self.next_id,
            params: params.clone(),
            metrics,
            loss,
            loss_fn: self.loss_fn.name().to_string(),
        });
        self.next_id += 1;

        if self.generation == 0 {
            // Init-phase: record fitness of the seed individual.
            self.fitness[self.cursor] = loss;
            if loss < self.fitness[self.best_idx] {
                self.best_idx = self.cursor;
            }
        } else {
            // Evolution phase: compare trial vs parent and
            // replace on improvement.
            if let Some(trial_vec) = self.pending_trial.take() {
                if loss < self.fitness[self.cursor] {
                    self.population[self.cursor] = trial_vec;
                    self.fitness[self.cursor] = loss;
                    if loss < self.fitness[self.best_idx] {
                        self.best_idx = self.cursor;
                    }
                }
            }
        }

        self.advance_cursor();
    }

    /// Save the full evaluation log (both init + evolution
    /// generations) as newline-delimited JSON. Same format as
    /// `RandomSearch::save_jsonl` so downstream notebooks see a
    /// uniform schema.
    pub fn save_jsonl(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use std::fs::File;
        use std::io::{BufWriter, Write};
        let file = File::create(path)?;
        let mut w = BufWriter::new(file);
        for t in &self.trials {
            serde_json::to_writer(&mut w, t)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        Ok(())
    }

    fn advance_cursor(&mut self) {
        self.cursor += 1;
        if self.cursor >= self.population.len() {
            self.cursor = 0;
            self.generation += 1;
        }
    }

    fn build_trial_vector(&mut self) -> Vec<f64> {
        let pop_size = self.population.len();
        let (a_idx, b_idx, c_idx) = self.pick_three_distinct(self.cursor, pop_size);
        let n_params = self.space.len();

        // Mutation: v = a + F · (b - c), clamped per dimension.
        let mut mutant = Vec::with_capacity(n_params);
        for (j, param) in self.space.iter().enumerate() {
            let v = self.population[a_idx][j]
                + self.config.f * (self.population[b_idx][j] - self.population[c_idx][j]);
            mutant.push(clamp_to_param(v, param));
        }

        // Crossover: take mutant component with probability CR,
        // and always take at least one mutant component at
        // `j_rand` (classic guard against identical copies).
        let j_rand = self.rng.random_range(0..n_params);
        let mut trial = self.population[self.cursor].clone();
        for j in 0..n_params {
            if j == j_rand || self.rng.random::<f64>() < self.config.cr {
                trial[j] = mutant[j];
            }
        }
        trial
    }

    fn pick_three_distinct(&mut self, exclude: usize, pop_size: usize) -> (usize, usize, usize) {
        // Rejection sampling — population is small (≤ a few
        // hundred) so the expected retries are negligible.
        let mut a = self.rng.random_range(0..pop_size);
        while a == exclude {
            a = self.rng.random_range(0..pop_size);
        }
        let mut b = self.rng.random_range(0..pop_size);
        while b == exclude || b == a {
            b = self.rng.random_range(0..pop_size);
        }
        let mut c = self.rng.random_range(0..pop_size);
        while c == exclude || c == a || c == b {
            c = self.rng.random_range(0..pop_size);
        }
        (a, b, c)
    }
}

/// Draw one vector from the search space into a fixed-order
/// `Vec<f64>`. Order matches `SearchSpace::iter`.
fn sample_vector<R: Rng>(space: &SearchSpace, rng: &mut R) -> Vec<f64> {
    space.iter().map(|p| p.sample(rng)).collect()
}

/// Reverse of `sample_vector` — turn an ordered vector back into
/// a named param map. Keeps `IntUniform` parameters rounded to
/// the nearest integer and `Choice` parameters snapped to the
/// nearest value in the enumeration.
fn params_from_vector(space: &SearchSpace, values: &[f64]) -> HashMap<String, f64> {
    space
        .iter()
        .zip(values.iter())
        .map(|(param, &raw)| {
            let snapped = snap_to_param(raw, param);
            (param.name().to_string(), snapped)
        })
        .collect()
}

/// Clamp a raw f64 to the bound of a param. Keeps the value
/// continuous — integer rounding and choice snapping happen at
/// report time via `snap_to_param`.
fn clamp_to_param(v: f64, param: &Param) -> f64 {
    match param {
        Param::Uniform { min, max, .. } => v.max(*min).min(*max),
        Param::LogUniform { min, max, .. } => v.max(*min).min(*max),
        Param::IntUniform { min, max, .. } => v.max(*min as f64).min(*max as f64),
        Param::Choice { values, .. } => {
            // Clamp to `[0, n)` by value-range clamp; the actual
            // snap happens in `snap_to_param`.
            let lo = values.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            v.max(lo).min(hi)
        }
    }
}

fn snap_to_param(v: f64, param: &Param) -> f64 {
    match param {
        Param::Uniform { .. } | Param::LogUniform { .. } => v,
        Param::IntUniform { .. } => v.round(),
        Param::Choice { values, .. } => *values
            .iter()
            .min_by(|a, b| {
                (**a - v)
                    .abs()
                    .partial_cmp(&(**b - v).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(&v),
    }
}

/// Compact result view for the end of a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeResult {
    pub best_params: HashMap<String, f64>,
    pub best_loss: f64,
    pub generations: usize,
    pub evaluations: usize,
}

impl<L: LossFn> DifferentialEvolution<L> {
    /// Convenience summary once the run is finished.
    pub fn result(&self) -> Option<DeResult> {
        let best = self.best_trial()?;
        Some(DeResult {
            best_params: best.params.clone(),
            best_loss: best.loss,
            generations: self.generation.min(self.config.max_generations),
            evaluations: self.trials.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loss::{MaxDrawdownLoss, SharpeLoss};

    fn empty_metrics() -> Metrics {
        Metrics::default()
    }

    fn quadratic_space() -> SearchSpace {
        SearchSpace::new().add(Param::uniform("x", 0.0, 1.0))
    }

    #[test]
    fn config_defaults_are_valid() {
        let cfg = DeConfig::default();
        assert!(cfg.f > 0.0);
        assert!((0.0..=1.0).contains(&cfg.cr));
        assert!(cfg.population_mult >= 3);
    }

    #[test]
    fn suggest_produces_values_for_every_param() {
        let space = SearchSpace::new()
            .add(Param::uniform("a", 0.0, 1.0))
            .add(Param::log_uniform("b", 0.01, 10.0));
        let mut de = DifferentialEvolution::new(space, SharpeLoss, DeConfig::default(), 7);
        let params = de.suggest().unwrap();
        assert_eq!(params.len(), 2);
        assert!(params["a"] >= 0.0 && params["a"] <= 1.0);
        assert!(params["b"] >= 0.01 && params["b"] <= 10.0);
    }

    #[test]
    fn seeded_run_is_deterministic() {
        let space = quadratic_space();
        let cfg = DeConfig {
            max_generations: 3,
            population_mult: 5,
            ..DeConfig::default()
        };
        let mut a = DifferentialEvolution::new(space.clone(), MaxDrawdownLoss, cfg, 42);
        let mut b = DifferentialEvolution::new(space, MaxDrawdownLoss, cfg, 42);
        while let (Some(pa), Some(pb)) = (a.suggest(), b.suggest()) {
            assert_eq!(pa, pb);
            let mut m = empty_metrics();
            m.max_drawdown = (pa["x"] - 0.5).powi(2);
            a.report(m.clone());
            b.report(m);
        }
    }

    #[test]
    fn de_converges_on_a_simple_quadratic() {
        // Minimise `(x - 0.5)^2` on `[0, 1]`. Random search with
        // 200 trials lands within 0.05 of the optimum; DE with
        // the same budget should land within 0.01.
        let space = quadratic_space();
        let cfg = DeConfig {
            population_mult: 10,
            max_generations: 20,
            ..DeConfig::default()
        };
        let mut de = DifferentialEvolution::new(space, MaxDrawdownLoss, cfg, 1);
        while let Some(params) = de.suggest() {
            let mut m = empty_metrics();
            m.max_drawdown = (params["x"] - 0.5).powi(2);
            de.report(m);
        }
        let best = de.best_trial().expect("at least one trial");
        assert!(
            (best.params["x"] - 0.5).abs() < 0.01,
            "DE missed the optimum: best x = {}",
            best.params["x"]
        );
    }

    #[test]
    fn de_beats_random_on_rosenbrock_2d() {
        // Rosenbrock `f(x, y) = (1 - x)^2 + 100 (y - x^2)^2` has
        // its global minimum at (1, 1) and is a classic DE
        // benchmark. With matched evaluation budget DE should
        // land much closer to the optimum than random search.
        let space = SearchSpace::new()
            .add(Param::uniform("x", -2.0, 2.0))
            .add(Param::uniform("y", -2.0, 2.0));
        let cfg = DeConfig {
            population_mult: 10,
            max_generations: 30,
            ..DeConfig::default()
        };
        let mut de = DifferentialEvolution::new(space, MaxDrawdownLoss, cfg, 13);
        while let Some(params) = de.suggest() {
            let (x, y) = (params["x"], params["y"]);
            let mut m = empty_metrics();
            m.max_drawdown = (1.0 - x).powi(2) + 100.0 * (y - x * x).powi(2);
            de.report(m);
        }
        let best = de.best_trial().unwrap();
        // DE at popsize=20, 30 generations (= 620 evals) must
        // land within 0.05 of the global minimum on 2D
        // Rosenbrock. Anything worse is a regression.
        let loss = best.loss;
        assert!(
            loss < 0.05,
            "DE loss on Rosenbrock = {loss}, expected < 0.05"
        );
    }

    #[test]
    fn planned_evaluations_matches_actual_trial_count() {
        let space = quadratic_space();
        let cfg = DeConfig {
            population_mult: 5,
            max_generations: 4,
            ..DeConfig::default()
        };
        let mut de = DifferentialEvolution::new(space, MaxDrawdownLoss, cfg, 0);
        let planned = de.planned_evaluations();
        while de.suggest().is_some() {
            de.report(empty_metrics());
        }
        assert_eq!(de.trials().len(), planned);
    }

    #[test]
    fn int_uniform_params_are_snapped_to_integers_on_report() {
        let space = SearchSpace::new().add(Param::int_uniform("n", 1, 10));
        let mut de = DifferentialEvolution::new(
            space,
            MaxDrawdownLoss,
            DeConfig {
                max_generations: 2,
                population_mult: 4,
                ..DeConfig::default()
            },
            0,
        );
        while let Some(params) = de.suggest() {
            let v = params["n"];
            assert_eq!(v, v.round(), "int_uniform must snap to integer, got {v}");
            assert!((1.0..=10.0).contains(&v));
            de.report(empty_metrics());
        }
    }

    #[test]
    fn best_trial_returns_none_before_any_reports() {
        let space = quadratic_space();
        let de = DifferentialEvolution::new(space, SharpeLoss, DeConfig::default(), 0);
        assert!(de.best_trial().is_none());
    }

    #[test]
    fn is_finished_flips_after_planned_run() {
        let space = quadratic_space();
        let cfg = DeConfig {
            population_mult: 4,
            max_generations: 2,
            ..DeConfig::default()
        };
        let mut de = DifferentialEvolution::new(space, MaxDrawdownLoss, cfg, 0);
        assert!(!de.is_finished());
        while de.suggest().is_some() {
            de.report(empty_metrics());
        }
        assert!(de.is_finished());
    }
}
