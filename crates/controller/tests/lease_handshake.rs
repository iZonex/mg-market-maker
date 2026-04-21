//! End-to-end PR-1 acceptance test.
//!
//! Spawns an `AgentSession` (controller-side) and a `LeaseClient`
//! (agent-side) wired together through an in-memory transport
//! pair. Verifies that:
//!
//! 1. **Registration handshake**: the agent's `Register`
//!    telemetry reaches the controller and the controller responds with a
//!    freshly-issued lease.
//! 2. **Authority transition**: the agent's `AuthorityHandle`
//!    reports `Held(_)` once the lease lands.
//! 3. **Refresh loop**: with a short lease TTL the agent
//!    requests a refresh and the controller issues a new expiry.
//! 4. **Expiration → terminal state**: if the controller stops
//!    responding, the agent's `run()` returns `AuthorityLost`.
//!
//! These four properties together are the skeleton contract PR-2
//! will build on (by having the agent's engine pool subscribe to
//! the `AuthorityHandle` and walk the fail-ladder on terminal
//! states).

use std::time::Duration;

use mm_agent::{AgentConfig, LeaseClient};
use mm_controller::{AgentSession, LeasePolicy};
use mm_control::in_memory_pair;
use mm_control::lease::LeaseState;
use mm_control::messages::AgentId;

#[tokio::test]
async fn agent_registers_and_receives_lease() {
    let (controller_side, agent_side) = in_memory_pair();
    let session = AgentSession::new(controller_side, LeasePolicy::default());

    let (client, authority) = LeaseClient::new(
        agent_side,
        AgentConfig {
            id: AgentId::new("eu-test-01"),
            ..Default::default()
        },
    );

    // Controller loop runs concurrently; we only need it up long
    // enough to grant the first lease.
    let controller_task = tokio::spawn(async move { session.run_until_disconnect().await });
    let agent_task = tokio::spawn(async move { client.run().await });

    // Wait for the authority handle to transition Unclaimed → Held.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let state = authority.current();
        if matches!(state, LeaseState::Held(_)) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("agent never received a lease (state = {:?})", state);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Tear down cleanly: aborting the agent drops its transport
    // endpoint which cascades Ok(None) into the controller loop.
    agent_task.abort();
    let _ = agent_task.await;
    let _ = controller_task.await;
}

#[tokio::test]
async fn agent_refreshes_before_lease_expires() {
    let (controller_side, agent_side) = in_memory_pair();
    // Short lease so the test finishes quickly. Refresh policy
    // min_refresh_interval stays at default but the ttl is tiny —
    // we're proving the agent DOES request a refresh, not rate-
    // limit behaviour.
    let policy = LeasePolicy {
        lease_ttl: Duration::from_millis(600),
        min_refresh_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let session = AgentSession::new(controller_side, policy);

    // Refresh at 1/3 of lifetime = ~200ms. We'll watch for two
    // distinct lease issuances in the authority channel.
    let (client, mut authority) = LeaseClient::new(
        agent_side,
        AgentConfig {
            id: AgentId::new("eu-test-02"),
            refresh_at_fraction: 1.0 / 3.0,
        },
    );

    let controller_task = tokio::spawn(async move { session.run_until_disconnect().await });
    let agent_task = tokio::spawn(async move { client.run().await });

    // Collect distinct issuance timestamps — the refresh path
    // reuses lease_id by design, so the signal of "refresh
    // happened" is the `issued_at` moving forward.
    let mut first_issued_at = None;
    let mut second_issued_at = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(200), authority.changed()).await;
        if let LeaseState::Held(lease) = authority.current() {
            match first_issued_at {
                None => first_issued_at = Some(lease.issued_at),
                Some(first) if lease.issued_at > first => {
                    second_issued_at = Some(lease.issued_at);
                    break;
                }
                _ => {}
            }
        }
    }

    assert!(first_issued_at.is_some(), "agent never got the first lease");
    assert!(
        second_issued_at.is_some(),
        "agent did not request a refresh before the lease expired"
    );

    agent_task.abort();
    let _ = agent_task.await;
    let _ = controller_task.await;
}

#[tokio::test]
async fn agent_loses_authority_when_controller_goes_silent() {
    let (controller_side, agent_side) = in_memory_pair();
    let policy = LeasePolicy {
        lease_ttl: Duration::from_millis(300),
        min_refresh_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let session = AgentSession::new(controller_side, policy);

    let (client, authority) = LeaseClient::new(
        agent_side,
        AgentConfig {
            id: AgentId::new("eu-test-03"),
            refresh_at_fraction: 1.0 / 3.0,
        },
    );

    // Start the controller, let it grant exactly one lease, then drop
    // it so all subsequent refresh requests go nowhere. The
    // agent should observe its lease expire and return from
    // run() with AuthorityLost.
    let controller_task = tokio::spawn(async move { session.run_until_disconnect().await });
    let agent_run = tokio::spawn(async move { client.run().await });

    // Wait for the first lease.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if matches!(authority.current(), LeaseState::Held(_)) {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("agent never received a lease");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Kill the controller session; the agent will keep trying to
    // refresh but its transport now has no sender side for the
    // controller channel, so the refresh ACK never arrives and the
    // lease runs out.
    controller_task.abort();
    let _ = controller_task.await;

    // The agent's run() should terminate within ~2× ttl after
    // the controller goes silent (one refresh miss + lease elapse).
    let res = tokio::time::timeout(Duration::from_secs(3), agent_run)
        .await
        .expect("agent run timed out waiting for authority-loss")
        .expect("agent task panicked");

    assert!(
        res.is_err(),
        "agent reported clean shutdown instead of authority loss"
    );
}
