//! Per-deployment detail ring buffers.
//!
//! Wave 1 R follow-up — the drilldown telemetry row carries
//! scalars (last funding-arb state, inflight bundle count,
//! calibration snapshot). Some panels want *history*: the last
//! 20 funding-arb events, the last N SOR decisions, the last
//! batch of atomic bundles. Those arrays are too heavy for a
//! 1 Hz telemetry tick so they live here: a process-global ring
//! buffer the engine writes to and the agent reads on-demand
//! when serving `FetchDeploymentDetails` commands.
//!
//! The store is keyed by `symbol` because that's the identifier
//! the engine already has on hand. The agent correlates symbols
//! back to deployment_ids via the registry when building the
//! reply payload.
//!
//! Cap size: each symbol's ring buffer is hard-limited (default
//! 20 entries). Older entries silently drop off the front so
//! memory stays bounded no matter how long the engine runs.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

/// One funding-arb driver event remembered for the details
/// endpoint. Fields match `DriverEvent` outcomes but carry a
/// stringified reason so the store stays engine-type-free —
/// consumers render by label.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FundingArbEventEntry {
    pub at_ms: i64,
    /// One of `entered`, `exited`, `taker_rejected`,
    /// `pair_break`, `pair_break_uncompensated`, `hold`,
    /// `input_unavailable`. Same vocabulary as the
    /// `mm_funding_arb_transitions_total{outcome}` Prometheus
    /// label — intentional so UI mappings stay consistent.
    pub outcome: String,
    /// Optional reason string the engine attached. `""` for
    /// outcomes that don't carry a reason (Entered / Hold).
    pub reason: String,
}

const RING_CAP: usize = 20;

/// Capacity for the recent-decisions mirror. Decision ledger
/// records are light (~300 bytes each) so we can hold more
/// history than the funding-arb event ring without blowing the
/// process memory. 200 is what the current UI asks for.
const DECISIONS_CAP: usize = 200;

/// Ring capacity for SOR decisions mirror. Route-heavy symbols
/// emit a decision every fill-attempt cycle, so keep a healthy
/// window for the operator to scroll through.
const SOR_DECISIONS_CAP: usize = 100;

#[derive(Default)]
struct Inner {
    funding_arb: HashMap<String, VecDeque<FundingArbEventEntry>>,
    /// Per-symbol mirror of `DecisionLedger::recent()`. The
    /// engine replaces the whole slot on each publish tick; the
    /// agent reads on demand via the `decisions_recent` topic.
    /// Value type is `serde_json::Value` so the store stays
    /// engine-type-free (DecisionSnapshot lives in mm-risk
    /// which the dashboard doesn't depend on).
    decisions: HashMap<String, Vec<serde_json::Value>>,
    /// SOR routing decisions per symbol, newest-first ring.
    /// Engine pushes on every `publish_route_decision`; agent
    /// serves on the `sor_decisions_recent` topic. Cap
    /// `SOR_DECISIONS_CAP` so a hot route-heavy deployment
    /// doesn't blow memory.
    sor_decisions: HashMap<String, VecDeque<serde_json::Value>>,
}

#[derive(Default)]
pub struct DeploymentDetailsStore {
    inner: Mutex<Inner>,
}

impl DeploymentDetailsStore {
    /// Append one funding-arb event for `symbol`, dropping the
    /// oldest entry if the ring is already full.
    pub fn push_funding_arb_event(
        &self,
        symbol: &str,
        entry: FundingArbEventEntry,
    ) {
        let Ok(mut g) = self.inner.lock() else { return };
        let ring = g.funding_arb.entry(symbol.to_string()).or_default();
        if ring.len() >= RING_CAP {
            ring.pop_front();
        }
        ring.push_back(entry);
    }

    /// Snapshot the current ring for `symbol`. Returns an empty
    /// vec when nothing has been recorded yet. Newest-first so
    /// the UI can render without sorting.
    pub fn funding_arb_events(&self, symbol: &str) -> Vec<FundingArbEventEntry> {
        let Ok(g) = self.inner.lock() else { return Vec::new() };
        g.funding_arb
            .get(symbol)
            .map(|ring| ring.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// Replace the recent-decisions slot for `symbol`. Engine
    /// calls this on every publish tick with `DecisionLedger::recent(N)`
    /// serialised as JSON values. Caps at `DECISIONS_CAP` so a
    /// misconfigured caller can't blow memory.
    pub fn set_decisions_snapshot(&self, symbol: &str, snapshot: Vec<serde_json::Value>) {
        let Ok(mut g) = self.inner.lock() else { return };
        let capped = if snapshot.len() > DECISIONS_CAP {
            snapshot.into_iter().take(DECISIONS_CAP).collect()
        } else {
            snapshot
        };
        g.decisions.insert(symbol.to_string(), capped);
    }

    /// Read the latest decisions snapshot. Returns empty when
    /// the engine hasn't pushed one yet (fresh deployment).
    pub fn decisions_snapshot(&self, symbol: &str) -> Vec<serde_json::Value> {
        let Ok(g) = self.inner.lock() else { return Vec::new() };
        g.decisions.get(symbol).cloned().unwrap_or_default()
    }

    /// Append one SOR decision for `symbol`, evicting the
    /// oldest entry once the ring is full.
    pub fn push_sor_decision(&self, symbol: &str, decision: serde_json::Value) {
        let Ok(mut g) = self.inner.lock() else { return };
        let ring = g.sor_decisions.entry(symbol.to_string()).or_default();
        if ring.len() >= SOR_DECISIONS_CAP {
            ring.pop_front();
        }
        ring.push_back(decision);
    }

    /// Snapshot the SOR decisions ring for `symbol`,
    /// newest-first. Empty when the deployment hasn't routed
    /// anything yet.
    pub fn sor_decisions(&self, symbol: &str) -> Vec<serde_json::Value> {
        let Ok(g) = self.inner.lock() else { return Vec::new() };
        g.sor_decisions
            .get(symbol)
            .map(|ring| ring.iter().rev().cloned().collect())
            .unwrap_or_default()
    }
}

static GLOBAL: OnceLock<Arc<DeploymentDetailsStore>> = OnceLock::new();

/// Process-global store. Engine + agent reach through this to
/// share the ring buffers since they run in the same binary.
pub fn global() -> Arc<DeploymentDetailsStore> {
    GLOBAL
        .get_or_init(|| Arc::new(DeploymentDetailsStore::default()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_latest_entries() {
        let s = DeploymentDetailsStore::default();
        for i in 0..30 {
            s.push_funding_arb_event(
                "BTCUSDT",
                FundingArbEventEntry {
                    at_ms: i as i64,
                    outcome: "entered".into(),
                    reason: String::new(),
                },
            );
        }
        let events = s.funding_arb_events("BTCUSDT");
        assert_eq!(events.len(), RING_CAP);
        // Newest first → first at_ms is 29 (the most recent push).
        assert_eq!(events[0].at_ms, 29);
        // Oldest kept is 30 - RING_CAP = 10.
        assert_eq!(events[RING_CAP - 1].at_ms, 10);
    }

    #[test]
    fn unknown_symbol_returns_empty() {
        let s = DeploymentDetailsStore::default();
        assert!(s.funding_arb_events("ETHUSDT").is_empty());
    }

    #[test]
    fn separate_symbols_separate_rings() {
        let s = DeploymentDetailsStore::default();
        s.push_funding_arb_event(
            "BTCUSDT",
            FundingArbEventEntry {
                at_ms: 1,
                outcome: "entered".into(),
                reason: "x".into(),
            },
        );
        s.push_funding_arb_event(
            "ETHUSDT",
            FundingArbEventEntry {
                at_ms: 2,
                outcome: "exited".into(),
                reason: "y".into(),
            },
        );
        assert_eq!(s.funding_arb_events("BTCUSDT").len(), 1);
        assert_eq!(s.funding_arb_events("ETHUSDT").len(), 1);
        assert_eq!(s.funding_arb_events("BTCUSDT")[0].outcome, "entered");
        assert_eq!(s.funding_arb_events("ETHUSDT")[0].outcome, "exited");
    }
}
