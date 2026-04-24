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
static ADD_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::Number)]);

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
            .evaluate(&EvalCtx::default(), &[Value::Number(dec!(5.1))], &mut st)
            .unwrap();
        assert_eq!(hit, vec![Value::Bool(true)]);
        let miss = node
            .evaluate(&EvalCtx::default(), &[Value::Number(dec!(4.9))], &mut st)
            .unwrap();
        assert_eq!(miss, vec![Value::Bool(false)]);
    }

    #[test]
    fn const_returns_configured_value() {
        let node = Const::from_config(&serde_json::json!({ "value": "1.75" })).unwrap();
        let mut st = NodeState::default();
        let out = node.evaluate(&EvalCtx::default(), &[], &mut st).unwrap();
        assert_eq!(out, vec![Value::Number(dec!(1.75))]);
    }

    #[test]
    fn const_default_is_zero() {
        let node = Const::from_config(&serde_json::Value::Null).unwrap();
        let mut st = NodeState::default();
        let out = node.evaluate(&EvalCtx::default(), &[], &mut st).unwrap();
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

static CONST_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("value", PortType::Number)]);
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
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "value",
            label: "Value",
            hint: None,
            default: serde_json::json!("0"),
            widget: ConfigWidget::Number {
                min: None,
                max: None,
                step: Some(0.01),
            },
        }]
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
static MUL_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::Number)]);

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

static TOBOOL_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("x", PortType::Number)]);
static TOBOOL_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::Bool)]);

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
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigEnumOption, ConfigField, ConfigWidget};
        vec![
            ConfigField {
                name: "threshold",
                label: "Threshold",
                hint: Some("Compared with the incoming number"),
                default: serde_json::json!("0"),
                widget: ConfigWidget::Number {
                    min: None,
                    max: None,
                    step: Some(0.01),
                },
            },
            ConfigField {
                name: "cmp",
                label: "Comparator",
                hint: None,
                default: serde_json::json!("ge"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption {
                            value: "ge",
                            label: "≥",
                        },
                        ConfigEnumOption {
                            value: "gt",
                            label: ">",
                        },
                        ConfigEnumOption {
                            value: "le",
                            label: "≤",
                        },
                        ConfigEnumOption {
                            value: "lt",
                            label: "<",
                        },
                        ConfigEnumOption {
                            value: "eq",
                            label: "=",
                        },
                    ],
                },
            },
        ]
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

// S4.3 — composable inventory skew.
//
// `|level| / cap` clamped to `[0, 1]`, raised to `exponent`,
// then signed by `level`. Output is a skew score in `[-1, 1]`
// that downstream nodes can multiply into a size/price term
// (long = +; short = −; flat = 0). Matches the hand-wired
// formula the grid strategy and advanced inventory manager use
// but exposes it as a graph-composable node so operators can
// dial it into any strategy pipeline without writing Rust.

/// `Math.InventorySkew` — see module comment above.
#[derive(Debug)]
pub struct InventorySkew {
    cap: Decimal,
    exponent: Decimal,
}

impl Default for InventorySkew {
    fn default() -> Self {
        Self {
            cap: Decimal::ONE,
            exponent: Decimal::from(2u8),
        }
    }
}

#[derive(Deserialize)]
struct InventorySkewCfg {
    #[serde(default)]
    cap: Option<String>,
    #[serde(default)]
    exponent: Option<String>,
}

impl InventorySkew {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: InventorySkewCfg = serde_json::from_value(cfg.clone()).ok()?;
        let cap = match parsed.cap {
            None => Decimal::ONE,
            Some(s) => {
                let v = Decimal::from_str(&s).ok()?;
                if v <= Decimal::ZERO {
                    return None;
                }
                v
            }
        };
        let exponent = match parsed.exponent {
            None => Decimal::from(2u8),
            Some(s) => {
                let v = Decimal::from_str(&s).ok()?;
                if v <= Decimal::ZERO {
                    return None;
                }
                v
            }
        };
        Some(Self { cap, exponent })
    }
}

static INV_SKEW_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("level", PortType::Number)]);
static INV_SKEW_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("skew", PortType::Number)]);

impl NodeKind for InventorySkew {
    fn kind(&self) -> &'static str {
        "Math.InventorySkew"
    }
    fn input_ports(&self) -> &[Port] {
        &INV_SKEW_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &INV_SKEW_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![
            ConfigField {
                name: "cap",
                label: "Inventory cap",
                hint: Some("|level| ≥ cap → |skew|=1; absolute base-asset units"),
                default: serde_json::json!("1"),
                widget: ConfigWidget::Number {
                    min: Some(0.0),
                    max: None,
                    step: Some(0.01),
                },
            },
            ConfigField {
                name: "exponent",
                label: "Curve exponent",
                hint: Some("> 1 steepens the ramp near cap (default 2 = quadratic)"),
                default: serde_json::json!("2"),
                widget: ConfigWidget::Number {
                    min: Some(0.1),
                    max: Some(10.0),
                    step: Some(0.1),
                },
            },
        ]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
        let Some(level) = inputs.first().and_then(Value::as_number) else {
            return Ok(vec![Value::Missing]);
        };
        if level.is_zero() {
            return Ok(vec![Value::Number(Decimal::ZERO)]);
        }
        let abs = level.abs();
        let normalised = (abs / self.cap).min(Decimal::ONE);
        // f64 hop to raise to an arbitrary power — Decimal has
        // no native pow. Values stay in `[0, 1]` so precision
        // loss is bounded.
        let base = normalised.to_f64().unwrap_or(0.0);
        let exp = self.exponent.to_f64().unwrap_or(2.0);
        let scaled = base.powf(exp).clamp(0.0, 1.0);
        let magnitude = Decimal::from_f64(scaled).unwrap_or(Decimal::ZERO);
        let signed = if level > Decimal::ZERO {
            magnitude
        } else {
            -magnitude
        };
        Ok(vec![Value::Number(signed)])
    }
}
