//! `Indicator.*` — stateful stream transforms wrapping
//! `mm-indicators`. Each node owns its own indicator instance
//! via `NodeState` and calls `update()` on every tick when the
//! input is present, else passes through `Missing`.
//!
//! All indicators use a `period` config field (usize). Bollinger
//! additionally takes `k_stddev` (Decimal string). Same shape
//! across the catalog so the UI can render one config form per
//! family.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::Value as Json;
use std::str::FromStr;

// Shared input/output port shapes — every single-series
// indicator takes a Number on `x` and emits a Number on `out`.
static IND_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("x", PortType::Number)]);
static IND_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("out", PortType::Number)]);

#[derive(Deserialize)]
struct PeriodCfg {
    #[serde(default)]
    period: Option<usize>,
}

fn parse_period(cfg: &Json, default: usize) -> Option<usize> {
    if cfg.is_null() {
        return Some(default);
    }
    let parsed: PeriodCfg = serde_json::from_value(cfg.clone()).ok()?;
    let p = parsed.period.unwrap_or(default);
    if p == 0 || p > 10_000 {
        return None;
    }
    Some(p)
}

/// GR-1 — shared `period` ConfigField for every `Indicator.*`
/// node. Keeps the UI schema in sync with the runtime range
/// enforced by [`parse_period`] (`1 ≤ period ≤ 10_000`). Every
/// indicator calls this in its `config_schema()` and then
/// optionally appends additional fields (Bollinger adds
/// `k_stddev`).
fn period_config_field(default: i64) -> crate::node::ConfigField {
    use crate::node::{ConfigField, ConfigWidget};
    ConfigField {
        name: "period",
        label: "Period",
        hint: Some("Lookback window (1–10000 samples)"),
        default: serde_json::json!(default),
        widget: ConfigWidget::Integer {
            min: Some(1),
            max: Some(10_000),
        },
    }
}

// ── Indicator.SMA ──────────────────────────────────────────

#[derive(Debug)]
pub struct SmaNode {
    period: usize,
}

impl Default for SmaNode {
    fn default() -> Self {
        Self { period: 14 }
    }
}

impl SmaNode {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        Some(Self {
            period: parse_period(cfg, 14)?,
        })
    }
}

impl NodeKind for SmaNode {
    fn kind(&self) -> &'static str {
        "Indicator.SMA"
    }
    fn input_ports(&self) -> &[Port] {
        &IND_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &IND_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        vec![period_config_field(14)]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        // Lazy-initialise with the configured period. `NodeState`
        // only holds `SmaState::default()` on first fetch —
        // replace with a correctly-sized tracker here. Subsequent
        // ticks skip this check: `already_initialised` guards.
        let st = state.get_or_insert_default::<SmaInitGuard>();
        if !st.inited {
            st.tracker = Some(mm_indicators::Sma::new(self.period));
            st.inited = true;
        }
        let tracker = st.tracker.as_mut().expect("init set above");
        if let Some(x) = inputs.first().and_then(Value::as_number) {
            tracker.update(x);
        }
        let out = tracker.value().map(Value::Number).unwrap_or(Value::Missing);
        Ok(vec![out])
    }
}

#[derive(Default)]
struct SmaInitGuard {
    inited: bool,
    tracker: Option<mm_indicators::Sma>,
}

// ── Indicator.EMA ──────────────────────────────────────────

#[derive(Debug)]
pub struct EmaNode {
    period: usize,
}

impl Default for EmaNode {
    fn default() -> Self {
        Self { period: 14 }
    }
}

impl EmaNode {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        Some(Self {
            period: parse_period(cfg, 14)?,
        })
    }
}

#[derive(Default)]
struct EmaInitGuard {
    inited: bool,
    tracker: Option<mm_indicators::Ema>,
}

impl NodeKind for EmaNode {
    fn kind(&self) -> &'static str {
        "Indicator.EMA"
    }
    fn input_ports(&self) -> &[Port] {
        &IND_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &IND_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        vec![period_config_field(14)]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let st = state.get_or_insert_default::<EmaInitGuard>();
        if !st.inited {
            st.tracker = Some(mm_indicators::Ema::new(self.period));
            st.inited = true;
        }
        let tracker = st.tracker.as_mut().expect("init");
        if let Some(x) = inputs.first().and_then(Value::as_number) {
            tracker.update(x);
        }
        let out = tracker.value().map(Value::Number).unwrap_or(Value::Missing);
        Ok(vec![out])
    }
}

// ── Indicator.HMA ──────────────────────────────────────────

#[derive(Debug)]
pub struct HmaNode {
    period: usize,
}

impl Default for HmaNode {
    fn default() -> Self {
        Self { period: 14 }
    }
}

impl HmaNode {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        Some(Self {
            period: parse_period(cfg, 14)?,
        })
    }
}

#[derive(Default)]
struct HmaInitGuard {
    inited: bool,
    tracker: Option<mm_indicators::Hma>,
}

impl NodeKind for HmaNode {
    fn kind(&self) -> &'static str {
        "Indicator.HMA"
    }
    fn input_ports(&self) -> &[Port] {
        &IND_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &IND_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        vec![period_config_field(14)]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let st = state.get_or_insert_default::<HmaInitGuard>();
        if !st.inited {
            st.tracker = Some(mm_indicators::Hma::new(self.period));
            st.inited = true;
        }
        let tracker = st.tracker.as_mut().expect("init");
        if let Some(x) = inputs.first().and_then(Value::as_number) {
            tracker.update(x);
        }
        let out = tracker.value().map(Value::Number).unwrap_or(Value::Missing);
        Ok(vec![out])
    }
}

// ── Indicator.RSI ──────────────────────────────────────────

#[derive(Debug)]
pub struct RsiNode {
    period: usize,
}

impl Default for RsiNode {
    fn default() -> Self {
        Self { period: 14 }
    }
}

impl RsiNode {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        Some(Self {
            period: parse_period(cfg, 14)?,
        })
    }
}

#[derive(Default)]
struct RsiInitGuard {
    inited: bool,
    tracker: Option<mm_indicators::Rsi>,
}

impl NodeKind for RsiNode {
    fn kind(&self) -> &'static str {
        "Indicator.RSI"
    }
    fn input_ports(&self) -> &[Port] {
        &IND_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &IND_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        vec![period_config_field(14)]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let st = state.get_or_insert_default::<RsiInitGuard>();
        if !st.inited {
            st.tracker = Some(mm_indicators::Rsi::new(self.period));
            st.inited = true;
        }
        let tracker = st.tracker.as_mut().expect("init");
        if let Some(x) = inputs.first().and_then(Value::as_number) {
            tracker.update(x);
        }
        let out = tracker.value().map(Value::Number).unwrap_or(Value::Missing);
        Ok(vec![out])
    }
}

// ── Indicator.ATR ──────────────────────────────────────────
//
// ATR takes (high, low, close). Expose as three input ports.
// Callers wire `Book.L1.ask_px` → high, `Book.L1.bid_px` → low,
// `Book.L1.mid` → close as a pragmatic top-of-book ATR; for
// bar-based ATR, a future `Candles.*` source node would feed the
// three from OHLC.

#[derive(Debug)]
pub struct AtrNode {
    period: usize,
}

impl Default for AtrNode {
    fn default() -> Self {
        Self { period: 14 }
    }
}

impl AtrNode {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        Some(Self {
            period: parse_period(cfg, 14)?,
        })
    }
}

#[derive(Default)]
struct AtrInitGuard {
    inited: bool,
    tracker: Option<mm_indicators::Atr>,
}

static ATR_INPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("high", PortType::Number),
        Port::new("low", PortType::Number),
        Port::new("close", PortType::Number),
    ]
});

impl NodeKind for AtrNode {
    fn kind(&self) -> &'static str {
        "Indicator.ATR"
    }
    fn input_ports(&self) -> &[Port] {
        &ATR_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &IND_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        vec![period_config_field(14)]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let st = state.get_or_insert_default::<AtrInitGuard>();
        if !st.inited {
            st.tracker = Some(mm_indicators::Atr::new(self.period));
            st.inited = true;
        }
        let tracker = st.tracker.as_mut().expect("init");
        let (h, l, c) = (
            inputs.first().and_then(Value::as_number),
            inputs.get(1).and_then(Value::as_number),
            inputs.get(2).and_then(Value::as_number),
        );
        if let (Some(h), Some(l), Some(c)) = (h, l, c) {
            tracker.update(h, l, c);
        }
        let out = tracker.value().map(Value::Number).unwrap_or(Value::Missing);
        Ok(vec![out])
    }
}

// ── Indicator.Bollinger ────────────────────────────────────
//
// Bollinger emits three numbers. Output shape: upper / middle /
// lower per-port so a graph can wire each band to a different
// downstream consumer. `k_stddev` config controls the band
// width in standard deviations.

#[derive(Debug)]
pub struct BollingerNode {
    period: usize,
    k_stddev: Decimal,
}

impl Default for BollingerNode {
    fn default() -> Self {
        Self {
            period: 20,
            k_stddev: dec!(2),
        }
    }
}

#[derive(Deserialize)]
struct BollingerCfg {
    #[serde(default)]
    period: Option<usize>,
    #[serde(default)]
    k_stddev: Option<String>,
}

impl BollingerNode {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: BollingerCfg = serde_json::from_value(cfg.clone()).ok()?;
        let period = parsed.period.unwrap_or(20);
        if period == 0 || period > 10_000 {
            return None;
        }
        let k_stddev = match parsed.k_stddev {
            None => dec!(2),
            Some(s) => {
                let v = Decimal::from_str(&s).ok()?;
                if v <= dec!(0) {
                    return None;
                }
                v
            }
        };
        Some(Self { period, k_stddev })
    }
}

#[derive(Default)]
struct BollingerInitGuard {
    inited: bool,
    tracker: Option<mm_indicators::BollingerBands>,
}

static BOLLINGER_OUTPUTS: Lazy<Vec<Port>> = Lazy::new(|| {
    vec![
        Port::new("upper", PortType::Number),
        Port::new("middle", PortType::Number),
        Port::new("lower", PortType::Number),
    ]
});

impl NodeKind for BollingerNode {
    fn kind(&self) -> &'static str {
        "Indicator.Bollinger"
    }
    fn input_ports(&self) -> &[Port] {
        &IND_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &BOLLINGER_OUTPUTS
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![
            period_config_field(20),
            ConfigField {
                name: "k_stddev",
                label: "σ multiplier",
                hint: Some("Band width in standard deviations (> 0)"),
                default: serde_json::json!("2"),
                widget: ConfigWidget::Number {
                    min: Some(0.0),
                    max: None,
                    step: Some(0.1),
                },
            },
        ]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let st = state.get_or_insert_default::<BollingerInitGuard>();
        if !st.inited {
            st.tracker = Some(mm_indicators::BollingerBands::new(
                self.period,
                self.k_stddev,
            ));
            st.inited = true;
        }
        let tracker = st.tracker.as_mut().expect("init");
        if let Some(x) = inputs.first().and_then(Value::as_number) {
            tracker.update(x);
        }
        match tracker.value() {
            Some(bands) => Ok(vec![
                Value::Number(bands.upper),
                Value::Number(bands.middle),
                Value::Number(bands.lower),
            ]),
            None => Ok(vec![Value::Missing, Value::Missing, Value::Missing]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(node: &dyn NodeKind, st: &mut NodeState, x: Decimal) -> Vec<Value> {
        node.evaluate(&EvalCtx::default(), &[Value::Number(x)], st)
            .unwrap()
    }

    #[test]
    fn sma_period_5_converges() {
        let node = SmaNode::from_config(&serde_json::json!({ "period": 5 })).unwrap();
        let mut st = NodeState::default();
        // Warmup 4 ticks — no value yet.
        for i in 1..=4 {
            let out = tick(&node, &mut st, Decimal::from(i));
            assert_eq!(out, vec![Value::Missing]);
        }
        // 5th tick: mean(1..5) = 3.
        let out = tick(&node, &mut st, Decimal::from(5));
        assert_eq!(out, vec![Value::Number(dec!(3))]);
    }

    #[test]
    fn rsi_rejects_zero_period_config() {
        assert!(RsiNode::from_config(&serde_json::json!({ "period": 0 })).is_none());
    }

    #[test]
    fn ema_updates_without_panic() {
        let node = EmaNode::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        for i in 1..=20 {
            let _ = tick(&node, &mut st, Decimal::from(i));
        }
    }

    #[test]
    fn bollinger_emits_three_ports() {
        let node = BollingerNode::default();
        assert_eq!(node.output_ports().len(), 3);
        assert_eq!(node.output_ports()[0].name, "upper");
        assert_eq!(node.output_ports()[1].name, "middle");
        assert_eq!(node.output_ports()[2].name, "lower");
    }

    #[test]
    fn atr_declares_hlc_inputs() {
        let node = AtrNode::default();
        assert_eq!(node.input_ports().len(), 3);
        let names: Vec<&str> = node.input_ports().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["high", "low", "close"]);
    }
}
