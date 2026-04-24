//! ⚠⚠⚠ R6.1 — Campaign orchestrator. **PENTEST ONLY.**
//!
//! Time-based multi-phase FSM that chains the documented
//! RAVE-pattern sub-behaviours end-to-end. Where
//! [`crate::pump_and_dump::PumpAndDumpStrategy`] uses tick
//! counts (simple but flaky under variable tick cadence), this
//! orchestrator uses wall-clock seconds — the operator
//! configures `(phase, duration_secs)` pairs once and the FSM
//! replays them exactly regardless of tick rate.
//!
//! # Phases
//!
//! ```text
//!   Accumulate → Pump → Distribute → Dump → Idle
//! ```
//!
//! Every phase is optional via a zero duration. Operators
//! experimenting with one-shot liquidity hunts might configure
//! only `(Pump, 30s) → (Idle, ∞)`; a full RAVE replay uses all
//! five. The `Idle` terminal phase emits nothing — useful as a
//! "stop after campaign" so the paired `Surveillance.RugScore`
//! guard has the same observed-tape window to fire against
//! post-mortem.
//!
//! # Why restricted
//!
//! Same reasons every other `Strategy.*` node in the `Exploit`
//! catalog group is restricted: running this against any venue
//! you are not explicitly authorized to pentest is illegal
//! under MiFID II, Dodd-Frank / SEA §9(a), MiCA, and a ToS
//! violation everywhere. The `restricted()=true` flag on the
//! graph node + the `MM_ALLOW_RESTRICTED=yes-pentest-mode` env gate stops the
//! graph from compiling without operator opt-in. See
//! `docs/guides/pentest.md` for the three operator conditions.

use chrono::{DateTime, Utc};
use mm_common::types::{Quote, QuotePair, Side};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Mutex;

use crate::r#trait::{Strategy, StrategyContext};

/// Phase in the campaign FSM. Mirrors
/// `PumpDumpPhase` plus a terminal `Idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CampaignPhase {
    Accumulate,
    Pump,
    Distribute,
    Dump,
    Idle,
}

#[derive(Debug, Clone)]
pub struct CampaignOrchestratorConfig {
    /// Seconds spent in each phase. `0` skips the phase.
    pub accumulate_secs: u64,
    pub pump_secs: u64,
    pub distribute_secs: u64,
    pub dump_secs: u64,

    // Sizing + depth knobs. Kept separate from PumpAndDumpConfig
    // so each pentest node has its own tuning surface.
    pub accumulate_size: Decimal,
    pub accumulate_offset_bps: Decimal,
    pub pump_size: Decimal,
    pub pump_depth_bps: Decimal,
    pub distribute_size: Decimal,
    pub distribute_offset_bps: Decimal,
    pub distribute_rungs: u32,
    pub distribute_step_bps: Decimal,
    pub dump_size: Decimal,
    pub dump_depth_bps: Decimal,

    /// Wrap the FSM and restart from `Accumulate` after `Dump`
    /// instead of terminating in `Idle`. Useful for repeated
    /// smoke runs in a test harness.
    pub loop_cycle: bool,
}

impl Default for CampaignOrchestratorConfig {
    fn default() -> Self {
        Self {
            // Roughly mirrors the published RAVE-pattern timeline
            // (accumulate days, pump hours, distribute hours,
            // dump minutes) scaled down for pentest smoke runs.
            accumulate_secs: 600,
            pump_secs: 120,
            distribute_secs: 600,
            dump_secs: 120,

            accumulate_size: dec!(0.002),
            accumulate_offset_bps: dec!(5),
            pump_size: dec!(0.002),
            pump_depth_bps: dec!(50),
            distribute_size: dec!(0.001),
            distribute_offset_bps: dec!(20),
            distribute_rungs: 4,
            distribute_step_bps: dec!(15),
            dump_size: dec!(0.002),
            dump_depth_bps: dec!(60),

            loop_cycle: false,
        }
    }
}

#[derive(Debug)]
pub struct CampaignOrchestratorStrategy {
    pub config: CampaignOrchestratorConfig,
    /// First-tick timestamp, stamped once. `Mutex` is cheap —
    /// the `Strategy` trait is `Send + Sync`, the trait's
    /// receiver is `&self`, and we only lock once per tick.
    first_tick_at: Mutex<Option<DateTime<Utc>>>,
}

impl CampaignOrchestratorStrategy {
    pub fn new() -> Self {
        Self::with_config(CampaignOrchestratorConfig::default())
    }

    pub fn with_config(config: CampaignOrchestratorConfig) -> Self {
        Self {
            config,
            first_tick_at: Mutex::new(None),
        }
    }

    fn cycle_len(&self) -> u64 {
        self.config.accumulate_secs
            + self.config.pump_secs
            + self.config.distribute_secs
            + self.config.dump_secs
    }

    /// Determine the active phase given `elapsed_secs`.
    pub fn phase_at(&self, elapsed_secs: u64) -> CampaignPhase {
        let cycle = self.cycle_len();
        if cycle == 0 {
            return CampaignPhase::Idle;
        }
        let t = if self.config.loop_cycle {
            elapsed_secs % cycle
        } else if elapsed_secs >= cycle {
            return CampaignPhase::Idle;
        } else {
            elapsed_secs
        };
        let a = self.config.accumulate_secs;
        let p = a + self.config.pump_secs;
        let d = p + self.config.distribute_secs;
        if t < a {
            CampaignPhase::Accumulate
        } else if t < p {
            CampaignPhase::Pump
        } else if t < d {
            CampaignPhase::Distribute
        } else {
            CampaignPhase::Dump
        }
    }

    /// Current phase based on `first_tick_at` + now.
    pub fn current_phase(&self, now: DateTime<Utc>) -> CampaignPhase {
        let start = self.first_tick_at.lock().ok().and_then(|g| *g);
        match start {
            Some(s) => {
                let elapsed = (now - s).num_seconds().max(0) as u64;
                self.phase_at(elapsed)
            }
            None => CampaignPhase::Accumulate, // pre-stamp → treat as start
        }
    }
}

impl Default for CampaignOrchestratorStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for CampaignOrchestratorStrategy {
    fn name(&self) -> &str {
        "campaign_orchestrator"
    }

    fn compute_quotes(&self, ctx: &StrategyContext) -> Vec<QuotePair> {
        let mid = ctx.mid_price;
        if mid <= Decimal::ZERO {
            return Vec::new();
        }

        let now = Utc::now();
        // Stamp on the first invocation; subsequent calls just
        // read. Never fight the Mutex if it happens to be
        // poisoned — degrade to Accumulate.
        if let Ok(mut g) = self.first_tick_at.lock() {
            if g.is_none() {
                *g = Some(now);
            }
        }

        let phase = self.current_phase(now);
        match phase {
            CampaignPhase::Accumulate => {
                let offset = mid * self.config.accumulate_offset_bps / dec!(10_000);
                let price = ctx.product.round_price((mid - offset).max(Decimal::ZERO));
                let qty = ctx.product.round_qty(self.config.accumulate_size);
                if price.is_zero() || !ctx.product.meets_min_notional(price, qty) {
                    return Vec::new();
                }
                vec![QuotePair {
                    bid: Some(Quote {
                        side: Side::Buy,
                        price,
                        qty,
                    }),
                    ask: None,
                }]
            }
            CampaignPhase::Pump => {
                let cross = mid * self.config.pump_depth_bps / dec!(10_000);
                let price = ctx.product.round_price(mid + cross);
                let qty = ctx.product.round_qty(self.config.pump_size);
                if !ctx.product.meets_min_notional(price, qty) {
                    return Vec::new();
                }
                vec![QuotePair {
                    bid: Some(Quote {
                        side: Side::Buy,
                        price,
                        qty,
                    }),
                    ask: None,
                }]
            }
            CampaignPhase::Distribute => {
                let rungs = self.config.distribute_rungs.max(1) as i64;
                let base_offset = mid * self.config.distribute_offset_bps / dec!(10_000);
                let step = mid * self.config.distribute_step_bps / dec!(10_000);
                let qty = ctx.product.round_qty(self.config.distribute_size);
                let mut out = Vec::with_capacity(rungs as usize);
                for r in 0..rungs {
                    let price = ctx
                        .product
                        .round_price(mid + base_offset + step * Decimal::from(r));
                    if price <= mid || !ctx.product.meets_min_notional(price, qty) {
                        continue;
                    }
                    out.push(QuotePair {
                        bid: None,
                        ask: Some(Quote {
                            side: Side::Sell,
                            price,
                            qty,
                        }),
                    });
                }
                out
            }
            CampaignPhase::Dump => {
                let cross = mid * self.config.dump_depth_bps / dec!(10_000);
                let price = ctx.product.round_price((mid - cross).max(Decimal::ZERO));
                let qty = ctx.product.round_qty(self.config.dump_size);
                if price.is_zero() || !ctx.product.meets_min_notional(price, qty) {
                    return Vec::new();
                }
                vec![QuotePair {
                    bid: None,
                    ask: Some(Quote {
                        side: Side::Sell,
                        price,
                        qty,
                    }),
                }]
            }
            CampaignPhase::Idle => Vec::new(),
        }
    }

    /// 22B-5 — persist `first_tick_at` so a crash mid-campaign
    /// doesn't rewind the timeline to Accumulate. Operators
    /// running a 4-phase attack simulation need the FSM to
    /// resume at the same wall-clock offset after a restart —
    /// otherwise the detectors downstream see a fresh
    /// Accumulate and the detection→kill loop fires against an
    /// already-completed phase.
    fn checkpoint_state(&self) -> Option<serde_json::Value> {
        let start = self.first_tick_at.lock().ok().and_then(|g| *g);
        Some(serde_json::json!({
            "schema_version": 1,
            // Epoch millis — serialisable and round-trips
            // cleanly via chrono's from_timestamp_millis.
            "first_tick_ms": start.map(|t| t.timestamp_millis()),
        }))
    }

    fn restore_state(&self, state: &serde_json::Value) -> Result<(), String> {
        let schema = state.get("schema_version").and_then(|v| v.as_u64());
        if schema != Some(1) {
            return Err(format!(
                "campaign_orchestrator checkpoint has unsupported schema_version {schema:?}"
            ));
        }
        let first_ms = state.get("first_tick_ms").and_then(|v| v.as_i64());
        let first = first_ms.and_then(chrono::DateTime::<Utc>::from_timestamp_millis);
        let mut g = self
            .first_tick_at
            .lock()
            .map_err(|_| "campaign_orchestrator: mutex poisoned".to_string())?;
        *g = first;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_at_walks_timeline_and_terminates_in_idle() {
        let s = CampaignOrchestratorStrategy::with_config(CampaignOrchestratorConfig {
            accumulate_secs: 10,
            pump_secs: 5,
            distribute_secs: 20,
            dump_secs: 5,
            loop_cycle: false,
            ..CampaignOrchestratorConfig::default()
        });
        assert_eq!(s.phase_at(0), CampaignPhase::Accumulate);
        assert_eq!(s.phase_at(9), CampaignPhase::Accumulate);
        assert_eq!(s.phase_at(10), CampaignPhase::Pump);
        assert_eq!(s.phase_at(14), CampaignPhase::Pump);
        assert_eq!(s.phase_at(15), CampaignPhase::Distribute);
        assert_eq!(s.phase_at(34), CampaignPhase::Distribute);
        assert_eq!(s.phase_at(35), CampaignPhase::Dump);
        assert_eq!(s.phase_at(39), CampaignPhase::Dump);
        // Past the full cycle → Idle (non-loop).
        assert_eq!(s.phase_at(40), CampaignPhase::Idle);
        assert_eq!(s.phase_at(1000), CampaignPhase::Idle);
    }

    #[test]
    fn loop_cycle_wraps_instead_of_idling() {
        let s = CampaignOrchestratorStrategy::with_config(CampaignOrchestratorConfig {
            accumulate_secs: 1,
            pump_secs: 1,
            distribute_secs: 1,
            dump_secs: 1,
            loop_cycle: true,
            ..CampaignOrchestratorConfig::default()
        });
        assert_eq!(s.phase_at(0), CampaignPhase::Accumulate);
        assert_eq!(s.phase_at(3), CampaignPhase::Dump);
        // Wraps — tick 4 is a new Accumulate.
        assert_eq!(s.phase_at(4), CampaignPhase::Accumulate);
        assert_eq!(s.phase_at(7), CampaignPhase::Dump);
        // Still wraps forever, never Idle.
        // cycle=4, so t=10_001 wraps to 1 → Pump.
        assert_eq!(s.phase_at(10_001), CampaignPhase::Pump);
        // And t=10_000 wraps to 0 → Accumulate.
        assert_eq!(s.phase_at(10_000), CampaignPhase::Accumulate);
    }

    #[test]
    fn zero_duration_config_is_pure_idle() {
        let s = CampaignOrchestratorStrategy::with_config(CampaignOrchestratorConfig {
            accumulate_secs: 0,
            pump_secs: 0,
            distribute_secs: 0,
            dump_secs: 0,
            ..CampaignOrchestratorConfig::default()
        });
        assert_eq!(s.phase_at(0), CampaignPhase::Idle);
        assert_eq!(s.phase_at(9999), CampaignPhase::Idle);
    }

    /// 22B-5 — first_tick_at survives round trip through the
    /// checkpoint. The restored strategy returns the same
    /// `current_phase(now)` as the source at the same `now`.
    #[test]
    fn first_tick_round_trip() {
        let src = CampaignOrchestratorStrategy::new();
        let t0 = Utc::now();
        {
            let mut g = src.first_tick_at.lock().unwrap();
            *g = Some(t0);
        }
        let snap = src.checkpoint_state().expect("has state");

        let dst = CampaignOrchestratorStrategy::new();
        dst.restore_state(&snap).unwrap();
        let got = dst
            .first_tick_at
            .lock()
            .unwrap()
            .map(|t| t.timestamp_millis());
        assert_eq!(got, Some(t0.timestamp_millis()));
    }

    /// 22B-5 — None first_tick_at round-trips as null.
    #[test]
    fn unset_first_tick_round_trip() {
        let src = CampaignOrchestratorStrategy::new();
        let snap = src.checkpoint_state().expect("has state");
        let dst = CampaignOrchestratorStrategy::new();
        dst.restore_state(&snap).unwrap();
        assert!(dst.first_tick_at.lock().unwrap().is_none());
    }

    #[test]
    fn restore_rejects_wrong_schema() {
        let s = CampaignOrchestratorStrategy::new();
        let bogus = serde_json::json!({
            "schema_version": 77,
            "first_tick_ms": null,
        });
        assert!(s.restore_state(&bogus).is_err());
    }
}
