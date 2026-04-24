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
static QUOTES_OUT: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("quotes", PortType::Quotes)]);

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

// ─── Quote.Hedge ──────────────────────────────────────────────
//
// Phase IV graph-native XEMM glue. Takes the (side, qty, price)
// payload from a `Trade.OwnFill` pulse (plus its `fired` bool as
// the gate), inverts the side, offsets the price by `cross_bps`
// in the favourable direction (so a market-cross actually fills),
// and emits a single-leg quote bundle. When the config declares
// a distinct `hedge_venue` the output is `VenueQuotes` (tagged
// with venue+symbol+product); when absent it's plain `Quotes`
// targeting the current engine's venue.
//
// Typical chain:
//   Trade.OwnFill(venue=primary).{fired,side,qty,price}
//      → Quote.Hedge(hedge_venue=B).quotes
//      + Trade.OwnFill.fired → Out.VenueQuotesIf.trigger
//
// Emits Missing when `fired=false`, `qty ≤ 0`, or any required
// input is Missing. Downstream gates fail closed on Missing, so
// a skipped tick never fires an unguarded hedge.

use crate::node::{ConfigEnumOption, ConfigField, ConfigWidget};

static HEDGE_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("fired", PortType::Bool),
        Port::new("side", PortType::Number),
        Port::new("qty", PortType::Number),
        Port::new("price", PortType::Number),
    ]
});

#[derive(Debug)]
pub struct Hedge {
    hedge_venue: String,
    hedge_symbol: String,
    hedge_product: String,
    cross_bps: Decimal,
}

impl Default for Hedge {
    fn default() -> Self {
        Self {
            hedge_venue: String::new(),
            hedge_symbol: String::new(),
            hedge_product: String::new(),
            cross_bps: dec!(10),
        }
    }
}

impl Hedge {
    pub fn from_config(cfg: &serde_json::Value) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let read_str = |k: &str| -> String {
            cfg.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let read_dec = |k: &str, default: Decimal| -> Decimal {
            cfg.get(k)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Decimal>().ok())
                .unwrap_or(default)
        };
        Some(Self {
            hedge_venue: read_str("hedge_venue"),
            hedge_symbol: read_str("hedge_symbol"),
            hedge_product: read_str("hedge_product"),
            cross_bps: read_dec("cross_bps", dec!(10)),
        })
    }
}

impl NodeKind for Hedge {
    fn kind(&self) -> &'static str {
        "Quote.Hedge"
    }
    fn input_ports(&self) -> &[Port] {
        &HEDGE_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &QUOTES_OUT
    }
    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField {
                name: "hedge_venue",
                label: "Hedge venue",
                hint: Some("Where the hedge goes. Empty → engine's own venue."),
                default: serde_json::json!(""),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "hedge_symbol",
                label: "Hedge symbol",
                hint: Some("Empty → engine's own symbol."),
                default: serde_json::json!(""),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "hedge_product",
                label: "Hedge product",
                hint: Some(
                    "Overrides engine product when targeting a different market (spot vs perp).",
                ),
                default: serde_json::json!(""),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption {
                            value: "",
                            label: "(engine default)",
                        },
                        ConfigEnumOption {
                            value: "spot",
                            label: "Spot",
                        },
                        ConfigEnumOption {
                            value: "linear_perp",
                            label: "Linear perp",
                        },
                        ConfigEnumOption {
                            value: "inverse_perp",
                            label: "Inverse perp",
                        },
                    ],
                },
            },
            ConfigField {
                name: "cross_bps",
                label: "Cross depth (bps)",
                hint: Some(
                    "How many bps past the fill price the hedge aims — guarantees IoC fill.",
                ),
                default: serde_json::json!("10"),
                widget: ConfigWidget::Number {
                    min: Some(0.0),
                    max: Some(500.0),
                    step: Some(1.0),
                },
            },
        ]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let fired = inputs.first().and_then(Value::as_bool).unwrap_or(false);
        if !fired {
            return Ok(vec![Value::Missing]);
        }
        let side_num = match inputs.get(1).and_then(Value::as_number) {
            Some(v) => v,
            None => return Ok(vec![Value::Missing]),
        };
        let qty = match inputs.get(2).and_then(Value::as_number) {
            Some(v) if v > Decimal::ZERO => v,
            _ => return Ok(vec![Value::Missing]),
        };
        let price = match inputs.get(3).and_then(Value::as_number) {
            Some(v) if v > Decimal::ZERO => v,
            _ => return Ok(vec![Value::Missing]),
        };

        // Opposite side of the fill: buy fill (+1) → sell hedge.
        let hedge_side = if side_num > Decimal::ZERO {
            QuoteSide::Sell
        } else {
            QuoteSide::Buy
        };
        // Cross the fill price so the IoC limit actually trades:
        //   - Hedge sell → aim BELOW fill price by cross_bps
        //   - Hedge buy  → aim ABOVE fill price by cross_bps
        let offset = price * self.cross_bps / dec!(10_000);
        let hedge_price = match hedge_side {
            QuoteSide::Sell => price - offset,
            QuoteSide::Buy => price + offset,
        };

        if !self.hedge_venue.is_empty() || !self.hedge_symbol.is_empty() {
            // Emit a venue-tagged leg. Empty fields stay empty so
            // the engine dispatcher can substitute its own defaults
            // (primary venue / primary symbol) where the operator
            // left them blank.
            Ok(vec![Value::VenueQuotes(vec![crate::types::VenueQuote {
                venue: self.hedge_venue.clone(),
                symbol: self.hedge_symbol.clone(),
                product: self.hedge_product.clone(),
                side: hedge_side,
                price: hedge_price,
                qty,
            }])])
        } else {
            Ok(vec![Value::Quotes(vec![GraphQuote {
                side: hedge_side,
                price: hedge_price,
                qty,
            }])])
        }
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
                    Value::Number(dec!(100)), // mid
                    Value::Number(dec!(10)),  // step_bps
                    Value::Number(dec!(3)),   // levels
                    Value::Number(dec!(0.5)), // size
                    Value::Number(dec!(0)),   // skew
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
