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
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        // GR-2 — optional `venue` scope. Empty = global
        // (every engine sharing the graph escalates). Match
        // values are case-insensitive; the engine compares
        // against `format!("{:?}", exchange_type).to_lowercase()`.
        vec![ConfigField {
            name: "venue",
            label: "Venue (optional)",
            hint: Some("Leave empty for a global kill; otherwise e.g. `binance`, `bybit`"),
            default: serde_json::json!(""),
            widget: ConfigWidget::Text,
        }]
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

static QUOTES_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("quotes", PortType::Quotes)]);

static VENUE_QUOTES_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("quotes", PortType::Quotes)]);

static ATOMIC_BUNDLE_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        // Both legs ride on the same Quotes port type — the evaluator
        // harvest picks the first of each bundle as the maker/hedge.
        Port::new("maker", PortType::Quotes),
        Port::new("hedge", PortType::Quotes),
        // timeout_ms — how long to wait for both venue acks before
        // rolling back.
        Port::new("timeout_ms", PortType::Number),
    ]
});

/// Multi-Venue 3.E — `Out.AtomicBundle`. Consumes a (maker, hedge)
/// pair of single-leg quote bundles plus a `timeout_ms` number.
/// Engine places both legs then watches for ack within timeout;
/// on one-sided failure it cancels the counter-leg. Empty inputs
/// abort the bundle cleanly (no dangling legs).
#[derive(Debug, Default)]
pub struct AtomicBundle;

impl NodeKind for AtomicBundle {
    fn kind(&self) -> &'static str {
        "Out.AtomicBundle"
    }
    fn input_ports(&self) -> &[Port] {
        &ATOMIC_BUNDLE_INPUTS
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

/// Multi-Venue 3.A — `Out.VenueQuotes`. Accepts a
/// `VenueQuotes(Vec<VenueQuote>)` bundle. Unlike `Out.Quotes`,
/// every entry carries its own `(venue, symbol, product)` tag
/// so the dispatcher can route to the right engine's order
/// manager. Missing → the engine falls back to its legacy
/// strategy path (same semantics as an absent `Out.Quotes`).
#[derive(Debug, Default)]
pub struct VenueQuotes;

impl NodeKind for VenueQuotes {
    fn kind(&self) -> &'static str {
        "Out.VenueQuotes"
    }
    fn input_ports(&self) -> &[Port] {
        &VENUE_QUOTES_INPUTS
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

/// Phase 4 — `Out.Quotes`. The graph took full authorship of the
/// quoting pipeline: every tick the incoming `Quotes` bundle is
/// what the engine places, replacing the built-in strategy.tick()
/// output entirely. If the upstream is `Missing` on a tick the
/// engine falls back to its default strategy for that tick (no
/// silent self-destruction when a sentiment feed drops out).
#[derive(Debug, Default)]
pub struct Quotes;

impl NodeKind for Quotes {
    fn kind(&self) -> &'static str {
        "Out.Quotes"
    }
    fn input_ports(&self) -> &[Port] {
        &QUOTES_INPUTS
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
