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
pub mod templates;
pub mod trace;
pub mod types;

pub use evaluator::{Evaluator, SinkAction};
pub use graph::{Graph, Node as GraphNode, Scope, ValidationError, CURRENT_SCHEMA_VERSION};
pub use node::{
    ConfigEnumOption, ConfigField, ConfigWidget, EvalCtx, NodeKind, NodeState,
};
pub use storage::{DeployRecord, GraphStore};
pub use trace::{ExecStatus, GraphAnalysis, NodeExec, TickTrace};
pub use types::{
    AtomicBundleSpec, Edge, GraphQuote, NodeId, Port, PortRef, PortType, QuoteSide, Value,
    VenueQuote,
};

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

    /// Phase IV — `Out.VenueQuotesIf` gates `SinkAction::VenueQuotes`
    /// on the boolean trigger input. False → no sink. True → the
    /// quotes bundle fires as a `VenueQuotes` action.
    #[test]
    fn venue_quotes_if_gates_on_trigger() {
        use crate::types::{GraphQuote, QuoteSide};
        let quotes_src = NodeId::new();
        let trigger_src = NodeId::new();
        let sink = NodeId::new();
        let mut g = Graph::empty("vq-if", GScope::Symbol("BTCUSDT".into()));
        g.nodes.push(graph::Node {
            id: quotes_src,
            kind: "Strategy.Avellaneda".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: trigger_src,
            kind: "Sentiment.Rate".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        // We abuse Cast.ToBool to convert the sentiment rate into
        // the trigger bool — same pattern the real xemm template
        // uses with Trade.OwnFill.fired.
        let cast = NodeId::new();
        g.nodes.push(graph::Node {
            id: cast,
            kind: "Cast.ToBool".into(),
            config: serde_json::json!({ "threshold": "1", "cmp": "ge" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink,
            kind: "Out.VenueQuotesIf".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        // Graph validator requires at least one `Out.SpreadMult`
        // sink as a fail-closed default. Wire a dummy so the
        // validation passes without influencing the test.
        let baseline_mult = NodeId::new();
        let baseline_sink = NodeId::new();
        g.nodes.push(graph::Node {
            id: baseline_mult,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "1" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: baseline_sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: baseline_mult, port: "value".into() },
            to: PortRef { node: baseline_sink, port: "mult".into() },
        });
        g.edges.push(Edge {
            from: PortRef { node: trigger_src, port: "value".into() },
            to: PortRef { node: cast, port: "x".into() },
        });
        g.edges.push(Edge {
            from: PortRef { node: cast, port: "out".into() },
            to: PortRef { node: sink, port: "trigger".into() },
        });
        g.edges.push(Edge {
            from: PortRef { node: quotes_src, port: "quotes".into() },
            to: PortRef { node: sink, port: "quotes".into() },
        });

        let mut ev = Evaluator::build(&g).expect("valid");
        let sample_quotes = vec![GraphQuote {
            side: QuoteSide::Sell,
            price: dec!(76_100),
            qty: dec!(0.01),
        }];
        let mut src: HashMap<(NodeId, String), Value> = HashMap::new();
        src.insert(
            (quotes_src, "quotes".into()),
            Value::Quotes(sample_quotes.clone()),
        );

        // Helper: does the sink-action list contain a Quotes
        // (not SpreadMult — the baseline sink always fires)?
        let quotes_fired = |sinks: &[SinkAction]| -> Option<Vec<GraphQuote>> {
            sinks.iter().find_map(|a| match a {
                SinkAction::Quotes(qs) => Some(qs.clone()),
                _ => None,
            })
        };

        // Rate 0 → Cast.ToBool(≥1) = false → VenueQuotesIf
        // suppressed. Baseline SpreadMult still fires — that's
        // fine; we only care about the gated quotes here.
        src.insert((trigger_src, "value".into()), Value::Number(dec!(0)));
        let cold = ev.tick(&EvalCtx::default(), &src).unwrap();
        assert!(
            quotes_fired(&cold).is_none(),
            "trigger=false must suppress the VenueQuotesIf sink; got {cold:?}"
        );

        // Rate 2 → Cast.ToBool(≥1) = true → VenueQuotesIf fires
        // as Quotes (strategy source emits plain Quotes, not
        // VenueQuotes).
        src.insert((trigger_src, "value".into()), Value::Number(dec!(2)));
        let hot = ev.tick(&EvalCtx::default(), &src).unwrap();
        assert_eq!(
            quotes_fired(&hot).as_deref(),
            Some(sample_quotes.as_slice()),
            "trigger=true must propagate the upstream quotes; got {hot:?}"
        );
    }

    /// M1-GOBS — `tick_with_full_trace` emits per-node exec records
    /// with declared inputs + produced outputs + a non-zero elapsed
    /// budget, and the sinks_fired vector mirrors the returned
    /// SinkActions.
    #[test]
    fn tick_with_full_trace_captures_every_node_and_sinks() {
        let add_id = NodeId::new();
        let sink_id = NodeId::new();
        let mut g = Graph::empty("full-trace", GScope::Symbol("BTCUSDT".into()));
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
            from: PortRef { node: add_id, port: "out".into() },
            to: PortRef { node: sink_id, port: "mult".into() },
        });

        let mut ev = Evaluator::build(&g).expect("valid");
        let mut src: HashMap<(NodeId, String), Value> = HashMap::new();
        src.insert((add_id, "a".into()), Value::Number(dec!(3)));
        src.insert((add_id, "b".into()), Value::Number(dec!(4)));
        let (sinks, trace) = ev
            .tick_with_full_trace(&EvalCtx::default(), &src)
            .expect("ok");

        assert_eq!(sinks, vec![SinkAction::SpreadMult(dec!(7))]);
        assert_eq!(trace.nodes.len(), 2, "one NodeExec per node");
        assert_eq!(trace.sinks_fired, sinks);
        let add = trace.nodes.iter().find(|n| n.id == add_id).unwrap();
        assert_eq!(add.kind, "Math.Add");
        assert_eq!(add.outputs.len(), 1);
        assert_eq!(add.outputs[0].0, "out");
        assert_eq!(add.outputs[0].1, Value::Number(dec!(7)));
        let snk = trace.nodes.iter().find(|n| n.id == sink_id).unwrap();
        assert_eq!(snk.kind, "Out.SpreadMult");
        // Sink takes the Math.Add output on its `mult` input.
        assert!(
            snk.inputs.iter().any(|(p, v)| p == "mult" && *v == Value::Number(dec!(7))),
            "sink must record the upstream value on its `mult` port",
        );
    }

    /// M6-GOBS — the engine's detector-gating derivation keys
    /// off `analyze().required_sources`. Encode the exact
    /// predicate the engine uses so a catalog rename would fail
    /// this test instead of silently leaving the gate open
    /// forever (or closed forever on a template that needs it).
    #[test]
    fn analyze_exposes_gate_keys_for_detector_templates() {
        let detector = crate::templates::load("rug-detector-composite")
            .expect("rug-detector template available")
            .expect("rug-detector parses cleanly");
        let ev = Evaluator::build(&detector).expect("valid graph");
        let a = ev.analyze(detector.content_hash());
        let manip_gate = a
            .required_sources
            .iter()
            .any(|k| k == "Surveillance.ManipulationScore" || k == "Surveillance.RugScore");
        assert!(
            manip_gate,
            "rug-detector references Surveillance.RugScore — gate must open; got required_sources={:?}",
            a.required_sources
        );

        // Plain avellaneda-via-graph has no Surveillance source
        // and no Onchain source — both gates stay closed.
        let plain = crate::templates::load("avellaneda-via-graph")
            .expect("avellaneda template available")
            .expect("avellaneda parses cleanly");
        let ev = Evaluator::build(&plain).expect("valid graph");
        let a = ev.analyze(plain.content_hash());
        let manip_gate = a
            .required_sources
            .iter()
            .any(|k| k == "Surveillance.ManipulationScore" || k == "Surveillance.RugScore");
        let onchain_gate = a
            .required_sources
            .iter()
            .any(|k| k.starts_with("Onchain."));
        assert!(
            !manip_gate,
            "avellaneda-via-graph should not trip the manipulation gate; got {:?}",
            a.required_sources
        );
        assert!(
            !onchain_gate,
            "avellaneda-via-graph should not trip the onchain gate; got {:?}",
            a.required_sources
        );
    }

    /// M1-GOBS — `Evaluator::analyze` returns required_sources,
    /// dead_nodes, unconsumed_outputs for a graph that has a
    /// dead branch (Math.Add node unreachable from any sink).
    #[test]
    fn analyze_flags_dead_branch_and_unconsumed_outputs() {
        let rate = NodeId::new();
        let cast = NodeId::new();
        let sink = NodeId::new();
        let dead_add = NodeId::new();
        let mut g = Graph::empty("dead-branch", GScope::Global);
        g.nodes.push(graph::Node {
            id: rate,
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
        g.nodes.push(graph::Node {
            id: dead_add,
            kind: "Math.Add".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        // Rate → Cast → nothing connects Cast.out to the sink.
        // Instead wire Rate.value straight into the sink's mult port
        // so validation passes (Sentiment.Rate is a Number-typed
        // source, and `Out.SpreadMult.mult` is Number-typed).
        g.edges.push(Edge {
            from: PortRef { node: rate, port: "value".into() },
            to: PortRef { node: cast, port: "x".into() },
        });
        g.edges.push(Edge {
            from: PortRef { node: rate, port: "value".into() },
            to: PortRef { node: sink, port: "mult".into() },
        });

        let ev = Evaluator::build(&g).expect("valid");
        let a = ev.analyze("abc123");

        assert_eq!(a.graph_hash, "abc123");
        // Rate is a required source (used by both Cast and sink).
        assert!(a.required_sources.iter().any(|k| k == "Sentiment.Rate"));
        // Math.Add has no inputs (it's actually treated as a source
        // with port ports when the catalog declares them, but
        // either way it has no path to the sink).
        assert!(
            a.dead_nodes.contains(&dead_add) || a.dead_nodes.contains(&cast),
            "unreachable nodes should surface in dead_nodes: {:?}",
            a.dead_nodes,
        );
        // Cast.out has no consumer — the dead branch's terminal
        // output should show up as unconsumed.
        assert!(
            a.unconsumed_outputs
                .iter()
                .any(|(id, _)| *id == cast || *id == dead_add),
            "unconsumed outputs should include cast or dead_add",
        );
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

    /// Sprint 5 — unknown config field triggers rejection.
    #[test]
    fn rejects_unknown_config_field() {
        let cast = NodeId::new();
        let sink = NodeId::new();
        let mut g = Graph::empty("bad-cfg", GScope::Global);
        g.nodes.push(graph::Node {
            id: cast,
            kind: "Cast.ToBool".into(),
            config: serde_json::json!({ "threshold": "3", "typo_field": "oops" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        let err = Evaluator::build(&g).expect_err("should reject");
        assert!(matches!(err, ValidationError::UnknownConfigField { .. }), "got {err:?}");
    }

    /// Sprint 5 — wrong-type config field (bool where number expected).
    #[test]
    fn rejects_wrong_type_config_field() {
        let cast = NodeId::new();
        let sink = NodeId::new();
        let mut g = Graph::empty("wrong-type", GScope::Global);
        g.nodes.push(graph::Node {
            id: cast,
            kind: "Cast.ToBool".into(),
            config: serde_json::json!({ "threshold": true }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        let err = Evaluator::build(&g).expect_err("should reject");
        assert!(matches!(err, ValidationError::InvalidConfigFieldType { .. }), "got {err:?}");
    }

    /// Sprint 5 — enum value outside allowed options.
    #[test]
    fn rejects_invalid_enum_value() {
        let cast = NodeId::new();
        let sink = NodeId::new();
        let mut g = Graph::empty("bad-enum", GScope::Global);
        g.nodes.push(graph::Node {
            id: cast,
            kind: "Cast.ToBool".into(),
            config: serde_json::json!({ "cmp": "does-not-exist" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        let err = Evaluator::build(&g).expect_err("should reject");
        assert!(matches!(err, ValidationError::InvalidConfigEnumValue { .. }), "got {err:?}");
    }

    /// Sprint 5b — unknown venue in Book.L1 config rejects.
    #[test]
    fn rejects_unknown_venue_in_book_l1() {
        let mut g = Graph::empty("bad-venue", GScope::Global);
        g.nodes.push(graph::Node {
            id: NodeId::new(),
            kind: "Book.L1".into(),
            config: serde_json::json!({ "venue": "notareal" }),
            pos: (0.0, 0.0),
        });
        let err = g
            .validate_venues(["binance", "bybit"])
            .expect_err("rejects");
        assert!(matches!(err, ValidationError::UnknownVenue { .. }), "got {err:?}");
    }

    /// Sprint 5b — BasisArb's spot_venue + perp_venue both checked.
    #[test]
    fn rejects_unknown_venue_in_basis_arb() {
        let mut g = Graph::empty("bad-basis", GScope::Global);
        g.nodes.push(graph::Node {
            id: NodeId::new(),
            kind: "Strategy.BasisArb".into(),
            config: serde_json::json!({
                "spot_venue": "binance",
                "perp_venue": "madeupexchange",
                "symbol": "BTCUSDT",
            }),
            pos: (0.0, 0.0),
        });
        let err = g
            .validate_venues(["binance", "bybit"])
            .expect_err("rejects perp_venue");
        match err {
            ValidationError::UnknownVenue { field, venue, .. } => {
                assert_eq!(field, "perp_venue");
                assert_eq!(venue, "madeupexchange");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// Sprint 5b — configured venues pass case-insensitively.
    #[test]
    fn accepts_configured_venue_case_insensitive() {
        let mut g = Graph::empty("ok-venue", GScope::Global);
        g.nodes.push(graph::Node {
            id: NodeId::new(),
            kind: "Book.L1".into(),
            config: serde_json::json!({ "venue": "Binance" }),
            pos: (0.0, 0.0),
        });
        g.validate_venues(["binance"]).expect("case-insensitive match");
    }

    /// Sprint 5b — empty / missing venue string skipped.
    #[test]
    fn skips_empty_venue_string() {
        let mut g = Graph::empty("empty-venue", GScope::Global);
        g.nodes.push(graph::Node {
            id: NodeId::new(),
            kind: "Book.L1".into(),
            config: serde_json::json!({ "venue": "" }),
            pos: (0.0, 0.0),
        });
        g.validate_venues::<[&str; 0], &str>([]).expect("empty venue ignored");
    }

    /// Sprint 5 — clean config that matches schema passes.
    #[test]
    fn accepts_valid_config() {
        let cast = NodeId::new();
        let sink = NodeId::new();
        let mut g = Graph::empty("good-cfg", GScope::Global);
        g.nodes.push(graph::Node {
            id: cast,
            kind: "Cast.ToBool".into(),
            config: serde_json::json!({ "threshold": "5.0", "cmp": "gt" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        Evaluator::build(&g).expect("clean config passes validation");
    }

    /// GR-2 — `Out.KillEscalate` with a `venue` config attaches
    /// the string to the emitted `SinkAction::KillEscalate`.
    /// Engine-side filtering happens in `mm-engine`; here we
    /// only prove the evaluator round-trips the config value.
    #[test]
    fn kill_escalate_sink_propagates_venue_config() {
        let trigger_src = NodeId::new();
        let level_src = NodeId::new();
        let kill_id = NodeId::new();
        let mult_sink = NodeId::new();
        let mut g = Graph::empty("gr-2", GScope::Global);
        // A pair of Math.Const inputs so the graph has no
        // free edges — KillEscalate gets a `trigger`, a
        // `level`, and nothing for `reason`.
        g.nodes.push(graph::Node {
            id: trigger_src,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "1" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: level_src,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "3" }),
            pos: (0.0, 0.0),
        });
        let to_bool = NodeId::new();
        g.nodes.push(graph::Node {
            id: to_bool,
            kind: "Cast.ToBool".into(),
            config: serde_json::json!({ "threshold": "0.5", "cmp": "ge" }),
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: trigger_src, port: "value".into() },
            to: PortRef { node: to_bool, port: "x".into() },
        });
        g.nodes.push(graph::Node {
            id: kill_id,
            kind: "Out.KillEscalate".into(),
            config: serde_json::json!({ "venue": "binance" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: mult_sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: to_bool, port: "out".into() },
            to: PortRef { node: kill_id, port: "trigger".into() },
        });
        // SpreadMult sink is required for validation — feed a
        // Math.Const(1) so nothing widens.
        let passthrough = NodeId::new();
        g.nodes.push(graph::Node {
            id: passthrough,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "1" }),
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: passthrough, port: "value".into() },
            to: PortRef { node: mult_sink, port: "mult".into() },
        });

        let mut ev = Evaluator::build(&g).expect("valid graph");
        // Supply a KillLevel for the `level` input port on the
        // kill sink — without an edge, the evaluator reads
        // `source_inputs`.
        let mut src: HashMap<(NodeId, String), Value> = HashMap::new();
        src.insert((kill_id, "level".into()), Value::KillLevel(3));
        let actions = ev.tick(&EvalCtx::default(), &src).expect("eval ok");
        let kill = actions
            .iter()
            .find(|a| matches!(a, SinkAction::KillEscalate { .. }))
            .expect("kill sink fired");
        match kill {
            SinkAction::KillEscalate { level, venue, .. } => {
                assert_eq!(*level, 3);
                assert_eq!(venue.as_deref(), Some("binance"));
            }
            _ => unreachable!(),
        }
    }

    /// Empty venue string behaves the same as omitted — engine
    /// treats it as a global kill. Guards against operators
    /// leaving the field blank in the UI and getting a surprise
    /// mismatch against `""`.
    #[test]
    fn kill_escalate_empty_venue_string_is_none() {
        let trigger_src = NodeId::new();
        let to_bool = NodeId::new();
        let kill_id = NodeId::new();
        let mult_sink = NodeId::new();
        let mut g = Graph::empty("gr-2-empty", GScope::Global);
        g.nodes.push(graph::Node {
            id: trigger_src,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "1" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: to_bool,
            kind: "Cast.ToBool".into(),
            config: serde_json::json!({ "threshold": "0.5", "cmp": "ge" }),
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: trigger_src, port: "value".into() },
            to: PortRef { node: to_bool, port: "x".into() },
        });
        g.nodes.push(graph::Node {
            id: kill_id,
            kind: "Out.KillEscalate".into(),
            config: serde_json::json!({ "venue": "" }),
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: to_bool, port: "out".into() },
            to: PortRef { node: kill_id, port: "trigger".into() },
        });
        let passthrough = NodeId::new();
        g.nodes.push(graph::Node {
            id: passthrough,
            kind: "Math.Const".into(),
            config: serde_json::json!({ "value": "1" }),
            pos: (0.0, 0.0),
        });
        g.nodes.push(graph::Node {
            id: mult_sink,
            kind: "Out.SpreadMult".into(),
            config: serde_json::Value::Null,
            pos: (0.0, 0.0),
        });
        g.edges.push(Edge {
            from: PortRef { node: passthrough, port: "value".into() },
            to: PortRef { node: mult_sink, port: "mult".into() },
        });

        let mut ev = Evaluator::build(&g).expect("valid graph");
        let mut src: HashMap<(NodeId, String), Value> = HashMap::new();
        src.insert((kill_id, "level".into()), Value::KillLevel(2));
        let actions = ev.tick(&EvalCtx::default(), &src).expect("eval ok");
        let kill = actions
            .iter()
            .find(|a| matches!(a, SinkAction::KillEscalate { .. }))
            .expect("kill sink fired");
        match kill {
            SinkAction::KillEscalate { venue, .. } => assert!(venue.is_none()),
            _ => unreachable!(),
        }
    }
}
