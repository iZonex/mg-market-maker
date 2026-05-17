//! `Stats.*` — stateful stream transforms.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::nodes::decimal_field;
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use mm_strategy::volatility::GarchEstimator;
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::Value as Json;

/// `Stats.EWMA` — `out_t = α·x_t + (1-α)·out_{t-1}`.
/// First valid input seeds directly (avoids the "decay toward 0"
/// artefact of treating the initial state as zero).
/// `Missing` inputs are pass-through — state is left untouched.
#[derive(Debug)]
pub struct Ewma {
    alpha: Decimal,
}

impl Default for Ewma {
    fn default() -> Self {
        Self { alpha: dec!(0.1) }
    }
}

#[derive(Deserialize)]
struct EwmaCfg {
    #[serde(default)]
    alpha: Option<String>,
}

impl Ewma {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: EwmaCfg = serde_json::from_value(cfg.clone()).ok()?;
        // Out-of-band α is rejected: 0 freezes state forever, > 1
        // disables smoothing.
        let alpha = decimal_field(parsed.alpha, dec!(0.1), |v| v > dec!(0) && v <= dec!(1))?;
        Some(Self { alpha })
    }
}

#[derive(Default)]
struct EwmaState {
    prev: Option<Decimal>,
}

static EWMA_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("x", PortType::Number)]);
static EWMA_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::Number)]);

impl NodeKind for Ewma {
    fn kind(&self) -> &'static str {
        "Stats.EWMA"
    }
    fn input_ports(&self) -> &[Port] {
        &EWMA_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &EWMA_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "alpha",
            label: "α (smoothing)",
            hint: Some("0 < α ≤ 1. Higher = more responsive, less smoothing"),
            default: serde_json::json!("0.1"),
            widget: ConfigWidget::Number {
                min: Some(0.0001),
                max: Some(1.0),
                step: Some(0.01),
            },
        }]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let st = state.get_or_insert_default::<EwmaState>();
        let Some(x) = inputs.first().and_then(Value::as_number) else {
            // Missing input — pass through the cached previous value
            // so downstream consumers still see a number.
            return Ok(vec![st.prev.map(Value::Number).unwrap_or(Value::Missing)]);
        };
        let out = match st.prev {
            None => x,
            Some(p) => self.alpha * x + (dec!(1) - self.alpha) * p,
        };
        st.prev = Some(out);
        Ok(vec![Value::Number(out)])
    }
}

/// `Stats.Garch` — GARCH(1,1) conditional volatility of a price
/// stream. Feeds each input price into a [`GarchEstimator`] and
/// emits the annualised σ. Unlike `Stats.EWMA` (a generic smoother),
/// this models volatility clustering + mean reversion — see
/// `mm_strategy::volatility::GarchEstimator`.
///
/// `Missing` inputs are pass-through: the estimator is left
/// untouched and the last σ (or `Missing`, if not yet warm) is
/// re-emitted.
#[derive(Debug)]
pub struct Garch {
    /// Seconds between price updates — sets the annualisation factor.
    tick_secs: Decimal,
}

impl Default for Garch {
    fn default() -> Self {
        Self {
            tick_secs: dec!(0.5),
        }
    }
}

#[derive(Deserialize)]
struct GarchCfg {
    #[serde(default)]
    tick_secs: Option<String>,
}

impl Garch {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: GarchCfg = serde_json::from_value(cfg.clone()).ok()?;
        // A non-positive tick interval makes the annualisation factor
        // degenerate.
        let tick_secs = decimal_field(parsed.tick_secs, dec!(0.5), |v| v > dec!(0))?;
        Some(Self { tick_secs })
    }
}

/// Node-local state — the estimator is built lazily on the first
/// `evaluate` so it picks up the parsed `tick_secs`.
#[derive(Default)]
struct GarchState {
    est: Option<GarchEstimator>,
}

static GARCH_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("price", PortType::Number)]);
static GARCH_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("vol", PortType::Number)]);

impl NodeKind for Garch {
    fn kind(&self) -> &'static str {
        "Stats.Garch"
    }
    fn input_ports(&self) -> &[Port] {
        &GARCH_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &GARCH_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "tick_secs",
            label: "Tick interval (s)",
            hint: Some("Seconds between price updates — sets the annualisation factor"),
            default: serde_json::json!("0.5"),
            widget: ConfigWidget::Number {
                min: Some(0.001),
                max: Some(86_400.0),
                step: Some(0.1),
            },
        }]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let st = state.get_or_insert_default::<GarchState>();
        let est = st
            .est
            .get_or_insert_with(|| GarchEstimator::new(self.tick_secs));
        if let Some(price) = inputs.first().and_then(Value::as_number) {
            est.update(price);
        }
        // `volatility()` is `None` until the estimator is warm —
        // surface that as `Missing` so downstream gating still works.
        Ok(vec![est
            .volatility()
            .map(Value::Number)
            .unwrap_or(Value::Missing)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_from_first_observation() {
        let node = Ewma::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        let out = node
            .evaluate(&EvalCtx::default(), &[Value::Number(dec!(10))], &mut st)
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(10))]);
    }

    #[test]
    fn smooths_toward_target() {
        let node = Ewma::from_config(&serde_json::json!({ "alpha": "0.5" })).unwrap();
        let mut st = NodeState::default();
        // seed = 0, then 10 → 0.5*10 + 0.5*0 = 5, then 10 → 7.5
        node.evaluate(&EvalCtx::default(), &[Value::Number(dec!(0))], &mut st)
            .unwrap();
        let s1 = node
            .evaluate(&EvalCtx::default(), &[Value::Number(dec!(10))], &mut st)
            .unwrap();
        assert_eq!(s1, vec![Value::Number(dec!(5))]);
        let s2 = node
            .evaluate(&EvalCtx::default(), &[Value::Number(dec!(10))], &mut st)
            .unwrap();
        assert_eq!(s2, vec![Value::Number(dec!(7.5))]);
    }

    #[test]
    fn rejects_out_of_band_alpha() {
        assert!(Ewma::from_config(&serde_json::json!({ "alpha": "0" })).is_none());
        assert!(Ewma::from_config(&serde_json::json!({ "alpha": "-0.1" })).is_none());
        assert!(Ewma::from_config(&serde_json::json!({ "alpha": "1.5" })).is_none());
    }

    #[test]
    fn missing_input_returns_prev() {
        let node = Ewma::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        let first = node
            .evaluate(&EvalCtx::default(), &[Value::Number(dec!(42))], &mut st)
            .unwrap();
        assert_eq!(first, vec![Value::Number(dec!(42))]);
        let miss = node
            .evaluate(&EvalCtx::default(), &[Value::Missing], &mut st)
            .unwrap();
        assert_eq!(miss, vec![Value::Number(dec!(42))]);
    }

    // -------------------------- Stats.Garch --------------------------

    #[test]
    fn garch_builds_from_null_and_rejects_bad_tick() {
        assert!(Garch::from_config(&Json::Null).is_some());
        assert!(Garch::from_config(&serde_json::json!({ "tick_secs": "1.0" })).is_some());
        assert!(Garch::from_config(&serde_json::json!({ "tick_secs": "0" })).is_none());
        assert!(Garch::from_config(&serde_json::json!({ "tick_secs": "-1" })).is_none());
    }

    #[test]
    fn garch_warms_up_then_emits_positive_vol() {
        let node = Garch::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        // Cold: one sample is far below the estimator's min — Missing.
        let cold = node
            .evaluate(&EvalCtx::default(), &[Value::Number(dec!(1000))], &mut st)
            .unwrap();
        assert_eq!(cold, vec![Value::Missing]);
        // Feed a varied price walk; after warmup σ is a positive number.
        let pattern = [dec!(0.002), dec!(-0.0015), dec!(0.003), dec!(-0.001)];
        let mut price = dec!(1000);
        for i in 0..80 {
            price *= dec!(1) + pattern[i % pattern.len()];
            node.evaluate(&EvalCtx::default(), &[Value::Number(price)], &mut st)
                .unwrap();
        }
        let warm = node
            .evaluate(&EvalCtx::default(), &[Value::Number(price)], &mut st)
            .unwrap();
        match warm.as_slice() {
            [Value::Number(v)] => assert!(*v > dec!(0), "warm σ must be positive, got {v}"),
            other => panic!("expected one Number output, got {other:?}"),
        }
    }

    #[test]
    fn garch_missing_input_is_passthrough() {
        let node = Garch::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        let out = node
            .evaluate(&EvalCtx::default(), &[Value::Missing], &mut st)
            .unwrap();
        assert_eq!(out, vec![Value::Missing]);
    }
}
