//! MM-3 — Execution plan nodes.
//!
//! Closes the "no execution plans" gap flagged by the user:
//! instead of pure tick-by-tick reactive quoting, the operator
//! can author a long-horizon plan like "accumulate 5 BTC over
//! 4 hours with 0.25 BTC per slice, abort if mid ≥ 80 000".
//!
//! ## Design
//!
//! `Plan.Accumulate` is a graph node with:
//! - one input: `mid: Number` (for the abort-price check)
//! - one output: `quotes: Quotes` (the slice emitted this tick)
//! - node-local state (`PlanState`) carrying `started_at_ms`,
//!   `qty_emitted`, `aborted`. Lives inside `NodeState` so graph
//!   swaps reset it cleanly.
//!
//! The slice schedule is open-loop time-based for MVP — the node
//! walks the elapsed-fraction of its horizon and emits the next
//! `slice_qty` when `qty_emitted < total * elapsed/duration`.
//! Fill-aware closed-loop (consume from `FillObservation` via the
//! MM-2 `on_fill` hook) lands with MM-4 when the per-decision
//! cost ledger adds the order_id → plan_id chain.
//!
//! Emitted quotes sit `post_offset_bps` behind mid by default
//! (passive post), so the slice is routed through the normal
//! PostOnly hot path. Operators who want aggressive execution
//! drop a `Math.Mul` + `Book.L1` composite in front and cross
//! the touch themselves; that's a separate pattern, not a knob
//! on the plan.

use anyhow::Result;
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::Value as Json;
use std::str::FromStr;

use crate::node::{
    ConfigEnumOption, ConfigField, ConfigWidget, EvalCtx, NodeKind, NodeState,
};
use crate::types::{GraphQuote, Port, PortType, QuoteSide, Value};

/// Per-node mutable state. Lives inside `NodeState` so graph
/// swaps reset it cleanly (see `Evaluator::build` which clears
/// per-node state).
#[derive(Debug, Default, Clone)]
pub struct PlanState {
    pub started_at_ms: Option<i64>,
    pub qty_emitted: Decimal,
    pub aborted: bool,
    pub last_slice_ms: i64,
}

/// `Plan.Accumulate` — time-sliced accumulation plan. Emits one
/// slice per tick when the elapsed fraction of the horizon says
/// we're behind schedule.
#[derive(Debug, Clone)]
pub struct Accumulate {
    side: QuoteSide,
    total_qty: Decimal,
    duration_secs: i64,
    slice_qty: Decimal,
    post_offset_bps: Decimal,
    /// Abort when `mid >= this` (for an accumulating buy — don't
    /// chase into a spike). `None` disables.
    abort_price_ge: Option<Decimal>,
    /// Abort when `mid <= this` (for a distributing sell — don't
    /// bail into a crash). `None` disables.
    abort_price_le: Option<Decimal>,
    /// Minimum gap between slices (ms). Extra throttle on top of
    /// the elapsed-schedule check so a slow graph tick doesn't
    /// bunch three slices into one refresh.
    min_slice_gap_ms: i64,
}

impl Default for Accumulate {
    fn default() -> Self {
        Self {
            side: QuoteSide::Buy,
            total_qty: dec!(0),
            duration_secs: 3_600,
            slice_qty: dec!(0.001),
            post_offset_bps: dec!(1),
            abort_price_ge: None,
            abort_price_le: None,
            min_slice_gap_ms: 500,
        }
    }
}

#[derive(Deserialize)]
struct AccumulateCfg {
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    total_qty: Option<String>,
    #[serde(default)]
    duration_secs: Option<i64>,
    #[serde(default)]
    slice_qty: Option<String>,
    #[serde(default)]
    post_offset_bps: Option<String>,
    #[serde(default)]
    abort_price_ge: Option<String>,
    #[serde(default)]
    abort_price_le: Option<String>,
    #[serde(default)]
    min_slice_gap_ms: Option<i64>,
}

impl Accumulate {
    pub fn from_config(cfg: &Json) -> Option<Self> {
        if cfg.is_null() {
            return Some(Self::default());
        }
        let parsed: AccumulateCfg = serde_json::from_value(cfg.clone()).ok()?;
        let mut a = Self::default();
        if let Some(s) = parsed.side {
            a.side = match s.as_str() {
                "sell" => QuoteSide::Sell,
                _ => QuoteSide::Buy,
            };
        }
        if let Some(s) = parsed.total_qty {
            a.total_qty = Decimal::from_str(&s).ok()?;
        }
        if let Some(d) = parsed.duration_secs {
            if d <= 0 {
                return None;
            }
            a.duration_secs = d;
        }
        if let Some(s) = parsed.slice_qty {
            let q = Decimal::from_str(&s).ok()?;
            if q <= Decimal::ZERO {
                return None;
            }
            a.slice_qty = q;
        }
        if let Some(s) = parsed.post_offset_bps {
            a.post_offset_bps = Decimal::from_str(&s).ok()?;
        }
        if let Some(s) = parsed.abort_price_ge {
            a.abort_price_ge = Some(Decimal::from_str(&s).ok()?);
        }
        if let Some(s) = parsed.abort_price_le {
            a.abort_price_le = Some(Decimal::from_str(&s).ok()?);
        }
        if let Some(g) = parsed.min_slice_gap_ms {
            if g < 0 {
                return None;
            }
            a.min_slice_gap_ms = g;
        }
        Some(a)
    }
}

static PLAN_INPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("mid", PortType::Number)]);
static PLAN_OUTPUTS: Lazy<Vec<Port>> =
    Lazy::new(|| vec![Port::new("quotes", PortType::Quotes)]);

impl NodeKind for Accumulate {
    fn kind(&self) -> &'static str {
        "Plan.Accumulate"
    }
    fn input_ports(&self) -> &[Port] {
        &PLAN_INPUTS
    }
    fn output_ports(&self) -> &[Port] {
        &PLAN_OUTPUTS
    }
    fn evaluate(
        &self,
        ctx: &EvalCtx,
        inputs: &[Value],
        state: &mut NodeState,
    ) -> Result<Vec<Value>> {
        let ps = state.get_or_insert_default::<PlanState>();

        // Total of 0 → nothing to do. Same for already-aborted or
        // already-finished plans.
        if self.total_qty <= Decimal::ZERO || ps.aborted {
            return Ok(vec![Value::Quotes(Vec::new())]);
        }
        if ps.qty_emitted >= self.total_qty {
            return Ok(vec![Value::Quotes(Vec::new())]);
        }

        // First call — latch the start time so the schedule is
        // measured from the first tick the plan sees, not from
        // engine boot.
        if ps.started_at_ms.is_none() {
            ps.started_at_ms = Some(ctx.now_ms);
        }
        let started = ps.started_at_ms.unwrap_or(ctx.now_ms);

        // Abort-price check. Mid may be `Missing` (source not
        // overlayed yet) — stay idle rather than racing past the
        // guard. Same for non-numeric values; treat as missing.
        let mid = match inputs.first() {
            Some(Value::Number(m)) => *m,
            _ => return Ok(vec![Value::Quotes(Vec::new())]),
        };
        if mid <= Decimal::ZERO {
            return Ok(vec![Value::Quotes(Vec::new())]);
        }
        if let Some(ge) = self.abort_price_ge {
            if mid >= ge {
                ps.aborted = true;
                return Ok(vec![Value::Quotes(Vec::new())]);
            }
        }
        if let Some(le) = self.abort_price_le {
            if mid <= le {
                ps.aborted = true;
                return Ok(vec![Value::Quotes(Vec::new())]);
            }
        }

        // Min-gap throttle. Keeps bursty tick rates from emitting
        // multiple slices in one refresh cycle.
        if self.min_slice_gap_ms > 0
            && ps.last_slice_ms > 0
            && ctx.now_ms - ps.last_slice_ms < self.min_slice_gap_ms
        {
            return Ok(vec![Value::Quotes(Vec::new())]);
        }

        // Schedule check — elapsed fraction of the horizon says
        // we should be this far along.
        let elapsed_ms = (ctx.now_ms - started).max(0);
        let duration_ms = self.duration_secs * 1_000;
        if duration_ms <= 0 {
            return Ok(vec![Value::Quotes(Vec::new())]);
        }
        let frac_elapsed_bps = Decimal::from(elapsed_ms)
            / Decimal::from(duration_ms)
            * dec!(10_000);
        let target_frac = (frac_elapsed_bps / dec!(10_000)).min(Decimal::ONE);
        let target_qty_by_now = self.total_qty * target_frac;

        // Behind schedule → emit a slice. On the first tick
        // (target ≈ 0) we skip, unless total - emitted would
        // leave us stranded at plan end — just wait for the
        // schedule to catch up on the next tick.
        if ps.qty_emitted + self.slice_qty / dec!(2) >= target_qty_by_now {
            return Ok(vec![Value::Quotes(Vec::new())]);
        }

        let slice = self
            .slice_qty
            .min(self.total_qty - ps.qty_emitted)
            .max(Decimal::ZERO);
        if slice <= Decimal::ZERO {
            return Ok(vec![Value::Quotes(Vec::new())]);
        }

        // Post price: `post_offset_bps` behind mid on the
        // accumulating side.
        let offset = mid * self.post_offset_bps / dec!(10_000);
        let price = match self.side {
            QuoteSide::Buy => mid - offset,
            QuoteSide::Sell => mid + offset,
        };
        ps.qty_emitted += slice;
        ps.last_slice_ms = ctx.now_ms;

        Ok(vec![Value::Quotes(vec![GraphQuote {
            side: self.side,
            price,
            qty: slice,
        }])])
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField {
                name: "side",
                label: "Side",
                hint: Some("buy = accumulate, sell = distribute"),
                default: serde_json::json!("buy"),
                widget: ConfigWidget::Enum {
                    options: vec![
                        ConfigEnumOption { value: "buy", label: "Accumulate (buy)" },
                        ConfigEnumOption { value: "sell", label: "Distribute (sell)" },
                    ],
                },
            },
            ConfigField {
                name: "total_qty",
                label: "Target size",
                hint: Some("Total base-asset qty to accumulate"),
                default: serde_json::json!("1"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "duration_secs",
                label: "Horizon (s)",
                hint: Some("Time window over which to slice"),
                default: serde_json::json!(3600),
                widget: ConfigWidget::Integer { min: Some(1), max: Some(86_400 * 30) },
            },
            ConfigField {
                name: "slice_qty",
                label: "Per-slice qty",
                hint: Some("Size of each slice when behind schedule"),
                default: serde_json::json!("0.05"),
                widget: ConfigWidget::Number { min: Some(0.0), max: None, step: Some(0.001) },
            },
            ConfigField {
                name: "post_offset_bps",
                label: "Post offset (bps)",
                hint: Some("How far behind mid to post each slice"),
                default: serde_json::json!("1"),
                widget: ConfigWidget::Number { min: Some(0.0), max: Some(500.0), step: Some(0.5) },
            },
            ConfigField {
                name: "abort_price_ge",
                label: "Abort if mid ≥",
                hint: Some("Stop accumulating on a spike"),
                default: serde_json::json!(""),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "abort_price_le",
                label: "Abort if mid ≤",
                hint: Some("Stop distributing on a crash"),
                default: serde_json::json!(""),
                widget: ConfigWidget::Text,
            },
            ConfigField {
                name: "min_slice_gap_ms",
                label: "Min gap between slices (ms)",
                hint: Some("Throttle to avoid bursty slice emission"),
                default: serde_json::json!(500),
                widget: ConfigWidget::Integer { min: Some(0), max: Some(60_000) },
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(
        node: &Accumulate,
        state: &mut NodeState,
        now_ms: i64,
        mid: Decimal,
    ) -> Vec<GraphQuote> {
        let ctx = EvalCtx { now_ms };
        let out = node
            .evaluate(&ctx, &[Value::Number(mid)], state)
            .expect("evaluate");
        match &out[0] {
            Value::Quotes(q) => q.clone(),
            other => panic!("expected Quotes, got {other:?}"),
        }
    }

    fn fast_plan() -> Accumulate {
        Accumulate {
            side: QuoteSide::Buy,
            total_qty: dec!(1),
            duration_secs: 10,
            slice_qty: dec!(0.1),
            post_offset_bps: dec!(2),
            abort_price_ge: None,
            abort_price_le: None,
            min_slice_gap_ms: 0,
        }
    }

    #[test]
    fn first_tick_latches_start_time_no_slice() {
        let p = fast_plan();
        let mut state = NodeState::default();
        // Elapsed = 0 → target qty = 0 → no slice yet.
        let q = eval(&p, &mut state, 0, dec!(100));
        assert!(q.is_empty());
        let ps = state.get::<PlanState>().unwrap();
        assert_eq!(ps.started_at_ms, Some(0));
    }

    #[test]
    fn slice_emitted_when_behind_schedule() {
        let p = fast_plan();
        let mut state = NodeState::default();
        // Latch start.
        eval(&p, &mut state, 0, dec!(100));
        // Jump 5s forward → target = 0.5, emitted = 0 → emit one
        // slice of 0.1.
        let q = eval(&p, &mut state, 5_000, dec!(100));
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].qty, dec!(0.1));
        assert_eq!(q[0].side, QuoteSide::Buy);
        // Post at mid - 2bps = 100 - 0.02 = 99.98.
        assert_eq!(q[0].price, dec!(99.98));
        let ps = state.get::<PlanState>().unwrap();
        assert_eq!(ps.qty_emitted, dec!(0.1));
    }

    #[test]
    fn abort_ge_trips_and_stays() {
        let mut p = fast_plan();
        p.abort_price_ge = Some(dec!(105));
        let mut state = NodeState::default();
        eval(&p, &mut state, 0, dec!(100));
        // Price spikes past the guard → abort.
        let q = eval(&p, &mut state, 5_000, dec!(106));
        assert!(q.is_empty());
        assert!(state.get::<PlanState>().unwrap().aborted);
        // Even after the spike resolves the plan stays aborted.
        let q = eval(&p, &mut state, 6_000, dec!(99));
        assert!(q.is_empty());
        assert!(state.get::<PlanState>().unwrap().aborted);
    }

    #[test]
    fn completes_after_all_slices_emitted() {
        let p = fast_plan();
        let mut state = NodeState::default();
        eval(&p, &mut state, 0, dec!(100));
        // Walk through the horizon in 11 ticks — by t = 10s we
        // should have emitted every 0.1 slice for total = 1.
        for sec in 1..=11 {
            eval(&p, &mut state, sec * 1_000, dec!(100));
        }
        let ps = state.get::<PlanState>().unwrap();
        assert_eq!(ps.qty_emitted, dec!(1));
        // Past the horizon the plan is idle.
        let q = eval(&p, &mut state, 20_000, dec!(100));
        assert!(q.is_empty());
    }

    #[test]
    fn min_gap_throttles_bursty_ticks() {
        let mut p = fast_plan();
        p.min_slice_gap_ms = 1_000;
        let mut state = NodeState::default();
        eval(&p, &mut state, 0, dec!(100));
        // Two slices in the same 500ms window — only the first
        // should emit.
        eval(&p, &mut state, 5_000, dec!(100)); // emits
        let q2 = eval(&p, &mut state, 5_400, dec!(100));
        assert!(q2.is_empty(), "second slice within gap was emitted");
    }

    #[test]
    fn sell_side_distributes_above_mid() {
        let mut p = fast_plan();
        p.side = QuoteSide::Sell;
        let mut state = NodeState::default();
        eval(&p, &mut state, 0, dec!(100));
        let q = eval(&p, &mut state, 5_000, dec!(100));
        assert_eq!(q[0].side, QuoteSide::Sell);
        assert_eq!(q[0].price, dec!(100.02));
    }

    #[test]
    fn config_parser_rejects_zero_slice() {
        let cfg = serde_json::json!({ "slice_qty": "0" });
        assert!(Accumulate::from_config(&cfg).is_none());
    }
}
