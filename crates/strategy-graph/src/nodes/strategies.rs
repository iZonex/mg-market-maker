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

use crate::node::{
    ConfigEnumOption, ConfigField, ConfigWidget, EvalCtx, NodeKind, NodeState,
};
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
// overrides `restricted() -> true` so `Evaluator::build` refuses to
// compile a graph referencing them unless the server was started
// with `MM_ALLOW_RESTRICTED=yes-pentest-mode`.
//
// Written out by hand (no macro) because every exploit has its own
// config schema — one per-knob row the frontend renders
// automatically.

/// Multi-Venue 3.D — cross-venue basis arbitrage composite. Emits
/// a `Quotes`-typed output (engine overlay materialises it as
/// `Value::VenueQuotes` when reading the strategy pool).
#[derive(Debug, Default)]
pub struct BasisArb;

impl NodeKind for BasisArb {
    fn kind(&self) -> &'static str {
        "Strategy.BasisArb"
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
    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField {
                name: "spot_venue",
                label: "Spot venue",
                hint: Some("e.g. binance"),
                default: serde_json::json!("binance"),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "perp_venue",
                label: "Perp venue",
                hint: Some("e.g. bybit"),
                default: serde_json::json!("bybit"),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "symbol",
                label: "Symbol",
                hint: Some("Same symbol on both legs"),
                default: serde_json::json!("BTCUSDT"),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "leg_size",
                label: "Leg size",
                hint: Some("Per-leg order qty in base asset"),
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "maker_offset_bps",
                label: "Maker offset (bps)",
                hint: Some("How far behind mid the maker-post leg sits"),
                default: serde_json::json!("2"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(200.0), step: Some(0.5) },
            },
            ConfigField {
                name: "min_basis_bps",
                label: "Min basis (bps)",
                hint: Some("Don't enter if basis is below this"),
                default: serde_json::json!("10"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(1000.0), step: Some(1.0) },
            },
            ConfigField {
                name: "max_delta",
                label: "Max net delta",
                hint: Some("Drop the long / short leg when over this"),
                default: serde_json::json!("0.05"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
        ]
    }
}

/// `Strategy.Wash` — pentest, emits buy+sell pair at the same
/// price every tick.
#[derive(Debug, Default)]
pub struct Wash;

impl NodeKind for Wash {
    fn kind(&self) -> &'static str {
        "Strategy.Wash"
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
    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField {
                name: "leg_size",
                label: "Leg size",
                hint: Some("Per-leg order qty (same on buy + sell)"),
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "offset_bps",
                label: "Offset from mid (bps)",
                hint: Some("0 = trade at mid (most visible)"),
                default: serde_json::json!("0"),
                widget: ConfigWidget::Number { min: Some(-200.0), max: Some(200.0), step: Some(1.0) },
            },
        ]
    }
}

/// `Strategy.Ignite` — pentest, burst aggressive cross-through
/// orders for N ticks then rest.
#[derive(Debug, Default)]
pub struct Ignite;

impl NodeKind for Ignite {
    fn kind(&self) -> &'static str {
        "Strategy.Ignite"
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
    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField {
                name: "push_side",
                label: "Push side",
                hint: Some("Buy forces price up, Sell forces it down"),
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy (up)" },
                        ConfigEnumOption { value: "sell", label: "Sell (down)" },
                    ],
                },
            },
            ConfigField {
                name: "burst_size",
                label: "Burst size",
                hint: Some("Per-burst order qty"),
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "cross_depth_bps",
                label: "Cross depth (bps)",
                hint: Some("How far past the opposite touch to cross"),
                default: serde_json::json!("30"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(500.0), step: Some(1.0) },
            },
            ConfigField {
                name: "burst_ticks",
                label: "Burst ticks",
                hint: Some("Consecutive ticks to push per cycle"),
                default: serde_json::json!(5),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(100) },
            },
            ConfigField {
                name: "rest_ticks",
                label: "Rest ticks",
                hint: Some("Flat ticks between bursts"),
                default: serde_json::json!(3),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(100) },
            },
        ]
    }
}

/// `Strategy.Mark` — pentest exploit for marking-the-close.
/// Places an aggressive cross-through limit order N bps past
/// the opposite touch inside the close-window before a session
/// boundary; idle otherwise.
#[derive(Debug, Default)]
pub struct Mark;

impl NodeKind for Mark {
    fn kind(&self) -> &'static str {
        "Strategy.Mark"
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
    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField {
                name: "push_side",
                label: "Push side",
                hint: Some("Which direction to mark"),
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy (mark up)" },
                        ConfigEnumOption { value: "sell", label: "Sell (mark down)" },
                    ],
                },
            },
            ConfigField {
                name: "window_secs",
                label: "Close window (s)",
                hint: Some("How many seconds before boundary to start marking"),
                default: serde_json::json!(60),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(3600) },
            },
            ConfigField {
                name: "burst_size",
                label: "Burst size",
                hint: Some("Per-tick aggressive order qty"),
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "cross_depth_bps",
                label: "Cross depth (bps)",
                hint: Some("How far past opposite touch to cross"),
                default: serde_json::json!("30"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(500.0), step: Some(1.0) },
            },
        ]
    }
}

#[derive(Debug, Default)]
pub struct Spoof;

impl NodeKind for Spoof {
    fn kind(&self) -> &'static str {
        "Strategy.Spoof"
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
    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField {
                name: "pressure_side",
                label: "Pressure side",
                hint: Some("Which side the fake order sits on"),
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy (push price up)" },
                        ConfigEnumOption { value: "sell", label: "Sell (push price down)" },
                    ],
                },
            },
            ConfigField {
                name: "pressure_size_mult",
                label: "Pressure size × order_size",
                hint: Some("How many times larger than a real order the fake is. ≥5 trips the detector."),
                default: serde_json::json!("10"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(50.0), step: Some(0.5) },
            },
            ConfigField {
                name: "pressure_distance_bps",
                label: "Pressure distance (bps)",
                hint: Some("How far from mid the fake sits. Close enough to look real, far enough not to fill."),
                default: serde_json::json!("15"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(200.0), step: Some(1.0) },
            },
            ConfigField {
                name: "real_size_mult",
                label: "Real size × order_size",
                hint: Some("How big the genuine capturing order is."),
                default: serde_json::json!("1"),
                widget: ConfigWidget::Number { min: Some(0.1), max: Some(10.0), step: Some(0.1) },
            },
            ConfigField {
                name: "real_distance_bps",
                label: "Real distance (bps)",
                hint: Some("Tight so the reaction lands on us."),
                default: serde_json::json!("3"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(100.0), step: Some(0.5) },
            },
        ]
    }
}

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
