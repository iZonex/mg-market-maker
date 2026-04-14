use std::collections::HashMap;

use rand::Rng;

/// One knob in the search space.
#[derive(Debug, Clone)]
pub enum Param {
    /// Uniformly sampled real in `[min, max]`.
    Uniform { name: String, min: f64, max: f64 },
    /// Log-uniformly sampled real in `[min, max]` (both > 0).
    LogUniform { name: String, min: f64, max: f64 },
    /// Uniformly sampled integer in `[min, max]` (inclusive). The
    /// value is stored as `f64` in the trial so every param lives
    /// in one map; cast on read if you need an integer.
    IntUniform { name: String, min: i64, max: i64 },
    /// Categorical choice from a fixed set. The chosen value's index
    /// is stored as `f64`; the caller maps it back.
    Choice {
        name: String,
        values: Vec<f64>,
    },
}

impl Param {
    pub fn uniform(name: &str, min: f64, max: f64) -> Self {
        assert!(min <= max, "uniform({name}): min {min} > max {max}");
        Self::Uniform {
            name: name.into(),
            min,
            max,
        }
    }

    pub fn log_uniform(name: &str, min: f64, max: f64) -> Self {
        assert!(min > 0.0, "log_uniform({name}): min must be > 0");
        assert!(min <= max, "log_uniform({name}): min {min} > max {max}");
        Self::LogUniform {
            name: name.into(),
            min,
            max,
        }
    }

    pub fn int_uniform(name: &str, min: i64, max: i64) -> Self {
        assert!(min <= max, "int_uniform({name}): min {min} > max {max}");
        Self::IntUniform {
            name: name.into(),
            min,
            max,
        }
    }

    pub fn choice(name: &str, values: Vec<f64>) -> Self {
        assert!(!values.is_empty(), "choice({name}): empty values");
        Self::Choice {
            name: name.into(),
            values,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Uniform { name, .. }
            | Self::LogUniform { name, .. }
            | Self::IntUniform { name, .. }
            | Self::Choice { name, .. } => name,
        }
    }

    pub fn sample<R: Rng>(&self, rng: &mut R) -> f64 {
        match self {
            Self::Uniform { min, max, .. } => rng.gen_range(*min..=*max),
            Self::LogUniform { min, max, .. } => {
                let log_min = min.ln();
                let log_max = max.ln();
                rng.gen_range(log_min..=log_max).exp()
            }
            Self::IntUniform { min, max, .. } => rng.gen_range(*min..=*max) as f64,
            Self::Choice { values, .. } => {
                let idx = rng.gen_range(0..values.len());
                values[idx]
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchSpace {
    params: Vec<Param>,
}

impl SearchSpace {
    pub fn new() -> Self {
        Self::default()
    }

    // Builder-style method; the trait-name collision with
    // `std::ops::Add::add` is irrelevant here because `SearchSpace`
    // is never used in arithmetic context.
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, param: Param) -> Self {
        self.params.push(param);
        self
    }

    pub fn len(&self) -> usize {
        self.params.len()
    }

    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    pub fn sample<R: Rng>(&self, rng: &mut R) -> HashMap<String, f64> {
        self.params
            .iter()
            .map(|p| (p.name().to_string(), p.sample(rng)))
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Param> {
        self.params.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn uniform_samples_within_range() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let p = Param::uniform("x", 1.0, 5.0);
        for _ in 0..1000 {
            let v = p.sample(&mut rng);
            assert!((1.0..=5.0).contains(&v));
        }
    }

    #[test]
    fn log_uniform_samples_within_range() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let p = Param::log_uniform("x", 0.01, 10.0);
        for _ in 0..1000 {
            let v = p.sample(&mut rng);
            assert!((0.01..=10.0).contains(&v));
        }
    }

    #[test]
    fn int_uniform_yields_integer_values() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let p = Param::int_uniform("n", 1, 10);
        for _ in 0..1000 {
            let v = p.sample(&mut rng);
            assert!((1.0..=10.0).contains(&v));
            assert_eq!(v, v.trunc(), "int_uniform must yield integer values");
        }
    }

    #[test]
    fn choice_only_returns_listed_values() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let p = Param::choice("x", vec![0.1, 0.2, 0.5]);
        for _ in 0..200 {
            let v = p.sample(&mut rng);
            assert!([0.1, 0.2, 0.5].contains(&v));
        }
    }

    #[test]
    fn search_space_samples_every_param() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let space = SearchSpace::new()
            .add(Param::uniform("a", 0.0, 1.0))
            .add(Param::log_uniform("b", 0.1, 10.0))
            .add(Param::int_uniform("c", 1, 5))
            .add(Param::choice("d", vec![0.25, 0.5, 0.75]));
        let s = space.sample(&mut rng);
        assert_eq!(s.len(), 4);
        assert!(s.contains_key("a"));
        assert!(s.contains_key("b"));
        assert!(s.contains_key("c"));
        assert!(s.contains_key("d"));
    }

    /// `log_uniform` samples must be log-uniform in the interval, so
    /// the **geometric mean** of many samples converges to
    /// `sqrt(min × max)` — this is the defining property of the
    /// log-uniform distribution (e.g. Bengio & Bergstra 2012,
    /// "Random Search for Hyper-Parameter Optimization", §3.2). A
    /// linearly-uniform `rng.gen_range(min..=max)` would converge
    /// toward the arithmetic mean `(min + max)/2` instead, failing
    /// this test decisively.
    ///
    /// With min = 0.01 and max = 100:
    ///   geometric mean = sqrt(0.01 × 100) = sqrt(1) = 1
    ///   arithmetic mean = (0.01 + 100) / 2 = 50.005
    ///
    /// Over 20_000 samples the empirical geometric mean should sit
    /// well under 10 (closer to 1), comfortably rejecting a
    /// uniform-only implementation.
    #[test]
    fn log_uniform_geometric_mean_matches_sqrt_of_range() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let p = Param::log_uniform("x", 0.01, 100.0);
        let n = 20_000;
        let mut log_sum = 0.0_f64;
        for _ in 0..n {
            let v = p.sample(&mut rng);
            log_sum += v.ln();
        }
        let geo_mean = (log_sum / n as f64).exp();
        // Geometric mean should converge to 1.0. Accept ±15% to
        // absorb sampling noise; the arithmetic-mean failure mode
        // would sit near 50, far outside the band.
        assert!(
            (0.85..=1.15).contains(&geo_mean),
            "log-uniform geometric mean {geo_mean} not near 1.0"
        );
    }

    #[test]
    fn seeded_sampling_is_deterministic() {
        let space = SearchSpace::new().add(Param::uniform("x", 0.0, 100.0));
        let mut r1 = ChaCha8Rng::seed_from_u64(123);
        let mut r2 = ChaCha8Rng::seed_from_u64(123);
        assert_eq!(space.sample(&mut r1), space.sample(&mut r2));
    }
}
