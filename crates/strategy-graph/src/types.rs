//! Core types — nothing here is engine-aware.
//!
//! Nodes carry `NodeId`s (UUIDs) + a `kind` string that maps back to
//! the runtime catalog. Ports are typed — [`PortType`] is a closed enum
//! so edge validation is a table comparison, not a runtime trait call.
//! Values flowing on edges are [`Value`]s; each `Value` tag matches
//! exactly one `PortType`.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a node within a graph.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct NodeId(pub Uuid);

impl NodeId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Deterministic id from a UUID-v4 string — used in tests + when
    /// roundtripping graphs through JSON.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        Uuid::parse_str(s).map(Self).map_err(Into::into)
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The set of types that can flow on an edge. Closed on purpose —
/// adding a new type is a code change in one place, not a plugin
/// surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    /// A single `Decimal` number. The default data channel.
    Number,
    /// A boolean.
    Bool,
    /// Explicit trigger signal — no payload. Used for state-machine
    /// transition outputs.
    Unit,
    /// A string, typically used for labels / audit tags.
    String,
    /// Kill level enum (mirrors `mm_risk::KillLevel` at the Number
    /// level — we don't pull the dep here).
    KillLevel,
    /// Phase 2 — which base strategy is running. Values are the
    /// catalog strings `"AvellanedaStoikov" | "GLFT" | "Grid" |
    /// "Basis" | "CrossExchange"`.
    StrategyKind,
    /// Phase 2 — pair-class classification (major-spot, meme-spot,
    /// alt-perp, etc.). The engine's classifier assigns one at
    /// startup; a graph can branch on it.
    PairClass,
    /// Phase 4 — a quote bundle. A `Vec<GraphQuote>` carrying the
    /// full set of levels the graph wants the engine to place. The
    /// engine consumes `Out.Quotes` by swapping its strategy.tick()
    /// result with these quotes, so a graph can author the whole
    /// pipeline not just overlay the multipliers.
    Quotes,
}

/// Phase 4 — a single quote level authored by a graph. Kept decoupled
/// from `mm_common::Quote` so the strategy-graph crate stays free of
/// engine types; the engine maps these into its own `Quote` on
/// consumption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphQuote {
    /// Buy or sell.
    pub side: QuoteSide,
    /// Limit price.
    pub price: Decimal,
    /// Quantity in base asset.
    pub qty: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteSide {
    Buy,
    Sell,
}

/// A value riding on an edge during evaluation. Exactly one variant
/// per `PortType`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Number(Decimal),
    Bool(bool),
    Unit,
    String(String),
    KillLevel(u8),
    /// Phase 2 — base strategy tag.
    StrategyKind(String),
    /// Phase 2 — pair-class tag.
    PairClass(String),
    /// Phase 4 — a quote bundle: the set of buy + sell levels the
    /// graph wants placed this tick. Consumed by `Out.Quotes`.
    Quotes(Vec<GraphQuote>),
    /// A source that had no observation this tick. Propagates through
    /// transforms as "hold last good" or "pass-through" depending on
    /// the node; sinks fall back to their neutral output after the
    /// configured `stale_hold_ms`.
    Missing,
}

impl Value {
    pub fn port_type(&self) -> PortType {
        match self {
            Value::Number(_) | Value::Missing => PortType::Number,
            Value::Bool(_) => PortType::Bool,
            Value::Unit => PortType::Unit,
            Value::String(_) => PortType::String,
            Value::KillLevel(_) => PortType::KillLevel,
            Value::StrategyKind(_) => PortType::StrategyKind,
            Value::PairClass(_) => PortType::PairClass,
            Value::Quotes(_) => PortType::Quotes,
        }
    }

    pub fn as_quotes(&self) -> Option<&Vec<GraphQuote>> {
        match self {
            Value::Quotes(q) => Some(q),
            _ => None,
        }
    }

    pub fn as_strategy_kind(&self) -> Option<&str> {
        match self {
            Value::StrategyKind(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_pair_class(&self) -> Option<&str> {
        match self {
            Value::PairClass(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Return the underlying `Decimal` when the value is a `Number`.
    /// `Missing` maps to `None` so callers can distinguish "no data
    /// this tick" from "zero".
    pub fn as_number(&self) -> Option<Decimal> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn is_missing(&self) -> bool {
        matches!(self, Value::Missing)
    }
}

/// One side of an edge — (node, port name).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortRef {
    pub node: NodeId,
    /// Matches one of the names returned by
    /// `NodeKind::{input_ports,output_ports}`.
    pub port: String,
}

/// A directed edge between a node's output and another node's input.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub from: PortRef,
    pub to: PortRef,
}

/// A typed port declaration — what a node exposes on its input or
/// output side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    pub name: String,
    pub ty: PortType,
}

impl Port {
    pub fn new(name: impl Into<String>, ty: PortType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}
