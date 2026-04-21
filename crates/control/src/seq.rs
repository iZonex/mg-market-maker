//! Monotonic sequence numbers and the persisted cursor.
//!
//! Every command the controller emits carries a [`Seq`]. The agent writes
//! its last-applied command seq to disk (the [`Cursor`]). On
//! reconnect the agent sends the cursor and the controller resumes the
//! stream right after that seq — no replay gaps, no duplicates.
//!
//! Telemetry in the opposite direction uses an independent
//! per-agent seq so the controller can detect upstream drops without
//! entangling the two directions.

use serde::{Deserialize, Serialize};

/// Opaque monotonic u64. Newtype so the two directions (command /
/// telemetry) don't accidentally get crossed at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    pub const ZERO: Seq = Seq(0);

    pub fn next(self) -> Self {
        Seq(self.0.saturating_add(1))
    }

    pub fn is_after(self, other: Seq) -> bool {
        self.0 > other.0
    }
}

/// The per-direction cursor an endpoint persists so a reconnect
/// can ask "resume after N" and the peer knows exactly where to
/// pick up. Serialized as JSON on disk — shape kept small on
/// purpose so a corrupted cursor file is cheap to replace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cursor {
    pub last_applied: Seq,
    /// UTC millis at which the cursor advanced. Cheap health hint:
    /// if the cursor hasn't moved for hours the control plane is
    /// silent, regardless of whether the transport itself is up.
    pub updated_at_ms: i64,
}

impl Cursor {
    pub fn fresh() -> Self {
        Self {
            last_applied: Seq::ZERO,
            updated_at_ms: chrono::Utc::now().timestamp_millis(),
        }
    }

    pub fn advance(&mut self, to: Seq) {
        if to.is_after(self.last_applied) {
            self.last_applied = to;
            self.updated_at_ms = chrono::Utc::now().timestamp_millis();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_next_increments() {
        assert_eq!(Seq(5).next(), Seq(6));
    }

    #[test]
    fn cursor_only_advances_forward() {
        let mut c = Cursor::fresh();
        c.advance(Seq(3));
        let t1 = c.updated_at_ms;
        c.advance(Seq(2));
        assert_eq!(c.last_applied, Seq(3), "older seq is ignored");
        assert_eq!(c.updated_at_ms, t1, "ts not touched on no-op");
        c.advance(Seq(5));
        assert_eq!(c.last_applied, Seq(5));
    }

    #[test]
    fn seq_saturates_at_max() {
        assert_eq!(Seq(u64::MAX).next(), Seq(u64::MAX));
    }
}
