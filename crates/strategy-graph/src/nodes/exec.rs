//! `Exec.*` — execution algo preset emitters.
//!
//! Phase 2 model: an `Exec.*` node carries a config for one of the
//! workspace's `mm_strategy::exec_algo::*` runtime algos and emits
//! a compact **policy string** the engine parses at flatten time.
//! The graph composes which preset wins (via `Logic.StringMux`);
//! the actual per-slice execution stays in the existing runtime
//! machinery — graphs do NOT drive per-slice orders in Phase 2.
//!
//! Policy string grammar:
//!
//!   `twap:DURATION_SECS:SLICE_COUNT`
//!   `vwap:DURATION_SECS`
//!   `pov:TARGET_PCT`        (e.g. `pov:10` = target 10 % of volume)
//!   `iceberg:DISPLAY_QTY`
//!
//! On `Out.Flatten` firing, the engine reads the policy string,
//! dispatches `kill_switch.manual_trigger(FlattenAll, format!("graph
//! flatten: {policy}"))`, and the existing `paired_unwind` /
//! `twap_executor` path picks the referenced algo from
//! `AppConfig.exec_algo.default_unwind_policy` (or the operator
//! overrides via admin config).

use crate::node::{EvalCtx, NodeKind, NodeState};
use crate::types::{Port, PortType, Value};
use anyhow::Result;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::Value as Json;

// Shared empty-input + single String output ports for every preset
// emitter. All four Exec.*Config nodes are sources.
static EMPTY_INPUTS: Lazy<Vec<Port>> = Lazy::new(Vec::new);
static POLICY_OUTPUT: Lazy<Vec<Port>> = Lazy::new(|| vec![Port::new("policy", PortType::String)]);

// ── Exec.TwapConfig ────────────────────────────────────────

#[derive(Debug)]
pub struct TwapConfig {
    duration_secs: u64,
    slice_count: u32,
}

impl Default for TwapConfig {
    fn default() -> Self {
        Self {
            duration_secs: 120,
            slice_count: 5,
        }
    }
}

#[derive(Deserialize)]
struct TwapCfg {
    #[serde(default)]
    duration_secs: Option<u64>,
    #[serde(default)]
    slice_count: Option<u32>,
}

impl TwapConfig {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: TwapCfg = serde_json::from_value(cfg.clone()).ok()?;
        let duration_secs = parsed.duration_secs.unwrap_or(120);
        let slice_count = parsed.slice_count.unwrap_or(5);
        if duration_secs == 0 || slice_count == 0 || slice_count > 1000 {
            return None;
        }
        Some(Self {
            duration_secs,
            slice_count,
        })
    }
}

impl NodeKind for TwapConfig {
    fn kind(&self) -> &'static str {
        "Exec.TwapConfig"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &POLICY_OUTPUT
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![
            ConfigField {
                name: "duration_secs",
                label: "Duration (s)",
                hint: Some("Total schedule window"),
                default: serde_json::json!(120),
                widget: ConfigWidget::Integer {
                    min: Some(1),
                    max: Some(86_400),
                },
            },
            ConfigField {
                name: "slice_count",
                label: "Slices",
                hint: Some("Number of equal-time slices (1-1000)"),
                default: serde_json::json!(5),
                widget: ConfigWidget::Integer {
                    min: Some(1),
                    max: Some(1000),
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
        Ok(vec![Value::String(format!(
            "twap:{}:{}",
            self.duration_secs, self.slice_count
        ))])
    }
}

// ── Exec.VwapConfig ────────────────────────────────────────

#[derive(Debug)]
pub struct VwapConfig {
    duration_secs: u64,
}

impl Default for VwapConfig {
    fn default() -> Self {
        Self { duration_secs: 300 }
    }
}

#[derive(Deserialize)]
struct VwapCfg {
    #[serde(default)]
    duration_secs: Option<u64>,
}

impl VwapConfig {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: VwapCfg = serde_json::from_value(cfg.clone()).ok()?;
        let duration_secs = parsed.duration_secs.unwrap_or(300);
        if duration_secs == 0 {
            return None;
        }
        Some(Self { duration_secs })
    }
}

impl NodeKind for VwapConfig {
    fn kind(&self) -> &'static str {
        "Exec.VwapConfig"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &POLICY_OUTPUT
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "duration_secs",
            label: "Duration (s)",
            hint: Some("Schedule window for the volume-weighted slicer"),
            default: serde_json::json!(300),
            widget: ConfigWidget::Integer {
                min: Some(1),
                max: Some(86_400),
            },
        }]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::String(format!("vwap:{}", self.duration_secs))])
    }
}

// ── Exec.PovConfig ─────────────────────────────────────────

#[derive(Debug)]
pub struct PovConfig {
    target_pct: u32,
}

impl Default for PovConfig {
    fn default() -> Self {
        Self { target_pct: 10 }
    }
}

#[derive(Deserialize)]
struct PovCfg {
    #[serde(default)]
    target_pct: Option<u32>,
}

impl PovConfig {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: PovCfg = serde_json::from_value(cfg.clone()).ok()?;
        let target_pct = parsed.target_pct.unwrap_or(10);
        if target_pct == 0 || target_pct > 100 {
            return None;
        }
        Some(Self { target_pct })
    }
}

impl NodeKind for PovConfig {
    fn kind(&self) -> &'static str {
        "Exec.PovConfig"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &POLICY_OUTPUT
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "target_pct",
            label: "Target % of volume",
            hint: Some("1-100 — how much of market volume to capture"),
            default: serde_json::json!(10),
            widget: ConfigWidget::Integer {
                min: Some(1),
                max: Some(100),
            },
        }]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::String(format!("pov:{}", self.target_pct))])
    }
}

// ── Exec.IcebergConfig ─────────────────────────────────────

#[derive(Debug)]
pub struct IcebergConfig {
    display_qty: String,
}

impl Default for IcebergConfig {
    fn default() -> Self {
        Self {
            display_qty: "0.1".into(),
        }
    }
}

#[derive(Deserialize)]
struct IcebergCfg {
    #[serde(default)]
    display_qty: Option<String>,
}

impl IcebergConfig {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: IcebergCfg = serde_json::from_value(cfg.clone()).ok()?;
        let display_qty = parsed.display_qty.unwrap_or_else(|| "0.1".into());
        // Round-trip as Decimal for validation — caller must pass
        // a parseable number string.
        if display_qty.parse::<rust_decimal::Decimal>().is_err() {
            return None;
        }
        Some(Self { display_qty })
    }
}

impl NodeKind for IcebergConfig {
    fn kind(&self) -> &'static str {
        "Exec.IcebergConfig"
    }
    fn input_ports(&self) -> &[Port] {
        &EMPTY_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &POLICY_OUTPUT
    }
    fn config_schema(&self) -> Vec<crate::node::ConfigField> {
        use crate::node::{ConfigField, ConfigWidget};
        vec![ConfigField {
            name: "display_qty",
            label: "Display quantity",
            hint: Some("Base-asset slice shown on the book (the iceberg tip)"),
            default: serde_json::json!("0.1"),
            widget: ConfigWidget::Text,
        }]
    }
    fn evaluate(
        &self,
        _ctx: &EvalCtx,
        _inputs: &[Value],
        _state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        Ok(vec![Value::String(format!("iceberg:{}", self.display_qty))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(node: &dyn NodeKind) -> String {
        let mut st = NodeState::default();
        let out = node.evaluate(&EvalCtx::default(), &[], &mut st).unwrap();
        match &out[0] {
            Value::String(s) => s.clone(),
            v => panic!("expected String, got {v:?}"),
        }
    }

    #[test]
    fn twap_emits_policy_with_duration_and_slices() {
        let n =
            TwapConfig::from_config(&serde_json::json!({ "duration_secs": 60, "slice_count": 3 }))
                .unwrap();
        assert_eq!(policy(&n), "twap:60:3");
    }

    #[test]
    fn vwap_emits_policy_with_duration() {
        let n = VwapConfig::from_config(&serde_json::json!({ "duration_secs": 240 })).unwrap();
        assert_eq!(policy(&n), "vwap:240");
    }

    #[test]
    fn pov_emits_policy_with_pct() {
        let n = PovConfig::from_config(&serde_json::json!({ "target_pct": 15 })).unwrap();
        assert_eq!(policy(&n), "pov:15");
    }

    #[test]
    fn iceberg_emits_policy_with_display_qty() {
        let n = IcebergConfig::from_config(&serde_json::json!({ "display_qty": "0.25" })).unwrap();
        assert_eq!(policy(&n), "iceberg:0.25");
    }

    #[test]
    fn twap_rejects_zero_slices() {
        assert!(TwapConfig::from_config(&serde_json::json!({ "slice_count": 0 })).is_none());
    }

    #[test]
    fn pov_rejects_out_of_range_pct() {
        assert!(PovConfig::from_config(&serde_json::json!({ "target_pct": 150 })).is_none());
        assert!(PovConfig::from_config(&serde_json::json!({ "target_pct": 0 })).is_none());
    }

    #[test]
    fn iceberg_rejects_non_numeric_display_qty() {
        assert!(IcebergConfig::from_config(&serde_json::json!({ "display_qty": "abc" })).is_none());
    }
}
