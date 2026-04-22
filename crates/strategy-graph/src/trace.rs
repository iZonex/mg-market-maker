//! Per-tick execution trace for the strategy-graph runtime.
//!
//! The preview endpoint already asks the evaluator for a shallow
//! edge-values snapshot (`EvalTrace = HashMap<(NodeId, String), Value>`).
//! Live observability needs richer data: per-node inputs, per-node
//! elapsed time, sink fires, and an `error` marker so the UI can
//! render a faulty node distinctly from a healthy one.
//!
//! `TickTrace` is that richer data shape. It's stored in a per-
//! deployment ring buffer (`DeploymentDetailsStore::graph_traces`)
//! and served over the `graph_trace_recent` details topic.
//!
//! The trace carries *values*, not edges. Edges are derivable from
//! the graph the UI already has on canvas — the UI maps
//! `(node_id, output_port) → value` onto its edges at render time.

use crate::evaluator::SinkAction;
use crate::types::{NodeId, Value};
use serde::{Deserialize, Serialize};

/// Status tag for one node's tick. `Source` distinguishes a node
/// that short-circuited as a pure producer (no inputs, value came
/// from `source_inputs` overlay) from a regular evaluated node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum ExecStatus {
    Ok,
    Source,
    Error(String),
}

/// One node's per-tick execution record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeExec {
    pub id: NodeId,
    /// Catalog kind string (`"Math.Add"`, `"Surveillance.RugScore"`, …).
    pub kind: String,
    /// Input port name → value pairs, in declared order. Empty for
    /// source nodes.
    pub inputs: Vec<(String, Value)>,
    /// Output port name → value pairs, in declared order.
    pub outputs: Vec<(String, Value)>,
    pub elapsed_ns: u32,
    pub status: ExecStatus,
}

/// Full per-tick trace — every node's execution plus the sinks that
/// fired. Carries its own graph hash so a UI viewing a stale trace
/// can detect a swap happened between subscription and delivery.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TickTrace {
    /// Unix-ms timestamp when the tick started. `0` when the evaluator
    /// produced the trace (the timestamp is filled by the caller that
    /// pushes into the ring buffer — evaluator is clock-free).
    pub tick_ms: u64,
    /// Monotonic per-deployment tick counter. Filled by the engine.
    pub tick_num: u64,
    /// Content hash of the graph that produced this trace. The UI
    /// uses this to detect a graph swap between subscribe and receive.
    pub graph_hash: String,
    /// Total tick elapsed time in nanoseconds.
    pub total_elapsed_ns: u32,
    /// One entry per node in topological order.
    pub nodes: Vec<NodeExec>,
    /// Sinks that fired on this tick. Mirrors what `tick()` returns
    /// so the UI can render "on this tick `Out.KillEscalate` fired
    /// with level=4".
    pub sinks_fired: Vec<SinkAction>,
}

/// Static structural analysis of a compiled graph. Computed once on
/// swap, stored in `DeploymentDetailsStore::graph_analysis`, served
/// over the `graph_analysis` details topic.
///
/// Not per-tick — this is topology, not runtime.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphAnalysis {
    pub graph_hash: String,
    /// Per-node topological depth (sources = 0, deepest sink = N).
    pub depth_map: Vec<(NodeId, u32)>,
    /// Set of node kinds that are *source-kind* and referenced by
    /// the graph. Examples: `"Book.L1"`, `"Surveillance.RugScore"`,
    /// `"Onchain.HolderConcentration"`.
    pub required_sources: Vec<String>,
    /// Nodes that have no path to any sink — authoring error.
    pub dead_nodes: Vec<NodeId>,
    /// Output ports produced but never consumed by any edge.
    pub unconsumed_outputs: Vec<(NodeId, String)>,
}

impl TickTrace {
    /// M5-GOBS — flatten source-node outputs into a kind-keyed
    /// lookup so a replay against a DIFFERENT graph can pull
    /// its own source values via `(kind, port)`. NodeIds on the
    /// original graph don't exist on the candidate so we erase
    /// them at this layer.
    pub fn source_kind_values(&self) -> std::collections::HashMap<(String, String), Value> {
        let mut out = std::collections::HashMap::new();
        for n in &self.nodes {
            if !matches!(n.status, ExecStatus::Source) {
                continue;
            }
            for (port, value) in &n.outputs {
                out.insert((n.kind.clone(), port.clone()), value.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn serde_roundtrip_tick_trace() {
        let id = NodeId::new();
        let trace = TickTrace {
            tick_ms: 1713891234567,
            tick_num: 42,
            graph_hash: "abc123".into(),
            total_elapsed_ns: 142_000,
            nodes: vec![NodeExec {
                id,
                kind: "Math.Add".into(),
                inputs: vec![
                    ("a".into(), Value::Number(dec!(3))),
                    ("b".into(), Value::Number(dec!(4))),
                ],
                outputs: vec![("out".into(), Value::Number(dec!(7)))],
                elapsed_ns: 1_200,
                status: ExecStatus::Ok,
            }],
            sinks_fired: vec![SinkAction::SpreadMult(dec!(1.5))],
        };
        let json = serde_json::to_string(&trace).unwrap();
        let back: TickTrace = serde_json::from_str(&json).unwrap();
        assert_eq!(back, trace);
    }

    #[test]
    fn exec_status_serde() {
        for s in [
            ExecStatus::Ok,
            ExecStatus::Source,
            ExecStatus::Error("divide by zero".into()),
        ] {
            let j = serde_json::to_string(&s).unwrap();
            let back: ExecStatus = serde_json::from_str(&j).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn graph_analysis_default() {
        let a = GraphAnalysis::default();
        assert!(a.graph_hash.is_empty());
        assert!(a.dead_nodes.is_empty());
    }
}
