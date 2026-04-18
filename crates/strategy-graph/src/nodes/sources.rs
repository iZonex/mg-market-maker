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
}
