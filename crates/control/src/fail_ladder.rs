//! Progressive failure response ladder.
//!
//! When the agent detects control-plane silence (heartbeat loss
//! AND grace expiry) it walks a ladder rather than firing one
//! action. Each rung specifies a silence duration and the kill
//! level to escalate to. The ladder halts at the last rung — once
//! the agent is flat there is nothing left to escalate to.
//!
//! Default ladders are class-specific because maker and driver
//! strategies have different failure profiles:
//!
//! - **Makers** (Avellaneda, GLFT, Grid, CrossExchange) live on
//!   spread. A live quote is defended by its spread; widening
//!   cheaply while we wait for controller is safer than paying cancel
//!   fees and re-posting when control returns. Only escalate to
//!   cancel / flatten after prolonged silence.
//! - **Drivers** (funding_arb, stat_arb, basis) dispatch discrete
//!   two-leg bundles. A half-filled bundle mid-silence leaves an
//!   unhedged position — widening is meaningless, the dangerous
//!   state is "hedge leg never fires". Halt dispatch immediately,
//!   cancel pending legs on grace expiry.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Which strategy class a deployed strategy belongs to — selects
/// the default ladder. Per-strategy override always wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyClass {
    /// Produces live quotes every tick (Avellaneda, GLFT, Grid,
    /// CrossExchange).
    Maker,
    /// Fires discrete atomic bundles on its own cadence
    /// (funding_arb, stat_arb, basis).
    Driver,
}

/// The kill level an agent reaches when a rung fires. Mirrors
/// the 5-level kill switch that already exists on the engine
/// (see `crates/risk/src/kill_switch`) so the agent's local
/// fail-ladder and the operator-initiated kill share one enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KillLevel {
    /// Normal operation.
    Normal,
    /// Widen quotes — keeps orders alive at safer prices.
    WidenSpreads,
    /// Refuse to place new orders; existing stay.
    StopNew,
    /// Cancel all resting orders; positions held.
    CancelAll,
    /// Cancel all and flatten positions via TWAP / market orders.
    Flatten,
    /// Hard disconnect; no further venue IO.
    Disconnect,
}

/// One step of the ladder: after this much silence, escalate to
/// this kill level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailRung {
    pub after: Duration,
    pub level: KillLevel,
}

/// Ordered sequence of rungs, strictly increasing by `after`.
/// Construction validates the invariant; later mutation should
/// re-validate via [`FailLadder::from_rungs`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailLadder {
    rungs: Vec<FailRung>,
}

#[derive(Debug, thiserror::Error)]
pub enum LadderError {
    #[error("fail ladder must have at least one rung")]
    Empty,
    #[error("rungs must be strictly increasing by duration")]
    NotMonotonic,
}

impl FailLadder {
    pub fn from_rungs(rungs: Vec<FailRung>) -> Result<Self, LadderError> {
        if rungs.is_empty() {
            return Err(LadderError::Empty);
        }
        let monotonic = rungs.windows(2).all(|w| w[0].after < w[1].after);
        if !monotonic {
            return Err(LadderError::NotMonotonic);
        }
        Ok(Self { rungs })
    }

    pub fn rungs(&self) -> &[FailRung] {
        &self.rungs
    }

    /// Select the rung corresponding to `silence` — the highest
    /// rung whose `after` is ≤ silence, or `None` if silence
    /// hasn't yet crossed the first rung.
    pub fn level_at(&self, silence: Duration) -> Option<KillLevel> {
        self.rungs
            .iter()
            .rev()
            .find(|r| silence >= r.after)
            .map(|r| r.level)
    }

    /// The canonical default ladder for a maker strategy.
    pub fn default_maker() -> Self {
        Self::from_rungs(vec![
            FailRung {
                after: Duration::from_secs(15),
                level: KillLevel::WidenSpreads,
            },
            FailRung {
                after: Duration::from_secs(60),
                level: KillLevel::StopNew,
            },
            FailRung {
                after: Duration::from_secs(300),
                level: KillLevel::Flatten,
            },
        ])
        .expect("static ladder is valid")
    }

    /// The canonical default ladder for a driver strategy.
    /// Drivers can't widen — halt dispatch is the first-line
    /// response, modelled as `StopNew` on the unified kill enum.
    pub fn default_driver() -> Self {
        Self::from_rungs(vec![
            FailRung {
                after: Duration::from_secs(15),
                level: KillLevel::StopNew,
            },
            FailRung {
                after: Duration::from_secs(60),
                level: KillLevel::CancelAll,
            },
            FailRung {
                after: Duration::from_secs(300),
                level: KillLevel::Flatten,
            },
        ])
        .expect("static ladder is valid")
    }

    pub fn default_for(class: StrategyClass) -> Self {
        match class {
            StrategyClass::Maker => Self::default_maker(),
            StrategyClass::Driver => Self::default_driver(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_maker_has_expected_shape() {
        let l = FailLadder::default_maker();
        assert_eq!(l.rungs().len(), 3);
        assert_eq!(l.rungs()[0].level, KillLevel::WidenSpreads);
        assert_eq!(l.rungs()[2].level, KillLevel::Flatten);
    }

    #[test]
    fn default_driver_first_step_is_stop() {
        let l = FailLadder::default_driver();
        assert_eq!(l.rungs()[0].level, KillLevel::StopNew);
    }

    #[test]
    fn level_at_picks_highest_crossed_rung() {
        let l = FailLadder::default_maker();
        assert_eq!(l.level_at(Duration::from_secs(5)), None);
        assert_eq!(
            l.level_at(Duration::from_secs(15)),
            Some(KillLevel::WidenSpreads)
        );
        assert_eq!(
            l.level_at(Duration::from_secs(59)),
            Some(KillLevel::WidenSpreads)
        );
        assert_eq!(
            l.level_at(Duration::from_secs(60)),
            Some(KillLevel::StopNew)
        );
        assert_eq!(
            l.level_at(Duration::from_secs(301)),
            Some(KillLevel::Flatten)
        );
    }

    #[test]
    fn non_monotonic_rejected() {
        let err = FailLadder::from_rungs(vec![
            FailRung {
                after: Duration::from_secs(60),
                level: KillLevel::StopNew,
            },
            FailRung {
                after: Duration::from_secs(30),
                level: KillLevel::WidenSpreads,
            },
        ]);
        assert!(matches!(err, Err(LadderError::NotMonotonic)));
    }

    #[test]
    fn empty_rejected() {
        assert!(matches!(
            FailLadder::from_rungs(vec![]),
            Err(LadderError::Empty)
        ));
    }
}
