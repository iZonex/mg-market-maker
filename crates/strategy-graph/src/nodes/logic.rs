//! `Logic.*` — boolean + control flow nodes.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;

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
static AND_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("out", PortType::Bool)]);

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
static MUX_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("out", PortType::Number)]);

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
