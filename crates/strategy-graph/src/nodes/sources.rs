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
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        // Multi-Venue Level 2.B — optional venue/symbol/product
        // pick a specific stream off the DataBus. All empty → the
        // node reads from the engine it's attached to (current
        // behaviour). Any one set → cross-stream read.
        use crate::node::{ConfigEnumOption, ConfigField, ConfigWidget};
        vec![
            ConfigField {
                name: "venue",
                label: "Venue (optional)",
                hint: Some("Leave empty to read from this engine's venue"),
                default: serde_json::json!(""),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "symbol",
                label: "Symbol (optional)",
                hint: Some("Leave empty to read from this engine's symbol"),
                default: serde_json::json!(""),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "product",
                label: "Product (optional)",
                hint: Some("Leave empty for engine default"),
                default: serde_json::json!(""),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "", label: "(engine default)" },
                        ConfigEnumOption { value: "spot", label: "Spot" },
                        ConfigEnumOption { value: "linear_perp", label: "Linear perp" },
                        ConfigEnumOption { value: "inverse_perp", label: "Inverse perp" },
                    ],
                },
            },
        ]
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

// ── Cost.Sweep (INT-4) ──────────────────────────────────────

/// `Cost.Sweep` — simulated sweep cost against the current book.
/// Engine overlays `impact_bps` + `vwap` by calling
/// `LocalOrderBook::sweep_vwap(side, size)`. Lets the graph
/// author rules like "if sweep-to-flatten > 30 bps, widen 2×"
/// or "if impact > X, pause quoting" without an off-graph
/// book-walking helper.
///
/// Config:
///   * `side`: "buy" (sweeps asks) or "sell" (sweeps bids)
///   * `size`: base-asset qty the caller would hypothetically
///     take right now
#[derive(Debug, Default)]
pub struct CostSweep;

static COST_SWEEP_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("impact_bps", PortType::Number),
        Port::new("vwap", PortType::Number),
        Port::new("fully_filled", PortType::Bool),
    ]
});

impl NodeKind for CostSweep {
    fn kind(&self) -> &'static str {
        "Cost.Sweep"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &COST_SWEEP_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigEnumOption, ConfigField, ConfigWidget};
        vec![
            ConfigField {
                name: "side",
                label: "Side",
                hint: Some("buy = sweep asks, sell = sweep bids"),
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Buy (sweep asks)" },
                        ConfigEnumOption { value: "sell", label: "Sell (sweep bids)" },
                    ],
                },
            },
            ConfigField {
                name: "size",
                label: "Target qty (base)",
                hint: Some("Hypothetical base-asset qty to take right now"),
                default: serde_json::json!("0.01"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
        ]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        // Engine overlays actual values per tick; `Missing`
        // falls through when the book is empty / size is zero.
        Ok(vec![Value::Missing; COST_SWEEP_OUTPUTS.len()])
    }
}

// ── Portfolio.CrossVenueNetDelta (INV-3) ──────────────────

/// `Portfolio.CrossVenueNetDelta(base_asset)` — sum of signed
/// inventory across every connected venue whose symbol starts
/// with `base_asset`. Config: `{ asset: "BTC" }`. A single rule
/// like `CrossVenueNetDelta > max_delta → Out.SpreadMult widen`
/// now composes on a graph instead of wiring N engines by hand.
#[derive(Debug, Default)]
pub struct PortfolioCrossVenueNetDelta;

impl NodeKind for PortfolioCrossVenueNetDelta {
    fn kind(&self) -> &'static str {
        "Portfolio.CrossVenueNetDelta"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        static PORTS: Lazy<Vec<Port>> =
            Lazy::new(|| vec![Port::new("value", PortType::Number)]);
        &PORTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "asset",
            label: "Base asset",
            hint: Some("Symbol prefix the aggregator matches on (e.g. BTC)"),
            default: serde_json::json!(""),
            widget: ConfigWidget::Text,
        }]
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

// ── Cost.CumulativeToday / Decision.RealizedCostBps (RS-4) ─

/// `Cost.CumulativeToday` — cumulative net trading cost since
/// UTC midnight in the engine's quote asset:
/// `fees_paid - rebate_income`. Positive = we paid the venue
/// today; negative = we're in the rebate green.
#[derive(Debug, Default)]
pub struct CumulativeTodayCost;

impl NodeKind for CumulativeTodayCost {
    fn kind(&self) -> &'static str {
        "Cost.CumulativeToday"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        static PORTS: Lazy<Vec<Port>> =
            Lazy::new(|| vec![Port::new("value", PortType::Number)]);
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

/// `Decision.RealizedCostBps` — rolling average of realized
/// cost bps across the most recent N resolved decisions for
/// this engine's symbol. `N` comes from config
/// (`window_decisions`, default 50). `Missing` when no
/// decisions have resolved yet.
#[derive(Debug, Default)]
pub struct DecisionRealizedCostBps;

impl NodeKind for DecisionRealizedCostBps {
    fn kind(&self) -> &'static str {
        "Decision.RealizedCostBps"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        static PORTS: Lazy<Vec<Port>> =
            Lazy::new(|| vec![Port::new("value", PortType::Number)]);
        &PORTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "window_decisions",
            label: "Window (decisions)",
            hint: Some("Number of most-recent resolved decisions to average"),
            default: serde_json::json!(50),
            widget: ConfigWidget::Integer { min: Some(1), max: Some(10_000) },
        }]
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

// ── Position.CostBasis / Risk.UnrealizedIfFlatten (RS-3) ──

/// `Position.CostBasis` — running average entry price of the
/// engine's open position in quote asset. Zero when flat.
#[derive(Debug, Default)]
pub struct PositionCostBasis;

impl NodeKind for PositionCostBasis {
    fn kind(&self) -> &'static str {
        "Position.CostBasis"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        static PORTS: Lazy<Vec<Port>> =
            Lazy::new(|| vec![Port::new("value", PortType::Number)]);
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

/// `Risk.UnrealizedIfFlatten` — hypothetical quote-asset PnL
/// we'd realise by flattening **right now** against the live
/// book. Composes `Position.CostBasis` + book sweep in one
/// node so operators don't have to chain Math.* to get a
/// correct signed answer. Missing when flat / no book / no
/// cost basis.
#[derive(Debug, Default)]
pub struct UnrealizedIfFlatten;

impl NodeKind for UnrealizedIfFlatten {
    fn kind(&self) -> &'static str {
        "Risk.UnrealizedIfFlatten"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        static PORTS: Lazy<Vec<Port>> =
            Lazy::new(|| vec![Port::new("value", PortType::Number)]);
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

// ── Risk.LiquidationDistance (RS-2) ───────────────────────
//
// `Risk.MarginRatio` was already declared via the shared
// `single_scalar_source!` macro below; RS-2 adds the engine-
// side overlay so the placeholder finally carries live data.
// Only `Risk.LiquidationDistance` is a genuinely new node here
// because its output port is `value_bps`, not the generic
// `value` the macro emits.

/// `Risk.LiquidationDistance` — `|liq_price - mid| / mid * 10_000`
/// in bps for the engine's current position. `None` when no
/// position, spot engine, missing liq_price, or mid is zero.
#[derive(Debug, Default)]
pub struct LiquidationDistanceSource;

impl NodeKind for LiquidationDistanceSource {
    fn kind(&self) -> &'static str {
        "Risk.LiquidationDistance"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        static PORTS: Lazy<Vec<Port>> =
            Lazy::new(|| vec![Port::new("value_bps", PortType::Number)]);
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

// ── Multi-Venue 2.B.2 — parameterised cross-stream sources ───

/// `Book.L2(venue, symbol, product, depth)` — top-N levels per side
/// off the shared DataBus. Outputs flatten depth×2 arrays into two
/// string-serialised CSV strings so a graph can read them without
/// needing a new port type for vectors.
#[derive(Debug, Default)]
pub struct BookL2;

static BOOK_L2_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("bids", PortType::String),
        Port::new("asks", PortType::String),
        Port::new("best_bid_px", PortType::Number),
        Port::new("best_ask_px", PortType::Number),
    ]
});

impl NodeKind for BookL2 {
    fn kind(&self) -> &'static str {
        "Book.L2"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &BOOK_L2_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        cross_venue_config_fields(Some(("depth", "Depth (levels/side)", 10)))
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

/// `Trade.Tape(venue, symbol, product, window_secs)` — rolling
/// public-trade window. Outputs the tape's size + signed-qty sum +
/// last price so simple detectors can consume it without parsing
/// a CSV.
#[derive(Debug, Default)]
pub struct TradeTape;

static TRADE_TAPE_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("trade_count", PortType::Number),
        Port::new("buy_qty", PortType::Number),
        Port::new("sell_qty", PortType::Number),
        Port::new("last_price", PortType::Number),
    ]
});

impl NodeKind for TradeTape {
    fn kind(&self) -> &'static str {
        "Trade.Tape"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &TRADE_TAPE_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        cross_venue_config_fields(Some(("window_secs", "Tape window (s)", 60)))
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

/// `Balance(venue, asset)` — wallet balance off the bus. `total`
/// and `available` are what graphs usually consume; `reserved` is
/// the margin + open-orders-locked portion for parity with
/// the exchange-reported shape.
#[derive(Debug, Default)]
pub struct BalanceSource;

static BALANCE_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("total", PortType::Number),
        Port::new("available", PortType::Number),
        Port::new("reserved", PortType::Number),
    ]
});

impl NodeKind for BalanceSource {
    fn kind(&self) -> &'static str {
        "Balance"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &BALANCE_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![
            ConfigField {
                name: "venue",
                label: "Venue",
                hint: Some("Empty = this engine's venue"),
                default: serde_json::json!(""),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "asset",
                label: "Asset",
                hint: Some("e.g. USDT, BTC"),
                default: serde_json::json!("USDT"),
                widget: ConfigWidget::Text,
            },
        ]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::Missing; 3])
    }
}

/// `Funding(venue, symbol)` — per-perp funding rate. `rate` is
/// per-hour as a fraction (e.g. 0.0001 = 1 bps/h); `seconds_to_next`
/// counts down to the next funding exchange.
#[derive(Debug, Default)]
pub struct FundingSource;

static FUNDING_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("rate", PortType::Number),
        Port::new("seconds_to_next", PortType::Number),
    ]
});

impl NodeKind for FundingSource {
    fn kind(&self) -> &'static str {
        "Funding"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &FUNDING_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        cross_venue_config_fields(None)
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::Missing; 2])
    }
}

/// `Portfolio.NetDelta(asset)` — signed cross-venue net exposure.
/// Long spot BTC + short BTC perp → 0 (neutral). Consumers use
/// this as a delta-aware hedge signal.
#[derive(Debug, Default)]
pub struct PortfolioNetDelta;

static NET_DELTA_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("value", PortType::Number)]);

impl NodeKind for PortfolioNetDelta {
    fn kind(&self) -> &'static str {
        "Portfolio.NetDelta"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &NET_DELTA_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "asset",
            label: "Asset",
            hint: Some("e.g. BTC, ETH"),
            default: serde_json::json!("BTC"),
            widget: ConfigWidget::Text,
        }]
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

/// `Portfolio.QuoteAvailable(venue)` — aggregate available quote
/// (USDT/USDC/USD) on a specific venue. Rebalancer-facing.
#[derive(Debug, Default)]
pub struct PortfolioQuoteAvailable;

impl NodeKind for PortfolioQuoteAvailable {
    fn kind(&self) -> &'static str {
        "Portfolio.QuoteAvailable"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &NET_DELTA_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "venue",
            label: "Venue",
            hint: Some("e.g. binance, bybit"),
            default: serde_json::json!("binance"),
            widget: ConfigWidget::Text,
        }]
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

/// Shared `(venue, symbol, product[, extra_int])` config schema
/// used by every cross-venue source. Factored out so a change to
/// the tuple (e.g. adding `subaccount`) touches one place.
fn cross_venue_config_fields(
    extra_int: Option<(&'static str, &'static str, i64)>,
) -> Vec<crate::node::ConfigField> {
    use crate::node::{ConfigEnumOption, ConfigField, ConfigWidget};
    let mut fields = vec![
        ConfigField {
            name: "venue",
            label: "Venue (optional)",
            hint: Some("Empty = this engine's venue"),
            default: serde_json::json!(""),
            widget: ConfigWidget::Text,
        },
        ConfigField {
            name: "symbol",
            label: "Symbol (optional)",
            hint: Some("Empty = this engine's symbol"),
            default: serde_json::json!(""),
            widget: ConfigWidget::Text,
        },
        ConfigField {
            name: "product",
            label: "Product (optional)",
            hint: None,
            default: serde_json::json!(""),
            widget: ConfigWidget::Enum {
                options: vec![
                    ConfigEnumOption { value: "", label: "(engine default)" },
                    ConfigEnumOption { value: "spot", label: "Spot" },
                    ConfigEnumOption { value: "linear_perp", label: "Linear perp" },
                    ConfigEnumOption { value: "inverse_perp", label: "Inverse perp" },
                ],
            },
        },
    ];
    if let Some((name, label, default)) = extra_int {
        fields.push(ConfigField {
            name,
            label,
            hint: None,
            default: serde_json::json!(default),
            widget: ConfigWidget::Integer { min: Some(1), max: Some(3600) },
        });
    }
    fields
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
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![
            ConfigField {
                name: "ratio_hot",
                label: "Cancel/fill ratio (hot)",
                hint: Some("≥ this → full score contribution"),
                default: serde_json::json!("0.9"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(1.0), step: Some(0.01) },
            },
            ConfigField {
                name: "lifetime_hot_ms",
                label: "Order lifetime (hot, ms)",
                hint: Some("≤ this → full score contribution"),
                default: serde_json::json!(100),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(5000) },
            },
            ConfigField {
                name: "size_ratio_hot",
                label: "Order size vs avg trade (hot)",
                hint: Some("≥ this × avg trade → full contribution"),
                default: serde_json::json!("5"),
                widget: ConfigWidget::Number { min: Some(1.0), max: Some(50.0), step: Some(0.5) },
            },
        ]
    }
}

/// `Surveillance.LayeringScore` — many small orders clustered on
/// one side at adjacent price ticks, with synchronous cancels.
/// Same 4-port shape as `SpoofingScore` for UI uniformity.
#[derive(Debug, Default)]
pub struct LayeringScore;

impl NodeKind for LayeringScore {
    fn kind(&self) -> &'static str {
        "Surveillance.LayeringScore"
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

/// `Surveillance.QuoteStuffingScore` — very high orders-per-second
/// + near-zero fill rate. Same output shape.
#[derive(Debug, Default)]
pub struct QuoteStuffingScore;

impl NodeKind for QuoteStuffingScore {
    fn kind(&self) -> &'static str {
        "Surveillance.QuoteStuffingScore"
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

/// `Surveillance.WashScore` — self-trade detection (own buy + own
/// sell same price within short window). Single-port output.
#[derive(Debug, Default)]
pub struct WashScore;

static SCORE_ONLY_OUTPUT: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("value", PortType::Number)]);

impl NodeKind for WashScore {
    fn kind(&self) -> &'static str {
        "Surveillance.WashScore"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &SCORE_ONLY_OUTPUT
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

macro_rules! simple_surveillance_score {
    ($struct_name:ident, $kind:literal) => {
        #[derive(Debug, Default)]
        pub struct $struct_name;
        impl NodeKind for $struct_name {
            fn kind(&self) -> &'static str {
                $kind
            }
            fn input_ports(&self) -> &[Port] {
                &EMPTY_INPUTS
            }
            fn output_ports(&self) -> &[Port] {
                &SCORE_ONLY_OUTPUT
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

simple_surveillance_score!(CrossMarketScore, "Surveillance.CrossMarketScore");
simple_surveillance_score!(LatencyExploitScore, "Surveillance.LatencyExploitScore");
simple_surveillance_score!(RebateAbuseScore, "Surveillance.RebateAbuseScore");
simple_surveillance_score!(ImbalanceManipulationScore, "Surveillance.ImbalanceManipulationScore");
simple_surveillance_score!(CancelOnReactionScore, "Surveillance.CancelOnReactionScore");
simple_surveillance_score!(OneSidedQuotingScore, "Surveillance.OneSidedQuotingScore");
simple_surveillance_score!(InventoryPushingScore, "Surveillance.InventoryPushingScore");
simple_surveillance_score!(StrategicNonFillingScore, "Surveillance.StrategicNonFillingScore");

/// `Session.TimeToBoundary` — seconds to the next session
/// boundary (funding window / settlement). Pairs with
/// `MarkingCloseDetector` + `Cast.ToBool(<=60)` to gate close-
/// window logic.
#[derive(Debug, Default)]
pub struct SessionTimeToBoundary;

static TTB_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("seconds_to_next", PortType::Number),
        Port::new("seconds_since_last", PortType::Number),
    ]
});

impl NodeKind for SessionTimeToBoundary {
    fn kind(&self) -> &'static str {
        "Session.TimeToBoundary"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &TTB_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::Missing; 2])
    }
}

/// `Surveillance.MarkingCloseScore` — trade-volume spike inside
/// the seconds-to-boundary window.
#[derive(Debug, Default)]
pub struct MarkingCloseScore;

impl NodeKind for MarkingCloseScore {
    fn kind(&self) -> &'static str {
        "Surveillance.MarkingCloseScore"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &SCORE_ONLY_OUTPUT
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

/// `Surveillance.FakeLiquidityScore` — orders evaporating near
/// the touch. One-port output.
#[derive(Debug, Default)]
pub struct FakeLiquidityScore;

impl NodeKind for FakeLiquidityScore {
    fn kind(&self) -> &'static str {
        "Surveillance.FakeLiquidityScore"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &SCORE_ONLY_OUTPUT
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

/// `Surveillance.ForeignTwap` — INT-3. Autocorrelation-based
/// detector of a competing participant's TWAP / iceberg
/// algorithm. Emits a score in [0, 1] where ≥ 0.8 is
/// conventional alert-grade. Engine overlays the live score
/// from the per-engine `ForeignTwapDetector` ring.
#[derive(Debug, Default)]
pub struct ForeignTwap;

impl NodeKind for ForeignTwap {
    fn kind(&self) -> &'static str {
        "Surveillance.ForeignTwap"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &SCORE_ONLY_OUTPUT
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

/// `Surveillance.MomentumIgnitionScore` — public-tape burst +
/// aggressor dominance + price-move. Single-port output.
#[derive(Debug, Default)]
pub struct MomentumIgnitionScore;

impl NodeKind for MomentumIgnitionScore {
    fn kind(&self) -> &'static str {
        "Surveillance.MomentumIgnitionScore"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &SCORE_ONLY_OUTPUT
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
