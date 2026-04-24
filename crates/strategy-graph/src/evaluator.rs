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
use crate::trace::{ExecStatus, NodeExec, TickTrace};
use crate::types::{AtomicBundleSpec, GraphQuote, NodeId, Value, VenueQuote};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::Instant;

/// Per-tick capture of every node's produced output values, keyed by
/// `(node_id, output_port_name)`. Populated only by
/// [`Evaluator::tick_with_trace`] — the dashboard preview endpoint
/// streams these back to the UI so the operator sees live values on
/// every edge of the canvas.
pub type EvalTrace = HashMap<(NodeId, String), Value>;

/// Engine-side action produced by a sink node firing on a given tick.
/// The evaluator collects these in order; the engine applies them
/// after the `tick()` returns.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum SinkAction {
    SpreadMult(Decimal),
    SizeMult(Decimal),
    KillEscalate {
        level: u8,
        reason: String,
        /// GR-2 — optional venue scope. When `Some(v)` the
        /// engine applies the escalation only if its own
        /// venue matches `v`; cross-engine fan-out stays
        /// idle. `None` keeps the legacy global semantics
        /// (every engine receiving this action escalates).
        venue: Option<String>,
    },
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
    /// Multi-Venue 3.A — like [`Self::Quotes`] but every entry
    /// carries its own `(venue, symbol, product)` destination.
    /// The engine dispatcher fans each entry to the matching
    /// engine's order manager; entries whose target is the engine
    /// itself collapse to the `Quotes` path for the degenerate case.
    VenueQuotes(Vec<VenueQuote>),
    /// Multi-Venue 3.E — atomic maker+hedge pair. The engine
    /// arms both legs then watches for ack within `timeout_ms`;
    /// if either venue fails to ack in time, cancels the other
    /// leg so we never leave a naked maker sitting on one book.
    AtomicBundle(Box<AtomicBundleSpec>),
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

/// M5-GOBS — helper that maps a candidate graph's source-node
/// outputs to a `source_inputs` map for re-evaluation, given a
/// kind-keyed lookup of values from some *other* graph's tick.
///
/// Walks the candidate evaluator's catalog-registered nodes,
/// filters to sources (zero declared input ports), looks up each
/// `(kind, port)` in `kind_values`, and emits the corresponding
/// `(NodeId, port) → Value` tuple for the candidate's own IDs.
/// Skips kinds not present in `kind_values` so a replay against
/// a graph that references detectors the source trace never fed
/// falls back to the defaults each node returns from `evaluate`.
pub fn replay_source_inputs(
    candidate: &Evaluator,
    kind_values: &HashMap<(String, String), Value>,
) -> HashMap<(NodeId, String), Value> {
    let mut out: HashMap<(NodeId, String), Value> = HashMap::new();
    // M5-GOBS — count candidate source nodes per kind. A kind
    // that appears more than once means we can't deterministically
    // dispatch trace values by kind alone (e.g. two `Math.Const`
    // literals with different `config.value`s would collapse onto
    // whichever sat last in the original trace). Skip those and
    // let the candidate's own `evaluate()` supply its per-node
    // default, which for literal-style sources is read from config.
    let mut kind_counts: HashMap<&str, usize> = HashMap::new();
    for id in &candidate.order {
        let Some(order) = candidate.input_order.get(id) else { continue };
        if !order.is_empty() {
            continue;
        }
        if let Some(kind) = candidate.kinds.get(id) {
            *kind_counts.entry(kind.as_str()).or_insert(0) += 1;
        }
    }

    for id in &candidate.order {
        let Some(order) = candidate.input_order.get(id) else { continue };
        if !order.is_empty() {
            continue;
        }
        let Some(kind) = candidate.kinds.get(id).cloned() else { continue };
        // Ambiguous dispatch — two source nodes share this kind;
        // we can't tell them apart from a kind-keyed trace. Fall
        // back to `evaluate()` so each literal uses its own config.
        if kind_counts.get(kind.as_str()).copied().unwrap_or(0) > 1 {
            continue;
        }
        let Some(node) = candidate.nodes.get(id) else { continue };
        for p in node.output_ports() {
            if let Some(v) = kind_values.get(&(kind.clone(), p.name.clone())) {
                out.insert((*id, p.name.clone()), v.clone());
            }
        }
    }
    out
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

    /// Static topology analysis — depth map, required sources,
    /// dead nodes, unconsumed outputs. Computed from the compiled
    /// evaluator so it sees the same graph the engine will
    /// execute. Safe to call any time; cheap (O(nodes + edges)).
    pub fn analyze(&self, graph_hash: impl Into<String>) -> crate::trace::GraphAnalysis {
        use std::collections::HashSet;

        // Depth map via Kahn (topological depth from sources).
        let mut depth: HashMap<NodeId, u32> = HashMap::new();
        for id in &self.order {
            let order = self.input_order.get(id).cloned().unwrap_or_default();
            let d = order
                .iter()
                .filter_map(|port| {
                    self.incoming
                        .get(&(*id, port.clone()))
                        .and_then(|(src, _)| depth.get(src).copied())
                })
                .max()
                .map(|m| m + 1)
                .unwrap_or(0);
            depth.insert(*id, d);
        }

        // Reverse-BFS from every sink. A sink is any node whose
        // catalog kind starts with `Out.`.
        let mut outgoing: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for ((to_node, _), (from_node, _)) in &self.incoming {
            outgoing.entry(*from_node).or_default().push(*to_node);
        }
        let sinks: Vec<NodeId> = self
            .kinds
            .iter()
            .filter(|(_, k)| k.starts_with("Out."))
            .map(|(id, _)| *id)
            .collect();
        let mut reachable: HashSet<NodeId> = HashSet::new();
        let mut stack: Vec<NodeId> = sinks.clone();
        // Walk predecessors: anything that can reach a sink stays.
        // Predecessor lookup via incoming map.
        while let Some(n) = stack.pop() {
            if !reachable.insert(n) {
                continue;
            }
            // Find predecessors of n (incoming edges pointing at n's
            // input ports). The `incoming` map is keyed by (to_node,
            // to_port) → (from_node, from_port) — scan it once.
            for ((to_node, _), (from_node, _)) in &self.incoming {
                if *to_node == n {
                    stack.push(*from_node);
                }
            }
        }

        let dead_nodes: Vec<NodeId> = self
            .order
            .iter()
            .filter(|id| !reachable.contains(id))
            .copied()
            .collect();

        // Required sources: nodes that are source (empty input
        // ports) AND reachable (have path to a sink) AND whose kind
        // string is a known source prefix (Book., Trade., Volatility.,
        // Sentiment., Surveillance., Onchain., Funding., Cost.,
        // Liquidity., Regime., etc. — anything that's not `Math.`,
        // `Logic.`, `Cast.`, `Strategy.`, `Exec.`, `Plan.`, `Out.`).
        // We filter by presence of a dot in the kind and by not
        // being in the "transform / sink / strategy" set.
        let mut required_sources: Vec<String> = self
            .order
            .iter()
            .filter(|id| {
                let order = self.input_order.get(id).cloned().unwrap_or_default();
                order.is_empty() && reachable.contains(id)
            })
            .filter_map(|id| self.kinds.get(id).cloned())
            .filter(|kind| !is_transform_kind(kind))
            .collect();
        required_sources.sort();
        required_sources.dedup();

        // Unconsumed outputs: each node's output_ports that has no
        // edge from (node, port) anywhere in `incoming.values()`.
        let consumed: HashSet<(NodeId, String)> = self
            .incoming
            .values()
            .map(|(n, p)| (*n, p.clone()))
            .collect();
        let mut unconsumed_outputs: Vec<(NodeId, String)> = Vec::new();
        for id in &self.order {
            let Some(node) = self.nodes.get(id) else { continue };
            // Sinks don't expose useful outputs; skip them.
            let is_sink = self
                .kinds
                .get(id)
                .map(|k| k.starts_with("Out."))
                .unwrap_or(false);
            if is_sink {
                continue;
            }
            for p in node.output_ports() {
                if !consumed.contains(&(*id, p.name.clone())) {
                    unconsumed_outputs.push((*id, p.name.clone()));
                }
            }
        }

        let mut depth_map: Vec<(NodeId, u32)> = depth.into_iter().collect();
        depth_map.sort_by_key(|(_, d)| *d);

        crate::trace::GraphAnalysis {
            graph_hash: graph_hash.into(),
            depth_map,
            required_sources,
            dead_nodes,
            unconsumed_outputs,
        }
    }

    /// UI-1 — snapshot of `Plan.*` node states. Reads
    /// `NodeState::get::<PlanState>` for every node whose kind
    /// starts with `Plan.`, clones the `PlanState` so callers
    /// don't hold a borrow on the evaluator.
    pub fn plan_snapshots(&self) -> Vec<(NodeId, String, crate::nodes::plan::PlanState)> {
        let mut out = Vec::new();
        for (id, kind) in &self.kinds {
            if !kind.starts_with("Plan.") {
                continue;
            }
            if let Some(state) = self.states.get(id) {
                if let Some(ps) = state.get::<crate::nodes::plan::PlanState>() {
                    out.push((*id, kind.clone(), ps.clone()));
                }
            }
        }
        out
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
        self.tick_inner(ctx, source_inputs, &mut None).map(|(s, _)| s)
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
        let mut full = Some(TickTrace::default());
        let (sinks, _) = self.tick_inner(ctx, source_inputs, &mut full)?;
        let trace = full
            .map(|t| eval_trace_from_tick_trace(&t))
            .unwrap_or_default();
        Ok((sinks, trace))
    }

    /// Live-observability evaluation: fills a [`TickTrace`] with
    /// per-node inputs / outputs / elapsed time + sinks fired, on top
    /// of returning the sinks themselves. Used by the engine tick hook
    /// when a subscriber has asked for graph trace streaming; the
    /// evaluator itself stays clock-free (the timestamp / tick counter /
    /// graph hash are filled by the caller).
    pub fn tick_with_full_trace(
        &mut self,
        ctx: &EvalCtx,
        source_inputs: &HashMap<(NodeId, String), Value>,
    ) -> anyhow::Result<(Vec<SinkAction>, TickTrace)> {
        let mut full = Some(TickTrace::default());
        let (sinks, _) = self.tick_inner(ctx, source_inputs, &mut full)?;
        Ok((sinks, full.unwrap_or_default()))
    }

    fn tick_inner(
        &mut self,
        ctx: &EvalCtx,
        source_inputs: &HashMap<(NodeId, String), Value>,
        full_trace: &mut Option<TickTrace>,
    ) -> anyhow::Result<(Vec<SinkAction>, ())> {
        let mut outputs: HashMap<NodeId, NodeOutputs> =
            HashMap::with_capacity(self.order.len());
        let mut sinks: Vec<SinkAction> = Vec::new();
        let tick_started = full_trace.as_ref().map(|_| Instant::now());
        if let Some(t) = full_trace.as_mut() {
            t.nodes.reserve(self.order.len());
        }

        for id in &self.order {
            let node = self
                .nodes
                .get(id)
                .expect("evaluator built with this id");
            let kind_name = self.kinds.get(id).cloned().unwrap_or_default();
            let node_started = full_trace.as_ref().map(|_| Instant::now());

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
            let mut output_pairs: Vec<(String, Value)> = Vec::new();
            for (port, value) in node.output_ports().iter().zip(produced.iter().cloned()) {
                if full_trace.is_some() {
                    output_pairs.push((port.name.clone(), value.clone()));
                }
                out_map.insert(port.name.clone(), value);
            }
            outputs.insert(*id, out_map);

            // Capture per-node exec record before the sink-harvest
            // below — the sink harvest `continue`s on suppressed
            // VenueQuotesIf, which would otherwise skip pushing a
            // NodeExec for that node.
            if let Some(t) = full_trace.as_mut() {
                let inputs_named: Vec<(String, Value)> = if is_source {
                    // Source nodes have no declared input ports.
                    // Record which `source_inputs` keys actually
                    // resolved into this node's outputs so the UI
                    // can show what the engine fed in.
                    node.output_ports()
                        .iter()
                        .filter_map(|p| {
                            source_inputs
                                .get(&(*id, p.name.clone()))
                                .cloned()
                                .map(|v| (p.name.clone(), v))
                        })
                        .collect()
                } else {
                    order
                        .iter()
                        .cloned()
                        .zip(input_vec.iter().cloned())
                        .collect()
                };
                let elapsed_ns = node_started
                    .map(|s| s.elapsed().as_nanos().min(u32::MAX as u128) as u32)
                    .unwrap_or(0);
                let status = if is_source {
                    ExecStatus::Source
                } else {
                    ExecStatus::Ok
                };
                t.nodes.push(NodeExec {
                    id: *id,
                    kind: kind_name.clone(),
                    inputs: inputs_named,
                    outputs: output_pairs,
                    elapsed_ns,
                    status,
                });
            }

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
                        use rust_decimal::prelude::ToPrimitive;
                        let level = input_vec
                            .get(1)
                            .and_then(|v| match v {
                                Value::KillLevel(l) => Some(*l),
                                // R2.14 — accept plain Number
                                // so operators pipe Math.Const
                                // into `level` directly. Clamp
                                // to the 1..=5 KillLevel range.
                                Value::Number(n) => n
                                    .to_u8()
                                    .map(|u| u.clamp(1, 5)),
                                _ => None,
                            })
                            .unwrap_or(2);
                        let reason = input_vec
                            .get(2)
                            .and_then(Value::as_string)
                            .unwrap_or("graph sink")
                            .to_string();
                        // GR-2 — honour an optional `venue`
                        // config string so a detector can scope
                        // the kill to a single venue's pool
                        // entry instead of every engine sharing
                        // the graph.
                        let venue = self
                            .configs
                            .get(id)
                            .and_then(|c| c.get("venue"))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());
                        sinks.push(SinkAction::KillEscalate { level, reason, venue });
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
                "Out.VenueQuotes" => {
                    if let Some(qs) = input_vec.first().and_then(Value::as_venue_quotes) {
                        sinks.push(SinkAction::VenueQuotes(qs.clone()));
                    }
                }
                "Out.VenueQuotesIf" => {
                    // Phase IV reactive-hedge gate. Only emit when
                    // the trigger fires; accept both VenueQuotes
                    // (tagged per-leg) and plain Quotes (untagged —
                    // the engine dispatcher tags them against the
                    // caller's primary venue). Missing trigger or
                    // quotes → no-op, so a stale upstream source
                    // never fires an unguarded hedge.
                    let trigger = input_vec
                        .first()
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !trigger {
                        continue;
                    }
                    if let Some(qs) = input_vec.get(1).and_then(Value::as_venue_quotes) {
                        if !qs.is_empty() {
                            sinks.push(SinkAction::VenueQuotes(qs.clone()));
                        }
                    } else if let Some(qs) = input_vec.get(1).and_then(Value::as_quotes) {
                        if !qs.is_empty() {
                            sinks.push(SinkAction::Quotes(qs.clone()));
                        }
                    }
                }
                "Out.AtomicBundle" => {
                    // Inputs: maker Quotes, hedge Quotes, timeout_ms
                    // Number. Takes the first entry of each bundle.
                    let maker = input_vec
                        .first()
                        .and_then(Value::as_venue_quotes)
                        .and_then(|v| v.first().cloned())
                        .or_else(|| {
                            input_vec
                                .first()
                                .and_then(Value::as_quotes)
                                .and_then(|v| v.first().cloned())
                                .map(|q| VenueQuote {
                                    venue: String::new(),
                                    symbol: String::new(),
                                    product: String::new(),
                                    side: q.side,
                                    price: q.price,
                                    qty: q.qty,
                                })
                        });
                    let hedge = input_vec
                        .get(1)
                        .and_then(Value::as_venue_quotes)
                        .and_then(|v| v.first().cloned());
                    let timeout_ms = input_vec
                        .get(2)
                        .and_then(Value::as_number)
                        .map(|d| d.trunc().to_string().parse::<u64>().unwrap_or(2_000))
                        .unwrap_or(2_000);
                    if let (Some(maker), Some(hedge)) = (maker, hedge) {
                        sinks.push(SinkAction::AtomicBundle(Box::new(
                            AtomicBundleSpec { maker, hedge, timeout_ms },
                        )));
                    }
                }
                _ => {}
            }
        }

        if let Some(t) = full_trace.as_mut() {
            t.sinks_fired = sinks.clone();
            t.total_elapsed_ns = tick_started
                .map(|s| s.elapsed().as_nanos().min(u32::MAX as u128) as u32)
                .unwrap_or(0);
        }

        Ok((sinks, ()))
    }
}

/// Project the rich [`TickTrace`] down to the legacy [`EvalTrace`]
/// (flat `(node_id, port) → value` map) used by the preview-tick
/// endpoint. Keeps the preview wire contract stable while the engine
/// tick hook reads the full trace.
fn eval_trace_from_tick_trace(trace: &TickTrace) -> EvalTrace {
    let mut out = HashMap::with_capacity(trace.nodes.len() * 2);
    for n in &trace.nodes {
        for (port, value) in &n.outputs {
            out.insert((n.id, port.clone()), value.clone());
        }
    }
    out
}

/// Kind-string classifier for the topology analyser. A "transform"
/// is any node whose role is to reshape upstream data — Math, Logic,
/// Cast, Strategy wrappers, Exec configs, Plan runners, and Out
/// sinks. Anything else with zero input ports is treated as a
/// source (Book.L1, Surveillance.RugScore, Onchain.HolderConcentration,
/// Funding.Rate, Sentiment.Rate, etc.). The list is kept explicit
/// so adding a new source family doesn't accidentally drop it from
/// the `required_sources` analysis.
fn is_transform_kind(kind: &str) -> bool {
    kind.starts_with("Math.")
        || kind.starts_with("Logic.")
        || kind.starts_with("Cast.")
        || kind.starts_with("Strategy.")
        || kind.starts_with("Exec.")
        || kind.starts_with("Plan.")
        || kind.starts_with("Out.")
}
