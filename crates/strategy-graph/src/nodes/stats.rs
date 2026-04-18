//! `Stats.*` — stateful stream transforms.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::Value as Json;
use std::str::FromStr;

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
        let alpha = match parsed.alpha {
            None => dec!(0.1),
            Some(s) => {
                let v = Decimal::from_str(&s).ok()?;
                // Guard against out-of-band alphas; 0 would freeze
                // state forever, 1 would disable smoothing.
                if v <= dec!(0) || v > dec!(1) {
                    return None;
                }
                v
            }
        };
        Some(Self { alpha })
    }
}

#[derive(Default)]
struct EwmaState {
    prev: Option<Decimal>,
}

static EWMA_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("x", PortType::Number)]);
static EWMA_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("out", PortType::Number)]);

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
            widget: ConfigWidget::Number { min: Some(0.0001), max: Some(1.0), step: Some(0.01) },
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
            return Ok(vec![st
                .prev
                .map(Value::Number)
                .unwrap_or(Value::Missing)]);
        };
        let out = match st.prev {
            None => x,
            Some(p) => self.alpha * x + (dec!(1) - self.alpha) * p,
        };
        st.prev = Some(out);
        Ok(vec![Value::Number(out)])
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
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(10))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(10))]);
    }

    #[test]
    fn smooths_toward_target() {
        let node = Ewma::from_config(&serde_json::json!({ "alpha": "0.5" })).unwrap();
        let mut st = NodeState::default();
        // seed = 0, then 10 → 0.5*10 + 0.5*0 = 5, then 10 → 7.5
        node.evaluate(
            &EvalCtx::default(),
            &[Value::Number(dec!(0))],
            &mut st,
        )
        .unwrap();
        let s1 = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(10))],
                &mut st,
            )
            .unwrap();
        assert_eq!(s1, vec![Value::Number(dec!(5))]);
        let s2 = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(10))],
                &mut st,
            )
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
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(42))],
                &mut st,
            )
            .unwrap();
        assert_eq!(first, vec![Value::Number(dec!(42))]);
        let miss = node
            .evaluate(&EvalCtx::default(), &[Value::Missing], &mut st)
            .unwrap();
        assert_eq!(miss, vec![Value::Number(dec!(42))]);
    }
}
