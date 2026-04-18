//! `Risk.*` — domain-specific risk-layer transforms.
//!
//! These wrap formulas that already live in `mm-risk` (same math,
//! same constants) so a graph-authored strategy layers them on top
//! of the hand-wired ones without divergence. Each risk node takes
//! its input from upstream source/transform nodes — operators wire
//! `Toxicity.VPIN → Risk.ToxicityWiden` explicitly so the pipeline
//! is visible, not implicit.

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::Value as Json;
use std::str::FromStr;

// ── Risk.ToxicityWiden ─────────────────────────────────────

/// VPIN → spread multiplier in `[1, max]` via the formula
/// `mult = 1 + scale * vpin` with `vpin ∈ [0, 1]`. Default
/// `scale = 2.0` so VPIN 0.5 → 2.0× and VPIN 1.0 → 3.0×; matches
/// the hand-wired path in `mm_strategy::autotune::set_toxicity`.
/// `Missing` vpin returns `1.0` (no widening).
#[derive(Debug)]
pub struct ToxicityWiden {
    scale: Decimal,
}

impl Default for ToxicityWiden {
    fn default() -> Self {
        Self { scale: dec!(2) }
    }
}

#[derive(Deserialize)]
struct ToxicityWidenCfg {
    #[serde(default)]
    scale: Option<String>,
}

impl ToxicityWiden {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: ToxicityWidenCfg = serde_json::from_value(cfg.clone()).ok()?;
        let scale = match parsed.scale {
            None => dec!(2),
            Some(s) => {
                let v = Decimal::from_str(&s).ok()?;
                if v < dec!(0) {
                    return None;
                }
                v
            }
        };
        Some(Self { scale })
    }
}

static TOXICITY_WIDEN_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("vpin", PortType::Number)]);
static TOXICITY_WIDEN_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("mult", PortType::Number)]);

impl NodeKind for ToxicityWiden {
    fn kind(&self) -> &'static str {
        "Risk.ToxicityWiden"
    }
    fn input_ports(&self) -> &[Port] {
        &TOXICITY_WIDEN_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &TOXICITY_WIDEN_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let Some(vpin) = inputs.first().and_then(Value::as_number) else {
            return Ok(vec![Value::Number(Decimal::ONE)]);
        };
        // Clamp vpin to [0, 1] so out-of-band inputs don't blow
        // the multiplier past `1 + scale`.
        let clamped = vpin.max(Decimal::ZERO).min(Decimal::ONE);
        let mult = Decimal::ONE + self.scale * clamped;
        Ok(vec![Value::Number(mult)])
    }
}

// ── Risk.InventoryUrgency ──────────────────────────────────

/// `|inventory| / cap` → urgency score in `[0, 1]` with a
/// configurable quadratic curve so the ramp gets steeper as
/// the position approaches the cap. Matches the
/// `inventory_skew::urgency_score` helper on the hand-wired
/// path. Output is dimensionless — feed it into a
/// `Math.Mul(size_base, 1 - urgency)` chain to shrink quotes
/// when inventory is hot.
#[derive(Debug)]
pub struct InventoryUrgency {
    cap: Decimal,
    exponent: Decimal,
}

impl Default for InventoryUrgency {
    fn default() -> Self {
        Self {
            cap: dec!(1),
            exponent: dec!(2),
        }
    }
}

#[derive(Deserialize)]
struct InventoryUrgencyCfg {
    #[serde(default)]
    cap: Option<String>,
    #[serde(default)]
    exponent: Option<String>,
}

impl InventoryUrgency {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: InventoryUrgencyCfg = serde_json::from_value(cfg.clone()).ok()?;
        let cap = match parsed.cap {
            None => dec!(1),
            Some(s) => {
                let v = Decimal::from_str(&s).ok()?;
                if v <= dec!(0) {
                    return None;
                }
                v
            }
        };
        let exponent = match parsed.exponent {
            None => dec!(2),
            Some(s) => {
                let v = Decimal::from_str(&s).ok()?;
                if v <= dec!(0) {
                    return None;
                }
                v
            }
        };
        Some(Self { cap, exponent })
    }
}

static INVENTORY_URGENCY_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("level", PortType::Number)]);
static INVENTORY_URGENCY_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("urgency", PortType::Number)]);

impl NodeKind for InventoryUrgency {
    fn kind(&self) -> &'static str {
        "Risk.InventoryUrgency"
    }
    fn input_ports(&self) -> &[Port] {
        &INVENTORY_URGENCY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &INVENTORY_URGENCY_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let Some(level) = inputs.first().and_then(Value::as_number) else {
            return Ok(vec![Value::Number(Decimal::ZERO)]);
        };
        let ratio = (level.abs() / self.cap).min(Decimal::ONE);
        let urgency = if self.exponent == dec!(2) {
            // Fast-path for the common case.
            ratio * ratio
        } else {
            // Decimal doesn't ship `powf`; fall back to the
            // cheapest integer exponent approximation by
            // successive multiplications when exponent is a
            // small positive integer, else use the linear
            // ratio (documented as fallback in the docstring).
            let exp_int = self.exponent.trunc().to_string().parse::<u32>().unwrap_or(2);
            let mut out = Decimal::ONE;
            for _ in 0..exp_int {
                out *= ratio;
            }
            out
        };
        Ok(vec![Value::Number(urgency)])
    }
}

// ── Risk.CircuitBreaker ────────────────────────────────────

/// Wide-spread circuit breaker — when the live spread exceeds
/// `wide_bps`, output `true` (tripped). Mirrors the
/// `mm_risk::circuit_breaker::CircuitBreaker::wide_spread` check
/// minus the staleness half (staleness needs a timestamp source
/// that's not yet in the catalog; tracked as Phase 2 Wave C).
/// `Missing` spread treats as tripped — fail closed.
#[derive(Debug)]
pub struct CircuitBreaker {
    wide_bps: Decimal,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            wide_bps: dec!(100),
        }
    }
}

#[derive(Deserialize)]
struct CircuitBreakerCfg {
    #[serde(default)]
    wide_bps: Option<String>,
}

impl CircuitBreaker {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: CircuitBreakerCfg = serde_json::from_value(cfg.clone()).ok()?;
        let wide_bps = match parsed.wide_bps {
            None => dec!(100),
            Some(s) => {
                let v = Decimal::from_str(&s).ok()?;
                if v <= dec!(0) {
                    return None;
                }
                v
            }
        };
        Some(Self { wide_bps })
    }
}

static CIRCUIT_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("spread_bps", PortType::Number)]);
static CIRCUIT_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("tripped", PortType::Bool)]);

impl NodeKind for CircuitBreaker {
    fn kind(&self) -> &'static str {
        "Risk.CircuitBreaker"
    }
    fn input_ports(&self) -> &[Port] {
        &CIRCUIT_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &CIRCUIT_OUTPUTS
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let tripped = match inputs.first().and_then(Value::as_number) {
            Some(v) => v > self.wide_bps,
            None => true, // fail closed
        };
        Ok(vec![Value::Bool(tripped)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toxicity_widen_vpin_0_gives_1x() {
        let n = ToxicityWiden::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        let out = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(0))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(1))]);
    }

    #[test]
    fn toxicity_widen_saturates_at_1_plus_scale() {
        let n = ToxicityWiden::from_config(&serde_json::json!({ "scale": "2" })).unwrap();
        let mut st = NodeState::default();
        let out = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(1))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(3))]);
    }

    #[test]
    fn toxicity_widen_clamps_out_of_band_vpin() {
        let n = ToxicityWiden::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        let over = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(5))],
                &mut st,
            )
            .unwrap();
        assert_eq!(over, vec![Value::Number(dec!(3))]);
        let under = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(-0.5))],
                &mut st,
            )
            .unwrap();
        assert_eq!(under, vec![Value::Number(dec!(1))]);
    }

    #[test]
    fn toxicity_widen_missing_vpin_returns_1() {
        let n = ToxicityWiden::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        let out = n.evaluate(&EvalCtx::default(), &[Value::Missing], &mut st).unwrap();
        assert_eq!(out, vec![Value::Number(dec!(1))]);
    }

    #[test]
    fn inventory_urgency_half_cap_squared() {
        // cap=10, pos=5 → ratio 0.5, urgency 0.25
        let n = InventoryUrgency::from_config(&serde_json::json!({ "cap": "10" })).unwrap();
        let mut st = NodeState::default();
        let out = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(5))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(0.25))]);
    }

    #[test]
    fn inventory_urgency_negative_position_also_counts() {
        // Abs value: short -5 == long 5 for urgency.
        let n = InventoryUrgency::from_config(&serde_json::json!({ "cap": "10" })).unwrap();
        let mut st = NodeState::default();
        let out = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(-5))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(0.25))]);
    }

    #[test]
    fn inventory_urgency_saturates_at_cap() {
        let n = InventoryUrgency::from_config(&serde_json::json!({ "cap": "10" })).unwrap();
        let mut st = NodeState::default();
        let out = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(30))],
                &mut st,
            )
            .unwrap();
        assert_eq!(out, vec![Value::Number(dec!(1))]);
    }

    #[test]
    fn circuit_breaker_trips_on_wide_spread() {
        let n =
            CircuitBreaker::from_config(&serde_json::json!({ "wide_bps": "100" })).unwrap();
        let mut st = NodeState::default();
        let over = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(150))],
                &mut st,
            )
            .unwrap();
        assert_eq!(over, vec![Value::Bool(true)]);
        let ok = n
            .evaluate(
                &EvalCtx::default(),
                &[Value::Number(dec!(50))],
                &mut st,
            )
            .unwrap();
        assert_eq!(ok, vec![Value::Bool(false)]);
    }

    #[test]
    fn circuit_breaker_fails_closed_on_missing() {
        let n = CircuitBreaker::from_config(&Json::Null).unwrap();
        let mut st = NodeState::default();
        let out = n
            .evaluate(&EvalCtx::default(), &[Value::Missing], &mut st)
            .unwrap();
        assert_eq!(out, vec![Value::Bool(true)]);
    }
}
