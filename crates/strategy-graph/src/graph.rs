//! `Graph` — topology + validation + topological sort.
//!
//! A graph is the operator-authored unit — `{ nodes, edges, scope }`
//! serialised to JSON. Built-in nodes from the catalog can be
//! re-hydrated from `Node.kind: String`; every graph carries its
//! serialised representation too so unknown kinds fail loudly at
//! validation time instead of silently evaluating as no-op.

use crate::node::{ConfigField, ConfigWidget};
use crate::types::{Edge, NodeId, PortType};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Graph scope — controls which engines consume the graph's outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Scope {
    /// Single symbol (e.g. `"BTCUSDT"`).
    Symbol(String),
    /// An asset class defined in `kill_switch.asset_classes`
    /// (`"major-spot"`, `"meme-spot"`, …).
    AssetClass(String),
    /// One client's engines.
    Client(String),
    /// All engines.
    Global,
}

/// Serialisable node shell — catalog kind + free-form config + UI
/// position + id. The runtime catalog hydrates `kind` into a
/// `Box<dyn NodeKind>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub pos: (f32, f32),
}

/// The graph's JSON face. Version-guarded so a breaking schema change
/// (new port types, renamed catalog entries) forces an explicit bump.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    pub version: u32,
    pub name: String,
    pub scope: Scope,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Optional — how long a stale source holds its last-good output
    /// before the reachable sinks fail closed. Default 30 s.
    #[serde(default = "default_stale_hold_ms")]
    pub stale_hold_ms: u64,
}

fn default_stale_hold_ms() -> u64 {
    30_000
}

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Maximum depth allowed during topological sort. Guards against
/// pathological imports that would otherwise run the evaluator out of
/// the engine's tick budget. Comfortably above the realistic node
/// count in a hand-authored graph.
pub const MAX_GRAPH_DEPTH: usize = 128;

/// Validation errors — grouped into one enum so the JSON response
/// can render them all without caller-side string fishing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("saved with schema v{0}, this build reads v{1} — re-save the graph")]
    UnsupportedVersion(u32, u32),
    #[error("two nodes share the same id — delete the duplicate")]
    DuplicateNodeId(NodeId),
    #[error(
        "a connection points at a node that no longer exists \
         (port \"{to_port}\") — delete the stale connection and re-wire"
    )]
    DanglingEdge {
        from: NodeId,
        from_port: String,
        to: NodeId,
        to_port: String,
    },
    #[error(
        "can't connect a {from_ty:?} output to a {to_ty:?} input \
         (edge \"{from_port}\" → \"{to_port}\") — the port types must match"
    )]
    PortTypeMismatch {
        from: NodeId,
        from_port: String,
        to: NodeId,
        to_port: String,
        from_ty: PortType,
        to_ty: PortType,
    },
    #[error("the graph has a loop (node {0} feeds into itself) — break the cycle")]
    Cycle(NodeId),
    #[error(
        "input port \"{to_port}\" has {count} incoming connections \
         but only accepts one — remove the extra"
    )]
    MultipleInputs {
        to: NodeId,
        to_port: String,
        count: usize,
    },
    #[error(
        "every graph needs one Out.SpreadMult sink — add a \
         \"Spread multiplier\" node and connect your output to it"
    )]
    NoSpreadMultSink,
    #[error("graph is too deep ({0} levels, limit {1}) — flatten some branches")]
    DepthExceeded(usize, usize),
    #[error("unknown node type \"{0}\" — was it renamed or removed from the catalog?")]
    UnknownKind(String),
    #[error(
        "node \"{0}\" is restricted (pentest-only) — deploy blocked \
         unless MM_RESTRICTED_ALLOW=1 is set on the server"
    )]
    RestrictedNotAllowed(String),
    #[error(
        "node \"{kind}\" config contains unknown field \"{field}\" — \
         typo? allowed fields: {allowed:?}"
    )]
    UnknownConfigField {
        node: NodeId,
        kind: String,
        field: String,
        allowed: Vec<String>,
    },
    #[error(
        "node \"{kind}\" config field \"{field}\" has wrong type: \
         expected {expected}, got {got}"
    )]
    InvalidConfigFieldType {
        node: NodeId,
        kind: String,
        field: String,
        expected: &'static str,
        got: String,
    },
    #[error(
        "node \"{kind}\" config field \"{field}\" enum value \
         \"{value}\" not in allowed options {options:?}"
    )]
    InvalidConfigEnumValue {
        node: NodeId,
        kind: String,
        field: String,
        value: String,
        options: Vec<String>,
    },
}

/// Result of a full topological sort — nodes in evaluation order
/// (sources first, sinks last).
#[derive(Debug, Clone)]
pub struct TopoOrder {
    pub order: Vec<NodeId>,
    pub depth: usize,
}

impl Graph {
    /// Build a fresh graph with no nodes.
    pub fn empty(name: impl Into<String>, scope: Scope) -> Self {
        Self {
            version: CURRENT_SCHEMA_VERSION,
            name: name.into(),
            scope,
            nodes: Vec::new(),
            edges: Vec::new(),
            stale_hold_ms: default_stale_hold_ms(),
        }
    }

    /// Canonical JSON → SHA-256 hash. The audit trail records this
    /// hash on every deploy so a regulator asking "was graph X ever
    /// live?" gets an answer with no ambiguity about which X.
    pub fn content_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let body = serde_json::to_string(self).unwrap_or_default();
        let digest = Sha256::digest(body.as_bytes());
        hex::encode(digest)
    }

    /// Find a node by id.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Validate the graph against the caller-supplied catalog. The
    /// catalog surface is intentionally a closure — that keeps this
    /// module from depending on the concrete catalog types.
    pub fn validate<F>(&self, resolve_kind: F) -> std::result::Result<TopoOrder, ValidationError>
    where
        F: Fn(&str) -> Option<KindShape>,
    {
        if self.version != CURRENT_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedVersion(
                self.version,
                CURRENT_SCHEMA_VERSION,
            ));
        }

        // 1. node id uniqueness
        let mut seen_ids: HashSet<NodeId> = HashSet::with_capacity(self.nodes.len());
        for n in &self.nodes {
            if !seen_ids.insert(n.id) {
                return Err(ValidationError::DuplicateNodeId(n.id));
            }
        }

        // 2. resolve every node's kind up-front so we can look up
        //    port shapes below. Also run per-node-kind config-schema
        //    validation here — catches typos + wrong-type fields
        //    before the graph is compiled.
        let mut shapes: HashMap<NodeId, KindShape> = HashMap::with_capacity(self.nodes.len());
        for n in &self.nodes {
            let Some(shape) = resolve_kind(&n.kind) else {
                return Err(ValidationError::UnknownKind(n.kind.clone()));
            };
            if shape.restricted && !allow_restricted_env() {
                return Err(ValidationError::RestrictedNotAllowed(n.kind.clone()));
            }
            validate_node_config(n, &shape)?;
            shapes.insert(n.id, shape);
        }

        // 3. fan-in: no input port may have more than one incoming edge
        let mut fan_in: HashMap<(NodeId, String), usize> = HashMap::new();
        for e in &self.edges {
            *fan_in.entry((e.to.node, e.to.port.clone())).or_insert(0) += 1;
        }
        for ((to, port), count) in &fan_in {
            if *count > 1 {
                return Err(ValidationError::MultipleInputs {
                    to: *to,
                    to_port: port.clone(),
                    count: *count,
                });
            }
        }

        // 4. every edge: endpoints exist + port types match
        for e in &self.edges {
            let src = shapes.get(&e.from.node).ok_or_else(|| {
                ValidationError::DanglingEdge {
                    from: e.from.node,
                    from_port: e.from.port.clone(),
                    to: e.to.node,
                    to_port: e.to.port.clone(),
                }
            })?;
            let dst = shapes.get(&e.to.node).ok_or_else(|| {
                ValidationError::DanglingEdge {
                    from: e.from.node,
                    from_port: e.from.port.clone(),
                    to: e.to.node,
                    to_port: e.to.port.clone(),
                }
            })?;
            let Some(src_ty) = src.output(&e.from.port) else {
                return Err(ValidationError::DanglingEdge {
                    from: e.from.node,
                    from_port: e.from.port.clone(),
                    to: e.to.node,
                    to_port: e.to.port.clone(),
                });
            };
            let Some(dst_ty) = dst.input(&e.to.port) else {
                return Err(ValidationError::DanglingEdge {
                    from: e.from.node,
                    from_port: e.from.port.clone(),
                    to: e.to.node,
                    to_port: e.to.port.clone(),
                });
            };
            if src_ty != dst_ty {
                return Err(ValidationError::PortTypeMismatch {
                    from: e.from.node,
                    from_port: e.from.port.clone(),
                    to: e.to.node,
                    to_port: e.to.port.clone(),
                    from_ty: src_ty,
                    to_ty: dst_ty,
                });
            }
        }

        // 5. topological sort (Kahn's algorithm) — rejects cycles.
        let mut indeg: HashMap<NodeId, usize> =
            self.nodes.iter().map(|n| (n.id, 0)).collect();
        let mut fwd: HashMap<NodeId, Vec<NodeId>> =
            self.nodes.iter().map(|n| (n.id, Vec::new())).collect();
        for e in &self.edges {
            *indeg.entry(e.to.node).or_insert(0) += 1;
            fwd.entry(e.from.node).or_default().push(e.to.node);
        }
        let mut ready: VecDeque<NodeId> = indeg
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut order: Vec<NodeId> = Vec::with_capacity(self.nodes.len());
        while let Some(id) = ready.pop_front() {
            order.push(id);
            if let Some(children) = fwd.get(&id).cloned() {
                for child in children {
                    let d = indeg.get_mut(&child).expect("indeg tracked on insert");
                    *d -= 1;
                    if *d == 0 {
                        ready.push_back(child);
                    }
                }
            }
        }
        if order.len() != self.nodes.len() {
            // Every cycle leaves at least one surviving positive indeg
            // node; pick the first such for the error payload.
            let stuck = indeg
                .into_iter()
                .find(|(_, d)| *d > 0)
                .map(|(id, _)| id)
                .unwrap_or_default();
            return Err(ValidationError::Cycle(stuck));
        }

        // 6. depth bound — longest path length from any source to
        //    any sink.
        let depth = longest_path_length(&order, &fwd);
        if depth > MAX_GRAPH_DEPTH {
            return Err(ValidationError::DepthExceeded(depth, MAX_GRAPH_DEPTH));
        }

        // 7. at least one Out.SpreadMult reachable (fail-closed
        //    default: an operator cannot silently disable spread
        //    widening by deleting the last sink).
        let has_sink = self
            .nodes
            .iter()
            .any(|n| n.kind == "Out.SpreadMult");
        if !has_sink {
            return Err(ValidationError::NoSpreadMultSink);
        }

        Ok(TopoOrder { order, depth })
    }
}

/// Compact view of a node kind's I/O shape for validation. Avoids
/// pulling the concrete catalog types into this module.
#[derive(Debug, Clone)]
pub struct KindShape {
    pub inputs: Vec<(String, PortType)>,
    pub outputs: Vec<(String, PortType)>,
    pub restricted: bool,
    pub config_schema: Vec<ConfigField>,
}

impl KindShape {
    fn input(&self, name: &str) -> Option<PortType> {
        self.inputs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| *t)
    }
    fn output(&self, name: &str) -> Option<PortType> {
        self.outputs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| *t)
    }
}

fn longest_path_length(order: &[NodeId], fwd: &HashMap<NodeId, Vec<NodeId>>) -> usize {
    let mut depth: HashMap<NodeId, usize> = order.iter().map(|id| (*id, 0)).collect();
    for id in order {
        let d = *depth.get(id).unwrap_or(&0);
        if let Some(children) = fwd.get(id) {
            for child in children {
                let entry = depth.entry(*child).or_insert(0);
                if *entry < d + 1 {
                    *entry = d + 1;
                }
            }
        }
    }
    depth.values().copied().max().unwrap_or(0)
}

fn allow_restricted_env() -> bool {
    std::env::var("MM_ALLOW_RESTRICTED")
        .map(|v| v == "yes-pentest-mode")
        .unwrap_or(false)
}

/// Walk the node's `config` JSON against the schema's declared
/// fields. Rejects unknown fields (typos → silent drop) and
/// wrong-type values (string instead of bool, non-option enum
/// value, etc.). Missing fields are allowed because consumers
/// always fall back to `ConfigField::default` on the read side.
fn validate_node_config(
    node: &Node,
    shape: &KindShape,
) -> std::result::Result<(), ValidationError> {
    // Null / absent config is always fine — the engine reads
    // individual keys with `.get().unwrap_or(default)`.
    let cfg = match &node.config {
        serde_json::Value::Null => return Ok(()),
        serde_json::Value::Object(m) => m,
        _ => return Ok(()),
    };
    // Opt-in: nodes that haven't declared a schema yet skip
    // validation. This keeps the check non-breaking while the
    // migration of every node to a declared schema lands
    // incrementally. Once a node has any `ConfigField` declared,
    // the full check fires — unknown fields + wrong types are
    // refused.
    if shape.config_schema.is_empty() {
        return Ok(());
    }
    let allowed: HashSet<&str> = shape.config_schema.iter().map(|f| f.name).collect();
    for key in cfg.keys() {
        if !allowed.contains(key.as_str()) {
            return Err(ValidationError::UnknownConfigField {
                node: node.id,
                kind: node.kind.clone(),
                field: key.clone(),
                allowed: allowed.iter().map(|s| s.to_string()).collect(),
            });
        }
    }
    for field in &shape.config_schema {
        let Some(v) = cfg.get(field.name) else { continue };
        if v.is_null() {
            continue;
        }
        let ok = match &field.widget {
            ConfigWidget::Number { .. } => {
                v.is_number() || v.as_str().map(|s| s.parse::<f64>().is_ok()).unwrap_or(false)
            }
            ConfigWidget::Integer { .. } => {
                v.is_i64() || v.is_u64()
                    || v.as_str().map(|s| s.parse::<i64>().is_ok()).unwrap_or(false)
            }
            ConfigWidget::Text => v.is_string(),
            ConfigWidget::Bool => v.is_boolean(),
            ConfigWidget::Enum { options } => {
                let Some(s) = v.as_str() else {
                    return Err(ValidationError::InvalidConfigFieldType {
                        node: node.id,
                        kind: node.kind.clone(),
                        field: field.name.to_string(),
                        expected: "enum string",
                        got: describe_json_type(v),
                    });
                };
                if !options.iter().any(|o| o.value == s) {
                    return Err(ValidationError::InvalidConfigEnumValue {
                        node: node.id,
                        kind: node.kind.clone(),
                        field: field.name.to_string(),
                        value: s.to_string(),
                        options: options.iter().map(|o| o.value.to_string()).collect(),
                    });
                }
                true
            }
        };
        if !ok {
            return Err(ValidationError::InvalidConfigFieldType {
                node: node.id,
                kind: node.kind.clone(),
                field: field.name.to_string(),
                expected: match &field.widget {
                    ConfigWidget::Number { .. } => "number",
                    ConfigWidget::Integer { .. } => "integer",
                    ConfigWidget::Text => "string",
                    ConfigWidget::Bool => "bool",
                    ConfigWidget::Enum { .. } => "enum",
                },
                got: describe_json_type(v),
            });
        }
    }
    Ok(())
}

fn describe_json_type(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
    .to_string()
}

