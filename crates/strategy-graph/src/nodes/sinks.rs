//! `Out.*` — sink nodes. Evaluator routes their outputs into engine
//! actions; the sinks' own `evaluate` is a pass-through that tags the
//! value with the right sink semantics.
//!
//! A sink produces exactly one output port named `action` of type
//! `Unit`. The evaluator reads the sink's **config + input** to
//! compose the engine-side action, not the output. Outputs exist
//! only so downstream tooling (preview mode, debug) can see "this
//! sink fired at tick T".

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;

static SPREAD_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("mult", PortType::Number)]);
static UNIT_OUT: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("action", PortType::Unit)]);

/// `Out.SpreadMult` — pushes the input number into the engine's
/// autotuner as the graph-authored spread multiplier. Floor at 1.0
/// applied engine-side (same invariant as `set_lead_lag_mult`).
#[derive(Debug, Default)]
pub struct SpreadMult;

impl NodeKind for SpreadMult {
    fn kind(&self) -> &'static str {
        "Out.SpreadMult"
    }
    fn input_ports(&self) -> &[Port] {
        &SPREAD_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &UNIT_OUT
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        // Sinks flush their action through the evaluator's sink
        // harvest pass; the returned `Unit` is a tick-fired marker
        // for observability.
        Ok(vec![Value::Unit])
    }
}

static SIZE_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("mult", PortType::Number)]);

/// `Out.SizeMult` — pushes into the autotuner as the graph-authored
/// size multiplier. Clamped to `(0, 1]` engine-side.
#[derive(Debug, Default)]
pub struct SizeMult;

impl NodeKind for SizeMult {
    fn kind(&self) -> &'static str {
        "Out.SizeMult"
    }
    fn input_ports(&self) -> &[Port] {
        &SIZE_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &UNIT_OUT
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::Unit])
    }
}

static KILL_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("trigger", PortType::Bool),
        Port::new("level", PortType::KillLevel),
        Port::new("reason", PortType::String),
    ]
});

/// `Out.KillEscalate` — fires the engine's `kill_switch.manual_trigger`
/// when `trigger = true` on a given tick. The evaluator suppresses
/// repeat fires on consecutive ticks (per-node state tracks last
/// fired ms) so a sustained-true predicate doesn't keep escalating.
#[derive(Debug, Default)]
pub struct KillEscalate;

impl NodeKind for KillEscalate {
    fn kind(&self) -> &'static str {
        "Out.KillEscalate"
    }
    fn input_ports(&self) -> &[Port] {
        &KILL_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &UNIT_OUT
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::Unit])
    }
}

static FLATTEN_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("trigger", PortType::Bool),
        Port::new("policy", PortType::String),
    ]
});

/// `Out.Flatten` — on `trigger = true`, fire a kill-switch L4
/// (FlattenAll) with the policy string from the connected
/// `Exec.*Config` node. The engine's existing `paired_unwind` +
/// `twap_executor` pipeline interprets the policy on escalation.
#[derive(Debug, Default)]
pub struct Flatten;

impl NodeKind for Flatten {
    fn kind(&self) -> &'static str {
        "Out.Flatten"
    }
    fn input_ports(&self) -> &[Port] {
        &FLATTEN_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &UNIT_OUT
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::Unit])
    }
}
