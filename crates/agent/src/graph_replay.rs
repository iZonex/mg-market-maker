//! GOBS-M5.2 — replay computation extracted from the inline
//! `"graph_replay"` details-topic handler so it can be unit-tested
//! without spinning up a full agent lease loop.
//!
//! The replay takes the deployment's live trace ring and a compiled
//! candidate `Evaluator`, re-runs each trace's source-node outputs
//! through the candidate, and returns a JSON payload the controller
//! forwards to the UI verbatim. The payload shape is stable:
//! frontends index into it without a schema version bump.
//!
//! Compared to M5 v1 (sink-set diff only), this stage adds per-
//! tick `diverging_kinds` — the set of node kinds whose aggregate
//! outputs differ between the deployed graph's captured trace and
//! the candidate's replayed trace. The UI uses it to highlight
//! matching nodes in both mini-canvases for side-by-side diff.
//!
//! NodeId identity is intentionally collapsed to node-KIND on the
//! diff side: the candidate's node IDs are a fresh set, so anchoring
//! on them would make every divergence "all candidate nodes are
//! new". Kind-level aggregation gives a stable anchor that
//! highlights "all `Math.Multiply` nodes diverge on tick 42" even
//! when the candidate uses three multipliers instead of two — the
//! operator then sees the glow on both canvases and knows which
//! sub-tree to inspect.

use std::collections::{BTreeMap, BTreeSet};

use mm_strategy_graph::{EvalCtx, Evaluator};
use mm_strategy_graph::trace::{ExecStatus, TickTrace};
use serde_json::Value;

/// Replay every entry in `original_traces` through `replay_ev`
/// and return the JSON payload the controller relays to the UI.
///
/// `original_traces` is expected oldest→newest so the resulting
/// `divergences` list preserves tick ordering — callers that read
/// the details-store ring (newest-first) should `.rev()` before
/// passing.
///
/// Never panics on a bad trace: a candidate that errors out of
/// `tick_with_full_trace` records an empty replay trace for that
/// tick and still participates in the divergence check.
pub fn compute_replay_payload(
    symbol: &str,
    original_traces: &[TickTrace],
    replay_ev: &mut Evaluator,
) -> Value {
    let ctx = EvalCtx::default();
    let mut divergences: Vec<Value> = Vec::new();

    for t in original_traces {
        let kind_values = t.source_kind_values();
        let src = mm_strategy_graph::evaluator::replay_source_inputs(
            replay_ev,
            &kind_values,
        );

        // `tick_with_full_trace` returns the candidate's sinks +
        // a fresh `TickTrace` we can diff against the captured
        // deployed trace at node-kind granularity. Errors fall
        // through to empty collections — a failing candidate
        // still counts as a divergence from a working deployed
        // trace, which is the correct signal for the operator.
        let (replay_sinks, replay_trace) = replay_ev
            .tick_with_full_trace(&ctx, &src)
            .unwrap_or_default();

        let replay_set: BTreeSet<String> = replay_sinks
            .iter()
            .filter_map(|a| serde_json::to_string(a).ok())
            .collect();
        let original_set: BTreeSet<String> = t
            .sinks_fired
            .iter()
            .filter_map(|a| serde_json::to_string(a).ok())
            .collect();

        let diverging_kinds = compute_diverging_kinds(t, &replay_trace);

        // A tick is divergent when the sink sets differ OR any
        // node kind produces different aggregate outputs. Pure
        // kind-level divergences matter because the downstream
        // sink may still land on the same value by luck (e.g.
        // two different multipliers → same clamped output).
        if replay_set != original_set || !diverging_kinds.is_empty() {
            divergences.push(serde_json::json!({
                "tick_num": t.tick_num,
                "tick_ms": t.tick_ms,
                "original_sinks": t.sinks_fired,
                "replay_sinks": replay_sinks,
                "diverging_kinds": diverging_kinds,
            }));
        }
    }

    let summary = if original_traces.is_empty() {
        format!("no traces for {symbol} — deployment may not have ticked yet")
    } else if divergences.is_empty() {
        format!(
            "{} tick(s) replayed · candidate matches deployed behaviour",
            original_traces.len()
        )
    } else {
        format!(
            "{} tick(s) replayed · candidate diverges on {} tick(s)",
            original_traces.len(),
            divergences.len()
        )
    };

    serde_json::json!({
        "summary": summary,
        "ticks_replayed": original_traces.len(),
        "divergence_count": divergences.len(),
        "divergences": divergences,
        "candidate_issues": Vec::<String>::new(),
    })
}

/// Aggregate every non-source node's outputs on both sides by
/// `(kind, port)` and return the set of kinds whose aggregate
/// differs. Source-kind nodes are skipped because they are
/// injected identically from the original trace via
/// `replay_source_inputs` — any "divergence" there is a
/// numerical artifact of serialisation, not strategy behaviour.
fn compute_diverging_kinds(
    deployed: &TickTrace,
    candidate: &TickTrace,
) -> Vec<String> {
    type KindPortToValues = BTreeMap<(String, String), Vec<String>>;

    fn collect(trace: &TickTrace) -> KindPortToValues {
        let mut m: KindPortToValues = BTreeMap::new();
        for n in &trace.nodes {
            if matches!(n.status, ExecStatus::Source) {
                continue;
            }
            for (port, value) in &n.outputs {
                let key = (n.kind.clone(), port.clone());
                // Serialise to a stable string — Value is not Ord
                // directly. BTreeMap keyed on `(kind, port)` so
                // order across values of the same (kind, port)
                // would itself drive false positives; sort.
                let s = serde_json::to_string(value).unwrap_or_default();
                m.entry(key).or_default().push(s);
            }
        }
        for v in m.values_mut() {
            v.sort();
        }
        m
    }

    let dep = collect(deployed);
    let can = collect(candidate);

    let mut diverging: BTreeSet<String> = BTreeSet::new();
    for (k, dv) in &dep {
        match can.get(k) {
            Some(cv) if cv == dv => {}
            _ => {
                diverging.insert(k.0.clone());
            }
        }
    }
    for k in can.keys() {
        if !dep.contains_key(k) {
            diverging.insert(k.0.clone());
        }
    }
    diverging.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_strategy_graph::trace::NodeExec;
    use mm_strategy_graph::{Graph, NodeId, SinkAction, Value as GValue};
    use rust_decimal_macros::dec;

    /// Parse a fixture JSON graph — lets the test build a valid
    /// candidate evaluator without hand-rolling every edge.
    fn build_evaluator(graph_json: &str) -> Evaluator {
        let g: Graph = serde_json::from_str(graph_json).expect("graph parses");
        Evaluator::build(&g).expect("evaluator builds")
    }

    const N1: &str = "00000000-0000-4000-8000-000000000001";
    const N2: &str = "00000000-0000-4000-8000-000000000002";
    const N3: &str = "00000000-0000-4000-8000-000000000003";
    const N4: &str = "00000000-0000-4000-8000-000000000004";

    /// Minimal graph: one constant (source) → one spread-mult sink.
    fn identity_constant_graph() -> String {
        serde_json::json!({
            "version": 1,
            "name": "t",
            "scope": { "kind": "global" },
            "nodes": [
                {
                    "id": N1,
                    "kind": "Math.Const",
                    "config": { "value": "1.5" },
                    "pos": [0, 0]
                },
                {
                    "id": N2,
                    "kind": "Out.SpreadMult",
                    "config": null,
                    "pos": [100, 0]
                }
            ],
            "edges": [
                {
                    "from": { "node": N1, "port": "value" },
                    "to":   { "node": N2, "port": "mult" }
                }
            ]
        })
        .to_string()
    }

    #[test]
    fn empty_traces_reports_no_ticks() {
        let mut ev = build_evaluator(&identity_constant_graph());
        let out = compute_replay_payload("BTCUSDT", &[], &mut ev);
        assert_eq!(out["ticks_replayed"], 0);
        assert_eq!(out["divergence_count"], 0);
        assert!(
            out["summary"].as_str().unwrap().contains("no traces"),
            "summary was {:?}",
            out["summary"]
        );
    }

    #[test]
    fn identical_graphs_produce_zero_divergences() {
        // Hand-build a trace that matches what the `identity_constant`
        // graph would have emitted.
        let n1 = NodeId::new();
        let n2 = NodeId::new();
        let trace = TickTrace {
            tick_ms: 100,
            tick_num: 1,
            graph_hash: "h1".into(),
            total_elapsed_ns: 1000,
            nodes: vec![
                NodeExec {
                    id: n1,
                    kind: "Math.Const".into(),
                    inputs: vec![],
                    outputs: vec![("value".into(), GValue::Number(dec!(1.5)))],
                    elapsed_ns: 100,
                    status: ExecStatus::Source,
                },
                NodeExec {
                    id: n2,
                    kind: "Out.SpreadMult".into(),
                    inputs: vec![("mult".into(), GValue::Number(dec!(1.5)))],
                    outputs: vec![("action".into(), GValue::Unit)],
                    elapsed_ns: 100,
                    status: ExecStatus::Ok,
                },
            ],
            sinks_fired: vec![SinkAction::SpreadMult(dec!(1.5))],
        };

        let mut ev = build_evaluator(&identity_constant_graph());
        let out = compute_replay_payload("BTCUSDT", &[trace], &mut ev);
        assert_eq!(out["ticks_replayed"], 1);
        assert_eq!(
            out["divergence_count"], 0,
            "identical candidate should match; got {out}"
        );
        assert!(out["summary"]
            .as_str()
            .unwrap()
            .contains("matches deployed behaviour"));
    }

    #[test]
    fn different_constant_flags_diverging_kind_and_sink() {
        // Build a deployed trace that captured the 1.5 constant.
        let n1 = NodeId::new();
        let n2 = NodeId::new();
        let trace = TickTrace {
            tick_ms: 100,
            tick_num: 1,
            graph_hash: "h1".into(),
            total_elapsed_ns: 1000,
            nodes: vec![
                NodeExec {
                    id: n1,
                    kind: "Math.Const".into(),
                    inputs: vec![],
                    outputs: vec![("value".into(), GValue::Number(dec!(1.5)))],
                    elapsed_ns: 100,
                    status: ExecStatus::Source,
                },
                NodeExec {
                    id: n2,
                    kind: "Out.SpreadMult".into(),
                    inputs: vec![("mult".into(), GValue::Number(dec!(1.5)))],
                    outputs: vec![("action".into(), GValue::Unit)],
                    elapsed_ns: 100,
                    status: ExecStatus::Ok,
                },
            ],
            sinks_fired: vec![SinkAction::SpreadMult(dec!(1.5))],
        };

        // Replay via a graph whose Math.Const outputs 2.0 —
        // source kind_values pulled from the trace give 1.5 on
        // the `"out"` port, BUT the candidate's Math.Const is
        // non-source after Evaluator::build resolves its
        // config — actually Math.Const is a source too so its
        // output is 1.5. The Out.SpreadMult *sink* in the
        // candidate receives 1.5 and fires 1.5 too. So this
        // test currently stays identical. Use a different shape:
        // add a Math.Mul on top so the candidate's non-source
        // path materially diverges.
        let candidate_json = serde_json::json!({
            "version": 1,
            "name": "t",
            "scope": { "kind": "global" },
            "nodes": [
                { "id": N1, "kind": "Math.Const",
                  "config": { "value": "1.5" }, "pos": [0, 0] },
                { "id": N3, "kind": "Math.Const",
                  "config": { "value": "2.0" }, "pos": [50, 0] },
                { "id": N4, "kind": "Math.Mul",
                  "config": null, "pos": [100, 0] },
                { "id": N2, "kind": "Out.SpreadMult",
                  "config": null, "pos": [200, 0] }
            ],
            "edges": [
                { "from": { "node": N1, "port": "value" },
                  "to":   { "node": N4, "port": "a" } },
                { "from": { "node": N3, "port": "value" },
                  "to":   { "node": N4, "port": "b" } },
                { "from": { "node": N4, "port": "out" },
                  "to":   { "node": N2, "port": "mult" } }
            ]
        })
        .to_string();

        let mut ev = build_evaluator(&candidate_json);
        let out = compute_replay_payload("BTCUSDT", &[trace], &mut ev);
        assert_eq!(out["ticks_replayed"], 1);
        assert_eq!(out["divergence_count"], 1, "expected divergence: {out}");
        let div = &out["divergences"][0];
        let kinds: Vec<String> = serde_json::from_value(div["diverging_kinds"].clone())
            .expect("diverging_kinds is a string list");
        assert!(
            kinds.iter().any(|k| k == "Math.Mul"),
            "expected Math.Mul in diverging_kinds, got {kinds:?}"
        );
    }
}
