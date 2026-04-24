//! `Logic.*` — boolean + control flow nodes.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::Value as Json;

// ── Logic.And ──────────────────────────────────────────────

/// Two-input boolean AND. `Missing` on either input collapses to
/// `false` (fail-closed default matches what `Cast.ToBool` does).
#[derive(Debug, Default)]
pub struct And;

static AND_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("a", PortType::Bool),
        Port::new("b", PortType::Bool),
    ]
});
static AND_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::Bool)]);

impl NodeKind for And {
    fn kind(&self) -> &'static str {
        "Logic.And"
    }
    fn input_ports(&self) -> &[Port] {
        &AND_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &AND_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let a = inputs.first().and_then(Value::as_bool).unwrap_or(false);
        let b = inputs.get(1).and_then(Value::as_bool).unwrap_or(false);
        Ok(vec![Value::Bool(a && b)])
    }
}

// ── Logic.Mux ──────────────────────────────────────────────

/// Ternary select — `cond ? then_val : else_val`. Works on `Number`
/// values; for `Bool` or `String` a future `Logic.Mux.Bool` /
/// `Logic.Mux.String` variant is added when needed. Missing `cond`
/// passes through the `else_val` (same fail-closed rationale).
#[derive(Debug, Default)]
pub struct Mux;

static MUX_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("cond", PortType::Bool),
        Port::new("then", PortType::Number),
        Port::new("else", PortType::Number),
    ]
});
static MUX_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::Number)]);

impl NodeKind for Mux {
    fn kind(&self) -> &'static str {
        "Logic.Mux"
    }
    fn input_ports(&self) -> &[Port] {
        &MUX_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &MUX_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let cond = inputs.first().and_then(Value::as_bool).unwrap_or(false);
        let pick = if cond { 1 } else { 2 };
        let v = inputs.get(pick).cloned().unwrap_or(Value::Missing);
        Ok(vec![v])
    }
}

// ── Logic.StringMux — ternary for String values ────────────

/// Same ternary shape as [`Mux`] but passes `String` values
/// through. Needed for piping exec-algo policy strings
/// (`Exec.*Config`) through a condition without round-tripping
/// via Number.
#[derive(Debug, Default)]
pub struct StringMux;

static STRING_MUX_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("cond", PortType::Bool),
        Port::new("then", PortType::String),
        Port::new("else", PortType::String),
    ]
});
static STRING_MUX_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::String)]);

impl NodeKind for StringMux {
    fn kind(&self) -> &'static str {
        "Logic.StringMux"
    }
    fn input_ports(&self) -> &[Port] {
        &STRING_MUX_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &STRING_MUX_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let cond = inputs.first().and_then(Value::as_bool).unwrap_or(false);
        let pick = if cond { 1 } else { 2 };
        let v = inputs.get(pick).cloned().unwrap_or(Value::Missing);
        Ok(vec![v])
    }
}

// ── Enum equality helpers ──────────────────────────────────

/// `Cast.StrategyEq` — compares the `Strategy.Active` output to a
/// configured target name (e.g. `"GLFT"`) and emits Bool. Use with
/// `Logic.Mux` to branch on which base strategy is running.
#[derive(Debug, Default)]
pub struct StrategyEq {
    target: String,
}

#[derive(Deserialize)]
struct StrategyEqCfg {
    #[serde(default)]
    target: Option<String>,
}

impl StrategyEq {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: StrategyEqCfg = serde_json::from_value(cfg.clone()).ok()?;
        Some(Self {
            target: parsed.target.unwrap_or_default(),
        })
    }
}

static STRATEGY_EQ_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("kind", PortType::StrategyKind)]);
static BOOL_OUT: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::Bool)]);

impl NodeKind for StrategyEq {
    fn kind(&self) -> &'static str {
        "Cast.StrategyEq"
    }
    fn input_ports(&self) -> &[Port] {
        &STRATEGY_EQ_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &BOOL_OUT
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let hit = inputs
            .first()
            .and_then(Value::as_strategy_kind)
            .map(|k| k == self.target)
            .unwrap_or(false);
        Ok(vec![Value::Bool(hit)])
    }
}

/// `Cast.PairClassEq` — same pattern for pair-class labels.
#[derive(Debug, Default)]
pub struct PairClassEq {
    target: String,
}

#[derive(Deserialize)]
struct PairClassEqCfg {
    #[serde(default)]
    target: Option<String>,
}

impl PairClassEq {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: PairClassEqCfg = serde_json::from_value(cfg.clone()).ok()?;
        Some(Self {
            target: parsed.target.unwrap_or_default(),
        })
    }
}

static PAIR_CLASS_EQ_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("class", PortType::PairClass)]);

impl NodeKind for PairClassEq {
    fn kind(&self) -> &'static str {
        "Cast.PairClassEq"
    }
    fn input_ports(&self) -> &[Port] {
        &PAIR_CLASS_EQ_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &BOOL_OUT
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let hit = inputs
            .first()
            .and_then(Value::as_pair_class)
            .map(|c| c == self.target)
            .unwrap_or(false);
        Ok(vec![Value::Bool(hit)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn and_returns_true_only_when_both_true() {
        let node = And;
        let call = |a: bool, b: bool| {
            let mut st = NodeState::default();
            node.evaluate(
                &EvalCtx::default(),
                &[Value::Bool(a), Value::Bool(b)],
                &mut st,
            )
            .unwrap()
        };
        assert_eq!(call(true, true), vec![Value::Bool(true)]);
        assert_eq!(call(true, false), vec![Value::Bool(false)]);
        assert_eq!(call(false, true), vec![Value::Bool(false)]);
        assert_eq!(call(false, false), vec![Value::Bool(false)]);
    }

    #[test]
    fn and_missing_input_collapses_to_false() {
        let node = And;
        let mut st = NodeState::default();
        let out = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::Missing, Value::Bool(true)],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Bool(false)]);
    }

    #[test]
    fn mux_picks_then_on_true() {
        let node = Mux;
        let mut st = NodeState::default();
        let out = node
            .evaluate(
                &EvalCtx::default(),
                &[
                    Value::Bool(true),
                    Value::Number(dec!(1.5)),
                    Value::Number(dec!(1.0)),
                ],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(1.5))]);
    }

    #[test]
    fn strategy_eq_matches_configured_target() {
        let node = StrategyEq::from_config(&serde_json::json!({ "target": "GLFT" })).unwrap();
        let mut st = NodeState::default();
        let hit = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::StrategyKind("GLFT".into())],
                &mut st,
            )
            .unwrap();
        assert_eq!(hit, vec![Value::Bool(true)]);
        let miss = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::StrategyKind("Grid".into())],
                &mut st,
            )
            .unwrap();
        assert_eq!(miss, vec![Value::Bool(false)]);
    }

    #[test]
    fn pair_class_eq_matches_configured_target() {
        let node =
            PairClassEq::from_config(&serde_json::json!({ "target": "major-spot" })).unwrap();
        let mut st = NodeState::default();
        let hit = node
            .evaluate(
                &EvalCtx::default(),
                &[Value::PairClass("major-spot".into())],
                &mut st,
            )
            .unwrap();
        assert_eq!(hit, vec![Value::Bool(true)]);
    }

    #[test]
    fn strategy_eq_missing_input_returns_false() {
        let node = StrategyEq::from_config(&serde_json::json!({ "target": "GLFT" })).unwrap();
        let mut st = NodeState::default();
        let out = node
            .evaluate(&EvalCtx::default(), &[Value::Missing], &mut st)
            .unwrap();
        assert_eq!(out, vec![Value::Bool(false)]);
    }

    #[test]
    fn mux_picks_else_on_false_or_missing() {
        let node = Mux;
        let mut st = NodeState::default();
        let out = node
            .evaluate(
                &EvalCtx::default(),
                &[
                    Value::Bool(false),
                    Value::Number(dec!(1.5)),
                    Value::Number(dec!(1.0)),
                ],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(1.0))]);

        let out2 = node
            .evaluate(
                &EvalCtx::default(),
                &[
                    Value::Missing,
                    Value::Number(dec!(1.5)),
                    Value::Number(dec!(1.0)),
                ],
                &mut st,
            )
            .unwrap();
        assert_eq!(out2, vec![Value::Number(dec!(1.0))]);
    }
}
