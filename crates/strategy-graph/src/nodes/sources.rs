//! Source nodes — read from engine state.
//!
//! A source node has zero input ports. The evaluator recognises this
//! shape and, instead of calling `evaluate()`, looks up each output
//! port in the per-tick `source_inputs: HashMap<(NodeId, String), Value>`
//! the engine populates at the start of each `tick()` call.
//!
//! From this crate's perspective the sources therefore carry only
//! their port declarations — no data access, no IO. The engine
//! (`mm-engine`) decides what to put in the source map. That keeps
//! `mm-strategy-graph` engine-free.
//!
//! `evaluate()` is still implemented (returns `Missing` for every
//! port) so the trait contract stays uniform; it should never be
//! reached in practice because the evaluator short-circuits
//! source nodes.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;

// ── Book.L1 ─────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct BookL1;

static BOOK_L1_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("bid_px", PortType::Number),
        Port::new("bid_qty", PortType::Number),
        Port::new("ask_px", PortType::Number),
        Port::new("ask_qty", PortType::Number),
        Port::new("mid", PortType::Number),
        Port::new("spread_bps", PortType::Number),
    ]
});
static EMPTY_INPUTS: Lazy<Vec<Port>> = Lazy::new(Vec::new);

impl NodeKind for BookL1 {
    fn kind(&self) -> &'static str {
        "Book.L1"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &BOOK_L1_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        // Never reached in practice — evaluator pre-populates from
        // source_inputs. The 6-wide Missing vector is the fall-back
        // when the engine forgot to set a port.
        Ok(vec![Value::Missing; BOOK_L1_OUTPUTS.len()])
    }
}

// Single-output source nodes share this helper since the only
// difference is the `kind()` string.
macro_rules! single_scalar_source {
    ($ty:ident, $kind_str:literal, $port_name:literal) => {
        #[derive(Debug, Default)]
        pub struct $ty;

        impl NodeKind for $ty {
            fn kind(&self) -> &'static str {
                $kind_str
            }
            fn input_ports(&self) -> &[Port] {
                &EMPTY_INPUTS
            }
            fn output_ports(&self) -> &[Port] {
                static PORTS: Lazy<Vec<Port>> =
                    Lazy::new(|| vec![Port::new($port_name, PortType::Number)]);
                &PORTS
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

single_scalar_source!(SentimentRate, "Sentiment.Rate", "value");
single_scalar_source!(SentimentScore, "Sentiment.Score", "value");
single_scalar_source!(VolatilityRealised, "Volatility.Realised", "value");
single_scalar_source!(ToxicityVpin, "Toxicity.VPIN", "value");
single_scalar_source!(MomentumOfiZ, "Momentum.OFIZ", "value");

// Phase 2 Wave B — risk layer signal sources.
single_scalar_source!(RiskMarginRatio, "Risk.MarginRatio", "value");
single_scalar_source!(RiskOtr, "Risk.OTR", "value");
single_scalar_source!(InventoryLevel, "Inventory.Level", "value");

// Phase 2 Wave C — signal + toxicity sources.
single_scalar_source!(SignalImbalance, "Signal.ImbalanceDepth", "value");
single_scalar_source!(SignalTradeFlow, "Signal.TradeFlow", "value");
single_scalar_source!(SignalMicroprice, "Signal.Microprice", "value");
single_scalar_source!(KyleLambda, "Toxicity.KyleLambda", "value");

// Phase 2 — strategy + pair-class metadata sources. Zero-input
// typed-enum outputs; the evaluator short-circuits both and the
// engine fills them from `strategy.name()` / `adaptive_tuner
// .pair_class()` on each tick.

/// `Strategy.Active` — emits which base strategy is running.
/// Lets a graph branch on `Logic.Mux` keyed by strategy kind so
/// per-strategy tuning (e.g. narrower spread on Grid, wider on
/// A-S) lives in the graph, not in config sprawl.
#[derive(Debug, Default)]
pub struct StrategyActive;

static STRATEGY_ACTIVE_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("kind", PortType::StrategyKind)]);

impl NodeKind for StrategyActive {
    fn kind(&self) -> &'static str {
        "Strategy.Active"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &STRATEGY_ACTIVE_OUTPUTS
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

/// `Regime.Detector` — emits the autotuner's current regime tag
/// as a `String`. Values: `"Quiet" | "Volatile" | "Trending" |
/// "MeanReverting"`. Pair with `Cast.StringEq` (future node) or
/// build the comparator inline as needed.
#[derive(Debug, Default)]
pub struct RegimeDetector;

static REGIME_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("regime", PortType::String)]);

impl NodeKind for RegimeDetector {
    fn kind(&self) -> &'static str {
        "Regime.Detector"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &REGIME_OUTPUTS
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

/// `PairClass.Current` — emits the classifier's current label
/// (`"major-spot"`, `"meme-spot"`, `"alt-perp"`, …).
#[derive(Debug, Default)]
pub struct PairClassCurrent;

static PAIR_CLASS_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("class", PortType::PairClass)]);

impl NodeKind for PairClassCurrent {
    fn kind(&self) -> &'static str {
        "PairClass.Current"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &PAIR_CLASS_OUTPUTS
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

// ── Epic R — Surveillance detectors ────────────────────────
//
// Source-only nodes. The engine holds the `OrderLifecycleTracker`
// + the pattern detectors and pushes their per-tick output into the
// evaluator's `source_inputs` map. Here we only declare the shape
// — a single `value: Number` port in `[0, 1]` + auxiliary per-pattern
// diagnostics the UI surfaces on the edge labels so a reviewer can
// see exactly what signals are driving the score.

/// `Surveillance.SpoofingScore` — likelihood our own order flow
/// looks like spoofing. `value ∈ [0, 1]` aggregates cancel-to-fill
/// ratio, median order lifetime, and biggest-open-vs-avg-trade size.
/// Pair with `Cast.ToBool(>=0.8)` + `Out.KillEscalate` to stand
/// down when the detector flags us.
#[derive(Debug, Default)]
pub struct SpoofingScore;

static SPOOFING_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("value", PortType::Number),
        Port::new("cancel_ratio", PortType::Number),
        Port::new("lifetime_ms", PortType::Number),
        Port::new("size_ratio", PortType::Number),
    ]
});

impl NodeKind for SpoofingScore {
    fn kind(&self) -> &'static str {
        "Surveillance.SpoofingScore"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &SPOOFING_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::Missing; 4])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_l1_declares_six_outputs_all_numbers() {
        let n = BookL1;
        assert!(n.input_ports().is_empty());
        assert_eq!(n.output_ports().len(), 6);
        assert!(n.output_ports().iter().all(|p| p.ty == PortType::Number));
    }

    #[test]
    fn single_scalar_source_has_one_output() {
        let n = SentimentRate;
        assert!(n.input_ports().is_empty());
        assert_eq!(n.output_ports().len(), 1);
        assert_eq!(n.output_ports()[0].name, "value");
    }

    #[test]
    fn strategy_active_declares_enum_output() {
        let n = StrategyActive;
        assert!(n.input_ports().is_empty());
        assert_eq!(n.output_ports().len(), 1);
        assert_eq!(n.output_ports()[0].ty, PortType::StrategyKind);
    }

    #[test]
    fn pair_class_current_declares_enum_output() {
        let n = PairClassCurrent;
        assert_eq!(n.output_ports().len(), 1);
        assert_eq!(n.output_ports()[0].ty, PortType::PairClass);
    }
}
