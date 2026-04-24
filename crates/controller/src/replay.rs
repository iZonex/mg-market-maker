//! Per-agent ring buffer of recent commands for reconnect
//! replay.
//!
//! The controller keeps the last N commands it issued to each agent
//! in memory. When an agent reconnects it advertises its
//! `last_applied` cursor via the Register telemetry; the controller
//! replays every command in the buffer whose seq is strictly
//! greater, in order. Out-of-range requests (agent's cursor is
//! older than the buffer's tail) are logged + the agent
//! effectively does a full resync — the controller forgets how many
//! commands have been applied and re-issues its current desired
//! state.
//!
//! Sizing: 1024 commands per agent by default. A deploy is ~1
//! command; heartbeats are stateless so they don't fill the
//! buffer. 1024 covers hours of normal operation.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use mm_control::messages::CommandPayload;
use mm_control::seq::Seq;

const DEFAULT_BUFFER_CAP: usize = 1024;

#[derive(Debug, Clone)]
struct BufferedCommand {
    seq: Seq,
    payload: CommandPayload,
}

/// Shared replay store. Cheaply cloneable; the accept loop
/// gives every `AgentSession` a clone so sessions can append +
/// replay. Internal per-agent state is behind a single RwLock.
#[derive(Debug, Clone, Default)]
pub struct ReplayStore {
    inner: Arc<RwLock<HashMap<String, VecDeque<BufferedCommand>>>>,
}

impl ReplayStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a command sent to `agent_id`. Older entries drop
    /// off the front once the buffer exceeds the cap.
    pub fn append(&self, agent_id: &str, seq: Seq, payload: CommandPayload) {
        if let Ok(mut guard) = self.inner.write() {
            let buf = guard.entry(agent_id.to_string()).or_default();
            buf.push_back(BufferedCommand { seq, payload });
            while buf.len() > DEFAULT_BUFFER_CAP {
                buf.pop_front();
            }
        }
    }

    /// Return every command in the buffer with seq > `after`,
    /// in insertion order. When the buffer's oldest seq is
    /// already > `after + 1` the caller knows there's a gap
    /// (agent missed commands older than the buffer) and should
    /// re-push its full desired state.
    pub fn replay_after(&self, agent_id: &str, after: Seq) -> (Vec<CommandPayload>, bool) {
        let mut out = Vec::new();
        let mut gap = false;
        if let Ok(guard) = self.inner.read() {
            if let Some(buf) = guard.get(agent_id) {
                if let Some(first) = buf.front() {
                    // Gap is when the oldest buffered seq is
                    // strictly greater than after+1 — the agent
                    // is missing commands we've already dropped.
                    if first.seq.0 > after.0 + 1 && after.0 != 0 {
                        gap = true;
                    }
                }
                for entry in buf.iter() {
                    if entry.seq.is_after(after) {
                        out.push(entry.payload.clone());
                    }
                }
            }
        }
        (out, gap)
    }

    /// Drop an agent's buffer — called on clean disconnect so
    /// we don't accumulate state for agents that have
    /// permanently gone away.
    pub fn forget(&self, agent_id: &str) {
        if let Ok(mut guard) = self.inner.write() {
            guard.remove(agent_id);
        }
    }

    #[cfg(test)]
    fn len_for(&self, agent_id: &str) -> usize {
        self.inner
            .read()
            .map(|g| g.get(agent_id).map(|b| b.len()).unwrap_or(0))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hb() -> CommandPayload {
        CommandPayload::Heartbeat
    }

    #[test]
    fn replay_after_returns_strictly_newer() {
        let s = ReplayStore::new();
        s.append("a1", Seq(1), hb());
        s.append("a1", Seq(2), hb());
        s.append("a1", Seq(3), hb());
        let (rows, gap) = s.replay_after("a1", Seq(1));
        assert_eq!(rows.len(), 2, "seq 2 + 3 replayed");
        assert!(!gap, "no gap when cursor is within buffer");
    }

    #[test]
    fn replay_after_flags_gap_when_cursor_older_than_buffer_tail() {
        let s = ReplayStore::new();
        // Buffer head at seq=10; cursor is at seq=5 → missed
        // 6..=9, which are not in the buffer. Gap flag must fire.
        s.append("a1", Seq(10), hb());
        s.append("a1", Seq(11), hb());
        let (_, gap) = s.replay_after("a1", Seq(5));
        assert!(gap, "gap flag fires when buffer tail > cursor+1");
    }

    #[test]
    fn replay_after_with_zero_cursor_is_not_a_gap() {
        // Fresh agent advertises last_applied = 0; controller has
        // only seq >= 10 in the buffer. That's NOT a gap —
        // fresh agents are allowed to start mid-stream.
        let s = ReplayStore::new();
        s.append("a1", Seq(10), hb());
        let (rows, gap) = s.replay_after("a1", Seq(0));
        assert!(!gap);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn append_beyond_cap_drops_oldest() {
        let s = ReplayStore::new();
        for i in 1..=(DEFAULT_BUFFER_CAP + 10) {
            s.append("a1", Seq(i as u64), hb());
        }
        assert_eq!(s.len_for("a1"), DEFAULT_BUFFER_CAP);
        let (_, gap) = s.replay_after("a1", Seq(5));
        assert!(
            gap,
            "oldest entries dropped — cursor inside dropped window is a gap"
        );
    }

    #[test]
    fn forget_removes_agent_state() {
        let s = ReplayStore::new();
        s.append("a1", Seq(1), hb());
        s.forget("a1");
        assert_eq!(s.len_for("a1"), 0);
    }
}
