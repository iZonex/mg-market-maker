//! Epic H — visual strategy builder backend.
//!
//! Typed DAG of nodes the engine evaluates on every tick. See
//! `docs/research/visual-strategy-builder.md` for the full
//! architecture.
//!
//! Public API surface for Phase 1:
//!
//!   [`Graph`] — the serialisable DAG (nodes, edges, scope, version)
//!   [`Evaluator`] — compiled, per-tick evaluator
//!   [`SinkAction`] — engine-side action produced by a sink firing
//!   [`catalog`] — closed node catalog (build, shape, kinds)
//!
//! Engine integration happens in `mm-engine` (Step 4 of the phase);
//! this crate holds the graph + evaluator + catalog only.

pub mod catalog;
pub mod evaluator;
pub mod graph;
pub mod node;
pub mod nodes;
pub mod storage;
pub mod types;

pub use evaluator::{Evaluator, SinkAction};
pub use graph::{Graph, Node as GraphNode, Scope, ValidationError, CURRENT_SCHEMA_VERSION};
pub use node::{EvalCtx, NodeKind, NodeState};
pub use storage::{DeployRecord, GraphStore};
pub use types::{Edge, NodeId, Port, PortRef, PortType, Value};

#[cfg(test)]
mod integration_tests {
    use super::*;
    use graph::Scope as GScope;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    /// Build a trivial graph:
    ///   source(a=3) ─┐
    ///                ├─ Math.Add ─ Out.SpreadMult
    ///   source(b=4) ─┘
    /// Expected: sink harvests SpreadMult(7).
    #[test]
    fn end_to_end_add_then_spread_mult() {
        let add_id = NodeId::new();
        let sink_id = NodeId::new();

        let mut g = Graph::empty("t1", GScope::Symbol("BTCUSDT".into()));
        g.nodes.push(graph::Node {
            id: add_id,
            kind: "Math.Add".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink_id,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef {
                node: add_id,
                port: "out".into(),
            },
            to: PortRef {
                node: sink_id,
                port: "mult".into(),
            },
        });

        let mut ev = Evaluator::build(&g).expect("valid graph");
        // Supply Math.Add's unconnected inputs as source values.
        let mut src: HashMap<(NodeId, String), Value> = HashMap::new();
        src.insert((add_id, "a".into()), Value::Number(dec!(3)));
        src.insert((add_id, "b".into()), Value::Number(dec!(4)));
        let actions = ev.tick(&EvalCtx::default(), &src).expect("eval ok");
        assert_eq!(actions, vec![SinkAction::SpreadMult(dec!(7))]);
    }

    /// Graph with no SpreadMult must fail validation — fail-closed
    /// default: operator can't silently delete the widening.
    #[test]
    fn rejects_graph_with_no_spread_mult_sink() {
        let mut g = Graph::empty("t2", GScope::Global);
        g.nodes.push(graph::Node {
            id: NodeId::new(),
            kind: "Math.Add".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        let err = Evaluator::build(&g).expect_err("should reject");
        assert!(matches!(err, ValidationError::NoSpreadMultSink));
    }

    #[test]
    fn rejects_unknown_kind() {
        let mut g = Graph::empty("t3", GScope::Global);
        g.nodes.push(graph::Node {
            id: NodeId::new(),
            kind: "Math.Divide.NotYet".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: NodeId::new(),
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        let err = Evaluator::build(&g).expect_err("should reject");
        assert!(matches!(err, ValidationError::UnknownKind(_)));
    }

    #[test]
    fn rejects_port_type_mismatch() {
        // Wire Out.KillEscalate's Bool-typed `trigger` port from a
        // Math.Add Number output — mismatch.
        let add_id = NodeId::new();
        let kill_id = NodeId::new();
        let sink_id = NodeId::new();
        let mut g = Graph::empty("t4", GScope::Global);
        g.nodes.push(graph::Node {
            id: add_id,
            kind: "Math.Add".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: kill_id,
            kind: "Out.KillEscalate".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink_id,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef {
                node: add_id,
                port: "out".into(),
            },
            to: PortRef {
                node: kill_id,
                port: "trigger".into(),
            },
        });
        let err = Evaluator::build(&g).expect_err("should reject");
        assert!(matches!(err, ValidationError::PortTypeMismatch { .. }));
    }

    #[test]
    fn rejects_cycle() {
        let a = NodeId::new();
        let b = NodeId::new();
        let sink = NodeId::new();
        let mut g = Graph::empty("t5", GScope::Global);
        for (id, kind) in [(a, "Math.Add"), (b, "Math.Add"), (sink, "Out.SpreadMult")]
        {
            g.nodes.push(graph::Node {
                id,
                kind: kind.into(),
                config: serde_json::Value::Null,
                pos: (0.0, 0.0),
            });
        }
        // a.out -> b.a, b.out -> a.a  — classic 2-node cycle.
        g.edges.push(Edge {
            from: PortRef {
                node: a,
                port: "out".into(),
            },
            to: PortRef {
                node: b,
                port: "a".into(),
            },
        });
        g.edges.push(Edge {
            from: PortRef {
                node: b,
                port: "out".into(),
            },
            to: PortRef {
                node: a,
                port: "a".into(),
            },
        });
        let err = Evaluator::build(&g).expect_err("should reject");
        assert!(matches!(err, ValidationError::Cycle(_)));
    }

    /// Real-world shape: sentiment rate + Cast.ToBool + Logic.Mux →
    /// SpreadMult. When rate >= 3, multiplier = 1.5; otherwise 1.0.
    /// Exercises a source node, a configurable transform, and the
    /// sink harvest in one graph.
    #[test]
    fn sentiment_threshold_picks_widen_branch() {
        let rate_src = NodeId::new();
        let cast = NodeId::new();
        let mux = NodeId::new();
        let widen = NodeId::new();
        let baseline = NodeId::new();
        let sink = NodeId::new();

        let mut g = Graph::empty("sentiment-widen", GScope::Symbol("BTCUSDT".into()));
        g.nodes.push(graph::Node {
            id: rate_src,
            kind: "Sentiment.Rate".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: cast,
            kind: "Cast.ToBool".into(),
            config: serde_json::json!({ "threshold": "3", "cmp": "ge" }),
            pos: (0.0, 0.0),
        });
        // The two arms of the mux — constant numbers supplied via
        // source_inputs for simplicity (no Math.Const node in MVP
        // yet; the UI supplies constant nodes in Phase 2).
        g.nodes.push(graph::Node {
            id: widen,
            kind: "Math.Add".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: baseline,
            kind: "Math.Add".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: mux,
            kind: "Logic.Mux".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        // Wire it.
        g.edges.push(Edge {
            from: PortRef {
                node: rate_src,
                port: "value".into(),
            },
            to: PortRef {
                node: cast,
                port: "x".into(),
            },
        });
        g.edges.push(Edge {
            from: PortRef {
                node: cast,
                port: "out".into(),
            },
            to: PortRef {
                node: mux,
                port: "cond".into(),
            },
        });
        g.edges.push(Edge {
            from: PortRef {
                node: widen,
                port: "out".into(),
            },
            to: PortRef {
                node: mux,
                port: "then".into(),
            },
        });
        g.edges.push(Edge {
            from: PortRef {
                node: baseline,
                port: "out".into(),
            },
            to: PortRef {
                node: mux,
                port: "else".into(),
            },
        });
        g.edges.push(Edge {
            from: PortRef {
                node: mux,
                port: "out".into(),
            },
            to: PortRef {
                node: sink,
                port: "mult".into(),
            },
        });

        let mut ev = Evaluator::build(&g).expect("valid");
        // Load the then-arm with 0.5 + 1.0 = 1.5, else-arm with
        // 0.5 + 0.5 = 1.0. Rate set to trip the threshold.
        let mut src: HashMap<(NodeId, String), Value> = HashMap::new();
        src.insert((widen, "a".into()), Value::Number(dec!(0.5)));
        src.insert((widen, "b".into()), Value::Number(dec!(1.0)));
        src.insert((baseline, "a".into()), Value::Number(dec!(0.5)));
        src.insert((baseline, "b".into()), Value::Number(dec!(0.5)));

        // Rate 5.0 >= 3 → mux picks then → 1.5.
        src.insert((rate_src, "value".into()), Value::Number(dec!(5)));
        let hot = ev.tick(&EvalCtx::default(), &src).unwrap();
        assert_eq!(hot, vec![SinkAction::SpreadMult(dec!(1.5))]);

        // Rate 1.0 < 3 → mux picks else → 1.0.
        src.insert((rate_src, "value".into()), Value::Number(dec!(1)));
        let cold = ev.tick(&EvalCtx::default(), &src).unwrap();
        assert_eq!(cold, vec![SinkAction::SpreadMult(dec!(1))]);
    }

    #[test]
    fn content_hash_is_stable_across_identical_graphs() {
        let a = Graph::empty("same", GScope::Global);
        let b = Graph::empty("same", GScope::Global);
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn content_hash_changes_with_content() {
        let a = Graph::empty("x", GScope::Global);
        let b = Graph::empty("y", GScope::Global);
        assert_ne!(a.content_hash(), b.content_hash());
    }
}
