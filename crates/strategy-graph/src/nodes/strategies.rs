//! `Strategy.*` — composite nodes that wrap the existing hand-wired
//! strategies (Avellaneda-Stoikov, GLFT, …) as single graph nodes.
//!
//! ## Why a node instead of a trait
//!
//! The graph crate intentionally has no `mm-strategy` dependency —
//! a cycle would force every edit to the strategy side to recompile
//! the DAG engine. Instead, a `Strategy.*` node is a *source* from
//! the evaluator's point of view: zero inputs, one `quotes: Quotes`
//! output. The engine's source marshaller spots the kind, calls the
//! corresponding `Strategy::compute_quotes()` on the real strategy
//! instance it already keeps, converts the resulting `Vec<QuotePair>`
//! into `Vec<GraphQuote>`, and injects the value into the evaluator's
//! `source_inputs` map keyed by `(node_id, "quotes")`.
//!
//! The node's `evaluate()` is therefore a stub — source overlay
//! always replaces the `Missing` default. Keeping the stub here (as
//! opposed to absent) means the catalog shape lookup still works
//! during `Evaluator::build` + `content_hash`, without the engine
//! being the only file that knows these nodes exist.
//!
//! ## Knobs
//!
//! The first revision exposes no config on the strategy nodes — the
//! engine uses its compiled `MarketMakerConfig` (gamma, kappa, sigma,
//! num_levels, …) as-is. A later revision will let a node override
//! a subset of those fields so the same Avellaneda can run with
//! different γ under two branches of a `Quote.Mux`.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;

static QUOTES_OUT: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("quotes", PortType::Quotes)]);

macro_rules! strategy_node {
    ($struct_name:ident, $kind_str:literal) => {
        /// Phase 4 composite wrapper — see module docs. The
        /// `evaluate()` default of `Missing` is replaced by the
        /// engine's source-overlay pass before the value ever
        /// reaches a downstream node.
        #[derive(Debug, Default)]
        pub struct $struct_name;

        impl NodeKind for $struct_name {
            fn kind(&self) -> &'static str {
                $kind_str
            }
            fn input_ports(&self) -> &[Port] {
                &[]
            }
            fn output_ports(&self) -> &[Port] {
                &QUOTES_OUT
            }
            fn evaluate(
                &self,
                _ctx: &EvalCtx,
                _inputs: &[Value],
                _state: &mut NodeState,
            ) -> Result<Vec<Value>> {
                Ok(vec![Value::Missing])
            }
        }
    };
}

strategy_node!(Avellaneda, "Strategy.Avellaneda");
strategy_node!(Glft, "Strategy.GLFT");
strategy_node!(Grid, "Strategy.Grid");
strategy_node!(Basis, "Strategy.Basis");
strategy_node!(CrossExchange, "Strategy.CrossExchange");

// ─── Epic R — exploit strategies (pentest-only) ──────────────
//
// These deliberately reproduce manipulative patterns for internal
// red-team testing against the user's own exchange. Every one
// overrides `restricted() -> true` so
// `Evaluator::build` refuses to compile a graph referencing them
// unless the server was started with `MM_RESTRICTED_ALLOW=1`. The
// deploy handler also emits a `StrategyGraphDeployRejected` audit
// row on refusal so the refusal is regulator-visible.

macro_rules! pentest_strategy_node {
    ($struct_name:ident, $kind_str:literal) => {
        #[derive(Debug, Default)]
        pub struct $struct_name;

        impl NodeKind for $struct_name {
            fn kind(&self) -> &'static str {
                $kind_str
            }
            fn input_ports(&self) -> &[Port] {
                &[]
            }
            fn output_ports(&self) -> &[Port] {
                &QUOTES_OUT
            }
            fn restricted(&self) -> bool {
                true
            }
            fn evaluate(
                &self,
                _ctx: &EvalCtx,
                _inputs: &[Value],
                _state: &mut NodeState,
            ) -> Result<Vec<Value>> {
                Ok(vec![Value::Missing])
            }
        }
    };
}

pentest_strategy_node!(Spoof, "Strategy.Spoof");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::EvalCtx;

    #[test]
    fn strategy_nodes_declare_quotes_output() {
        for node in [
            &Avellaneda as &dyn NodeKind,
            &Glft,
            &Grid,
            &Basis,
            &CrossExchange,
        ] {
            assert!(node.input_ports().is_empty(), "{} must have no inputs", node.kind());
            assert_eq!(node.output_ports().len(), 1);
            assert_eq!(node.output_ports()[0].name, "quotes");
            assert_eq!(node.output_ports()[0].ty, PortType::Quotes);
        }
    }

    #[test]
    fn stub_evaluate_returns_missing() {
        let mut state = NodeState::default();
        let out = Avellaneda
            .evaluate(&EvalCtx::default(), &[], &mut state)
            .unwrap();
        assert!(matches!(out[0], Value::Missing));
    }
}
