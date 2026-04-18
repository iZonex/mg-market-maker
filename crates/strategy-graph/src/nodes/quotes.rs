//! `Quote.*` — nodes that produce `Quotes` bundles.
//!
//! Phase 4 — replaces the overlay-only pipeline with full
//! graph-authored quoting. A `Quote.Grid` builds a symmetric grid
//! around a given mid, `Quote.Mux` picks between two bundles based
//! on a boolean (the building block for meta-strategies that switch
//! strategy by regime / pair class / news level).

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{GraphQuote, Port, PortType, QuoteSide, Value};
use anyhow::Result;
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

// ─── Quote.Grid ────────────────────────────────────────────────
//
// Inputs (all Number):
//   mid        — reference mid price
//   step_bps   — half-spread at level 0, widens linearly per level
//   levels     — integer count of levels per side (floor'd, clamped)
//   size       — per-level quantity
//   skew_bps   — (optional) shifts the centre away from inventory
//
// Output: `quotes: Quotes` — a `Vec<GraphQuote>` with 2 × levels
// entries (bid + ask per level). If `mid` or `step_bps` is Missing
// the node emits `Missing` so the engine's fallback kicks in (no
// broken grid placed on a stale tick).

static GRID_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("mid", PortType::Number),
        Port::new("step_bps", PortType::Number),
        Port::new("levels", PortType::Number),
        Port::new("size", PortType::Number),
        Port::new("skew_bps", PortType::Number),
    ]
});
static QUOTES_OUT: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("quotes", PortType::Quotes)]);

/// Symmetric grid builder. Produces 2N quote levels around a mid
/// price, spaced by `step_bps` bps (so level `k` sits at
/// `mid ± (step_bps/10_000) × (k+1) × mid`). `skew_bps` shifts the
/// whole grid in the opposite direction to inventory so a long
/// position gets wider asks and tighter bids (inventory-aware).
#[derive(Debug, Default)]
pub struct Grid;

impl NodeKind for Grid {
    fn kind(&self) -> &'static str {
        "Quote.Grid"
    }
    fn input_ports(&self) -> &[Port] {
        &GRID_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &QUOTES_OUT
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let mid = match inputs.first().and_then(Value::as_number) {
            Some(m) if m > Decimal::ZERO => m,
            _ => return Ok(vec![Value::Missing]),
        };
        let step_bps = match inputs.get(1).and_then(Value::as_number) {
            Some(s) if s > Decimal::ZERO => s,
            _ => return Ok(vec![Value::Missing]),
        };
        // `levels`: clamp to [1, 20] so a typo (`Math.Const` of 10_000)
        // can't spam the book with pathological depth.
        let levels_raw = inputs
            .get(2)
            .and_then(Value::as_number)
            .unwrap_or(dec!(3))
            .trunc()
            .to_string()
            .parse::<i64>()
            .unwrap_or(3);
        let levels = levels_raw.clamp(1, 20) as usize;
        let size = inputs
            .get(3)
            .and_then(Value::as_number)
            .filter(|q| *q > Decimal::ZERO)
            .unwrap_or(dec!(0));
        if size == Decimal::ZERO {
            return Ok(vec![Value::Missing]);
        }
        let skew_bps = inputs.get(4).and_then(Value::as_number).unwrap_or(dec!(0));

        let bp = dec!(10000);
        let skew_frac = skew_bps / bp;
        let centre = mid - skew_frac * mid;

        let mut out = Vec::with_capacity(levels * 2);
        for k in 0..levels {
            let k_dec = Decimal::from((k + 1) as u64);
            let offset = step_bps * k_dec / bp * mid;
            let bid_px = centre - offset;
            let ask_px = centre + offset;
            if bid_px > Decimal::ZERO {
                out.push(GraphQuote {
                    side: QuoteSide::Buy,
                    price: bid_px,
                    qty: size,
                });
            }
            out.push(GraphQuote {
                side: QuoteSide::Sell,
                price: ask_px,
                qty: size,
            });
        }
        Ok(vec![Value::Quotes(out)])
    }
}

// ─── Quote.Mux ─────────────────────────────────────────────────
//
// Inputs: `cond: Bool`, `a: Quotes`, `b: Quotes`. Returns a when
// cond is true, else b. The meta-strategy primitive: pipe two
// competing strategies into a Mux, drive the selector from a
// regime / news / pair-class source, deploy.

static MUX_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("cond", PortType::Bool),
        Port::new("a", PortType::Quotes),
        Port::new("b", PortType::Quotes),
    ]
});

#[derive(Debug, Default)]
pub struct Mux;

impl NodeKind for Mux {
    fn kind(&self) -> &'static str {
        "Quote.Mux"
    }
    fn input_ports(&self) -> &[Port] {
        &MUX_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &QUOTES_OUT
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let cond = inputs.first().and_then(Value::as_bool).unwrap_or(false);
        let pick = if cond { inputs.get(1) } else { inputs.get(2) };
        Ok(vec![match pick {
            Some(Value::Quotes(q)) => Value::Quotes(q.clone()),
            _ => Value::Missing,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::EvalCtx;

    fn ctx() -> EvalCtx {
        EvalCtx::default()
    }

    #[test]
    fn grid_emits_six_quotes_for_three_levels() {
        let mut state = NodeState::default();
        let out = Grid
            .evaluate(
                &ctx(),
                &[
                    Value::Number(dec!(100)),  // mid
                    Value::Number(dec!(10)),   // step_bps
                    Value::Number(dec!(3)),    // levels
                    Value::Number(dec!(0.5)),  // size
                    Value::Number(dec!(0)),    // skew
                ],
                &mut state,
            )
            .unwrap();
        let qs = match &out[0] {
            Value::Quotes(q) => q,
            v => panic!("expected Quotes, got {v:?}"),
        };
        assert_eq!(qs.len(), 6);
        // Alternating bid/ask by level: level 0 bid < level 0 ask.
        assert_eq!(qs[0].side, QuoteSide::Buy);
        assert_eq!(qs[1].side, QuoteSide::Sell);
        assert!(qs[0].price < qs[1].price);
        // Level 2 (outer) strictly wider than level 0.
        assert!(qs[4].price < qs[0].price, "deeper bid below inner bid");
        assert!(qs[5].price > qs[1].price, "deeper ask above inner ask");
    }

    #[test]
    fn grid_returns_missing_on_bad_inputs() {
        let mut state = NodeState::default();
        let missing_mid = Grid
            .evaluate(
                &ctx(),
                &[
                    Value::Missing,
                    Value::Number(dec!(10)),
                    Value::Number(dec!(3)),
                    Value::Number(dec!(0.5)),
                    Value::Number(dec!(0)),
                ],
                &mut state,
            )
            .unwrap();
        assert!(matches!(missing_mid[0], Value::Missing));
    }

    #[test]
    fn mux_picks_a_when_cond_true() {
        let mut state = NodeState::default();
        let a = Value::Quotes(vec![GraphQuote {
            side: QuoteSide::Buy,
            price: dec!(1),
            qty: dec!(1),
        }]);
        let b = Value::Quotes(vec![GraphQuote {
            side: QuoteSide::Sell,
            price: dec!(2),
            qty: dec!(2),
        }]);
        let out = Mux
            .evaluate(
                &ctx(),
                &[Value::Bool(true), a.clone(), b.clone()],
                &mut state,
            )
            .unwrap();
        assert_eq!(out[0], a);
    }

    #[test]
    fn mux_picks_b_when_cond_false() {
        let mut state = NodeState::default();
        let a = Value::Quotes(vec![]);
        let b = Value::Quotes(vec![GraphQuote {
            side: QuoteSide::Sell,
            price: dec!(2),
            qty: dec!(2),
        }]);
        let out = Mux
            .evaluate(
                &ctx(),
                &[Value::Bool(false), a.clone(), b.clone()],
                &mut state,
            )
            .unwrap();
        assert_eq!(out[0], b);
    }
}
