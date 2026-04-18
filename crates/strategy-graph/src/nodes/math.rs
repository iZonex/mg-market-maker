//! `Math.*` + `Cast.*` — arithmetic + type casts.
//!
//! `Cast.ToBool` lives here rather than a separate module because it
//! is structurally a numeric-comparison node; moving it later if the
//! file grows is trivial.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value as Json;
use std::str::FromStr;

/// `Math.Add` — two-input numeric sum. `Missing` on either input
/// propagates as `Missing` on the output (additive identity of the
/// missing-data protocol).
#[derive(Debug, Default)]
pub struct Add;

static ADD_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("a", PortType::Number),
        Port::new("b", PortType::Number),
    ]
});
static ADD_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("out", PortType::Number)]);

impl NodeKind for Add {
    fn kind(&self) -> &'static str {
        "Math.Add"
    }

    fn input_ports(&self) -> &[Port] {
        &ADD_INPUTS
    }

    fn output_ports(&self) -> &[Port] {
        &ADD_OUTPUTS
    }

    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let a = inputs.first().cloned().unwrap_or(Value::Missing);
        let b = inputs.get(1).cloned().unwrap_or(Value::Missing);
        if a.is_missing() || b.is_missing() {
            return Ok(vec![Value::Missing]);
        }
        let (Some(x), Some(y)) = (a.as_number(), b.as_number()) else {
            return Ok(vec![Value::Missing]);
        };
        Ok(vec![Value::Number(x + y)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn adds_two_numbers() {
        let node = Add;
        let mut st = NodeState::default();
        let out = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(3)), Value::Number(dec!(4))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(7))]);
    }

    #[test]
    fn missing_input_propagates() {
        let node = Add;
        let mut st = NodeState::default();
        let out = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::Missing, Value::Number(dec!(4))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Missing]);
    }

    #[test]
    fn mul_multiplies_two_numbers() {
        let node = Mul;
        let mut st = NodeState::default();
        let out = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(3)), Value::Number(dec!(4))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(12))]);
    }

    #[test]
    fn to_bool_ge_threshold() {
        let node = ToBool::from_config(&serde_json::json!({
            "threshold": "5", "cmp": "ge"
        }))
        .unwrap();
        let mut st = NodeState::default();
        let hit = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(5.1))],
                &mut st,
            )
            .unwrap();
        assert_eq!(hit, vec![Value::Bool(true)]);
        let miss = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(4.9))],
                &mut st,
            )
            .unwrap();
        assert_eq!(miss, vec![Value::Bool(false)]);
    }

    #[test]
    fn const_returns_configured_value() {
        let node = Const::from_config(&serde_json::json!({ "value": "1.75" })).unwrap();
        let mut st = NodeState::default();
        let out = node
            .evaluate(&EvalCtx::default(), &[], &mut st)
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(1.75))]);
    }

    #[test]
    fn const_default_is_zero() {
        let node = Const::from_config(&serde_json::Value::Null).unwrap();
        let mut st = NodeState::default();
        let out = node
            .evaluate(&EvalCtx::default(), &[], &mut st)
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(0))]);
    }

    #[test]
    fn to_bool_missing_returns_false() {
        let node = ToBool::from_config(&serde_json::Value::Null).unwrap();
        let mut st = NodeState::default();
        let out = node
            .evaluate(&EvalCtx::default(), &[Value::Missing], &mut st)
            .unwrap();
        assert_eq!(out, vec![Value::Bool(false)]);
    }
}

/// `Math.Const` — configured constant `{ value: "1.5" }`.
/// Zero input ports — classified as a source by the evaluator, so
/// its configured value is available at tick time without any
/// engine-side plumbing. Use this for literal arms of a
/// `Logic.Mux`, spread floors, etc.
#[derive(Debug)]
pub struct Const {
    value: Decimal,
}

impl Default for Const {
    fn default() -> Self {
        Self {
            value: Decimal::ZERO,
        }
    }
}

#[derive(Deserialize)]
struct ConstCfg {
    #[serde(default)]
    value: Option<String>,
}

impl Const {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: ConstCfg = serde_json::from_value(cfg.clone()).ok()?;
        let value = match parsed.value {
            None => Decimal::ZERO,
            Some(s) => Decimal::from_str(&s).ok()?,
        };
        Some(Self { value })
    }
}

static CONST_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("value", PortType::Number)]);
static EMPTY_INPUTS: Lazy<Vec<Port>> = Lazy::new(Vec::new);

impl NodeKind for Const {
    fn kind(&self) -> &'static str {
        "Math.Const"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &CONST_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::Number(self.value)])
    }
}

/// `Math.Mul` — two-input numeric product. Missing-propagating.
#[derive(Debug, Default)]
pub struct Mul;

static MUL_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("a", PortType::Number),
        Port::new("b", PortType::Number),
    ]
});
static MUL_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("out", PortType::Number)]);

impl NodeKind for Mul {
    fn kind(&self) -> &'static str {
        "Math.Mul"
    }
    fn input_ports(&self) -> &[Port] {
        &MUL_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &MUL_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let a = inputs.first().cloned().unwrap_or(Value::Missing);
        let b = inputs.get(1).cloned().unwrap_or(Value::Missing);
        if a.is_missing() || b.is_missing() {
            return Ok(vec![Value::Missing]);
        }
        let (Some(x), Some(y)) = (a.as_number(), b.as_number()) else {
            return Ok(vec![Value::Missing]);
        };
        Ok(vec![Value::Number(x * y)])
    }
}

/// Comparison operators used by `Cast.ToBool`.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Cmp {
    /// `>=` — default; most common for "above threshold" checks.
    #[default]
    Ge,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `<`
    Lt,
    /// `==`
    Eq,
}

/// `Cast.ToBool` — configured with `{ threshold: "5.0", cmp: "ge" }`.
/// `Missing` input evaluates to `false` (fail-closed default: if we
/// can't prove the condition, we don't trip downstream logic).
#[derive(Debug)]
pub struct ToBool {
    threshold: Decimal,
    cmp: Cmp,
}

#[derive(Deserialize)]
struct ToBoolCfg {
    #[serde(default)]
    threshold: Option<String>,
    #[serde(default)]
    cmp: Option<Cmp>,
}

impl Default for ToBool {
    fn default() -> Self {
        Self {
            threshold: Decimal::ZERO,
            cmp: Cmp::Ge,
        }
    }
}

impl ToBool {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: ToBoolCfg = serde_json::from_value(cfg.clone()).ok()?;
        let threshold = match parsed.threshold {
            None => Decimal::ZERO,
            Some(s) => Decimal::from_str(&s).ok()?,
        };
        Some(Self {
            threshold,
            cmp: parsed.cmp.unwrap_or_default(),
        })
    }
}

static TOBOOL_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("x", PortType::Number)]);
static TOBOOL_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("out", PortType::Bool)]);

impl NodeKind for ToBool {
    fn kind(&self) -> &'static str {
        "Cast.ToBool"
    }
    fn input_ports(&self) -> &[Port] {
        &TOBOOL_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &TOBOOL_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let Some(x) = inputs.first().and_then(Value::as_number) else {
            return Ok(vec![Value::Bool(false)]);
        };
        let out = match self.cmp {
            Cmp::Ge => x >= self.threshold,
            Cmp::Gt => x > self.threshold,
            Cmp::Le => x <= self.threshold,
            Cmp::Lt => x < self.threshold,
            Cmp::Eq => x == self.threshold,
        };
        Ok(vec![Value::Bool(out)])
    }
}
