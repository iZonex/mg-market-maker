//! Leader lease — the "authority" layer of the three-layer
//! dead-man's switch.
//!
//! The controller grants a lease with an absolute expiry time. The
//! agent refreshes it well before expiry (3× margin is the
//! default — refresh every 10s, expire at 30s). When the lease
//! expires the agent loses trading authority and MUST trigger
//! its configured fail-ladder (widen / stop / flatten — see
//! [`crate::fail_ladder`]) regardless of whether the underlying
//! transport is still connected.
//!
//! Why separate from heartbeat: the heartbeat layer protects
//! against silent transport (liveness); the lease protects
//! against "controller is alive but has disavowed this agent"
//! (authority). Revoking a lease is a first-class command — the
//! controller can kill an agent's authority even when the WS link is
//! healthy.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A signed, time-bounded grant of trading authority. The agent
/// acts only while its held lease is current.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaderLease {
    pub lease_id: uuid::Uuid,
    pub agent_id: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// The sequence number this lease was attached to, so the
    /// agent can reject out-of-order lease issuances (a late
    /// packet from a previous session should not reinstate
    /// authority that the controller has already revoked).
    pub issued_seq: crate::seq::Seq,
}

impl LeaderLease {
    /// True iff `now` is strictly before the lease expiry. No
    /// grace period here — grace lives at the watchdog layer.
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at
    }

    /// Fraction of the lease's total lifetime already consumed.
    /// Used by the agent's refresh loop to decide when to ask for
    /// an extension — typical policy is "refresh at 1/3 consumed".
    pub fn consumed_fraction_at(&self, now: DateTime<Utc>) -> f32 {
        let total = (self.expires_at - self.issued_at).num_milliseconds().max(1);
        let spent = (now - self.issued_at).num_milliseconds().clamp(0, total);
        (spent as f32) / (total as f32)
    }
}

/// Agent-side view of lease state. Held in memory, reset on each
/// successful refresh. Not persisted: crash recovery must fall
/// through to "no lease, request fresh" rather than reusing a
/// potentially-revoked one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseState {
    /// Agent has not yet received its first lease from controller.
    /// Engines MUST NOT quote in this state.
    Unclaimed,
    /// Agent holds `lease` and it has not yet expired.
    Held(LeaderLease),
    /// Held a lease that has now expired; agent is executing the
    /// fail-ladder. Engines are being wound down.
    Expired(LeaderLease),
    /// Controller explicitly revoked our authority. Same effect as
    /// `Expired` but labelled separately for audit clarity.
    Revoked {
        previous: LeaderLease,
        reason: String,
    },
}

impl LeaseState {
    /// True iff trading is currently authorised.
    pub fn is_authorised_at(&self, now: DateTime<Utc>) -> bool {
        matches!(self, LeaseState::Held(l) if l.is_valid_at(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn lease(issued: DateTime<Utc>, ttl_secs: i64) -> LeaderLease {
        LeaderLease {
            lease_id: uuid::Uuid::nil(),
            agent_id: "agent-1".into(),
            issued_at: issued,
            expires_at: issued + Duration::seconds(ttl_secs),
            issued_seq: crate::seq::Seq(1),
        }
    }

    #[test]
    fn lease_valid_until_expiry() {
        let t0 = Utc::now();
        let l = lease(t0, 30);
        assert!(l.is_valid_at(t0));
        assert!(l.is_valid_at(t0 + Duration::seconds(29)));
        assert!(!l.is_valid_at(t0 + Duration::seconds(30)));
    }

    #[test]
    fn consumed_fraction_monotonic() {
        let t0 = Utc::now();
        let l = lease(t0, 30);
        assert_eq!(l.consumed_fraction_at(t0), 0.0);
        let half = l.consumed_fraction_at(t0 + Duration::seconds(15));
        assert!((0.48..=0.52).contains(&half));
        assert!((l.consumed_fraction_at(t0 + Duration::seconds(40)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lease_state_authority_follows_expiry() {
        let t0 = Utc::now();
        let l = lease(t0, 30);
        assert!(LeaseState::Held(l.clone()).is_authorised_at(t0));
        assert!(!LeaseState::Held(l.clone()).is_authorised_at(t0 + Duration::seconds(31)));
        assert!(!LeaseState::Unclaimed.is_authorised_at(t0));
        assert!(!LeaseState::Expired(l.clone()).is_authorised_at(t0));
        assert!(!LeaseState::Revoked {
            previous: l,
            reason: "test".into()
        }
        .is_authorised_at(t0));
    }
}
