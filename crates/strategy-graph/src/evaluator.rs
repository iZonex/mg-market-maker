//! Pull-based per-tick evaluator.
//!
//! Call flow:
//!
//!   1. `Evaluator::build(graph)` → validates + compiles the topological
//!      order + instantiates every node's `Box<dyn NodeKind>`.
//!   2. `Evaluator::tick(ctx)` → for each node in order: read inputs
//!      from cached outputs, call `evaluate`, stash outputs in cache,
//!      harvest `SinkAction` if the node is a sink.
//!   3. Returned `Vec<SinkAction>` is what the engine applies.
//!
//! Source nodes are handled via an `EvalInputs` slot that the engine
//! populates pre-tick — for Phase 1 every source is stubbed as
//! `Value::Missing` so the end-to-end path is exercised without
//! pulling the engine in yet.

use crate::catalog;
use crate::graph::{Graph, ValidationError};
use crate::node::{EvalCtx, NodeKind, NodeOutputs, NodeState};
use crate::types::{GraphQuote, NodeId, Value};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Per-tick capture of every node's produced output values, keyed by
/// `(node_id, output_port_name)`. Populated only by
/// [`Evaluator::tick_with_trace`] — the dashboard preview endpoint
/// streams these back to the UI so the operator sees live values on
/// every edge of the canvas.
pub type EvalTrace = HashMap<(NodeId, String), Value>;

/// Engine-side action produced by a sink node firing on a given tick.
/// The evaluator collects these in order; the engine applies them
/// after the `tick()` returns.
#[derive(Debug, Clone, PartialEq)]
pub enum SinkAction {
    SpreadMult(Decimal),
    SizeMult(Decimal),
    KillEscalate { level: u8, reason: String },
    /// Phase 2 Wave D — graph-authored flatten. `policy` is the
    /// compact string emitted by an `Exec.*Config` node
    /// (`twap:120:5`, `vwap:300`, `pov:10`, `iceberg:0.1`). The
    /// engine fires kill L4 + passes the policy into its exec
    /// pipeline.
    Flatten {
        policy: String,
    },
    /// Phase 4 — the graph fully authored the quoting output. Engine
    /// replaces its `strategy.compute_quotes()` result with this
    /// bundle. If the bundle is empty the engine interprets it as
    /// "cancel everything" (same semantics as an explicit empty
    /// `QuotePair` list from a strategy). Missing / absent `Out.Quotes`
    /// falls back to the hard-wired strategy.
    Quotes(Vec<GraphQuote>),
}

/// Pre-compiled graph ready for per-tick evaluation. Holds the
/// ordered node list + state slots + the catalog-built node
/// implementations.
pub struct Evaluator {
    order: Vec<NodeId>,
    nodes: HashMap<NodeId, Box<dyn NodeKind>>,
    kinds: HashMap<NodeId, String>,
    states: HashMap<NodeId, NodeState>,
    /// `(to_node, to_port) -> (from_node, from_port)` — reverse
    /// lookup used on every tick to marshal each node's inputs.
    incoming: HashMap<(NodeId, String), (NodeId, String)>,
    /// Declared input port order per node (so we pass values in the
    /// same order the node's `input_ports()` declares).
    input_order: HashMap<NodeId, Vec<String>>,
    /// Per-node raw config JSON cloned at build time. Exposed via
    /// [`Self::node_configs`] so the engine source overlay can read
    /// each `Strategy.*` node's `γ` / `κ` / `σ` / size overrides
    /// without reparsing the graph JSON on every tick.
    configs: HashMap<NodeId, serde_json::Value>,
}

impl std::fmt::Debug for Evaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evaluator")
            .field("n_nodes", &self.order.len())
            .field("order", &self.order)
            .finish()
    }
}

impl Evaluator {
    /// Read-only view of every node's `kind` string, in topological
    /// order. The engine's source-marshaller needs this to decide
    /// which `source_inputs` entries to populate per kind (`Book.L1`,
    /// `Sentiment.Rate`, etc.).
    pub fn nodes_by_kind(&self) -> Vec<(NodeId, String)> {
        self.order
            .iter()
            .map(|id| (*id, self.kinds.get(id).cloned().unwrap_or_default()))
            .collect()
    }

    /// Phase 4.7 — per-node raw JSON config, indexed by `NodeId`.
    /// Engine reads this when building a per-node `StrategyContext`
    /// so each composite `Strategy.*` node can run its owning strategy
    /// under its own `γ` / `κ` / `σ` / size / spread overrides. Empty
    /// when no graph was registered (the evaluator takes ownership of
    /// the compiled nodes, not their config — we cache the JSON on
    /// build for this read-only view).
    pub fn node_configs(&self) -> &HashMap<NodeId, serde_json::Value> {
        &self.configs
    }

    /// Validate + compile. Resolves every node via the catalog; any
    /// unknown kind, cycle, or port-type mismatch surfaces as a
    /// `ValidationError` here and the caller refuses the deploy.
    pub fn build(graph: &Graph) -> Result<Self, ValidationError> {
        let topo = graph.validate(catalog::shape)?;
        let mut nodes: HashMap<NodeId, Box<dyn NodeKind>> =
            HashMap::with_capacity(graph.nodes.len());
        let mut kinds: HashMap<NodeId, String> = HashMap::with_capacity(graph.nodes.len());
        let mut states: HashMap<NodeId, NodeState> = HashMap::with_capacity(graph.nodes.len());
        let mut input_order: HashMap<NodeId, Vec<String>> =
            HashMap::with_capacity(graph.nodes.len());
        let mut configs: HashMap<NodeId, serde_json::Value> =
            HashMap::with_capacity(graph.nodes.len());
        for n in &graph.nodes {
            let built = catalog::build(&n.kind, &n.config)
                .ok_or_else(|| ValidationError::UnknownKind(n.kind.clone()))?;
            input_order.insert(
                n.id,
                built.input_ports().iter().map(|p| p.name.clone()).collect(),
            );
            nodes.insert(n.id, built);
            kinds.insert(n.id, n.kind.clone());
            states.insert(n.id, NodeState::default());
            configs.insert(n.id, n.config.clone());
        }
        let mut incoming: HashMap<(NodeId, String), (NodeId, String)> =
            HashMap::with_capacity(graph.edges.len());
        for e in &graph.edges {
            incoming.insert(
                (e.to.node, e.to.port.clone()),
                (e.from.node, e.from.port.clone()),
            );
        }
        Ok(Self {
            order: topo.order,
            nodes,
            kinds,
            states,
            incoming,
            input_order,
            configs,
        })
    }

    /// Evaluate the graph once. `source_inputs` supplies values for
    /// source-node output ports (anything reachable by incoming
    /// edges that isn't produced by another node in the graph). For
    /// Phase 1 end-to-end testing, tests pass an empty map and
    /// every input resolves to `Value::Missing`; the sink harvest
    /// still runs.
    pub fn tick(
        &mut self,
        ctx: &EvalCtx,
        source_inputs: &HashMap<(NodeId, String), Value>,
    ) -> anyhow::Result<Vec<SinkAction>> {
        self.tick_inner(ctx, source_inputs, false).map(|(s, _)| s)
    }

    /// Preview-mode evaluation: same as `tick` but ALSO captures
    /// every node's produced output keyed by `(node_id, port)`.
    /// The UI draws these as live labels on the corresponding
    /// edges so an operator sees exactly what the graph would
    /// do *before* deploying. Sink actions are returned too so
    /// the operator sees which branches fire — but the caller
    /// is expected to discard them (this is preview, not
    /// production).
    pub fn tick_with_trace(
        &mut self,
        ctx: &EvalCtx,
        source_inputs: &HashMap<(NodeId, String), Value>,
    ) -> anyhow::Result<(Vec<SinkAction>, EvalTrace)> {
        self.tick_inner(ctx, source_inputs, true)
    }

    fn tick_inner(
        &mut self,
        ctx: &EvalCtx,
        source_inputs: &HashMap<(NodeId, String), Value>,
        capture: bool,
    ) -> anyhow::Result<(Vec<SinkAction>, EvalTrace)> {
        let mut outputs: HashMap<NodeId, NodeOutputs> =
            HashMap::with_capacity(self.order.len());
        let mut sinks: Vec<SinkAction> = Vec::new();
        let mut trace: EvalTrace = if capture {
            HashMap::with_capacity(self.order.len() * 2)
        } else {
            HashMap::new()
        };

        for id in &self.order {
            let node = self
                .nodes
                .get(id)
                .expect("evaluator built with this id");
            let kind_name = self.kinds.get(id).cloned().unwrap_or_default();

            // Source node short-circuit — zero input ports means
            // the node is a pure producer. We pull its outputs
            // directly from `source_inputs` keyed by
            // `(node_id, output_port_name)` without calling
            // `evaluate()`. Unset ports default to `Missing` which
            // then propagates through the DAG.
            let mut input_vec: Vec<Value> = Vec::new();
            let order = self.input_order.get(id).cloned().unwrap_or_default();
            let is_source = order.is_empty();

            let produced: Vec<Value> = if is_source {
                // Source nodes: call `evaluate()` first to get any
                // node-authored defaults (e.g. `Math.Const` returns
                // its configured value), then overlay engine-
                // provided `source_inputs` per port so live data
                // always beats static defaults. That lets us mix
                // "read from engine" sources (Book.L1 / Sentiment
                // / Volatility / ...) with "literal" sources
                // (Math.Const) under one uniform catalog entry.
                let state = self.states.get_mut(id).expect("state slot per node");
                let defaults = node.evaluate(ctx, &input_vec, state)?;
                node.output_ports()
                    .iter()
                    .zip(defaults.into_iter())
                    .map(|(p, default)| {
                        source_inputs
                            .get(&(*id, p.name.clone()))
                            .cloned()
                            .unwrap_or(default)
                    })
                    .collect()
            } else {
                // Marshal inputs in declared order. If an input port
                // has an incoming edge, pull from the upstream
                // output; else try `source_inputs`; else Missing.
                for port_name in &order {
                    let key = (*id, port_name.clone());
                    let v = match self.incoming.get(&key) {
                        Some((src_id, src_port)) => outputs
                            .get(src_id)
                            .and_then(|m| m.get(src_port))
                            .cloned()
                            .unwrap_or(Value::Missing),
                        None => source_inputs
                            .get(&key)
                            .cloned()
                            .unwrap_or(Value::Missing),
                    };
                    input_vec.push(v);
                }
                let state = self.states.get_mut(id).expect("state slot per node");
                node.evaluate(ctx, &input_vec, state)?
            };

            // Stash produced values by output port name.
            let mut out_map: NodeOutputs = HashMap::new();
            for (port, value) in node.output_ports().iter().zip(produced.iter().cloned()) {
                if capture {
                    trace.insert((*id, port.name.clone()), value.clone());
                }
                out_map.insert(port.name.clone(), value);
            }
            outputs.insert(*id, out_map);

            // Sink harvest — turn the node's input into a SinkAction
            // for the engine to apply. We look up the input on the
            // input_vec we just built; missing inputs skip the
            // action (fail-closed default stays in effect).
            match kind_name.as_str() {
                "Out.SpreadMult" => {
                    if let Some(mult) =
                        input_vec.first().and_then(Value::as_number)
                    {
                        sinks.push(SinkAction::SpreadMult(mult));
                    }
                }
                "Out.SizeMult" => {
                    if let Some(mult) =
                        input_vec.first().and_then(Value::as_number)
                    {
                        sinks.push(SinkAction::SizeMult(mult));
                    }
                }
                "Out.KillEscalate" => {
                    let trigger = input_vec.first().and_then(Value::as_bool).unwrap_or(false);
                    if trigger {
                        let level = input_vec
                            .get(1)
                            .and_then(|v| match v {
                                Value::KillLevel(l) => Some(*l),
                                _ => None,
                            })
                            .unwrap_or(2);
                        let reason = input_vec
                            .get(2)
                            .and_then(Value::as_string)
                            .unwrap_or("graph sink")
                            .to_string();
                        sinks.push(SinkAction::KillEscalate { level, reason });
                    }
                }
                "Out.Flatten" => {
                    let trigger =
                        input_vec.first().and_then(Value::as_bool).unwrap_or(false);
                    if trigger {
                        let policy = input_vec
                            .get(1)
                            .and_then(Value::as_string)
                            .unwrap_or("twap:120:5")
                            .to_string();
                        sinks.push(SinkAction::Flatten { policy });
                    }
                }
                "Out.Quotes" => {
                    // Propagate a Quotes bundle to the engine. An
                    // absent input (`Missing`) skips the sink so the
                    // engine falls back to its own strategy tick —
                    // a stale feed never poisons the order placement.
                    if let Some(qs) = input_vec.first().and_then(Value::as_quotes) {
                        sinks.push(SinkAction::Quotes(qs.clone()));
                    }
                }
                _ => {}
            }
        }

        Ok((sinks, trace))
    }
}
