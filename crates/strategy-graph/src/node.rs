//! `NodeKind` trait + per-node state container.
//!
//! Implementations live in the `nodes/` module as small structs that
//! hold their config (parsed from `Node.config` JSON) and expose
//! `evaluate`. State that must survive across ticks (EWMA accumulators,
//! cooldown timers) goes in a `NodeState` slot keyed by `NodeId` that
//! the evaluator owns.

use crate::types::{Port, Value};
use anyhow::Result;
use std::any::Any;
use std::collections::HashMap;

/// Free-form scratch space a node owns across ticks. The evaluator
/// threads `&mut NodeState` through every call; a node stashes its
/// own state struct with `get_or_insert_default`.
///
/// `Send + Sync` bound so the enclosing `Evaluator` can live inside
/// a `MarketMakerEngine` that's moved into a `tokio::spawn` task.
#[derive(Default)]
pub struct NodeState(Option<Box<dyn Any + Send + Sync>>);

impl std::fmt::Debug for NodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeState")
            .field("has_state", &self.0.is_some())
            .finish()
    }
}

impl NodeState {
    /// Fetch (or initialise) the node-local state. Caller's type
    /// parameter must be consistent across every call on the same
    /// `NodeState` — a type mismatch is a programming bug and
    /// returns `None`.
    pub fn get_or_insert_default<T: Default + Send + Sync + 'static>(&mut self) -> &mut T {
        if self.0.is_none() {
            self.0 = Some(Box::new(T::default()));
        }
        self.0
            .as_mut()
            .expect("just initialised")
            .downcast_mut::<T>()
            .expect("stable type parameter per node")
    }

    /// Read-only peek at the typed state, `None` if not yet initialised
    /// or if the type doesn't match.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.0.as_ref().and_then(|b| b.downcast_ref::<T>())
    }

    /// Forget the stored state. Used on graph swap so a new deploy
    /// starts from a clean slate.
    pub fn clear(&mut self) {
        self.0 = None;
    }
}

/// Per-tick execution context — things a node can read that are not
/// graph-local. Stays empty for MVP since every node either reads its
/// inputs or its own state; source nodes will later get a richer
/// context (engine state accessors) and this is where those accessors
/// will live.
#[derive(Debug, Default, Clone, Copy)]
pub struct EvalCtx {
    /// Logical tick timestamp — millisecond epoch. Populated by the
    /// evaluator so cooldown / staleness logic has one source of truth.
    pub now_ms: i64,
}

/// A concrete node kind. Implementors are small structs with their
/// parsed config.
pub trait NodeKind: std::fmt::Debug + Send + Sync {
    /// The catalog key. MUST be stable across versions — JSON graphs
    /// reference nodes by this string.
    fn kind(&self) -> &'static str;

    /// Declared input ports. Order defines the positional argument
    /// layout the evaluator passes into `evaluate`.
    fn input_ports(&self) -> &[Port];

    /// Declared output ports. Same order as produced `Vec<Value>` in
    /// `evaluate`.
    fn output_ports(&self) -> &[Port];

    /// Pure-ish function of `inputs + config + state` → outputs.
    /// Length of the returned vector must equal `output_ports().len()`;
    /// order must match.
    fn evaluate(
        &self,
        ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>>;

    /// Restricted (pentest-only) node flag. Catalog default is `false`.
    /// Predatory node kinds return `true`; validation refuses to load
    /// a graph containing any such node unless
    /// `MM_ALLOW_RESTRICTED=yes-pentest-mode` is set at startup.
    fn restricted(&self) -> bool {
        false
    }

    /// Declared config fields. Empty vec by default means "no config
    /// — the node takes only its inputs". When a node overrides this,
    /// the catalog API ships the schema to the frontend, which renders
    /// a form automatically — no per-kind `if kind === ...` branch on
    /// the UI side. See [`ConfigField`].
    fn config_schema(&self) -> Vec<ConfigField> {
        Vec::new()
    }
}

/// One field on a node's config blob. Schema-driven: adding a field
/// is one struct-literal + one default; the UI form gets it for free.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigField {
    /// JSON key the engine reads out of `node.config`.
    pub name: &'static str,
    /// Human-facing label on the form.
    pub label: &'static str,
    /// One-line hint rendered under the input. Kept short so the
    /// right-hand config panel doesn't outgrow its width.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
    /// Default value the form prefills with. Stored as a JSON value
    /// so numbers / strings / bools all use one shape.
    pub default: serde_json::Value,
    /// Widget hint for the form.
    pub widget: ConfigWidget,
}

/// Tag the frontend uses to pick a form widget. New widget types go
/// here — never grow the set by encoding hints into strings.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConfigWidget {
    /// Plain number / decimal input. `step` hints the HTML
    /// `step=` attribute for a rocker control.
    Number {
        #[serde(skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
    },
    /// Integer counterpart — JS `<input type="number" step="1">`.
    Integer {
        #[serde(skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
    },
    /// Free-form text.
    Text,
    /// Boolean checkbox / switch.
    Bool,
    /// Dropdown with a finite set of string options. Every option
    /// also carries a human label so we can show "≥" instead of
    /// `"ge"` while the graph JSON still stores the compact form.
    Enum { options: Vec<ConfigEnumOption> },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigEnumOption {
    pub value: &'static str,
    pub label: &'static str,
}

/// Cached per-node eval output keyed by output port name. Lives on the
/// evaluator so downstream nodes pull their inputs from here.
pub type NodeOutputs = HashMap<String, Value>;
