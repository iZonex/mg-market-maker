//! Walks a [`FailLadder`] after authority loss.
//!
//! Once the control-plane silence begins — either the lease
//! expires or the controller explicitly revokes — the agent enters
//! a pre-shutdown phase where it must escalate through its
//! ladder. Each rung has an `after` duration; when that much
//! silence has accumulated, the rung's [`KillLevel`] action
//! fires exactly once.
//!
//! PR-2c-ii keeps actions abstract — the walker emits a
//! [`FailAction`] value (`WidenSpreads`, `StopNew`, `CancelAll`,
//! `Flatten`, `Disconnect`) and a tracing call logs what would
//! happen. PR-2c-iii replaces the tracing log with real
//! connector calls (cancel_all, flatten via TWAP, etc.) once the
//! agent owns an OrderManager per deployment.
//!
//! The walker is deliberately external to the runner's main
//! loop so tests can drive it with synthetic time without
//! needing tokio. The runner polls `poll_next_action(now)` on
//! each iteration and executes whatever the walker hands back.

use std::time::Duration;

use mm_control::fail_ladder::{FailLadder, KillLevel};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailAction {
    pub level: KillLevel,
    pub rung_index: usize,
}

pub struct FailLadderWalker {
    ladder: FailLadder,
    entered_at: std::time::Instant,
    /// Indices of rungs we have already fired. A rung fires
    /// exactly once — on re-entry we do not re-fire a level the
    /// agent has already escalated to.
    fired: Vec<bool>,
}

impl FailLadderWalker {
    pub fn start(ladder: FailLadder, entered_at: std::time::Instant) -> Self {
        let fired = vec![false; ladder.rungs().len()];
        Self {
            ladder,
            entered_at,
            fired,
        }
    }

    /// Consume elapsed time and return the highest rung whose
    /// `after` threshold has been crossed and not yet fired.
    /// Returns `None` when no rung is ready; the walker is NOT
    /// finished until `is_complete()` — a None just means "come
    /// back later".
    pub fn poll_at(&mut self, now: std::time::Instant) -> Option<FailAction> {
        let silence = now.saturating_duration_since(self.entered_at);
        // Walk from the highest rung down — firing the most
        // severe level first gives us "skip intermediates when
        // we're already past them" on resumed processes.
        for (idx, rung) in self.ladder.rungs().iter().enumerate().rev() {
            if silence >= rung.after && !self.fired[idx] {
                // Mark everything up to + including idx as fired
                // so we never revisit an earlier rung for this
                // silence episode.
                for f in self.fired.iter_mut().take(idx + 1) {
                    *f = true;
                }
                return Some(FailAction {
                    level: rung.level,
                    rung_index: idx,
                });
            }
        }
        None
    }

    /// True once every rung has fired. Runner exits its loop
    /// after observing this because no further escalation is
    /// possible — the deployment is as-flat-as-we-can-make-it.
    pub fn is_complete(&self) -> bool {
        self.fired.iter().all(|f| *f)
    }

    /// How long until the next rung would fire, or `None` if all
    /// rungs have fired. Runner uses this to size its next sleep
    /// interval so we don't busy-wait between rungs.
    pub fn next_rung_at(&self) -> Option<Duration> {
        self.ladder
            .rungs()
            .iter()
            .enumerate()
            .find(|(i, _)| !self.fired[*i])
            .map(|(_, r)| r.after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_control::fail_ladder::{FailRung, StrategyClass};

    fn ladder(rungs: &[(u64, KillLevel)]) -> FailLadder {
        FailLadder::from_rungs(
            rungs
                .iter()
                .map(|(s, l)| FailRung {
                    after: Duration::from_millis(*s),
                    level: *l,
                })
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn rungs_fire_in_order_as_silence_accumulates() {
        let l = ladder(&[
            (100, KillLevel::WidenSpreads),
            (300, KillLevel::StopNew),
            (900, KillLevel::Flatten),
        ]);
        let t0 = std::time::Instant::now();
        let mut w = FailLadderWalker::start(l, t0);

        assert!(w.poll_at(t0).is_none(), "no rung yet at t=0");
        assert_eq!(
            w.poll_at(t0 + Duration::from_millis(120)).unwrap().level,
            KillLevel::WidenSpreads
        );
        assert_eq!(
            w.poll_at(t0 + Duration::from_millis(350)).unwrap().level,
            KillLevel::StopNew
        );
        assert_eq!(
            w.poll_at(t0 + Duration::from_millis(950)).unwrap().level,
            KillLevel::Flatten
        );
        assert!(w.is_complete());
    }

    #[test]
    fn rung_fires_exactly_once() {
        let l = ladder(&[(100, KillLevel::WidenSpreads)]);
        let t0 = std::time::Instant::now();
        let mut w = FailLadderWalker::start(l, t0);

        assert!(w.poll_at(t0 + Duration::from_millis(150)).is_some());
        assert!(
            w.poll_at(t0 + Duration::from_millis(200)).is_none(),
            "rung must not re-fire"
        );
    }

    #[test]
    fn skipped_rungs_are_backfilled_at_catchup() {
        // The walker has been idle long enough to cross THREE
        // rungs at once — the system was paused, descheduled, or
        // the runner was late to poll. We should fire the
        // highest rung we crossed and mark all earlier ones as
        // fired so they don't re-fire later. The operator cares
        // about "what level are we at right now," not "did we
        // log WidenSpreads → StopNew → Flatten in sequence" when
        // the answer is already Flatten.
        let l = ladder(&[
            (100, KillLevel::WidenSpreads),
            (300, KillLevel::StopNew),
            (900, KillLevel::Flatten),
        ]);
        let t0 = std::time::Instant::now();
        let mut w = FailLadderWalker::start(l, t0);

        let action = w.poll_at(t0 + Duration::from_secs(10)).unwrap();
        assert_eq!(action.level, KillLevel::Flatten, "fires highest-crossed");
        assert!(w.is_complete(), "all rungs marked fired after catchup");
    }

    #[test]
    fn next_rung_at_tracks_progress() {
        let l = FailLadder::default_for(StrategyClass::Maker);
        let t0 = std::time::Instant::now();
        let mut w = FailLadderWalker::start(l, t0);
        let first = w.next_rung_at().unwrap();
        assert_eq!(first, Duration::from_secs(15));
        // After firing the first rung, the pointer advances.
        let _ = w.poll_at(t0 + Duration::from_secs(15));
        assert_eq!(w.next_rung_at().unwrap(), Duration::from_secs(60));
    }
}
