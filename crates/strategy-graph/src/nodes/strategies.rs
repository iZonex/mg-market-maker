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

// ─── STRAT-2 — queue-aware size scaler ────────────────────────

/// `Strategy.QueueAware` — composable quote-size modulator that
/// takes a `Quotes` bundle and a `Book.FillProbability` scalar,
/// then scales every quote's `qty` by a probability-derived
/// multiplier. Pipe any `Strategy.*` source through this node
/// upstream of `Out.Quotes` to get a queue-position-aware
/// variant of that strategy without touching its internals.
///
/// Multiplier shape: `mult = floor + (1 - floor) · p` where
/// `floor = 0.3` (never flatten completely so a stalled feed
/// can't drop size to zero) and `p ∈ [0, 1]`. A `Missing` or
/// `Bool(false)` probability falls through with `mult = floor`,
/// the conservative default.
#[derive(Debug, Default)]
pub struct QueueAware;

static QUEUE_AWARE_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("quotes", PortType::Quotes),
        Port::new("probability", PortType::Number),
    ]
});

static QUEUE_AWARE_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("quotes", PortType::Quotes)]);

impl NodeKind for QueueAware {
    fn kind(&self) -> &'static str {
        "Strategy.QueueAware"
    }
    fn input_ports(&self) -> &[Port] {
        &QUEUE_AWARE_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &QUEUE_AWARE_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        use rust_decimal::prelude::FromPrimitive;
        let Some(quotes) = inputs.first().and_then(Value::as_quotes) else {
            return Ok(vec![Value::Missing]);
        };
        let p_raw = inputs.get(1).and_then(Value::as_number);
        // Clamp probability to `[0, 1]`. Missing / out-of-range
        // falls through to `0.0` so the floor multiplier does
        // the work — conservative, matches the rest of the
        // graph's fail-closed posture.
        let p = p_raw
            .map(|v| v.max(rust_decimal::Decimal::ZERO).min(rust_decimal::Decimal::ONE))
            .unwrap_or(rust_decimal::Decimal::ZERO);
        let floor = rust_decimal::Decimal::from_f64(0.3).unwrap_or(rust_decimal::Decimal::ZERO);
        let mult = floor + (rust_decimal::Decimal::ONE - floor) * p;

        let scaled: Vec<crate::types::GraphQuote> = quotes
            .iter()
            .map(|q| crate::types::GraphQuote {
                side: q.side,
                price: q.price,
                qty: (q.qty * mult).max(rust_decimal::Decimal::ZERO),
            })
            .collect();
        Ok(vec![Value::Quotes(scaled)])
    }
}

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

/// `Strategy.Layer` — structured multi-level layering exploit.
/// Real behaviour wired in engine via `LayerStrategy`.
#[derive(Debug, Default)]
pub struct Layer;

impl NodeKind for Layer {
    fn kind(&self) -> &'static str {
        "Strategy.Layer"
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
                hint: None,
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy" },
                        ConfigEnumOption { value: "sell", label: "Sell" },
                    ],
                },
            },
            ConfigField {
                name: "levels",
                label: "Layers",
                hint: Some("How many stacked levels per cycle"),
                default: serde_json::json!(5),
                widget: ConfigWidget::Integer { min: Some(2), max: Some(20) },
            },
            ConfigField {
                name: "cluster_bps",
                label: "Cluster spacing (bps)",
                hint: Some("Tight = clear layering signal"),
                default: serde_json::json!("1"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(100.0), step: Some(0.5) },
            },
            ConfigField {
                name: "offset_bps",
                label: "Innermost offset (bps)",
                hint: None,
                default: serde_json::json!("5"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(200.0), step: Some(1.0) },
            },
            ConfigField {
                name: "leg_size",
                label: "Per-level size",
                hint: None,
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
        ]
    }
}

/// `Strategy.Stuff` — quote-stuffing exploit. Real behaviour in
/// `StuffStrategy`.
#[derive(Debug, Default)]
pub struct Stuff;

impl NodeKind for Stuff {
    fn kind(&self) -> &'static str {
        "Strategy.Stuff"
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
                hint: None,
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy" },
                        ConfigEnumOption { value: "sell", label: "Sell" },
                    ],
                },
            },
            ConfigField {
                name: "orders_per_tick",
                label: "Orders per tick",
                hint: Some("Higher = more stuffing noise"),
                default: serde_json::json!(20),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(200) },
            },
            ConfigField {
                name: "step_bps",
                label: "Step (bps)",
                hint: Some("Tiny offsets between tiered orders"),
                default: serde_json::json!("0.1"),
                widget: ConfigWidget::Number { min: Some(0.01), max: Some(10.0), step: Some(0.1) },
            },
            ConfigField {
                name: "leg_size",
                label: "Leg size",
                hint: None,
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
        ]
    }
}

/// `Strategy.CrossMarket` — burst/rest cross-venue push. Real
/// behaviour in `CrossMarketStrategy`.
#[derive(Debug, Default)]
pub struct CrossMarket;

impl NodeKind for CrossMarket {
    fn kind(&self) -> &'static str {
        "Strategy.CrossMarket"
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
                hint: None,
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy" },
                        ConfigEnumOption { value: "sell", label: "Sell" },
                    ],
                },
            },
            ConfigField {
                name: "burst_size",
                label: "Burst size",
                hint: None,
                default: serde_json::json!("0.01"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "cross_depth_bps",
                label: "Cross depth (bps)",
                hint: None,
                default: serde_json::json!("25"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(500.0), step: Some(1.0) },
            },
            ConfigField {
                name: "burst_ticks",
                label: "Burst ticks",
                hint: None,
                default: serde_json::json!(8),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(100) },
            },
            ConfigField {
                name: "rest_ticks",
                label: "Rest ticks",
                hint: None,
                default: serde_json::json!(4),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(100) },
            },
        ]
    }
}

/// `Strategy.LatencyHunt` — fires on book skew. Real behaviour in
/// `LatencyHuntStrategy`.
#[derive(Debug, Default)]
pub struct LatencyHunt;

impl NodeKind for LatencyHunt {
    fn kind(&self) -> &'static str {
        "Strategy.LatencyHunt"
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
                name: "burst_size",
                label: "Burst size",
                hint: None,
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "cross_depth_bps",
                label: "Cross depth (bps)",
                hint: None,
                default: serde_json::json!("50"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(500.0), step: Some(1.0) },
            },
            ConfigField {
                name: "skew_threshold",
                label: "Skew threshold",
                hint: Some("|bid_qty - ask_qty| / total above which to fire"),
                default: serde_json::json!("0.5"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(1.0), step: Some(0.05) },
            },
        ]
    }
}

/// `Strategy.RebateFarm` — tight symmetric maker quotes for fee
/// rebates. Real behaviour in `RebateFarmStrategy`.
#[derive(Debug, Default)]
pub struct RebateFarm;

impl NodeKind for RebateFarm {
    fn kind(&self) -> &'static str {
        "Strategy.RebateFarm"
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
                hint: None,
                default: serde_json::json!("0.01"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "offset_bps",
                label: "Offset from mid (bps)",
                hint: Some("Tight offset maximises churn"),
                default: serde_json::json!("1"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(200.0), step: Some(0.5) },
            },
        ]
    }
}

/// `Strategy.Imbalance` — alternating heavy-qty side to skew the
/// book. Real behaviour in `ImbalanceStrategy`.
#[derive(Debug, Default)]
pub struct Imbalance;

impl NodeKind for Imbalance {
    fn kind(&self) -> &'static str {
        "Strategy.Imbalance"
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
                name: "heavy_size",
                label: "Heavy-side size",
                hint: None,
                default: serde_json::json!("0.05"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "light_size",
                label: "Light-side size",
                hint: None,
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "offset_bps",
                label: "Offset from mid (bps)",
                hint: None,
                default: serde_json::json!("2"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(200.0), step: Some(0.5) },
            },
            ConfigField {
                name: "flip_ticks",
                label: "Flip interval (ticks)",
                hint: Some("Ticks held before swapping heavy side"),
                default: serde_json::json!(3),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(100) },
            },
        ]
    }
}

/// `Strategy.ReactCancel` — post then cancel if no nearby trade.
/// Real behaviour in `ReactCancelStrategy`.
#[derive(Debug, Default)]
pub struct ReactCancel;

impl NodeKind for ReactCancel {
    fn kind(&self) -> &'static str {
        "Strategy.ReactCancel"
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
                hint: None,
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy" },
                        ConfigEnumOption { value: "sell", label: "Sell" },
                    ],
                },
            },
            ConfigField {
                name: "burst_size",
                label: "Burst size",
                hint: None,
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "offset_bps",
                label: "Offset from mid (bps)",
                hint: None,
                default: serde_json::json!("3"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(200.0), step: Some(0.5) },
            },
            ConfigField {
                name: "idle_ticks",
                label: "Idle ticks between posts",
                hint: Some("Cancels synchronously between posts"),
                default: serde_json::json!(2),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(20) },
            },
        ]
    }
}

/// `Strategy.OneSided` — post only the configured side. Real
/// behaviour in `OneSidedStrategy`.
#[derive(Debug, Default)]
pub struct OneSided;

impl NodeKind for OneSided {
    fn kind(&self) -> &'static str {
        "Strategy.OneSided"
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
                name: "side",
                label: "Quote side",
                hint: None,
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy-only" },
                        ConfigEnumOption { value: "sell", label: "Sell-only" },
                    ],
                },
            },
            ConfigField {
                name: "offset_bps",
                label: "Offset from mid (bps)",
                hint: None,
                default: serde_json::json!("2"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(200.0), step: Some(0.5) },
            },
            ConfigField {
                name: "leg_size",
                label: "Leg size",
                hint: None,
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
        ]
    }
}

/// `Strategy.InvPush` — cross-through toward inventory-unwind
/// direction. Real behaviour in `InvPushStrategy`.
#[derive(Debug, Default)]
pub struct InvPush;

impl NodeKind for InvPush {
    fn kind(&self) -> &'static str {
        "Strategy.InvPush"
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
                name: "burst_size",
                label: "Burst size",
                hint: None,
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "cross_depth_bps",
                label: "Cross depth (bps)",
                hint: None,
                default: serde_json::json!("20"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(500.0), step: Some(1.0) },
            },
            ConfigField {
                name: "min_inventory",
                label: "Minimum |inventory|",
                hint: Some("Fires only when inventory magnitude ≥ this"),
                default: serde_json::json!("0.01"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
        ]
    }
}

/// `Strategy.NonFill` — near-touch orders yanked every other tick.
/// Real behaviour in `NonFillStrategy`.
#[derive(Debug, Default)]
pub struct NonFill;

impl NodeKind for NonFill {
    fn kind(&self) -> &'static str {
        "Strategy.NonFill"
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
                hint: None,
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy" },
                        ConfigEnumOption { value: "sell", label: "Sell" },
                    ],
                },
            },
            ConfigField {
                name: "leg_size",
                label: "Leg size",
                hint: None,
                default: serde_json::json!("0.001"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "offset_bps",
                label: "Offset from touch (bps)",
                hint: Some("Near-touch so placements look real before yank"),
                default: serde_json::json!("1"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(50.0), step: Some(0.5) },
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

/// `Strategy.PumpAndDump` — pentest four-phase exploit
/// orchestrator. See `mm_strategy::pump_and_dump` for the phase
/// FSM and each phase's quote shape.
#[derive(Debug, Default)]
pub struct PumpAndDump;

impl NodeKind for PumpAndDump {
    fn kind(&self) -> &'static str {
        "Strategy.PumpAndDump"
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
                name: "accumulate_ticks",
                label: "Accumulate ticks",
                hint: Some("How long to quietly build inventory before the pump."),
                default: serde_json::json!(20),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(10_000) },
            },
            ConfigField {
                name: "accumulate_size",
                label: "Accumulate size",
                hint: Some("Per-tick passive bid qty during accumulate."),
                default: serde_json::json!("0.002"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "pump_ticks",
                label: "Pump ticks",
                hint: Some("How long to aggressively push price up."),
                default: serde_json::json!(10),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(10_000) },
            },
            ConfigField {
                name: "pump_depth_bps",
                label: "Pump depth (bps)",
                hint: Some("How far across the ask the crossing buy goes."),
                default: serde_json::json!("50"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(1000.0), step: Some(1.0) },
            },
            ConfigField {
                name: "distribute_ticks",
                label: "Distribute ticks",
                hint: Some("How long to sell into FOMO via an ask ladder."),
                default: serde_json::json!(20),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(10_000) },
            },
            ConfigField {
                name: "distribute_rungs",
                label: "Distribute rungs",
                hint: Some("Simultaneous ask ladder rungs above mid."),
                default: serde_json::json!(4),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(50) },
            },
            ConfigField {
                name: "dump_ticks",
                label: "Dump ticks",
                hint: Some("Aggressive cross-through sell tail at the end."),
                default: serde_json::json!(10),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(10_000) },
            },
            ConfigField {
                name: "dump_depth_bps",
                label: "Dump depth (bps)",
                hint: Some("How far across the bid the crossing sell goes."),
                default: serde_json::json!("60"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(1000.0), step: Some(1.0) },
            },
        ]
    }
}

// ⚠⚠⚠ Epic R4 — multi-venue exploit orchestration.
//
// The three nodes below are PENTEST-ONLY. They exist so the
// operator can stress-test their own exchange's surveillance
// stack against documented market-manipulation patterns
// (liquidation hunts, leveraged perp attacks, multi-phase
// coordinated campaigns). Running these against a venue you
// do not own or are not explicitly authorized to pentest is:
//   - A violation of every exchange ToS we are aware of.
//   - Likely illegal under MiFID II (EU), Dodd-Frank / SEA
//     §9(a) (US), FSA (Japan), MiCA (EU from 2024-12).
//   - Civilly actionable by any trader who takes a loss.
//
// Every `Strategy.*` node in this block sets `restricted() =
// true` so `Evaluator::build` refuses to compile unless
// `MM_RESTRICTED_ALLOW=1` is set at process start. The
// operator is expected to have read `docs/guides/pentest.md`
// and confirmed written authorization before flipping that
// switch.

/// ⚠ RESTRICTED — `Strategy.LiquidationHunt` — aggressive
/// crossing orders calibrated to trigger a known
/// liquidation cascade cluster on a thin perp book.
/// Typically used by the offensive side of a RAVE-style
/// campaign to force a cascade on over-leveraged retail
/// shorts / longs before distributing.
///
/// Reads `Surveillance.LiquidationHeatmap`'s
/// `nearest_above_bps` / `nearest_below_bps` to target
/// exactly the right price — no blind push.
#[derive(Debug, Default)]
pub struct LiquidationHunt;

static LIQ_HUNT_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("target_bps", PortType::Number),
        Port::new("cluster_notional", PortType::Number),
    ]
});

impl NodeKind for LiquidationHunt {
    fn kind(&self) -> &'static str {
        "Strategy.LiquidationHunt"
    }
    fn input_ports(&self) -> &[Port] {
        &LIQ_HUNT_INPUTS
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
                name: "push_size",
                label: "⚠ Push order size",
                hint: Some("Per-tick cross-through qty. PENTEST ONLY — authorized venue only."),
                default: serde_json::json!("0.002"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "max_bps_overshoot",
                label: "⚠ Max bps past cluster",
                hint: Some("Bps to cross past the target cluster. Higher = more aggressive trigger."),
                default: serde_json::json!("5"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(200.0), step: Some(1.0) },
            },
        ]
    }
}

/// ⚠ RESTRICTED — `Strategy.LeverageBuilder` — opens a
/// leveraged perp position on the running venue. Config
/// specifies side, size, and target leverage. Paired with a
/// spot short / long on another venue via
/// `Strategy.CampaignOrchestrator` to build the asymmetric
/// exposure characteristic of a RAVE-style setup.
///
/// Uses the engine's existing perp order path; `set_leverage`
/// on the connector is called during orchestrator phase setup
/// so the broker state is consistent. PENTEST venues only.
#[derive(Debug, Default)]
pub struct LeverageBuilder;

impl NodeKind for LeverageBuilder {
    fn kind(&self) -> &'static str {
        "Strategy.LeverageBuilder"
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
                name: "direction",
                label: "⚠ Direction",
                hint: Some("long = buy perp to set up a squeeze; short = sell perp before spot dump"),
                default: serde_json::json!("long"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "long", label: "Long (buy)" },
                        ConfigEnumOption { value: "short", label: "Short (sell)" },
                    ],
                },
            },
            ConfigField {
                name: "position_size",
                label: "⚠ Position size (base units)",
                hint: Some("Notional = size × current mark. Bigger = more market impact."),
                default: serde_json::json!("0.01"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "leverage",
                label: "⚠ Leverage (1–125)",
                hint: Some("Higher = more position per margin dollar. Venue-capped."),
                default: serde_json::json!(5),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(125) },
            },
            ConfigField {
                name: "max_slippage_bps",
                label: "⚠ Max slippage (bps)",
                hint: Some("Refuse to open if fill would cross further than this past mid."),
                default: serde_json::json!("100"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(1000.0), step: Some(1.0) },
            },
        ]
    }
}

/// ⚠ RESTRICTED — `Strategy.CampaignOrchestrator` — multi-
/// phase multi-venue timeline FSM. Chains a sequence of
/// sub-strategies (accumulate → pump → liquidation-hunt →
/// distribute → dump) across venues, each phase timed in
/// seconds. Reproduces the full RAVE campaign structure so
/// the operator can validate their own exchange's
/// surveillance + circuit-breaker response.
///
/// Config is a JSON array of `{name, duration_secs, venue,
/// sub_strategy, sub_config}`. Phase output is routed via
/// `Out.VenueQuotes` when `venue` differs from the engine's
/// primary.
#[derive(Debug, Default)]
pub struct CampaignOrchestrator;

impl NodeKind for CampaignOrchestrator {
    fn kind(&self) -> &'static str {
        "Strategy.CampaignOrchestrator"
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
                name: "accumulate_secs",
                label: "⚠ Accumulate phase (secs)",
                hint: Some("Quiet passive buys at / just below mid."),
                default: serde_json::json!(600),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(86_400) },
            },
            ConfigField {
                name: "pump_secs",
                label: "⚠ Pump phase (secs)",
                hint: Some("Aggressive crossing buys to push price up."),
                default: serde_json::json!(120),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(86_400) },
            },
            ConfigField {
                name: "distribute_secs",
                label: "⚠ Distribute phase (secs)",
                hint: Some("Laddered sells above the new mid."),
                default: serde_json::json!(600),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(86_400) },
            },
            ConfigField {
                name: "dump_secs",
                label: "⚠ Dump phase (secs)",
                hint: Some("Aggressive crossing sells to exit."),
                default: serde_json::json!(120),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(86_400) },
            },
            ConfigField {
                name: "pump_depth_bps",
                label: "⚠ Pump cross depth (bps)",
                hint: Some("How far across the ask the pump cross goes."),
                default: serde_json::json!("50"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(1000.0), step: Some(1.0) },
            },
            ConfigField {
                name: "distribute_rungs",
                label: "⚠ Distribute ladder rungs",
                hint: Some("Simultaneous ask rungs above mid during distribute."),
                default: serde_json::json!(4),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(50) },
            },
            ConfigField {
                name: "dump_depth_bps",
                label: "⚠ Dump cross depth (bps)",
                hint: Some("How far across the bid the dump cross goes."),
                default: serde_json::json!("60"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(1000.0), step: Some(1.0) },
            },
            ConfigField {
                name: "loop_cycle",
                label: "Loop after final phase",
                hint: Some("Restart from Accumulate after Dump — useful for smoke replays. Off by default (terminates in Idle)."),
                default: serde_json::json!(false),
                widget: ConfigWidget::Bool,
            },
        ]
    }
}

/// ⚠ RESTRICTED — `Strategy.CascadeHunter` — gated liquidation-
/// cascade trigger. Takes a `target_bps` input (from
/// `Signal.LiquidationLevelEstimate` or
/// `Surveillance.LiquidationHeatmap::nearest_above_bps`) and
/// a `trigger` bool (typically a composite of
/// `Signal.LongShortRatio > threshold` AND a size condition).
/// Emits a single crossing order aimed at the target when the
/// trigger is true.
///
/// Unlike `Strategy.LiquidationHunt` which hunts on every
/// tick, this node is one-shot-per-tick and explicitly gated
/// by a graph condition the operator controls — the idea is
/// to couple attack timing to a visible crowd-side signal
/// (e.g. "only hunt when longs are > 70 % of accounts AND
/// heatmap shows a cluster within 200 bps"). Honest MM side:
/// operators validate their own surveillance catches the
/// full attack-defense loop with this node + RugScore +
/// CascadeCompleted in one template.
///
/// Pentest only, restricted behind `MM_RESTRICTED_ALLOW=1`.
#[derive(Debug, Default)]
pub struct CascadeHunter;

static CASCADE_HUNTER_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("target_bps", PortType::Number),
        Port::new("trigger", PortType::Bool),
    ]
});

impl NodeKind for CascadeHunter {
    fn kind(&self) -> &'static str {
        "Strategy.CascadeHunter"
    }
    fn input_ports(&self) -> &[Port] {
        &CASCADE_HUNTER_INPUTS
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
                name: "push_size",
                label: "⚠ Push size",
                hint: Some("Per-fire cross qty when triggered. Pentest venue only."),
                default: serde_json::json!("0.005"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "side",
                label: "⚠ Push side",
                hint: Some("Which cluster to hunt — buy = pushes up into short-liq cluster; sell = pushes down into long-liq cluster."),
                default: serde_json::json!("sell"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy (short-squeeze)" },
                        ConfigEnumOption { value: "sell", label: "Sell (long-squeeze)" },
                    ],
                },
            },
            ConfigField {
                name: "max_bps_overshoot",
                label: "⚠ Max bps past cluster",
                hint: Some("How far to overshoot the target_bps input to guarantee the cluster triggers."),
                default: serde_json::json!("10"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(200.0), step: Some(1.0) },
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

    // ─── STRAT-2 — Strategy.QueueAware scaling ────────────────

    fn gq(side: crate::types::QuoteSide, qty: rust_decimal::Decimal) -> crate::types::GraphQuote {
        crate::types::GraphQuote {
            side,
            price: rust_decimal::Decimal::ONE,
            qty,
        }
    }

    #[test]
    fn queue_aware_passes_full_qty_at_probability_one() {
        use rust_decimal_macros::dec;
        let mut state = NodeState::default();
        let quotes = vec![gq(crate::types::QuoteSide::Buy, dec!(10))];
        let out = QueueAware
            .evaluate(
                &EvalCtx::default(),
                &[Value::Quotes(quotes), Value::Number(dec!(1))],
                &mut state,
            )
            .unwrap();
        match &out[0] {
            Value::Quotes(qs) => assert_eq!(qs[0].qty, dec!(10)),
            other => panic!("expected Quotes, got {other:?}"),
        }
    }

    #[test]
    fn queue_aware_floors_at_0_3_when_probability_zero() {
        use rust_decimal::prelude::FromPrimitive;
        use rust_decimal_macros::dec;
        let mut state = NodeState::default();
        let quotes = vec![gq(crate::types::QuoteSide::Buy, dec!(10))];
        let out = QueueAware
            .evaluate(
                &EvalCtx::default(),
                &[Value::Quotes(quotes), Value::Number(dec!(0))],
                &mut state,
            )
            .unwrap();
        match &out[0] {
            Value::Quotes(qs) => {
                let expected = dec!(10) * rust_decimal::Decimal::from_f64(0.3).unwrap();
                assert_eq!(qs[0].qty, expected);
            }
            other => panic!("expected Quotes, got {other:?}"),
        }
    }

    #[test]
    fn queue_aware_scales_linearly_between_floor_and_full() {
        use rust_decimal::prelude::FromPrimitive;
        use rust_decimal_macros::dec;
        let mut state = NodeState::default();
        let quotes = vec![gq(crate::types::QuoteSide::Sell, dec!(10))];
        let out = QueueAware
            .evaluate(
                &EvalCtx::default(),
                &[Value::Quotes(quotes), Value::Number(dec!(0.5))],
                &mut state,
            )
            .unwrap();
        match &out[0] {
            Value::Quotes(qs) => {
                // mult = 0.3 + 0.7 * 0.5 = 0.65 → qty = 6.5
                let expected = dec!(10) * rust_decimal::Decimal::from_f64(0.65).unwrap();
                assert_eq!(qs[0].qty, expected);
            }
            other => panic!("expected Quotes, got {other:?}"),
        }
    }

    #[test]
    fn queue_aware_missing_probability_falls_through_to_floor() {
        use rust_decimal::prelude::FromPrimitive;
        use rust_decimal_macros::dec;
        let mut state = NodeState::default();
        let quotes = vec![gq(crate::types::QuoteSide::Buy, dec!(10))];
        let out = QueueAware
            .evaluate(
                &EvalCtx::default(),
                &[Value::Quotes(quotes), Value::Missing],
                &mut state,
            )
            .unwrap();
        match &out[0] {
            Value::Quotes(qs) => {
                let expected = dec!(10) * rust_decimal::Decimal::from_f64(0.3).unwrap();
                assert_eq!(qs[0].qty, expected);
            }
            other => panic!("expected Quotes, got {other:?}"),
        }
    }

    #[test]
    fn queue_aware_missing_quotes_input_returns_missing() {
        use rust_decimal_macros::dec;
        let mut state = NodeState::default();
        let out = QueueAware
            .evaluate(
                &EvalCtx::default(),
                &[Value::Missing, Value::Number(dec!(1))],
                &mut state,
            )
            .unwrap();
        assert!(matches!(out[0], Value::Missing));
    }
}
